use crate::interaction::{AskAnswer, AskQuestion, UserAsker};
use crate::model::CancelToken;
use crate::{
    ApplicationEvent, ApplicationRunResult, EventSink, PermissionApprover, PermissionDecision,
    PermissionRequest, RunEvent,
};
use std::sync::mpsc::{self, Sender};
use std::time::Duration;

/// TUI-local event multiplexing. It carries core facts and terminal input;
/// it does not assemble or execute business runtime components.
pub(crate) enum UiEvent {
    Terminal(crossterm::event::Event),
    Worker(WorkerMessage),
    Application(ApplicationEvent),
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
    /// W1-13：完成消息携带 run 纪元（TUI 本地单调计数）。收尾窗口里
    /// 陈旧的上一 run 完成会晚于新 run 启动送达——无身份时 finish_run
    /// 会 take/join **新** run 的句柄（UI 冻结至新 run 结束、产出错档）。
    Done {
        epoch: u64,
        result: ApplicationRunResult,
    },
}

pub(crate) struct ChannelEventSink(pub(crate) Sender<UiEvent>);

impl EventSink for ChannelEventSink {
    fn emit(&mut self, event: RunEvent) {
        let _ = self.0.send(UiEvent::Worker(WorkerMessage::Event(event)));
    }
}

pub(crate) struct ChannelApprover {
    sender: Sender<UiEvent>,
}

impl ChannelApprover {
    pub(crate) fn new(sender: Sender<UiEvent>) -> Self {
        Self { sender }
    }
}

impl PermissionApprover for ChannelApprover {
    fn decide(&self, request: PermissionRequest) -> PermissionDecision {
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
        // The receiver has no sender retained by the approver itself. If the
        // UI exits and drops its pending dialog, recv disconnects and the run
        // can unwind instead of blocking Application teardown forever.
        decision_rx
            .recv()
            .ok()
            .unwrap_or_else(|| PermissionDecision::Deny {
                reason: "no permission decision available".into(),
            })
    }
}

/// ask-user 前端实现：把问题送进统一事件通道并阻塞等待对话框应答。
/// 断连与取消都归约为 Declined（isError 工具结果，run 继续）。
pub(crate) struct ChannelUserAsker {
    sender: Sender<UiEvent>,
}

impl ChannelUserAsker {
    pub(crate) fn new(sender: Sender<UiEvent>) -> Self {
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

    #[test]
    fn dropping_the_permission_dialog_unblocks_the_approver() {
        let (sender, receiver) = mpsc::channel();
        let approver = ChannelApprover::new(sender);
        let worker = std::thread::spawn(move || {
            approver.decide(PermissionRequest {
                tool: "write_file".into(),
                effect: ToolEffect::Write,
                reason: "test".into(),
                arguments: json!({}),
                call_id: "call-1".into(),
            })
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
        let (sender, receiver) = mpsc::channel();
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
}
