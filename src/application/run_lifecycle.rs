use crate::CancelToken;
use crate::event::EventSink;
use crate::model::Usage;
use crate::permission::PermissionApprover;
use crate::plugin::Plugin;
use crate::plugins::run_catalog;
use std::sync::{Arc, mpsc};

use super::*;

use super::run_execution::{RunExecutionEngine, RunExecutionSpec, RunHandle};
use super::threads::MCP_STARTUP_RUN_WAIT;

impl TrustedProjectApplication {
    pub fn start_run(
        &mut self,
        request: ApplicationRunRequest,
    ) -> Result<RunHandle, ApplicationError> {
        if !self.input_queue().accepts_initial(&request.message) {
            return Err(ApplicationError::presented(crate::i18n::UiMessage::new(
                "input.changed",
            )));
        }
        // An ordinary human prompt competes with any previously armed goal.
        // Preserve durable phase, remove only process-local continuation
        // authority, matching DSH goal-round-driver's competing prompt fence.
        self.goal.disarm();
        let client_message_id = request.message.client_message_id.clone();
        let cancel = self
            .task
            .deadline()
            .map_or_else(CancelToken::new, CancelToken::with_deadline);
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
        self.check_goal_activation()?;
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

    fn start_run_with_catalog(
        &mut self,
        request: ApplicationRunRequest,
        run_plugins: Vec<Arc<dyn Plugin>>,
        goal_round: Option<crate::goal::GoalRound>,
    ) -> Result<RunHandle, ApplicationError> {
        self.task
            .ensure_start_allowed()
            .map_err(ApplicationError::new)?;
        self.prepare_execution_slot()?;
        // 模型配置检查在前：无模型时立即失败，不为 MCP 白等。
        let (config, credentials) = self.model_state()?;
        if !config.is_configured() {
            return Err(ApplicationError::new(
                "model is not configured; configure a model and endpoint first",
            ));
        }
        self.freeze_run_tools()?;
        let pending_context = self.prepare_run_context(config.clone(), &request.message)?;
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
                target: None,
                run_plugins,
                config: config.clone(),
                credentials: credentials.clone(),
                approver: Arc::clone(&approver),
                events,
                completion,
            },
        )?;

        let mut emitted_header_value = None;
        let handle = waiting_execution.start(
            || {
                // Freeze workflow context only after Fresh birth events commit.
                let prepared = self.admit_run(&request_message, goal_round.as_ref())?;
                self.commit_initial_input(&request_message);
                let start = pending_context.admitted(self, prepared)?;
                emitted_header_value = start.header_reason.is_some().then(|| {
                    crate::session::recorder::persisted_request_header(&start.request_header.header)
                });
                Ok(start)
            },
            asker_for_host,
        )?;
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

    fn freeze_run_tools(&self) -> Result<(), ApplicationError> {
        // MCP 后台启动落定（有界等待）后再冻结工具注册表（INV-M2/M3）：
        // 任何 run 看到的都是完整工具集——除非等待超时（此时以现状冻结，
        // 状态面板可见未落定 server，修复后需重启以重新挂载）。无 MCP
        // 配置时立即返回，零等待成本。
        let _settled = self.mcp_status.wait_until_settled(MCP_STARTUP_RUN_WAIT);
        self.tools
            .freeze()
            .map_err(|error| ApplicationError::new(error.to_string()))?;
        self.prompts.freeze();
        Ok(())
    }

    pub(super) fn prepare_execution_slot(&mut self) -> Result<(), ApplicationError> {
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
        Ok(())
    }

    fn selected_run(&self) -> Option<&RunHandle> {
        let handle = self.active_run.as_ref()?;
        (handle.session_id() == self.selection.as_ref()).then_some(handle)
    }

