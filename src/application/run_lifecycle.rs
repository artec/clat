use crate::CancelToken;
use crate::event::{EventSink, RunEvent};
use crate::model::Usage;
use crate::permission::PermissionApprover;
use crate::plugin::{Plugin, ScopeKind};
use crate::plugins::run_catalog;
use crate::plugins::services::{AgentFailure, AgentRequest, RUN_SCOPE_SERVICE};
use crate::session::event::{TurnEndCancelCause, TurnEndReason, payloads};
use crate::session::id::SessionId;
use crate::session::recorder::SessionRecorder;
use crate::session::run_journal::{NewSessionEvent, RunJournal};
use crate::session::use_cases::SetTitleExpectation;
use serde_json::json;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread::JoinHandle;

use super::*;

use super::compaction::{RecorderHandle, run_auto_compaction};
use super::threads::MCP_STARTUP_RUN_WAIT;
use super::title::AutotitleJob;

impl TrustedProjectApplication {
    pub fn start_run(
        &mut self,
        request: ApplicationRunRequest,
    ) -> Result<RunHandle, ApplicationError> {
        // An ordinary human prompt competes with any previously armed goal.
        // Preserve durable phase, remove only process-local continuation
        // authority, matching DSH goal-round-driver's competing prompt fence.
        self.goal.disarm();
        let client_message_id = request.message.client_message_id.clone();
        let cancel = CancelToken::new();
        let catalog = run_catalog(cancel.clone(), Arc::clone(&request.approver));
        self.start_run_with_catalog(request, catalog, None)
            .map_err(|error| {
                error.with_rollback_if_unclassified(client_message_id, "run-start-pre-commit")
            })
    }

    /// Start the explicitly armed bounded goal driver. The caller supplies
    /// normal frontend channels, but core owns the exact synthetic prompt and
    /// its durable `source.kind=goal` admission record.
    pub fn start_goal_run(
        &mut self,
        mut request: ApplicationRunRequest,
    ) -> Result<(RunHandle, String), ApplicationError> {
        if !request.message.staged_attachments.is_empty() {
            return Err(ApplicationError::new(
                "goal continuation rounds do not accept attachments",
            ));
        }
        let round = self.goal.next_round().map_err(ApplicationError::new)?;
        // goal 续轮是 core 合成消息：覆盖客户端载荷，无幂等键。
        request.message = crate::message::PendingMessage::text(round.prompt.clone());
        let visible_prompt = round.prompt.clone();
        let remaining = self
            .goal
            .remaining_time()
            .ok_or_else(|| ApplicationError::new("no current goal time budget"))?;
        if remaining.is_zero() {
            self.goal.disarm();
            return Err(ApplicationError::new("goal time budget is exhausted"));
        }
        let cancel = CancelToken::with_deadline(std::time::Instant::now() + remaining);
        let catalog = run_catalog(cancel, Arc::clone(&request.approver));
        match self.start_run_with_catalog(request, catalog, Some(round)) {
            Ok(handle) => Ok((handle, visible_prompt)),
            Err(error) => {
                self.goal.disarm();
                Err(error)
            }
        }
    }

