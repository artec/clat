use crate::CancelToken;
use crate::event::EventSink;
use crate::model::Usage;
use crate::permission::PermissionApprover;
use crate::plugin::Plugin;
use crate::plugins::run_catalog;
use crate::session::event::payloads;
use crate::session::id::SessionId;
use crate::session::run_journal::{NewSessionEvent, RunJournal};
use serde_json::json;
use std::sync::{Arc, mpsc};

use super::*;

use super::run_execution::{RunExecutionEngine, RunExecutionSpec, RunExecutionStart, RunHandle};
use super::threads::MCP_STARTUP_RUN_WAIT;

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
            if !previous.steering().is_sealed() {
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
        // The execution module owns the waiting worker, run scope and every
        // cleanup pair. Application keeps only admission timing and the final
        // active handle.
        let waiting_execution = RunExecutionEngine::spawn(
            self,
            RunExecutionSpec {
                run_plugins,
                config: config.clone(),
                credentials: credentials.clone(),
                approver: Arc::clone(&approver),
                events,
                completion,
            },
        )?;

        // 预备（CAS + 首批耐久批）发生在 worker 就位之后。Fresh Plan
        // birth 已在这里 append+flush；只有此后才冻结本 run 的工作流视图，
        // 因而 request/header、模型工具目录与宿主硬门消费同一状态事实。
        let prepared = match self.prepare_run(&request_message, goal_round.as_ref()) {
            Ok(prepared) => prepared,
            Err(error) => {
                waiting_execution.abort()?;
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
                waiting_execution.abort().map_err(|join_error| {
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

        let handle = waiting_execution
            .activate(
                RunExecutionStart {
                    prepared,
                    request_header,
                    header_reason,
                    context,
                },
                asker_for_host,
            )
            .map_err(|failure| post_commit_start_error(failure.error, failure.phase))?;
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
        match handle.steering().try_push(message) {
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
            .steering()
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

    /// Locate the unique project session that durably admitted one client
    /// message without changing the current selection. Recovery callers
    /// must fail closed when the durable journals are unreadable or contain
    /// more than one owner for the same idempotency key.
    pub(crate) fn find_committed_admission_session(
        &self,
        client_message_id: &str,
    ) -> Result<Option<(crate::SessionId, crate::message::CommittedAdmission)>, ApplicationError>
    {
        self.sessions
            .find_committed_admission_session(&self.project_key(), client_message_id)
            .map_err(session_error)
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

pub(super) struct PreparedRun {
    pub(super) session_id: SessionId,
    pub(super) turn: u64,
    pub(super) history: Vec<crate::model::ModelItem>,
    pub(super) journal: Arc<dyn RunJournal>,
    pub(super) goal_round_started: Option<std::time::Instant>,
    /// MM-1A：被接纳的初始消息（descriptor 投影）与客户端幂等键——
    /// worker 用它发 `RunStarted`，完成/失败结果携带 committed 回执。
    pub(super) message: crate::message::MessageContent,
    pub(super) client_message_id: Option<crate::message::ClientMessageId>,
    pub(super) receipt: Option<Box<crate::message::AdmissionReceipt>>,
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