    pub fn cancel_active_run(&self) {
        self.cancel_workflow_continuation();
        self.goal.disarm();
        self.transient_inputs.queue.pause();
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
    pub fn steer(&self, message: crate::message::PendingMessage) -> SteerOutcome {
        if message
            .client_message_id
            .as_ref()
            .is_some_and(|id| self.input_queue().existing(id).is_some())
        {
            return SteerOutcome::Refused {
                reason: crate::i18n::LocaleContext::new(crate::i18n::Locale::English)
                    .render(&crate::i18n::UiMessage::new("input.changed")),
                receipt: steering_rollback_receipt(&message, "steering-admission"),
            };
        }
        if message.content.has_images() && message.admitted_images.is_empty() {
            return SteerOutcome::Refused {
                reason: "image content must be admitted from staged sources by core".into(),
                receipt: steering_rollback_receipt(&message, "steering-admission"),
            };
        }
        let Some(handle) = self.selected_run() else {
            return SteerOutcome::NotRunning {
                receipt: steering_rollback_receipt(&message, "steering-not-running"),
            };
        };
        if handle.is_finished() {
            return SteerOutcome::NotRunning {
                receipt: steering_rollback_receipt(&message, "steering-not-running"),
            };
        }
        match self.prepare_input_message(message.clone()) {
            Ok(prepared) => self.queue_steering_message(handle, prepared),
            Err(error) => SteerOutcome::Refused {
                reason: error.reason,
                receipt: steering_rollback_receipt(&message, error.phase),
            },
        }
    }

    fn queue_steering_message(
        &self,
        handle: &RunHandle,
        message: crate::message::PendingMessage,
    ) -> SteerOutcome {
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
        let handle = self.selected_run()?;
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

pub(super) fn validate_route_images(
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

impl TrustedProjectApplication {
    pub(super) fn memory_for_message(
        &self,
        message: &crate::message::PendingMessage,
    ) -> Result<crate::memory::MemoryInjection, ApplicationError> {
        let memory_query = message.content.plain_text();
        // Image-only is a valid multimodal user message. Memory retrieval is
        // text-indexed, so use a stable semantic sentinel rather than letting
        // its non-empty query guard reject the entire run before admission.
        let memory_query = if memory_query.is_empty()
            && (!message.staged_attachments.is_empty() || message.content.has_images())
        {
            "[image-only user message]"
        } else {
            memory_query.as_str()
        };
        let Some(memory) = &self.memory else {
            return Ok(crate::memory::MemoryInjection::empty());
        };
        memory
            .injection(memory_query)
            .map_err(ApplicationError::new)
    }
}

impl TrustedProjectApplication {
    /// A task's embedded composer may cancel only the run belonging to its target.
    pub fn cancel_conversation_run(&self, session: &str) -> bool {
        if self
            .selection
            .as_ref()
            .is_none_or(|id| id.as_str() != session)
        {
            return false;
        }
        if self.active_run.is_some() && self.selected_run().is_none() {
            return false;
        }
        self.cancel_active_run();
        true
    }
}

#[cfg(test)]
mod acceptance_cleanup_tests {
    use super::*;
    use crate::plugin::{
        DisposeError, PluginContext, PluginDescriptor, PluginError, PluginId, ScopeKind,
    };
    struct FailingCleanup {
        sessions: Arc<SessionService>,
        fail_write: bool,
    }
    impl Plugin for FailingCleanup {
        fn descriptor(&self) -> &'static PluginDescriptor {
            static D: PluginDescriptor = PluginDescriptor {
                id: PluginId::new("test.ga2.cleanup"),
                scope: ScopeKind::Run,
                provides: &[],
                requires: &[],
                optional: &[],
            };
            &D
        }
        fn mount(&self, context: &mut PluginContext<'_>) -> Result<(), PluginError> {
            let sessions = self.sessions.clone();
            let fail_write = self.fail_write;
            context.defer(move || {
                if fail_write {
                    sessions.inject_persistence_faults(crate::session::persistence::FaultHooks {
                        fail_batch_write: true,
                        ..Default::default()
                    });
                    Ok(())
                } else {
                    Err(DisposeError::new("injected run disposer failure"))
                }
            });
            Ok(())
        }
    }
    #[test]
    fn ga2_actual_run_scope_disposer_failure_blocks_verified_completion() {
        scope_failure(false);
    }
    #[test]
    fn ga2_rework_final_task_and_terminal_write_failure_never_publishes_success() {
        scope_failure(true);
    }
    fn scope_failure(fail_write: bool) {
        let (mut app, root) =
            crate::application::task_acceptance_tests::pending_completion_app("ga2-scope-failure");
        let check = app.task_check().unwrap();
        let resume = crate::task::TaskResume {
            task_id: check.task.id.clone(),
            revision: check.task.revision,
            check_digest: check.digest.clone(),
            accept_changes: false,
        };
        app.task.begin(&resume, &check, Some(200000)).unwrap();
        let approver: Arc<dyn PermissionApprover> = Arc::new(
            crate::test_support::CountingApprover(Arc::new(std::sync::atomic::AtomicUsize::new(0))),
        );
        let mut catalog = run_catalog(CancelToken::new(), approver.clone());
        catalog.push(Arc::new(FailingCleanup {
            sessions: app.sessions.clone(),
            fail_write,
        }));
        let observed = Arc::new(std::sync::Mutex::new(Vec::new()));
        let (completion, rx) = mpsc::channel();
        let handle = app
            .start_run_with_catalog(
                ApplicationRunRequest {
                    message: crate::message::PendingMessage::text("explicit checked task"),
                    asker: None,
                    approver,
                    events: Box::new(crate::test_support::SharedEvents(observed.clone())),
                    completion,
                },
                catalog,
                None,
            )
            .unwrap();
        let failure = rx
            .recv_timeout(std::time::Duration::from_secs(20))
            .unwrap()
            .unwrap_err();
        handle.join().unwrap();
        assert!(failure.error.contains(if fail_write {
            "task completion gate failed"
        } else {
            "run scope cleanup failed"
        }));
        app.sessions
            .inject_persistence_faults(crate::session::persistence::FaultHooks::default());
        crate::application::task_acceptance_tests::assert_task_terminal_history(
            &mut app, &observed, "error",
        );
        let s = app.task_state().unwrap().unwrap();
        assert_eq!(s.phase, "paused");
        let a = s.acceptance_state.unwrap();
        assert!(a.completion.is_none());
        assert_eq!(
            a.candidate.unwrap().state,
            if fail_write { "pending" } else { "rejected" }
        );
        assert!(s.runs[0].settled && s.runs[0].tokens > 0);
        let saved = app.task_state().unwrap().unwrap();
        app.close().unwrap();
        let storage = root.parent().unwrap().join("storage");
        let mut app = crate::test_support::reopen_after_close(|| {
            Ok::<_, ApplicationError>(crate::application::task_acceptance_tests::mount_checked(
                &root,
                &storage,
                crate::test_support::TestBehavior::Success,
            ))
        });
        assert_eq!(app.task_state().unwrap().unwrap(), saved);
        crate::application::task_acceptance_tests::assert_task_terminal_history(
            &mut app, &observed, "error",
        );
        app.close().unwrap();
        crate::test_support::cleanup_tree(root.parent().unwrap());
    }
}