    /// The durable prelude of a run: ensure a session with a live writer,
    /// then atomically append `turn/start` + `user/message` and flush —
    /// the model is only called after the durable batch committed. For a
    /// fresh session the fact lands FIRST (MP-1 §4.3); workspace
    /// registration and the selection pointer follow as projections, so a
    /// crash between them is healed by mount-time reconciliation.
    ///
    /// MM-1A：typed 消息在这里完成接纳——附件导入（复制 + 元数据实
    /// 测）、`admitted_user_message` 载荷（descriptor 元数据 + 幂等键 +
    /// digest 随事件耐久）、committed 回执构造。commit point 即
    /// append+flush：此后的一切失败都带着 `Committed` 回执返回
    ///（INV-M1A-4）。
    fn prepare_run(
        &mut self,
        message: &crate::message::PendingMessage,
        goal_round: Option<&crate::goal::GoalRound>,
    ) -> Result<PreparedRun, ApplicationError> {
        let prompt = message.content.plain_text();
        let attachments = &message.staged_attachments;
        // INV-MM2-2（MM-2 W1 attach 门）：模型能力不收图 → 整轮拒绝，
        // 且必须发生在任何会话物化/journal 写入**之前**（零痕迹）。
        // 错误可行动：点名「换视觉模型（如 GLM 5.3 Flash）/移除图片」。
        // 能力快照来自 model_state（preset stamp 或 custom 持久值），
        // unverified 的视觉声明（仅厂商文档）同样拒绝——无第一方
        // 证据不开图。
        if !attachments.is_empty() {
            let (config, _) = self.model_state()?;
            if !config.capabilities.accepts_image_input() {
                let names = attachments
                    .iter()
                    .map(|path| path.display().to_string())
                    .collect::<Vec<_>>()
                    .join(", ");
                return Err(ApplicationError::new(format!(
                    "this model ({}) does not accept image input; switch to a vision model (e.g. GLM 5.3 Flash) or remove the image(s): {names}",
                    config.model
                )));
            }
            if attachments.len() > config.image_policy.max_images {
                return Err(ApplicationError::new(format!(
                    "this model ({}) accepts at most {} images per message; remove {} image(s) before sending",
                    config.model,
                    config.image_policy.max_images,
                    attachments.len() - config.image_policy.max_images,
                )));
            }
        }
        let pending_goal_birth = self
            .goal
            .pending_birth_event()
            .map_err(ApplicationError::new)?;
        let mut materialized: Option<SessionId> = None;
        match self.selection.clone() {
            None => {
                // `new_session` no longer self-quiesces (the /new flow
                // persists the pointer first); the run path detaches
                // whatever is still active.
                self.sessions.quiesce_active().map_err(session_error)?;
                let summary = self
                    .sessions
                    .new_session(&self.project_key())
                    .map_err(session_error)?;
                materialized = Some(summary.id);
            }
            Some(id) => {
                if self.sessions.active_id().as_ref() != Some(&id) {
                    // Mounted but not attached (e.g. after a failed load):
                    // attach now or fail loudly.
                    self.sessions.quiesce_active().map_err(session_error)?;
                    self.sessions
                        .resume(&self.session_key(&id))
                        .map_err(session_error)?;
                    // 换了活跃会话：档位 cell 对齐到它自己的 fold（PS1）。
                    self.reseed_permission_mode_from_session();
                    self.fresh_session_open = true;
                    self.emitted_request_header = self.sessions.last_request_header();
                }
            }
        }
        let id = self
            .sessions
            .active_id()
            .ok_or_else(|| ApplicationError::new("no active session for the run"))?;
        // Fresh 刚物化的新会话：TodoService 挂靠该会话（空清单起步）。
        // resume/switch 路径在挂载时已从投影恢复。
        if let Some(todo_service) = &self.todo
            && todo_service.session().as_ref() != Some(&id)
        {
            todo_service.restore(Some(id.clone()), &[]);
        }
        let turn = self.sessions.active_turns().map_err(session_error)? + 1;
        let journal = self.sessions.journal().map_err(session_error)?;
        // 附件导入（M4；MM-1A 元数据化）：会话目录已物化，先复制、后
        // 落盘——journal 拿到的是会话附件目录内的绝对引用 + descriptor
        // 元数据。校验失败（不存在/类型/超大）发生在任何 journal 写入
        // 之前，本轮不留任何痕迹。
        let images = self
            .sessions
            .import_attachments(attachments)
            .map_err(session_error)?;
        if !images.is_empty() {
            let (config, _) = self.model_state()?;
            validate_route_images(&config, &images).map_err(ApplicationError::new)?;
        }
        // 首个耐久批：出生档（仅新物化的会话）→ turn/start →
        // user/message。DSH pinInitialPermission 在会话创建期 pin 档位，
        // 对应物即出生事件排在首个 turn 之前（PS2）——回放从第一条
        // 事件起就有确定的档位。Classic（exec）不落此事件（PS4）。
        let plan_birth = materialized.is_some() && self.plan_mode.pending_birth();
        let mut first_batch = Vec::new();
        if materialized.is_some() && self.permission_modes_enabled {
            first_batch.push(NewSessionEvent::new(
                "sandbox/mode",
                payloads::sandbox_mode(&self.permission_mode()),
            ));
        }
        if plan_birth {
            first_batch.push(NewSessionEvent::new("plan/mode", json!({ "active": true })));
        }
        if materialized.is_some()
            && let Some(event) = pending_goal_birth
        {
            first_batch.push(event);
        }
        first_batch.push(NewSessionEvent::new(
            "turn/start",
            payloads::turn_start(turn),
        ));
        // MM-1A：普通消息走 admitted 载荷（幂等键 + 提交 digest + 图块
        // 元数据；message id 由回执持有）。digest 是**提交幂等**身份
        //（文本 + staged 引用），不掺导入后重铸的 attachmentId——
        // 崩溃重试不得翻案。goal 轮是 core 合成消息，沿用 GoalRound
        // 预构造载荷（无客户端键）。
        let message_id = uuid::Uuid::new_v4().to_string();
        let request_digest = message
            .client_message_id
            .as_ref()
            .map(|_| message.request_digest());
        let user_payload = match goal_round {
            Some(round) => round.message.clone(),
            None => payloads::admitted_user_message(
                &message_id,
                prompt.as_str(),
                &images,
                message.client_message_id.as_deref(),
                request_digest.as_deref(),
            ),
        };
        first_batch.push(NewSessionEvent::new("user/message", user_payload).append(Vec::new()));
        journal
            .append_atomic(&first_batch)
            .map_err(|error| ApplicationError::new(format!("session append failed: {error}")))?;
        journal
            .flush()
            .map_err(|error| ApplicationError::new(format!("session flush failed: {error}")))?;

        // The append+flush above is the sole admission commit point. Build
        // the receipt immediately: every fallible projection below must
        // preserve this committed fact instead of returning an ambiguous
        // plain startup error.
        let mut admitted_blocks = message.content.blocks.clone();
        admitted_blocks.extend(
            images
                .iter()
                .map(|image| crate::message::ContentBlock::Image {
                    attachment: image.descriptor.clone(),
                }),
        );
        let admitted = crate::message::MessageContent::from_blocks(admitted_blocks);
        let receipt = message.client_message_id.clone().map(|client_message_id| {
            Box::new(crate::message::AdmissionReceipt::committed(
                client_message_id,
                message_id,
                images
                    .iter()
                    .map(|image| image.descriptor.attachment_id.clone())
                    .collect(),
            ))
        });
        let post_commit_error = |error: ApplicationError, phase: &'static str| {
            let failed_receipt = receipt
                .clone()
                .map(|receipt| Box::new((*receipt).with_failure_phase(phase)));
            error.with_receipt(failed_receipt)
        };
        if plan_birth {
            self.plan_mode.materialized();
        }
        if materialized.is_some() {
            self.goal
                .materialized(&id)
                .map_err(ApplicationError::new)
                .map_err(|error| post_commit_error(error, "goal-materialize"))?;
            self.subagents.materialized(&id);
        }
        // 事实已耐久：投影随后（注册工作区 + 账本 + 指针）。两者之间的
        // 崩溃由挂载期对账收编自愈（会话日志永远赢）。
        if let Some(id) = materialized {
            self.ensure_registered()
                .map_err(|error| post_commit_error(error, "workspace-register"))?;
            if let Some(workspace_id) = self.workspace_id.clone() {
                self.control
                    .append_session_to_workspace(&workspace_id, id.as_str())
                    .map_err(|error| ApplicationError::new(error.to_string()))
                    .map_err(|error| post_commit_error(error, "workspace-ledger"))?;
            }
            self.selection = Some(id.clone());
            self.persist_selection(Some(&id))
                .map_err(|error| post_commit_error(error, "selection-pointer"))?;
        }
        // The model-facing history is shared with `/context`: compaction surface
        // plus the same non-durable todo runtime item, so inspection cannot drift
        // from the next real request.
        let history = self
            .current_model_history()
            .map_err(|error| post_commit_error(error, "history-rebuild"))?;
        Ok(PreparedRun {
            session_id: id,
            turn,
            history,
            journal,
            goal_round_started: goal_round.map(|round| round.started_at),
            message: admitted,
            client_message_id: message.client_message_id.clone(),
            receipt,
        })
    }

    fn start_run_with_catalog(
        &mut self,
        request: ApplicationRunRequest,
        run_plugins: Vec<Arc<dyn Plugin>>,
        goal_round: Option<crate::goal::GoalRound>,
    ) -> Result<RunHandle, ApplicationError> {
        if let Some(previous) = self
            .active_run
            .as_ref()
            .filter(|handle| !handle.is_finished())
        {
            // W1-04：队列已封口 = run 已判定终态、只剩收尾（steer
            // 回退普通提交正是落在这个窗口）。有界等待收尾完成后
            // 允许立即开新 run；未到终态的活动 run 仍然拒绝。
            if !previous.steering.is_sealed() {
                return Err(ApplicationError::new("another run is already active"));
            }
            if !wait_for_sealed_wrapup(previous) {
                return Err(ApplicationError::new(
                    "the previous run is still finishing; submit again in a moment",
                ));
            }
        }
        if let Some(previous) = self.active_run.take() {
            previous.join()?;
        }
        if !self.fresh_session_open
            && let Some(header) = self.sessions.last_request_header()
        {
            // A dynamic project-instruction change may have appended a
            // request/header after the run started. Re-seed dedupe from the
            // durable projection instead of the start-time snapshot.
            self.emitted_request_header = Some(header);
        }
        if self
            .active_compaction
            .as_ref()
            .is_some_and(|handle| !handle.is_finished())
        {
            return Err(ApplicationError::new("a compaction is already active"));
        }
        if let Some(previous) = self.active_compaction.take() {
            previous.join()?;
        }
        // 模型配置检查在前：无模型时立即失败，不为 MCP 白等。
        let (config, credentials) = self.model_state()?;
        if !config.is_configured() {
            return Err(ApplicationError::new(
                "model is not configured; configure a model and endpoint first",
            ));
        }
        // MCP 后台启动落定（有界等待）后再冻结工具注册表（INV-M2/M3）：
        // 任何 run 看到的都是完整工具集——除非等待超时（此时以现状冻结，
        // 状态面板可见未落定 server，修复后需重启以重新挂载）。无 MCP
        // 配置时立即返回，零等待成本。
        let _settled = self.mcp_status.wait_until_settled(MCP_STARTUP_RUN_WAIT);
        self.tools
            .freeze()
            .map_err(|error| ApplicationError::new(error.to_string()))?;
        self.prompts.freeze();
        let instruction_snapshot = self
            .dynamic_instructions
            .snapshot()
            .map_err(ApplicationError::new)?;
        let skill_snapshot = self.skills.snapshot().map_err(ApplicationError::new)?;
        let memory_query = request.message.content.plain_text();
        // Image-only is a valid multimodal user message. Memory retrieval is
        // text-indexed, so use a stable semantic sentinel rather than letting
        // its non-empty query guard reject the entire run before admission.
        let memory_query = if memory_query.is_empty()
            && (!request.message.staged_attachments.is_empty()
                || request.message.content.has_images())
        {
            "[image-only user message]"
        } else {
            memory_query.as_str()
        };
        let memory_injection = self
            .memory
            .injection(memory_query)
            .map_err(ApplicationError::new)?;
        let ApplicationRunRequest {
            message: request_message,
            approver,
            asker,
            events,
            completion,
        } = request;
        // ask-user 前端按本次请求安装（None 清除旧实现——headless 与
        // 交互前端交替使用同一 Application 时正确降级）。插件宿主桥
        // 的 elicitation 与它共享同一前端实现。
        let asker_for_host = asker.clone();
        self.asker_slot.install(asker);
        // 标题生成与 worker 各自持有模型配置副本；原值留在主线程，
        // 等 session birth state 耐久后再构造 request/header 与宿主上下文。
        let title_config = config.clone();
        let title_credentials = credentials.clone();
        let worker_config = config.clone();
        let worker_credentials = credentials.clone();
        let mut run_scope = self
            .project_manager
            .as_mut()
            .ok_or_else(|| ApplicationError::new("project scope is closed"))?
            .child(ScopeKind::Run)
            .map_err(|error| ApplicationError::new(error.to_string()))?;
        run_scope
            .mount_all(run_plugins)
            .map_err(|error| ApplicationError::new(error.to_string()))?;
        let resources = run_scope
            .require(RUN_SCOPE_SERVICE)
            .map_err(|error| ApplicationError::new(error.to_string()))?;
        let cancel = resources.cancel.clone();
        // 宿主桥按本次 run 安装（INV-S1）：sampling 的权限门/记账与
        // elicitation 的问答拿到的都是本 run 的模型配置、审批人与前
        // 端；worker 收尾 clear（跨 run 不泄漏旧 approver）。记账单元
        // 与 recorder 共享（INV-S6：journal 落账点在 ModelResponded）。
        // sampling 预算（W1-03）同样 per-run：独立于权限档位的花费闸
        // 门，跨 WASM/MCP/DSH 三种传输共用，run 结束即随上下文丢弃。
        let sampling_usage = Arc::new(Mutex::new(Usage::default()));
        let sampling_budget = Arc::new(Mutex::new(crate::plugin_host::SamplingBudget::per_run()));
        let busy = Arc::new(AtomicBool::new(true));
        let join_slot = Arc::new(Mutex::new(None));
        // In-run steering: the same queue is shared by the frontend
        // (`steer`), the handle (`RunHandle::steering`), and the worker's
        // `AgentRequest` — the run drains it at each model-request boundary.
        let steering = crate::run::SteeringQueue::new();
        let handle = RunHandle {
            cancel: cancel.clone(),
            busy: Arc::clone(&busy),
            join: Arc::clone(&join_slot),
            steering: steering.clone(),
        };
        let sessions = Arc::clone(&self.sessions);
        let agent = Arc::clone(&self.agent);
        let process_service = Arc::clone(&self.process_service);
        let monitor = Arc::clone(&self.monitor);
        let compactor = self.compactor.clone();
        let todo_service = self.todo.clone();
        let goal_service = Arc::clone(&self.goal);
        let subagent_service = Arc::clone(&self.subagents);
        let titler = self.titler.clone();
        let title_sender = self
            .title_worker
            .as_ref()
            .map(|worker| worker.sender.clone());
        let subscribers = Arc::clone(&self.subscribers);
        let steering_for_worker = steering.clone();
        let plugin_host_worker = Arc::clone(&self.plugin_host);
        let tool_access_worker = Arc::clone(&self.tool_access);
        let skill_catalog_worker = Arc::clone(&self.skill_catalog);
        let view_image_worker = Arc::clone(&self.view_image);
        let sampling_usage_worker = Arc::clone(&sampling_usage);
        let cancel_worker = cancel.clone();
        let approver_worker = Arc::clone(&approver);
        // 档位快照（run 起点）：仅进系统指令说明。决策读共享 cell，
        // 运行中切档即时生效（P3）。
        let permission_mode_snapshot = self
            .permission_modes_enabled
            .then(|| self.permission_mode());
        // 门控通道（A-03 不变量）：worker 先就位并阻塞等待；持久化预备
        // 在 spawn 之后才发生——mount/spawn 失败不可能留下一条已落盘、
        // 却永远得不到回答的 user 消息；预备失败则撤掉发送端，worker
        // 干净退出，同样不留半份状态。用户消息在模型执行前已耐久。
        let (start_sender, start_receiver) = mpsc::sync_channel::<WorkerStart>(1);
        let fail_run_start_receive = {
            #[cfg(test)]
            {
                std::mem::take(&mut self.fail_next_run_start_receive)
            }
            #[cfg(not(test))]
            {
                false
            }
        };
        let (receiver_closed_sender, receiver_closed_receiver) = mpsc::sync_channel::<()>(0);
        #[cfg(test)]
        if std::mem::take(&mut self.fail_next_run_spawn) {
            return Err(ApplicationError::new(
                "intentional run worker spawn failure",
            ));
        }
        let worker = std::thread::Builder::new()
            .name("clat-run".into())
            .spawn(move || {
                let cancel = cancel_worker;
                let request_approver = approver_worker;
                if fail_run_start_receive {
                    // W7 fault seam: prove that a worker-start channel loss
                    // after durable admission reports Committed rather than
                    // inviting a duplicate resend.
                    drop(start_receiver);
                    let _ = receiver_closed_sender.send(());
                    let _ = run_scope.close();
                    plugin_host_worker.clear();
                    tool_access_worker.clear();
                    skill_catalog_worker.clear();
                    view_image_worker.clear();
                    busy.store(false, Ordering::Release);
                    return;
                }
                let start = match start_receiver.recv() {
                    Ok(start) => start,
                    Err(_) => {
                        // 发送端被撤（预备失败）：持久层无状态可清理，但
                        // 宿主桥上下文是 run 启动路径上装的——卸掉。
                        let _ = run_scope.close();
                        plugin_host_worker.clear();
                        tool_access_worker.clear();
                        skill_catalog_worker.clear();
                        view_image_worker.clear();
                        busy.store(false, Ordering::Release);
                        return;
                    }
                };
                let WorkerStart {
                    prepared,
                    request_header,
                    header_reason,
                    context,
                } = start;
                let PreparedRun {
                    session_id,
                    turn,
                    mut history,
                    journal,
                    goal_round_started,
                    message: mut current_message,
                    client_message_id: mut current_client_id,
                    receipt: run_receipt,
                } = prepared;
                // todo（INV-T3）：事件直达日志——write 在绑定 journal 上
                // 追加 todo/write，恢复走 todo 投影。
                if let Some(todo_service) = &todo_service {
                    todo_service.bind_run(&session_id, Arc::clone(&journal));
                }
                let subagent_bind_error = subagent_service
                    .bind_run(
                        &session_id,
                        turn,
                        Arc::clone(&journal),
                        worker_config.clone(),
                        worker_credentials.clone(),
                    )
                    .err();
                let captured_text = Arc::new(Mutex::new(String::new()));
                let ui_events: Box<dyn EventSink + Send> = Box::new(CapturingEventSink {
                    inner: events,
                    text: Arc::clone(&captured_text),
                });
                let ui_sink = Arc::new(Mutex::new(ui_events));
                let goal_service = Arc::clone(&goal_service);
                let mut current_turn = turn;
                let goal_mode = goal_round_started.is_some();
                let mut round_started =
                    goal_round_started.unwrap_or_else(std::time::Instant::now);
                let mut first_round = true;
                let mut durable_request_header = request_header;
                let mut aggregate_usage = Usage::default();
                let mut aggregate_turns = 0usize;
                let result = loop {
                    subagent_service.update_turn(current_turn);
                    // Every round starts from a durable user/message and may
                    // compact only the already-committed surface before it.
                    if let Some(compactor) = &compactor {
                        let note = run_auto_compaction(
                            compactor.as_ref(),
                            sessions.as_ref(),
                            journal.as_ref(),
                            &worker_config,
                            &worker_credentials,
                            &cancel,
                            current_turn,
                        );
                        if let Some(note) = note {
                            broadcast_to(
                                &subscribers,
                                ApplicationEvent::CompactionUpdated(CompactionStatus::Finished {
                                    note: note.0,
                                    succeeded: note.1,
                                }),
                            );
                        }
                        if let Ok(nodes) = sessions.surface_nodes() {
                            history = nodes.into_iter().map(|(_, item)| item).collect();
                        }
                    }
                    plugin_host_worker.update_run_metadata(&session_id.to_string(), &history);
                    let mut round_request_header = durable_request_header.clone();
                    let mut round_workflow_instructions = context.workflow_instructions.clone();
                    let mut round_header_reason = first_round.then_some(header_reason).flatten();
                    let goal_refresh_error = if goal_mode {
                        match goal_service.injection() {
                            Ok(goal) => {
                                let workflow = crate::plan_mode::compose_workflow_instructions(
                                    context.workflow_base.clone(),
                                    (!goal.instructions.is_empty())
                                        .then_some(goal.instructions.as_str()),
                                );
                                round_workflow_instructions =
                                    (!workflow.is_empty()).then_some(workflow.clone());
                                if let Some(header) = round_request_header.header.as_object_mut() {
                                    if goal.header.is_null() {
                                        header.remove("goal");
                                    } else {
                                        header.insert("goal".into(), goal.header);
                                    }
                                }
                                round_request_header.base_system =
                                    crate::plan_mode::compose_workflow_instructions(
                                        context.base_instructions.clone(),
                                        round_workflow_instructions.as_deref(),
                                    );
                                match round_request_header
                                    .dynamic_instructions
                                    .as_ref()
                                    .map(|source| source.snapshot())
                                    .transpose()
                                {
                                    Ok(snapshot) => {
                                        crate::plugins::services::apply_instructions_to_header(
                                            &mut round_request_header.header,
                                            &round_request_header.base_system,
                                            snapshot.flatten().as_ref(),
                                        );
                                        if !first_round
                                            && round_request_header.header
                                                != durable_request_header.header
                                        {
                                            round_header_reason = Some("change");
                                        }
                                        None
                                    }
                                    Err(error) => Some(format!(
                                        "goal round could not refresh project instructions: {error}"
                                    )),
                                }
                            }
                            Err(error) => Some(format!(
                                "goal round could not refresh durable goal context: {error}"
                            )),
                        }
                    } else {
                        None
                    };
                    durable_request_header = round_request_header.clone();
                    let process_generation =
                        process_service.bind_run(session_id.as_str(), cancel.clone());
                    let (mut recorder_core, journaling_approver) =
                        SessionRecorder::with_approver(
                            Arc::clone(&journal),
                            Arc::clone(&request_approver),
                            round_request_header,
                            &title_config.protocol.to_string(),
                            &title_config.model,
                            current_turn,
                            round_header_reason,
                        );
                    recorder_core.attach_aux_usage(Arc::clone(&sampling_usage_worker));
                    let configured_budget = worker_config.effective_run_token_budget();
                    let round_budget = if goal_mode {
                        let remaining = goal_service.remaining_tokens().unwrap_or(1);
                        Some(configured_budget.map_or(remaining, |cap| cap.min(remaining)))
                    } else {
                        configured_budget
                    };
                    let spend_ledger =
                        Arc::new(crate::model::RunSpendLedger::new(round_budget));
                    let subagent_round_error = subagent_service
                        .begin_round(Arc::clone(&spend_ledger))
                        .err();
                    recorder_core.set_run_ledger(Arc::clone(&spend_ledger));
                    let recorder = Arc::new(Mutex::new(recorder_core));
                    let recorder_sink: Box<dyn EventSink + Send> = Box::new(RecorderHandle {
                        recorder: Arc::clone(&recorder),
                        sink: Arc::clone(&ui_sink),
                    });
                    let approver: Arc<dyn PermissionApprover> = Arc::new(journaling_approver);
                    let panic_text_slot = Arc::clone(&captured_text);
                    let execution = catch_unwind(AssertUnwindSafe(|| {
                        if let Some(error) = &goal_refresh_error {
                            return Err(AgentFailure {
                                error: crate::RunError::new(error.clone()),
                            });
                        }
                        if let Some(error) = &subagent_bind_error {
                            return Err(AgentFailure {
                                error: crate::RunError::new(format!(
                                    "subagent service could not bind this run: {error}"
                                )),
                            });
                        }
                        if let Some(error) = &subagent_round_error {
                            return Err(AgentFailure {
                                error: crate::RunError::new(format!(
                                    "subagent accounting could not bind this round: {error}"
                                )),
                            });
                        }
                        process_generation.as_ref().map_err(|error| AgentFailure {
                            error: crate::RunError::new(format!(
                                "process service could not bind this run: {error}"
                            )),
                        })?;
                        agent.execute(AgentRequest {
                            config: worker_config.clone(),
                            spend_ledger: Some(Arc::clone(&spend_ledger)),
                            credentials: worker_credentials.clone(),
                            history_items: history.clone(),
                            message: current_message.clone(),
                            client_message_id: current_client_id.clone(),
                            cancel: cancel.clone(),
                            steering: steering_for_worker.clone(),
                            approver,
                            events: recorder_sink,
                            tool_access: context.tool_access.clone(),
                            workflow_instructions: round_workflow_instructions,
                            permission_mode: permission_mode_snapshot,
                        })
                    }));
                    let process_bind_error = process_generation.as_ref().err().cloned();
                    let process_cleanup_error = process_generation
                        .as_ref()
                        .ok()
                        .and_then(|generation| process_service.unbind_run(*generation).err());
                    if let Some(error) = process_bind_error
                        .as_ref()
                        .or(process_cleanup_error.as_ref())
                    {
                        recorder
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner)
                            .force_terminal_failure(format!(
                                "process lifecycle failed before run completion: {error}"
                            ));
                    }
                    let (outcome, panic_text) = match execution {
                        Ok(outcome) => (Some(outcome), None),
                        Err(payload) => (
                            None,
                            Some(format!(
                                "{}\npartial output: {}",
                                panic_message(payload),
                                panic_text_slot
                                    .lock()
                                    .map(|text| text.clone())
                                    .unwrap_or_default()
                            )),
                        ),
                    };
                    let was_cancelled = cancel.is_cancelled()
                        || outcome
                            .as_ref()
                            .is_some_and(|result| result.as_ref().is_ok_and(|done| done.cancelled));
                    let reason = match (&outcome, &process_bind_error, &process_cleanup_error) {
                        (_, Some(error), _) => TurnEndReason::Error {
                            error: json!({ "message": format!("process bind failed: {error}") }),
                        },
                        (_, None, Some(error)) => TurnEndReason::Error {
                            error: json!({ "message": format!("process cleanup failed: {error}") }),
                        },
                        (Some(Ok(_)), None, None) if was_cancelled => TurnEndReason::Aborted {
                            reason: TurnEndCancelCause::User,
                        },
                        (Some(Ok(_)), None, None) => TurnEndReason::Completed,
                        (Some(Err(failure)), None, None) => TurnEndReason::Error {
                            error: json!({ "message": failure.error.to_string() }),
                        },
                        (None, None, None) => TurnEndReason::Error {
                            error: json!({ "message": "run worker panicked" }),
                        },
                    };
                    let (finish_error, published) = {
                        let mut recorder = recorder
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner);
                        recorder.finish(reason)
                    };
                    let journal_error =
                        finish_error.map(|error| format!("session journal failed: {error}"));
                    for event in published {
                        let mut sink = ui_sink
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner);
                        let forwarded = std::panic::catch_unwind(std::panic::AssertUnwindSafe(
                            || sink.emit(event),
                        ));
                        if forwarded.is_err() {
                            eprintln!(
                                "clat: warning: frontend event sink panicked while publishing the terminal event"
                            );
                        }
                    }
                    let _ = sessions.sync_active();
                    // SessionRecorder may have refreshed project instructions
                    // between model steps. Seed the next synthetic round from
                    // the actual durable header so only a real goal/context
                    // change emits another `request/header` fact.
                    if let Some(header) = sessions.last_request_header() {
                        durable_request_header.header = header;
                    }
                    let mut round_result = match (
                        outcome,
                        journal_error,
                        panic_text,
                        process_cleanup_error,
                    ) {
                        (Some(result), journal_error, panic_text, process_cleanup_error) => {
                            let base = result
                                .map(|done| ApplicationRunDone {
                                    receipt: None,
                                    output: done.text,
                                    turns: done.turns,
                                    usage: done.usage,
                                    cancelled: was_cancelled,
                                })
                                .map_err(|failure| {
                                    let (message, turns, usage, _) = failure.error.into_parts();
                                    ApplicationRunFailure {
                                        receipt: None,
                                        error: message,
                                        turns,
                                        usage,
                                    }
                                });
                            let base = match (base, process_cleanup_error) {
                                (Ok(done), Some(error)) => Err(ApplicationRunFailure {
                                    receipt: None,
                                    error: format!("process cleanup failed: {error}"),
                                    turns: done.turns,
                                    usage: done.usage,
                                }),
                                (base, _) => base,
                            };
                            match (base, journal_error, panic_text) {
                                (base, None, None) => base,
                                (Ok(done), Some(error), _) => Err(ApplicationRunFailure {
                                    receipt: None,
                                    error,
                                    turns: done.turns,
                                    usage: done.usage,
                                }),
                                (Ok(done), None, Some(text)) => Err(ApplicationRunFailure {
                                    receipt: None,
                                    error: format!(
                                        "{text} (run had completed: {})",
                                        done.output
                                    ),
                                    turns: done.turns,
                                    usage: done.usage,
                                }),
                                (Err(failure), Some(error), _) => Err(ApplicationRunFailure {
                                    receipt: None,
                                    error: format!("{}; {error}", failure.error),
                                    turns: failure.turns,
                                    usage: failure.usage,
                                }),
                                (Err(failure), None, Some(text)) => Err(ApplicationRunFailure {
                                    receipt: None,
                                    error: format!("{text}; {}", failure.error),
                                    turns: failure.turns,
                                    usage: failure.usage,
                                }),
                            }
                        }
                        (None, journal_error, panic_text, process_cleanup_error) => {
                            Err(ApplicationRunFailure {
                                receipt: None,
                                error: match (panic_text, journal_error) {
                                    (Some(text), Some(error)) => format!("{text}; {error}"),
                                    (Some(text), None) => text,
                                    (None, Some(error)) => error,
                                    (None, None) => process_cleanup_error
                                        .map(|error| {
                                            format!("process cleanup failed: {error}")
                                        })
                                        .unwrap_or_else(|| "run worker panicked".into()),
                                },
                                turns: 0,
                                usage: Usage::default(),
                            })
                        }
                    };
                    let (mut round_usage, round_turns, round_text, succeeded) = match &round_result {
                        Ok(done) => (
                            done.usage.clone(),
                            done.turns,
                            done.output.clone(),
                            !done.cancelled,
                        ),
                        Err(failure) => (
                            failure.usage.clone(),
                            failure.turns,
                            failure.error.clone(),
                            false,
                        ),
                    };
                    round_usage.add_assign(&subagent_service.take_round_usage());
                    let round_cancelled = was_cancelled
                        || matches!(&round_result, Ok(done) if done.cancelled);
                    // The shared ledger is the billing guard's sole meter. It
                    // includes provider retry reservations, plugin sampling,
                    // and read-only child calls that are not all represented
                    // in the parent RunOutput usage fields.
                    let accounted_round_tokens = spend_ledger.used();
                    aggregate_usage.add_assign(&round_usage);
                    aggregate_turns = aggregate_turns.saturating_add(round_turns);
                    if goal_mode {
                        match goal_service.finish_round(
                            accounted_round_tokens,
                            round_started.elapsed(),
                            succeeded,
                            round_cancelled,
                            &round_text,
                        ) {
                            Ok(crate::goal::GoalContinuation::Continue) if !round_cancelled => {
                                let next = match goal_service.next_round() {
                                    Ok(next) => next,
                                    Err(error) => {
                                        round_result = Err(ApplicationRunFailure {
                                            receipt: None,
                                            error: format!(
                                                "goal continuation reservation failed: {error}"
                                            ),
                                            turns: round_turns,
                                            usage: round_usage,
                                        });
                                        goal_service.disarm();
                                        break round_result;
                                    }
                                };
                                let next_turn = match sessions.active_turns() {
                                    Ok(turns) => turns.saturating_add(1),
                                    Err(error) => {
                                        goal_service.disarm();
                                        break Err(ApplicationRunFailure {
                                            receipt: None,
                                            error: format!(
                                                "goal continuation could not read turn state: {error}"
                                            ),
                                            turns: aggregate_turns,
                                            usage: aggregate_usage.clone(),
                                        });
                                    }
                                };
                                let batch = [
                                    NewSessionEvent::new(
                                        "turn/start",
                                        payloads::turn_start(next_turn),
                                    ),
                                    NewSessionEvent::new("user/message", next.message)
                                        .append(Vec::new()),
                                ];
                                round_started = next.started_at;
                                if let Err(error) = journal
                                    .append_atomic(&batch)
                                    .and_then(|_| journal.flush())
                                {
                                    goal_service.disarm();
                                    break Err(ApplicationRunFailure {
                                        receipt: None,
                                        error: format!(
                                            "goal continuation durable prelude failed: {error}"
                                        ),
                                        turns: aggregate_turns,
                                        usage: aggregate_usage.clone(),
                                    });
                                }
                                let _ = sessions.sync_active();
                                history = match sessions.surface_nodes() {
                                    Ok(nodes) => {
                                        nodes.into_iter().map(|(_, item)| item).collect()
                                    }
                                    Err(error) => {
                                        goal_service.disarm();
                                        break Err(ApplicationRunFailure {
                                            receipt: None,
                                            error: format!(
                                                "goal continuation history rebuild failed: {error}"
                                            ),
                                            turns: aggregate_turns,
                                            usage: aggregate_usage.clone(),
                                        });
                                    }
                                };
                                current_turn = next_turn;
                                // goal 续轮是 core 合成消息：无客户端幂等键。
                                current_message =
                                    crate::message::MessageContent::text(next.prompt);
                                current_client_id = None;
                                first_round = false;
                                continue;
                            }
                            Ok(crate::goal::GoalContinuation::Stop) => {}
                            Ok(crate::goal::GoalContinuation::Continue) => {}
                            Err(error) => {
                                goal_service.disarm();
                                round_result = Err(ApplicationRunFailure {
                                    receipt: None,
                                    error: format!(
                                        "goal progress could not commit after the round: {error}"
                                    ),
                                    turns: round_turns,
                                    usage: round_usage,
                                });
                            }
                        }
                    }
                    match round_result {
                        Ok(mut done) => {
                            done.turns = aggregate_turns;
                            done.usage = aggregate_usage.clone();
                            break Ok(done);
                        }
                        Err(mut failure) => {
                            failure.turns = aggregate_turns;
                            failure.usage = aggregate_usage.clone();
                            break Err(failure);
                        }
                    }
                };
                if let Some(todo_service) = &todo_service {
                    todo_service.unbind();
                }
                subagent_service.unbind();
                let close_result = run_scope.close();
                monitor.refresh();
                let result = match (result, close_result) {
                    (result, Ok(())) => result,
                    (Ok(done), Err(error)) => Err(ApplicationRunFailure {
                        receipt: None,
                        error: format!("run scope cleanup failed: {error}"),
                        turns: done.turns,
                        usage: done.usage,
                    }),
                    (Err(mut failure), Err(error)) => {
                        failure
                            .error
                            .push_str(&format!("; run scope cleanup failed: {error}"));
                        Err(failure)
                    }
                };
                // 宿主桥卸载（INV-S1：跨 run 不泄漏）+ sampling 余量归
                // 并（INV-S6）：journal 已在 ModelResponded 处归并，这里
                // 只补取消/失败路径的尾巴；桥先 clear 再取余量，杜绝迟
                // 到加账。
                plugin_host_worker.clear();
                tool_access_worker.clear();
                skill_catalog_worker.clear();
                view_image_worker.clear();
                let sampled = sampling_usage_worker
                    .lock()
                    .map(|mut cell| std::mem::take(&mut *cell))
                    .unwrap_or_default();
                let result = result
                    .map(|mut done| {
                        done.usage.add_assign(&sampled);
                        done
                    })
                    .map_err(|mut failure| {
                        failure.usage.add_assign(&sampled);
                        failure
                    });
                // CB1-04：自动命名移出 run worker——独立线程执行。成功未
                // 取消的 run 之后，若会话**仍无显式标题**（首轮命名失败、
                // 或早于命名功能的旧会话）就再排一次——每次成功的 run 都
                // 是自愈机会，标题落盘（provider 或用户）后自然停止
                //（2026-08-19 用户实测：几百轮的会话因首轮一次性触发失败
                // 而永远无名，`/rename` 又被门槛拦住）。CAS 保证与用户
                // 改名的竞争安全。
                let (_, title_seq) = sessions.title_state();
                if title_seq.is_none()
                    && let Ok(done) = &result
                    && !done.cancelled
                    && titler.is_some()
                    && let Some(sender) = &title_sender
                {
                    let expectation = SetTitleExpectation::NoTitle;
                    // 有界队列满时直接放弃；绝不让 run completion 等标题。
                    // job 绑定会话（F-A）：迟到的标题绝不能写进切换后的
                    // 新会话。
                    let _ = sender.try_send(AutotitleJob {
                        session_id: session_id.clone(),
                        config: title_config,
                        credentials: title_credentials,
                        expectation,
                    });
                }
                // MM-1A（MM-I11）：commit point 之后的任何结果都携带
                // committed 回执——run 失败时前端知道消息已耐久、不得
                // 重发。回执在这里统一附着，round 内部的构造点不感知。
                let result = result
                    .map(|mut done| {
                        done.receipt = run_receipt.clone();
                        done
                    })
                    .map_err(|mut failure| {
                        failure.receipt = run_receipt.clone();
                        failure
                    });
                busy.store(false, Ordering::Release);
                let _ = completion.send(result);
            })
            .map_err(|error| ApplicationError::new(format!("spawn run worker: {error}")))?;
        *join_slot
            .lock()
            .map_err(|_| ApplicationError::new("run join lock poisoned"))? = Some(worker);
        if fail_run_start_receive {
            receiver_closed_receiver.recv().map_err(|_| {
                ApplicationError::new("run worker fault seam failed before admission")
            })?;
        }

        // 预备（CAS + 首批耐久批）发生在 worker 就位之后。Fresh Plan
        // birth 已在这里 append+flush；只有此后才冻结本 run 的工作流视图，
        // 因而 request/header、模型工具目录与宿主硬门消费同一状态事实。
        let prepared = match self.prepare_run(&request_message, goal_round.as_ref()) {
            Ok(prepared) => prepared,
            Err(error) => {
                drop(start_sender);
                handle.join()?;
                return Err(error);
            }
        };
        let committed_receipt = prepared.receipt.clone();
        let post_commit_start_error = |error: ApplicationError, phase: &'static str| {
            let failed_receipt = committed_receipt
                .clone()
                .map(|receipt| Box::new((*receipt).with_failure_phase(phase)));
            error.with_receipt(failed_receipt)
        };
        let goal_injection = match self.goal.injection().map_err(ApplicationError::new) {
            Ok(injection) => injection,
            Err(error) => {
                drop(start_sender);
                handle.join().map_err(|join_error| {
                    post_commit_start_error(join_error, "worker-start-cleanup")
                })?;
                return Err(post_commit_start_error(error, "goal-injection"));
            }
        };
        let context =
            self.run_context_snapshot(&config, skill_snapshot, memory_injection, goal_injection);
        let request_header =
            self.request_header_data(&config, &context, instruction_snapshot.as_ref());
        let header_reason = self.request_header_reason(&request_header.header);
        let emitted_header_value = header_reason
            .is_some()
            .then(|| request_header.header.clone());

        self.tool_access.install(context.tool_access.clone());
        self.skill_catalog.install(Arc::clone(&context.skills));
        self.view_image.begin_run();
        self.plugin_host
            .install(crate::plugin_host::RunHostContext {
                providers: Arc::clone(&self.providers),
                model_config: config.clone(),
                credentials: credentials.clone(),
                approver: Arc::clone(&approver),
                permission_mode: self
                    .permission_modes_enabled
                    .then(|| Arc::clone(&self.permission_mode)),
                asker: asker_for_host,
                cancel: cancel.clone(),
                usage_cell: Arc::clone(&sampling_usage),
                budget: sampling_budget,
            });
        let start = WorkerStart {
            prepared,
            request_header,
            header_reason,
            context,
        };
        if start_sender.send(start).is_err() {
            self.plugin_host.clear();
            self.tool_access.clear();
            self.skill_catalog.clear();
            self.view_image.clear();
            handle
                .join()
                .map_err(|error| post_commit_start_error(error, "worker-start-cleanup"))?;
            return Err(post_commit_start_error(
                ApplicationError::new("run worker stopped before execution started"),
                "worker-start-send",
            ));
        }
        // The run is committed to execute: only now does the header count
        // as emitted and the session stop being freshly opened. (The
        // recorder journals the header at the first dispatch; a crash in
        // that tiny window self-heals — reopening reseeds the state from
        // the log's requestHeader projection.)
        self.fresh_session_open = false;
        if let Some(header) = emitted_header_value {
            self.emitted_request_header = Some(header);
        }
        self.active_run = Some(handle.clone());
        Ok(handle)
    }

    pub fn cancel_active_run(&self) {
        if let Some(handle) = &self.active_run {
            handle.cancel();
        }
    }

    /// 运行中插话（DSH `steer()`）：typed 消息进入活动 run 的队列，在
    /// 下一次模型请求边界并入对话（不打断在途请求）。run 不在执行、或
    /// 已到终态（队列封口，W1-04）时返回 `NotRunning`，调用方回退为普
    /// 通提交。未被 claim 的消息不落盘。
    ///
    /// MM-3 image steering follows the same two-phase ownership as text:
    /// sources are normalized into unreachable session blobs before queueing,
    /// then the recorder makes descriptor refs reachable only when the run
    /// claims and append+flush commits the user event. A sealed/not-running
    /// queue is checked first, so rejected drafts do not create orphan work.
    pub fn steer(&self, mut message: crate::message::PendingMessage) -> SteerOutcome {
        if message.content.has_images() && message.admitted_images.is_empty() {
            return SteerOutcome::Refused {
                reason: "image content must be admitted from staged sources by core".into(),
                receipt: steering_rollback_receipt(&message, "steering-admission"),
            };
        }
        let Some(handle) = &self.active_run else {
            return SteerOutcome::NotRunning {
                receipt: steering_rollback_receipt(&message, "steering-not-running"),
            };
        };
        if handle.is_finished() {
            return SteerOutcome::NotRunning {
                receipt: steering_rollback_receipt(&message, "steering-not-running"),
            };
        }
        if !message.staged_attachments.is_empty() {
            message.freeze_request_digest();
            let (config, _) = match self.model_state() {
                Ok(state) => state,
                Err(error) => {
                    return SteerOutcome::Refused {
                        reason: error.to_string(),
                        receipt: steering_rollback_receipt(&message, "steering-model-state"),
                    };
                }
            };
            if !config.capabilities.accepts_image_input() {
                return SteerOutcome::Refused {
                    reason: format!(
                        "this model ({}) does not accept image input; switch to a verified vision model or remove the image(s)",
                        config.model
                    ),
                    receipt: steering_rollback_receipt(&message, "steering-capability"),
                };
            }
            if message.staged_attachments.len() > config.image_policy.max_images {
                return SteerOutcome::Refused {
                    reason: format!(
                        "this model ({}) accepts at most {} images per message; remove {} image(s) before sending",
                        config.model,
                        config.image_policy.max_images,
                        message.staged_attachments.len() - config.image_policy.max_images,
                    ),
                    receipt: steering_rollback_receipt(&message, "steering-route-policy"),
                };
            }
            let images = match self
                .sessions
                .import_attachments(&message.staged_attachments)
            {
                Ok(images) => images,
                Err(error) => {
                    return SteerOutcome::Refused {
                        reason: error.to_string(),
                        receipt: steering_rollback_receipt(&message, "steering-import"),
                    };
                }
            };
            if let Err(reason) = validate_route_images(&config, &images) {
                return SteerOutcome::Refused {
                    reason,
                    receipt: steering_rollback_receipt(&message, "steering-route-policy"),
                };
            }
            message.content.blocks.extend(images.iter().map(|image| {
                crate::message::ContentBlock::Image {
                    attachment: image.descriptor.clone(),
                }
            }));
            message.admitted_images = images;
        }
        let reserved_receipt = message.client_message_id.clone().map(|client_message_id| {
            Box::new(crate::message::AdmissionReceipt::reserved(
                client_message_id,
                message.content.attachment_ids(),
            ))
        });
        // 入队与终态封口同一把锁（W1-04）：Sealed 意味着 run 已判定
        // 结束、消息永远无人 claim——绝不能当 Queued 返回。
        match handle.steering.try_push(message) {
            crate::run::PushOutcome::Accepted => {
                // A human interjection wins over unattended continuation. It
                // still enters the current Run normally, while subsequent
                // synthetic rounds are disarmed at the next boundary.
                self.goal.disarm();
                SteerOutcome::Queued {
                    receipt: reserved_receipt,
                }
            }
            crate::run::PushOutcome::Sealed => SteerOutcome::NotRunning {
                receipt: reserved_receipt.map(|receipt| {
                    Box::new(crate::message::AdmissionReceipt::rolled_back(
                        receipt.client_message_id.clone(),
                        receipt.attachment_ids.clone(),
                        "steering-not-running",
                    ))
                }),
            },
        }
    }

    /// 召回最后一条未 claim 的插话（ESC 栈式语义的第一优先级）：完整
    /// typed 消息（内容 + 客户端幂等键）退回调用方，前端可原样重发
    ///（MM-I11 recall 语义；图片草稿不被静默丢成纯文本——虽然当前
    /// admission 只放行文本，召回仍走类型化通道）。无活动 run、run 已
    /// 结束、或消息已被 claim（进入 journal、不可撤回）时返回 `None`
    ///——此时前端的 ESC 应回落到取消 run。召回不触碰 journal。
    pub fn recall_pending_steering(&self) -> Option<RecalledSteering> {
        let handle = self.active_run.as_ref()?;
        if handle.is_finished() {
            return None;
        }
        handle
            .steering
            .recall_last()
            .map(|message| RecalledSteering {
                receipt: steering_rollback_receipt(&message, "steering-recall"),
                message,
            })
    }

    /// MM-1A：按客户端幂等键查询 committed 回执。权威来源是 journal
    /// 投影（重启/重放后同一答案）；进程内状态不参与——"committed 重
    /// 试返回原 receipt" 的判定基础。无活动会话或键不在回执窗口返回
    /// None（调用方按新消息走正常接纳）。
    pub fn committed_receipt(
        &self,
        client_message_id: &str,
    ) -> Option<crate::message::AdmissionReceipt> {
        self.sessions.committed_receipt(client_message_id)
    }

    /// M-02（审查 2026-08-27）：回执 + 落盘 digest 的生产判别查询——
    /// serve 的幂等重试拦截经此消费（同 key 同 digest 幂等成功、异
    /// digest conflict），不在 serve 复刻投影逻辑。
    pub fn committed_admission(
        &self,
        client_message_id: &str,
    ) -> Option<crate::message::CommittedAdmission> {
        self.sessions.committed_admission(client_message_id)
    }
}

