use crate::interaction::{AskAnswer, AskQuestion, UserAsker};
use crate::model::CancelToken;
use crate::{
    ApplicationEvent, ApplicationRunResult, EventSink, PermissionApprover, PermissionDecision,
    PermissionRequest, RunEvent,
};
use std::sync::mpsc::{self, Receiver, Sender, SyncSender};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

/// TUI-local event multiplexing. It carries core facts and terminal input;
/// it does not assemble or execute business runtime components.
pub(crate) enum UiEvent {
    Terminal(crossterm::event::Event),
    Worker(WorkerMessage),
    Application(ApplicationEvent),
    /// dsh 模式第四源（D-2 §1.3）：连接/WS/HTTP 三线程的 DshEvent 经
    /// 转发线程汇入——dsh 生命周期与本地 run worker 不同构，独立成源
    /// 防两边状态机互相污染（设计否决记录 1）。
    Dsh(crate::dsh::backend::DshEvent),
}

pub(crate) enum WorkerMessage {
    Event(RunEvent),
    PermissionRequest {
        request: PermissionRequest,
        decision_tx: Sender<PermissionDecision>,
    },
    AskUserRequest {
        question: AskQuestion,
        answer_tx: Sender<AskAnswer>,
    },
    /// Result of the single bounded explicit clipboard worker. Image paths
    /// are minted by the existing core draft staging registry; Ctrl+V may
    /// instead return ordinary composer text or an empty result.
    ClipboardPastePrepared(Result<super::attachments::PreparedClipboardPaste, String>),
    /// Initial attachment admission can decode and normalize up to eight
    /// images. The whole application is temporarily handed to a bounded
    /// worker so none of that filesystem/codec work runs on the render/input
    /// thread. The first run event is held behind `gate` until the frontend
    /// has restored the application and installed its run state.
    RunStartFinished(Box<RunStartFinished>),
    /// An image-bearing steering message needs the same bounded decode and
    /// admission work as an initial send. The existing run may keep emitting
    /// events while the application is temporarily owned by this worker; the
    /// frontend gates new input until this message restores that sole owner.
    SteeringAdmissionFinished(Box<SteeringAdmissionFinished>),
    /// W1-13：完成消息携带 run 纪元（TUI 本地单调计数）。收尾窗口里
    /// 陈旧的上一 run 完成会晚于新 run 启动送达——无身份时 finish_run
    /// 会 take/join **新** run 的句柄（UI 冻结至新 run 结束、产出错档）。
    Done {
        epoch: u64,
        result: ApplicationRunResult,
    },
}

pub(crate) struct RunStartFinished {
    pub(crate) application: crate::TrustedProjectApplication,
    pub(crate) prompt: String,
    pub(crate) outcome: Result<PreparedTuiRun, String>,
}

pub(crate) struct SteeringAdmissionFinished {
    pub(crate) application: crate::TrustedProjectApplication,
    pub(crate) prompt: String,
    pub(crate) outcome: crate::SteerOutcome,
}

pub(crate) struct PreparedTuiRun {
    pub(crate) handle: crate::RunHandle,
    pub(crate) completed: Receiver<crate::ApplicationRunResult>,
    pub(crate) gate: RunStartGate,
}

/// One-shot barrier between core run startup and frontend event delivery.
/// The run worker blocks at its first event rather than letting RunStarted,
/// provider deltas, or permission requests race ahead of RunStartFinished.
#[derive(Clone)]
pub(crate) struct RunStartGate(Arc<(Mutex<bool>, Condvar)>);

impl RunStartGate {
    pub(crate) fn closed() -> Self {
        Self(Arc::new((Mutex::new(false), Condvar::new())))
    }

    pub(crate) fn open(&self) {
        let (lock, ready) = &*self.0;
        let mut open = lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        *open = true;
        ready.notify_all();
    }

    fn wait(&self) {
        let (lock, ready) = &*self.0;
        let mut open = lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        while !*open {
            open = ready
                .wait(open)
                .unwrap_or_else(|poisoned| poisoned.into_inner());
        }
    }
}

pub(crate) struct DeferredChannelEventSink {
    sender: SyncSender<UiEvent>,
    gate: RunStartGate,
}

impl DeferredChannelEventSink {
    pub(crate) fn new(sender: SyncSender<UiEvent>, gate: RunStartGate) -> Self {
        Self { sender, gate }
    }
}

