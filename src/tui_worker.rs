use crate::{
    ApplicationEvent, ApplicationRunResult, EventSink, PermissionApprover, PermissionDecision,
    PermissionRequest, RunEvent,
};
use std::sync::mpsc::{self, Sender};

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
    Done(ApplicationRunResult),
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
}