pub struct ApplicationRunRequest {
    /// MM-1A typed 初始消息：内容块 + 可选客户端幂等键 + pre-admission
    /// 附件来源（旧 `prompt`+`attachments` 的合一表示）。prepare 阶段
    /// 导入附件、以 descriptor 元数据 + 会话内引用落 journal。
    pub message: crate::message::PendingMessage,
    pub approver: Arc<dyn PermissionApprover>,
    /// 本次 run 的 ask-user 前端实现；`None`（headless）时 `ask_user`
    /// 工具返回结构化错误。TUI 的实现是无状态的通道包装，随请求安装。
    pub asker: Option<Arc<dyn crate::interaction::UserAsker>>,
    pub events: Box<dyn EventSink + Send>,
    pub completion: mpsc::Sender<ApplicationRunResult>,
}

struct PreparedRun {
    session_id: SessionId,
    turn: u64,
    history: Vec<crate::model::ModelItem>,
    journal: Arc<dyn RunJournal>,
    goal_round_started: Option<std::time::Instant>,
    /// MM-1A：被接纳的初始消息（descriptor 投影）与客户端幂等键——
    /// worker 用它发 `RunStarted`，完成/失败结果携带 committed 回执。
    message: crate::message::MessageContent,
    client_message_id: Option<crate::message::ClientMessageId>,
    receipt: Option<Box<crate::message::AdmissionReceipt>>,
}