impl EventSink for DeferredChannelEventSink {
    fn emit(&mut self, event: RunEvent) {
        self.gate.wait();
        let _ = self
            .sender
            .send(UiEvent::Worker(WorkerMessage::Event(event)));
    }
}

pub(crate) struct ChannelEventSink(pub(crate) SyncSender<UiEvent>);

impl EventSink for ChannelEventSink {
    fn emit(&mut self, event: RunEvent) {
        let _ = self.0.send(UiEvent::Worker(WorkerMessage::Event(event)));
    }
}

pub(crate) struct ChannelApprover {
    sender: SyncSender<UiEvent>,
}

impl ChannelApprover {
    pub(crate) fn new(sender: SyncSender<UiEvent>) -> Self {
        Self { sender }
    }
}

impl PermissionApprover for ChannelApprover {
    fn decide(&self, request: PermissionRequest, cancel: &CancelToken) -> PermissionDecision {
        let (decision_tx, decision_rx) = mpsc::channel();
        if self
            .sender
            .send(UiEvent::Worker(WorkerMessage::PermissionRequest {
                request,
                decision_tx,
            }))
            .is_err()
        {
            return PermissionDecision::Deny {
                reason: "permission UI is unavailable".into(),
            };
        }
        // W1-17/A1：轮询而非裸阻塞（对齐 ChannelUserAsker 的 50ms 模式）
        // ——run 取消（Esc 之外的取消源，如收尾/退出路径）必须能解开
        // worker 线程，不许把审批等待挂到人答。取消语义 = Deny；断连
        //（UI 丢弃对话框）同 Deny，run 可正常退栈。
        loop {
            match decision_rx.recv_timeout(Duration::from_millis(50)) {
                Ok(decision) => return decision,
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    if cancel.is_cancelled() {
                        return PermissionDecision::Deny {
                            reason: "interrupted by run cancellation".into(),
                        };
                    }
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    return PermissionDecision::Deny {
                        reason: "no permission decision available".into(),
                    };
                }
            }
        }
    }
}

/// ask-user 前端实现：把问题送进统一事件通道并阻塞等待对话框应答。
/// 断连与取消都归约为 Declined（isError 工具结果，run 继续）。
pub(crate) struct ChannelUserAsker {
    sender: SyncSender<UiEvent>,
}

impl ChannelUserAsker {
    pub(crate) fn new(sender: SyncSender<UiEvent>) -> Self {
        Self { sender }
    }
}

