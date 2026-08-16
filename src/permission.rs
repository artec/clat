use crate::project::Project;
use crate::tool::{ToolCall, ToolDefinition, ToolEffect};
use serde_json::Value;
use std::sync::Arc;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PermissionDecision {
    Allow,
    Ask { reason: String },
    Deny { reason: String },
}

/// A side-effecting tool call a policy needs a human to approve or deny.
///
/// `Run` itself never constructs this; it is the contract between an
/// [`InteractivePermissionPolicy`] and whatever front end (TUI, IDE, headless
/// prompt) can answer it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PermissionRequest {
    pub tool: String,
    pub effect: ToolEffect,
    pub reason: String,
    pub arguments: Value,
}

/// UI-independent port implemented by TUI, desktop, headless clients, or
/// tests. It answers requests; permission classification remains in core.
pub trait PermissionApprover: Send + Sync {
    fn decide(&self, request: PermissionRequest) -> PermissionDecision;
}

impl<F> PermissionApprover for F
where
    F: Fn(PermissionRequest) -> PermissionDecision + Send + Sync,
{
    fn decide(&self, request: PermissionRequest) -> PermissionDecision {
        self(request)
    }
}

pub trait PermissionPolicy: Send + Sync {
    fn check(
        &self,
        project: &Project,
        tool: &ToolDefinition,
        call: &ToolCall,
    ) -> PermissionDecision;
}

/// Wraps a base policy and resolves `Ask` decisions through an injected
/// interactive approver instead of failing the run.
///
/// The approver is a plain closure, so any client can implement it: the TUI
/// shows a dialog and blocks on the user, while a test or future headless
/// mode can answer programmatically. `Allow` and `Deny` from the base policy
/// pass through untouched; only `Ask` invokes the approver.
pub struct InteractivePermissionPolicy {
    delegate: Box<dyn PermissionPolicy>,
    approver: Arc<dyn PermissionApprover>,
}

impl InteractivePermissionPolicy {
    pub fn new(
        delegate: impl PermissionPolicy + 'static,
        ask: Box<dyn Fn(PermissionRequest) -> PermissionDecision + Send + Sync>,
    ) -> Self {
        Self::with_approver(delegate, Arc::new(BoxedApprover(ask)))
    }

    pub fn with_approver(
        delegate: impl PermissionPolicy + 'static,
        approver: Arc<dyn PermissionApprover>,
    ) -> Self {
        Self {
            delegate: Box::new(delegate),
            approver,
        }
    }
}

struct BoxedApprover(Box<dyn Fn(PermissionRequest) -> PermissionDecision + Send + Sync>);

impl PermissionApprover for BoxedApprover {
    fn decide(&self, request: PermissionRequest) -> PermissionDecision {
        (self.0)(request)
    }
}