struct WorkerStart {
    prepared: PreparedRun,
    request_header: crate::session::recorder::RequestHeaderData,
    header_reason: Option<&'static str>,
    context: RunContextSnapshot,
}

pub type ApplicationRunResult = Result<ApplicationRunDone, ApplicationRunFailure>;

#[derive(Clone, Debug)]
pub struct ApplicationRunDone {
    /// MM-1A：本次初始消息的接纳回执。commit point（user event
    /// append+flush）跨过之后的一切完成/失败都携带 `Committed`——
    /// run 失败 ≠ 消息未送达（MM-I11）。core 合成消息（goal 轮）无
    /// 客户端键，为 None。装箱保持 Err 变体精瘦（result_large_err）。
    pub receipt: Option<Box<crate::message::AdmissionReceipt>>,
    pub output: String,
    pub turns: usize,
    pub usage: Usage,
    pub cancelled: bool,
}

#[derive(Clone, Debug)]
pub struct ApplicationRunFailure {
    /// 同 [`ApplicationRunDone::receipt`]：Committed 回执 + run 失败，
    /// 前端不得把消息重新入箱。
    pub receipt: Option<Box<crate::message::AdmissionReceipt>>,
    pub error: String,
    pub turns: usize,
    pub usage: Usage,
}

/// `Application::steer` 的结果：入队成功、当前没有可插话的活动 run、
/// 或消息被 admission 拒绝（MM-1A：图片/附件 steering fail-closed——
/// 不冒充 NotRunning，调用方如实向用户报告原因）。
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SteerOutcome {
    Queued {
        /// `Reserved` until the run claims and durably journals the message.
        receipt: Option<Box<crate::message::AdmissionReceipt>>,
    },
    NotRunning {
        /// Client-keyed drafts are rolled back and remain safe to submit as
        /// an ordinary prompt.
        receipt: Option<Box<crate::message::AdmissionReceipt>>,
    },
    Refused {
        reason: String,
        receipt: Option<Box<crate::message::AdmissionReceipt>>,
    },
}