impl UserAsker for ChannelUserAsker {
    fn ask(&self, question: AskQuestion, cancel: &CancelToken) -> AskAnswer {
        let (answer_tx, answer_rx) = mpsc::channel();
        if self
            .sender
            .send(UiEvent::Worker(WorkerMessage::AskUserRequest {
                question,
                answer_tx,
            }))
            .is_err()
        {
            return AskAnswer::Declined;
        }
        // 轮询而非裸阻塞：Esc 之外还有共享取消令牌（run 取消）必须能
        // 解开 worker 线程。
        loop {
            match answer_rx.recv_timeout(Duration::from_millis(50)) {
                Ok(answer) => return answer,
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    if cancel.is_cancelled() {
                        return AskAnswer::Declined;
                    }
                }
                // 对话框被丢弃（UI 退出）：断连 = 拒绝回答。
                Err(mpsc::RecvTimeoutError::Disconnected) => return AskAnswer::Declined,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ToolEffect;
    use serde_json::json;

    /// RA-02 最后一跳判别腿：DSH 专用队列被转发线程消费后，统一 UI
    /// 队列仍须有界，否则洪水只是换了一个无界容器。
    #[test]
    fn production_ui_event_queue_applies_backpressure() {
        use std::sync::mpsc::TrySendError;

        let (sender, _receiver) = crate::tui::ui_event_channel();
        for generation in 0..crate::tui::UI_EVENT_QUEUE_CAPACITY {
            assert!(
                sender
                    .try_send(UiEvent::Dsh(crate::dsh::backend::DshEvent::LinkDown {
                        generation: generation as u64,
                        reason: "fill".into(),
                    }))
                    .is_ok()
            );
        }
        assert!(matches!(
            sender.try_send(UiEvent::Dsh(crate::dsh::backend::DshEvent::LinkDown {
                generation: u64::MAX,
                reason: "overflow".into(),
            })),
            Err(TrySendError::Full(_))
        ));
    }

    #[test]
    fn deferred_run_sink_cannot_race_events_ahead_of_frontend_handoff() {
        let (sender, receiver) = crate::tui::ui_event_channel();
        let gate = RunStartGate::closed();
        let worker_gate = gate.clone();
        let (entered_tx, entered_rx) = mpsc::channel();
        let worker = std::thread::spawn(move || {
            let mut sink = DeferredChannelEventSink::new(sender, worker_gate);
            entered_tx.send(()).expect("test barrier");
            sink.emit(RunEvent::RunFailed {
                message: "sent only after handoff".into(),
            });
        });

        entered_rx.recv().expect("producer reached emit");
        assert!(
            receiver.try_recv().is_err(),
            "closed gate leaked a run event"
        );
        assert!(!worker.is_finished(), "producer must wait at the gate");

        gate.open();
        assert!(matches!(
            receiver.recv_timeout(Duration::from_secs(1)),
            Ok(UiEvent::Worker(WorkerMessage::Event(RunEvent::RunFailed { message })))
                if message == "sent only after handoff"
        ));
        worker.join().expect("event producer");
    }

    #[test]
    fn dropping_the_permission_dialog_unblocks_the_approver() {
        let (sender, receiver) = crate::tui::ui_event_channel();
        let approver = ChannelApprover::new(sender);
        let worker = std::thread::spawn(move || {
            approver.decide(
                PermissionRequest {
                    tool: "write_file".into(),
                    effect: ToolEffect::Write,
                    reason: "test".into(),
                    arguments: json!({}),
                    call_id: "call-1".into(),
                },
                &crate::model::CancelToken::new(),
            )
        });
        match receiver.recv().expect("permission event") {
            UiEvent::Worker(WorkerMessage::PermissionRequest { decision_tx, .. }) => {
                drop(decision_tx);
            }
            _ => panic!("expected permission request"),
        }
        assert!(matches!(
            worker.join().expect("approver thread"),
            PermissionDecision::Deny { .. }
        ));
    }

    #[test]
    fn dropping_the_ask_dialog_declines_the_question() {
        let (sender, receiver) = crate::tui::ui_event_channel();
        let asker = ChannelUserAsker::new(sender);
        let worker = std::thread::spawn(move || {
            asker.ask(
                AskQuestion {
                    question: "ship it?".into(),
                    options: Vec::new(),
                    allow_custom: true,
                },
                &CancelToken::new(),
            )
        });
        match receiver.recv().expect("ask event") {
            UiEvent::Worker(WorkerMessage::AskUserRequest { answer_tx, .. }) => {
                drop(answer_tx);
            }
            _ => panic!("expected ask request"),
        }
        assert!(matches!(
            worker.join().expect("asker thread"),
            AskAnswer::Declined
        ));
    }

    /// W1-17/A1：run 取消令牌必须能解开滞留的审批等待——无人应答的
    /// 权限请求在令牌置位后 ≤ 有界间隔返回 Deny，而不是挂到人答。
    /// 判别：去掉 50ms 轮询（回到裸 recv）即本测试挂死而红。
    #[test]
    fn a_cancelled_run_unblocks_the_pending_permission_wait() {
        let (sender, _receiver) = crate::tui::ui_event_channel();
        let approver = ChannelApprover::new(sender);
        let cancel = CancelToken::new();
        let wait_cancel = cancel.clone();
        let worker_cancel = cancel.clone();
        let worker = std::thread::spawn(move || {
            approver.decide(
                PermissionRequest {
                    tool: "write_file".into(),
                    effect: ToolEffect::Write,
                    reason: "test".into(),
                    arguments: serde_json::json!({}),
                    call_id: "call-1".into(),
                },
                &worker_cancel,
            )
        });
        // 模拟 run 收尾：请求在途时令牌置位（UI 侧不答复）。
        std::thread::sleep(Duration::from_millis(100));
        let started = std::time::Instant::now();
        wait_cancel.cancel();
        let decision = worker.join().expect("approver thread");
        assert!(
            matches!(&decision, PermissionDecision::Deny { reason } if reason.contains("cancel")),
            "unexpected decision: {decision:?}"
        );
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "cancel must unblock the wait within a bounded interval: {:?}",
            started.elapsed()
        );
    }
}
