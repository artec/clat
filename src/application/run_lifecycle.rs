use crate::CancelToken;
use crate::event::{EventSink, RunEvent};
use crate::model::Usage;
use crate::permission::PermissionApprover;
use crate::plugin::{Plugin, ScopeKind};
use crate::plugins::run_catalog;
use crate::plugins::services::{AgentRequest, RUN_SCOPE_SERVICE};
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
        let cancel = CancelToken::new();
        let catalog = run_catalog(cancel.clone(), Arc::clone(&request.approver));
        self.start_run_with_catalog(request, catalog)
    }

    /// The durable prelude of a run: ensure a session with a live writer,
    /// then atomically append `turn/start` + `user/message` and flush —
    /// the model is only called after the durable batch committed. For a
    /// fresh session the fact lands FIRST (MP-1 §4.3); workspace
    /// registration and the selection pointer follow as projections, so a
    /// crash between them is healed by mount-time reconciliation.
    fn prepare_run(
        &mut self,
        prompt: &str,
        attachments: &[std::path::PathBuf],
    ) -> Result<PreparedRun, ApplicationError> {
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
        // 附件导入（M4）：会话目录已物化，先复制、后落盘——journal 拿到
        // 的是会话附件目录内的绝对引用。校验失败（不存在/类型/超大）
        // 发生在任何 journal 写入之前，本轮不留任何痕迹。
        let images = self
            .sessions
            .import_attachments(attachments)
            .map_err(session_error)?;
        // 首个耐久批：出生档（仅新物化的会话）→ turn/start →
        // user/message。DSH pinInitialPermission 在会话创建期 pin 档位，
        // 对应物即出生事件排在首个 turn 之前（PS2）——回放从第一条
        // 事件起就有确定的档位。Classic（exec）不落此事件（PS4）。
        let mut first_batch = Vec::new();
        if materialized.is_some() && self.permission_modes_enabled {
            first_batch.push(NewSessionEvent::new(
                "sandbox/mode",
                payloads::sandbox_mode(&self.permission_mode()),
            ));
        }
        first_batch.push(NewSessionEvent::new(
            "turn/start",
            payloads::turn_start(turn),
        ));
        first_batch.push(
            NewSessionEvent::new(
                "user/message",
                payloads::user_message_with_images(prompt, &images),
            )
            .append(Vec::new()),
        );
        journal
            .append_atomic(&first_batch)
            .map_err(|error| ApplicationError::new(format!("session append failed: {error}")))?;
        journal
            .flush()
            .map_err(|error| ApplicationError::new(format!("session flush failed: {error}")))?;
        // 事实已耐久：投影随后（注册工作区 + 账本 + 指针）。两者之间的
        // 崩溃由挂载期对账收编自愈（会话日志永远赢）。
        if let Some(id) = materialized {
            self.ensure_registered()?;
            if let Some(workspace_id) = self.workspace_id.clone() {
                self.control
                    .append_session_to_workspace(&workspace_id, id.as_str())
                    .map_err(|error| ApplicationError::new(error.to_string()))?;
            }
            self.selection = Some(id.clone());
            self.persist_selection(Some(&id))?;
        }
        let history_nodes = self.sessions.surface_nodes().map_err(session_error)?;
        let mut history: Vec<crate::model::ModelItem> =
            history_nodes.into_iter().map(|(_, item)| item).collect();
        // todo 运行时上下文（CB1-05）：非耐久请求组装，不进事件日志。
        if let Some(todo_service) = &self.todo
            && let Some(context) = todo_service.model_context()
        {
            history.insert(
                0,
                crate::model::ModelItem::user_text(format!(
                    "CLAT runtime context (not a new user command):\n{context}"
                )),
            );
        }
        Ok(PreparedRun {
            session_id: id,
            turn,
            history,
            journal,
        })
    }

    fn start_run_with_catalog(
        &mut self,
        request: ApplicationRunRequest,
        run_plugins: Vec<Arc<dyn Plugin>>,
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
        let ApplicationRunRequest {
            attachments,
            prompt,
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
        // 标题生成需要 config/credentials，而它们随后被 move 进
        // AgentRequest；提前克隆。request/header 在 spawn 前从真实的
        // 请求输入构建（审计 P1-14）。
        let title_config = config.clone();
        let title_credentials = credentials.clone();
        let request_header = self.request_header_data(&config)?;
        let header_reason = self.request_header_reason(&request_header.header);
        let emitted_header_value = header_reason
            .is_some()
            .then(|| request_header.header.clone());
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
        self.plugin_host
            .install(crate::plugin_host::RunHostContext {
                providers: Arc::clone(&self.providers),
                model_config: config.clone(),
                credentials: credentials.clone(),
                approver: Arc::clone(&approver),
                // 档位 cell 仅 Shared（TUI）模式传入：FA 档 sampling 免
                // 弹框；Classic（exec）的审批语义由 ExecApprover 表达。
                permission_mode: self
                    .permission_modes_enabled
                    .then(|| Arc::clone(&self.permission_mode)),
                asker: asker_for_host,
                cancel: cancel.clone(),
                usage_cell: Arc::clone(&sampling_usage),
                budget: sampling_budget,
            });
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
        let monitor = Arc::clone(&self.monitor);
        let compactor = self.compactor.clone();
        let todo_service = self.todo.clone();
        let titler = self.titler.clone();
        let title_sender = self
            .title_worker
            .as_ref()
            .map(|worker| worker.sender.clone());
        let subscribers = Arc::clone(&self.subscribers);
        let worker_prompt = prompt.clone();
        let steering_for_worker = steering.clone();
        let plugin_host_worker = Arc::clone(&self.plugin_host);
        let sampling_usage_worker = Arc::clone(&sampling_usage);
        // 档位快照（run 起点）：仅进系统指令说明。决策读共享 cell，
        // 运行中切档即时生效（P3）。
        let permission_mode_snapshot = self
            .permission_modes_enabled
            .then(|| self.permission_mode());
        // 门控通道（A-03 不变量）：worker 先就位并阻塞等待；持久化预备
        // 在 spawn 之后才发生——mount/spawn 失败不可能留下一条已落盘、
        // 却永远得不到回答的 user 消息；预备失败则撤掉发送端，worker
        // 干净退出，同样不留半份状态。用户消息在模型执行前已耐久。
        let (start_sender, start_receiver) = mpsc::sync_channel::<PreparedRun>(1);
        #[cfg(test)]
        if std::mem::take(&mut self.fail_next_run_spawn) {
            return Err(ApplicationError::new(
                "intentional run worker spawn failure",
            ));
        }
        let worker = std::thread::Builder::new()
            .name("clat-run".into())
            .spawn(move || {
                let prepared = match start_receiver.recv() {
                    Ok(prepared) => prepared,
                    Err(_) => {
                        // 发送端被撤（预备失败）：持久层无状态可清理，但
                        // 宿主桥上下文是 run 启动路径上装的——卸掉。
                        let _ = run_scope.close();
                        plugin_host_worker.clear();
                        busy.store(false, Ordering::Release);
                        return;
                    }
                };
                let PreparedRun {
                    session_id,
                    turn,
                    mut history,
                    journal,
                } = prepared;
                // todo（INV-T3）：事件直达日志——write 在绑定 journal 上
                // 追加 todo/write，恢复走 todo 投影。
                if let Some(todo_service) = &todo_service {
                    todo_service.bind_run(&session_id, Arc::clone(&journal));
                }
                // 自动压缩（INV-C6/C11）：surface 节点重建后按预算压缩；
                // 事件族 + replace 原子落盘。网络摘要只发生在 worker
                // 内；失败降级绝不 fail run。
                if let Some(compactor) = &compactor {
                    let note = run_auto_compaction(
                        compactor.as_ref(),
                        sessions.as_ref(),
                        journal.as_ref(),
                        &config,
                        &credentials,
                        &cancel,
                        turn,
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
                    // Post-replace surface is the model history now.
                    if let Ok(nodes) = sessions.surface_nodes() {
                        history = nodes.into_iter().map(|(_, item)| item).collect();
                    }
                }
                plugin_host_worker.update_run_metadata(&session_id.to_string(), &history);
                let captured_text = Arc::new(Mutex::new(String::new()));
                let ui_events: Box<dyn EventSink + Send> = Box::new(CapturingEventSink {
                    inner: events,
                    text: Arc::clone(&captured_text),
                });
                let (mut recorder_core, journaling_approver) = SessionRecorder::with_approver(
                    Arc::clone(&journal),
                    Arc::clone(&approver),
                    request_header,
                    &title_config.protocol.to_string(),
                    &title_config.model,
                    turn,
                    header_reason,
                );
                // INV-S6：recorder 在 ModelResponded 落账点归并 sampling
                // usage（journal 侧唯一记账点）。
                recorder_core.attach_aux_usage(Arc::clone(&sampling_usage_worker));
                // B1 + F-1（审计）：唯一花费仪表——recorder 落账充值
                //（INV-S6 口径，含插件采样），run.rs 检查点读同一实例。
                let spend_ledger = Arc::new(crate::model::RunSpendLedger::new(
                    config.effective_run_token_budget(),
                ));
                recorder_core.set_run_ledger(Arc::clone(&spend_ledger));
                let recorder = Arc::new(Mutex::new(recorder_core));
                // FP-09（架构层）：frontend sink 移出 recorder——
                // RecorderHandle 在 recorder 锁**外**转发事件；终态在
                // finish 后由 worker 经同一 sink 发布（journal 已闭合，
                // frontend 故障不再能毒化持久化临界区）。
                let ui_sink = Arc::new(Mutex::new(ui_events));
                let recorder_sink: Box<dyn EventSink + Send> = Box::new(RecorderHandle {
                    recorder: Arc::clone(&recorder),
                    sink: Arc::clone(&ui_sink),
                });
                let approver: Arc<dyn PermissionApprover> = Arc::new(journaling_approver);
                let panic_text_slot = Arc::clone(&captured_text);
                let prompt_for_request = worker_prompt.clone();
                let permission_mode_for_request = permission_mode_snapshot;
                let execution = catch_unwind(AssertUnwindSafe(|| {
                    agent.execute(AgentRequest {
                        config,
                        spend_ledger: Some(Arc::clone(&spend_ledger)),
                        credentials,
                        history_items: history,
                        prompt: prompt_for_request,
                        cancel: cancel.clone(),
                        steering: steering_for_worker,
                        approver,
                        events: recorder_sink,
                        permission_mode: permission_mode_for_request,
                    })
                }));
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
                let was_cancelled = cancel.is_cancelled();
                let reason = match &outcome {
                    Some(Ok(_)) if was_cancelled => TurnEndReason::Aborted {
                        reason: TurnEndCancelCause::User,
                    },
                    Some(Ok(_)) => TurnEndReason::Completed,
                    Some(Err(failure)) => TurnEndReason::Error {
                        error: json!({ "message": failure.error.to_string() }),
                    },
                    None => TurnEndReason::Error {
                        error: json!({ "message": "run worker panicked" }),
                    },
                };
                // FP-09：毒锁恢复（防御）+ finish 返回待发布终态。
                // recorder 锁在 finish 后立即释放——终态转发在锁外执行：
                // frontend 故障（panic）不能反噬已闭合的 journal 与 run
                // 结果上报（P1-09 顺序不变量：journal 闭合先于发布）。
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
                    let forwarded =
                        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| sink.emit(event)));
                    if forwarded.is_err() {
                        eprintln!(
                            "clat: warning: frontend event sink panicked while publishing the terminal event"
                        );
                    }
                }
                let _ = sessions.sync_active();
                if let Some(todo_service) = &todo_service {
                    todo_service.unbind();
                }
                let result = match (outcome, journal_error, panic_text) {
                    (Some(result), journal_error, panic_text) => {
                        let base = result
                            .map(|done| ApplicationRunDone {
                                output: done.text,
                                turns: done.turns,
                                usage: done.usage,
                                cancelled: was_cancelled,
                            })
                            .map_err(|failure| {
                                let (message, turns, usage, _) = failure.error.into_parts();
                                ApplicationRunFailure {
                                    error: message,
                                    turns,
                                    usage,
                                }
                            });
                        match (base, journal_error, panic_text) {
                            (base, None, None) => base,
                            (Ok(done), Some(error), _) => Err(ApplicationRunFailure {
                                error,
                                turns: done.turns,
                                usage: done.usage,
                            }),
                            (Ok(done), None, Some(text)) => Err(ApplicationRunFailure {
                                error: format!("{text} (run had completed: {})", done.output),
                                turns: done.turns,
                                usage: done.usage,
                            }),
                            (Err(failure), Some(error), _) => Err(ApplicationRunFailure {
                                error: format!("{}; {error}", failure.error),
                                turns: failure.turns,
                                usage: failure.usage,
                            }),
                            (Err(failure), None, Some(text)) => Err(ApplicationRunFailure {
                                error: format!("{text}; {}", failure.error),
                                turns: failure.turns,
                                usage: failure.usage,
                            }),
                        }
                    }
                    (None, journal_error, panic_text) => Err(ApplicationRunFailure {
                        error: match (panic_text, journal_error) {
                            (Some(text), Some(error)) => format!("{text}; {error}"),
                            (Some(text), None) => text,
                            (None, Some(error)) => error,
                            (None, None) => "run worker panicked".into(),
                        },
                        turns: 0,
                        usage: Usage::default(),
                    }),
                };
                let close_result = run_scope.close();
                monitor.refresh();
                let result = match (result, close_result) {
                    (result, Ok(())) => result,
                    (Ok(done), Err(error)) => Err(ApplicationRunFailure {
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
                busy.store(false, Ordering::Release);
                let _ = completion.send(result);
            })
            .map_err(|error| ApplicationError::new(format!("spawn run worker: {error}")))?;
        *join_slot
            .lock()
            .map_err(|_| ApplicationError::new("run join lock poisoned"))? = Some(worker);

        // 预备（CAS + 首批耐久批）发生在 worker 就位之后：失败时撤掉
        // 发送端并 join，持久层不留任何本轮痕迹。
        let prepared = match self.prepare_run(&prompt, &attachments) {
            Ok(prepared) => prepared,
            Err(error) => {
                drop(start_sender);
                handle.join()?;
                return Err(error);
            }
        };
        if start_sender.send(prepared).is_err() {
            handle.join()?;
            return Err(ApplicationError::new(
                "run worker stopped before execution started",
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

    /// 运行中插话（DSH `steer()`）：消息进入活动 run 的队列，在下一次
    /// 模型请求边界并入对话（不打断在途请求）。run 不在执行、或已到
    /// 终态（队列封口，W1-04）时返回 `NotRunning`，调用方回退为普通
    /// 提交。未被 claim 的消息不落盘。
    pub fn steer(&self, text: impl Into<String>) -> SteerOutcome {
        let Some(handle) = &self.active_run else {
            return SteerOutcome::NotRunning;
        };
        if handle.is_finished() {
            return SteerOutcome::NotRunning;
        }
        // 入队与终态封口同一把锁（W1-04）：Sealed 意味着 run 已判定
        // 结束、消息永远无人 claim——绝不能当 Queued 返回。
        match handle.steering.try_push(text) {
            crate::run::PushOutcome::Accepted => SteerOutcome::Queued,
            crate::run::PushOutcome::Sealed => SteerOutcome::NotRunning,
        }
    }

    /// 召回最后一条未 claim 的插话（ESC 栈式语义的第一优先级）：
    /// 文本退回调用方（前端放回编辑框，可改可重发）。无活动 run、run
    /// 已结束、或消息已被 claim（进入 journal、不可撤回）时返回
    /// `None`——此时前端的 ESC 应回落到取消 run。召回不触碰 journal。
    pub fn recall_pending_steering(&self) -> Option<String> {
        let handle = self.active_run.as_ref()?;
        if handle.is_finished() {
            return None;
        }
        handle.steering.recall_last()
    }
}

pub struct ApplicationRunRequest {
    pub prompt: String,
    /// 随本次消息附加的本地图片（用户绝对路径）。prepare 阶段复制进
    /// 会话附件目录、以绝对引用落 journal（M4）；空 = 纯文本消息。
    pub attachments: Vec<std::path::PathBuf>,
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
}

pub type ApplicationRunResult = Result<ApplicationRunDone, ApplicationRunFailure>;

#[derive(Clone, Debug)]
pub struct ApplicationRunDone {
    pub output: String,
    pub turns: usize,
    pub usage: Usage,
    pub cancelled: bool,
}

#[derive(Clone, Debug)]
pub struct ApplicationRunFailure {
    pub error: String,
    pub turns: usize,
    pub usage: Usage,
}

/// `Application::steer` 的结果：入队成功，或当前没有可插话的活动 run。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SteerOutcome {
    Queued,
    NotRunning,
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