/// A steering draft recalled before claim. The typed message is returned
/// intact and a client-keyed draft carries its authoritative rollback.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RecalledSteering {
    pub message: crate::message::PendingMessage,
    pub receipt: Option<Box<crate::message::AdmissionReceipt>>,
}

fn validate_route_images(
    config: &crate::model::ModelConfig,
    images: &[crate::message::JournalImage],
) -> Result<(), String> {
    if images.len() > config.image_policy.max_images {
        return Err(format!(
            "this model ({}) accepts at most {} images per message; remove {} image(s) before sending",
            config.model,
            config.image_policy.max_images,
            images.len() - config.image_policy.max_images,
        ));
    }
    for image in images {
        if !config
            .image_policy
            .media_types
            .iter()
            .any(|allowed| allowed == &image.descriptor.media_type)
        {
            return Err(format!(
                "this model ({}) does not accept normalized {}; use a route-supported image format",
                config.model, image.descriptor.media_type,
            ));
        }
        if image.descriptor.bytes > config.image_policy.max_bytes {
            return Err(format!(
                "the normalized image `{}` is {} bytes, above this model's {}-byte per-image limit; resize it before sending",
                image.descriptor.display_name.as_deref().unwrap_or("image"),
                image.descriptor.bytes,
                config.image_policy.max_bytes,
            ));
        }
    }
    Ok(())
}