impl PermissionPolicy for InteractivePermissionPolicy {
    fn check(
        &self,
        project: &Project,
        tool: &ToolDefinition,
        call: &ToolCall,
    ) -> PermissionDecision {
        match self.delegate.check(project, tool, call) {
            PermissionDecision::Ask { reason } => {
                let request = PermissionRequest {
                    tool: tool.name.clone(),
                    effect: tool.effect,
                    reason,
                    arguments: call.arguments.clone(),
                };
                match self.approver.decide(request) {
                    // The approver answers with a final decision only.
                    PermissionDecision::Ask { .. } => PermissionDecision::Deny {
                        reason: "approver returned an unresolved decision".into(),
                    },
                    decision => decision,
                }
            }
            decision => decision,
        }
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct SafeByDefault;

impl PermissionPolicy for SafeByDefault {
    fn check(
        &self,
        _project: &Project,
        tool: &ToolDefinition,
        _call: &ToolCall,
    ) -> PermissionDecision {
        match tool.effect {
            ToolEffect::Pure | ToolEffect::Read | ToolEffect::SessionWrite => {
                PermissionDecision::Allow
            }
            ToolEffect::Write
            | ToolEffect::Execute
            | ToolEffect::Network
            | ToolEffect::ExternalRead
            | ToolEffect::Destructive => PermissionDecision::Ask {
                reason: format!("tool `{}` can cause side effects", tool.name),
            },
        }
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct AllowAll;

impl PermissionPolicy for AllowAll {
    fn check(
        &self,
        _project: &Project,
        _tool: &ToolDefinition,
        _call: &ToolCall,
    ) -> PermissionDecision {
        PermissionDecision::Allow
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;
    use std::sync::mpsc;
    use std::thread;

    fn definition(effect: ToolEffect) -> ToolDefinition {
        ToolDefinition {
            name: "test".into(),
            description: "test tool".into(),
            input_schema: serde_json::json!({"type": "object"}),
            effect,
            strict: true,
        }
    }

    fn call() -> ToolCall {
        ToolCall {
            id: "call-1".into(),
            name: "test".into(),
            arguments: serde_json::json!({"path": "notes.txt"}),
        }
    }

    #[test]
    fn safe_policy_allows_read_only_tools() {
        let policy = SafeByDefault;
        let project = Project::new(".");

        assert_eq!(
            policy.check(&project, &definition(ToolEffect::Read), &call()),
            PermissionDecision::Allow
        );
    }

    #[test]
    fn safe_policy_allows_session_write_but_not_other_side_effects() {
        // INV-T2：SessionWrite 由准确 effect 分类获得免审；Pure 语义
        // 不变，其余副作用仍需询问。
        let policy = SafeByDefault;
        let project = Project::new(".");
        assert_eq!(
            policy.check(&project, &definition(ToolEffect::SessionWrite), &call()),
            PermissionDecision::Allow
        );
        assert_eq!(
            policy.check(&project, &definition(ToolEffect::Pure), &call()),
            PermissionDecision::Allow
        );
        assert!(matches!(
            policy.check(&project, &definition(ToolEffect::Write), &call()),
            PermissionDecision::Ask { .. }
        ));
    }

    #[test]
    fn safe_policy_asks_before_side_effects() {
        let policy = SafeByDefault;
        let project = Project::new(".");

        assert!(matches!(
            policy.check(&project, &definition(ToolEffect::Write), &call()),
            PermissionDecision::Ask { .. }
        ));
    }

    #[test]
    fn interactive_policy_passes_through_decisions_without_asking() {
        let (request_tx, request_rx) = mpsc::channel();
        let (decision_tx, decision_rx) = mpsc::channel();
        let decision_rx = Mutex::new(decision_rx);
        let ask = move |request: PermissionRequest| {
            request_tx.send(request).expect("request");
            decision_rx.lock().expect("lock").recv().expect("decision")
        };
        let policy = InteractivePermissionPolicy::new(SafeByDefault, Box::new(ask));
        let project = Project::new(".");

        // Read tools are auto-allowed by the delegate and never reach the approver.
        assert_eq!(
            policy.check(&project, &definition(ToolEffect::Read), &call()),
            PermissionDecision::Allow
        );
        assert!(request_rx.try_recv().is_err());
        drop(decision_tx);
    }

    #[test]
    fn interactive_policy_asks_and_applies_user_decision() {
        let (request_tx, request_rx) = mpsc::channel();
        let (decision_tx, decision_rx) = mpsc::channel();
        let decision_rx = Mutex::new(decision_rx);
        let ask = move |request: PermissionRequest| {
            request_tx.send(request).expect("request");
            decision_rx.lock().expect("lock").recv().expect("decision")
        };
        let policy = InteractivePermissionPolicy::new(SafeByDefault, Box::new(ask));
        let project = Project::new(".");
        let definition = definition(ToolEffect::Write);
        let call = call();

        let handle = thread::spawn(move || policy.check(&project, &definition, &call));
        let request = request_rx.recv().expect("request");
        assert_eq!(request.tool, "test");
        assert_eq!(request.effect, ToolEffect::Write);
        assert_eq!(request.arguments, serde_json::json!({"path": "notes.txt"}));

        decision_tx
            .send(PermissionDecision::Allow)
            .expect("decision");
        assert_eq!(handle.join().expect("join"), PermissionDecision::Allow);
    }

    #[test]
    fn interactive_policy_turns_an_unresolved_answer_into_deny() {
        let (request_tx, request_rx) = mpsc::channel();
        let (decision_tx, decision_rx) = mpsc::channel();
        let decision_rx = Mutex::new(decision_rx);
        let ask = move |request: PermissionRequest| {
            request_tx.send(request).expect("request");
            decision_rx.lock().expect("lock").recv().expect("decision")
        };
        let policy = InteractivePermissionPolicy::new(SafeByDefault, Box::new(ask));
        let project = Project::new(".");
        let definition = definition(ToolEffect::Execute);
        let call = call();

        let handle = thread::spawn(move || policy.check(&project, &definition, &call));
        let _request = request_rx.recv().expect("request");
        decision_tx
            .send(PermissionDecision::Ask {
                reason: "still unsure".into(),
            })
            .expect("decision");
        assert!(matches!(
            handle.join().expect("join"),
            PermissionDecision::Deny { .. }
        ));
    }
}