fn steering_rollback_receipt(
    message: &crate::message::PendingMessage,
    phase: &'static str,
) -> Option<Box<crate::message::AdmissionReceipt>> {
    message.client_message_id.clone().map(|client_message_id| {
        Box::new(crate::message::AdmissionReceipt::rolled_back(
            client_message_id,
            message.content.attachment_ids(),
            phase,
        ))
    })
}

/// `/rename` 的语义结果（内部 I/O 失败走 `Err`）。
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RenameOutcome {
    /// 已落盘（session/title + flush + checkpoint），TitleUpdated 已广播。
    Renamed { title: String },
    /// 无活动会话（或 set_title 会话守卫拦下）。
    NoSession,
    /// 清洗后为空（空白/纯控制字符）。
    Invalid,
}

#[derive(Clone)]
pub struct RunHandle {
    cancel: CancelToken,
    busy: Arc<AtomicBool>,
    join: Arc<Mutex<Option<JoinHandle<()>>>>,
    steering: crate::run::SteeringQueue,
}

impl RunHandle {
    pub fn cancel(&self) {
        self.cancel.cancel();
    }

    pub fn is_finished(&self) -> bool {
        !self.busy.load(Ordering::Acquire)
    }

    pub fn join(&self) -> Result<(), ApplicationError> {
        let handle = self
            .join
            .lock()
            .map_err(|_| ApplicationError::new("run join lock poisoned"))?
            .take();
        if let Some(handle) = handle {
            handle
                .join()
                .map_err(|_| ApplicationError::new("run worker panicked"))?;
        }
        Ok(())
    }
}

struct CapturingEventSink {
    inner: Box<dyn EventSink + Send>,
    text: Arc<Mutex<String>>,
}

/// 已封口 run 收尾的有界等待上限（W1-04 回退路径）：正常收尾是
/// journal flush + run-scope teardown，毫秒级；超时说明收尾本身卡住，
/// 宁可让用户重按一次 Enter 也不阻塞 UI 或放弃线程。
const SEALED_RUN_WRAPUP_WAIT: std::time::Duration = std::time::Duration::from_secs(2);

/// 轮询等待一个已封口 run 的 `busy` 落定；超时返回 false。
fn wait_for_sealed_wrapup(previous: &RunHandle) -> bool {
    let deadline = std::time::Instant::now() + SEALED_RUN_WRAPUP_WAIT;
    while !previous.is_finished() {
        if std::time::Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
    true
}

impl EventSink for CapturingEventSink {
    fn emit(&mut self, event: RunEvent) {
        if let RunEvent::ModelStream {
            event: crate::model::ModelEvent::TextDelta { delta },
            ..
        } = &event
            && let Ok(mut text) = self.text.lock()
        {
            text.push_str(delta);
        }
        self.inner.emit(event);
    }
}

fn panic_message(payload: Box<dyn std::any::Any + Send>) -> String {
    if let Some(text) = payload.downcast_ref::<&str>() {
        return (*text).to_string();
    }
    if let Some(text) = payload.downcast_ref::<String>() {
        return text.clone();
    }
    "run worker panicked".into()
}
