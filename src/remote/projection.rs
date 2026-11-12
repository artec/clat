use crate::event::RunEvent;
use crate::i18n::{UiMessage, tool_action, tool_effect};
use crate::permission::PermissionRequest;

pub(crate) enum RemoteProjection {
    Typing(bool),
    Text(String),
    Notice(UiMessage),
}

pub(crate) fn approval_prompt(request: &PermissionRequest, id: &str) -> UiMessage {
    let reason = crate::redact::redact_secrets(&request.reason);
    let mut summary = reason.chars().take(240).collect::<String>();
    if reason.chars().count() > 240 {
        summary.push('…');
    }
    UiMessage::new("remote.approval_request")
        .arg("id", id)
        .arg_message("action", tool_action(&request.tool))
        .arg_message("effect", tool_effect(request.effect))
        .arg("reason", summary)
}

pub(crate) fn project_run_event(event: &RunEvent) -> Vec<RemoteProjection> {
    match event {
        RunEvent::ModelRequested { .. } => vec![RemoteProjection::Typing(true)],
        RunEvent::ToolStarted { tool, .. } => vec![RemoteProjection::Notice(
            UiMessage::new("remote.tool_running").arg_message("action", tool_action(tool)),
        )],
        RunEvent::RunCompleted { output, .. } => vec![
            RemoteProjection::Typing(false),
            RemoteProjection::Text(crate::redact::redact_secrets(output)),
        ],
        RunEvent::RunCancelled { .. } => vec![
            RemoteProjection::Typing(false),
            RemoteProjection::Notice(UiMessage::new("remote.run_cancelled")),
        ],
        RunEvent::RunFailed { .. } => vec![
            RemoteProjection::Typing(false),
            RemoteProjection::Notice(UiMessage::new("remote.run_failed")),
        ],
        _ => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::i18n::{Locale, LocaleContext};
    use crate::tool::ToolEffect;

    #[test]
    fn tool_projection_is_localized_and_never_exposes_unknown_identifiers() {
        for tool in ["run_command", "plugin_private_internal_name"] {
            let event = RunEvent::ToolStarted {
                call_id: "call".into(),
                tool: tool.into(),
            };
            let mut projected = project_run_event(&event);
            let Some(RemoteProjection::Notice(message)) = projected.pop() else {
                panic!("tool start must produce a notice")
            };
            for locale in [Locale::English, Locale::Simplified, Locale::Traditional] {
                let text = LocaleContext::new(locale).render(&message);
                assert!(!text.contains(tool));
                assert!(!text.contains("[missing translation]"));
            }
        }
    }

    #[test]
    fn approval_projection_keeps_arguments_and_unknown_tool_ids_private() {
        let request = PermissionRequest {
            tool: "plugin_private_tool".into(),
            effect: ToolEffect::Execute,
            reason: "approval required".into(),
            arguments: serde_json::json!({"command": "private command content"}),
            call_id: "wire-call-id".into(),
        };
        let message = approval_prompt(&request, "approval-id");
        for locale in [Locale::English, Locale::Simplified, Locale::Traditional] {
            let text = LocaleContext::new(locale).render(&message);
            assert!(text.contains("approval-id"));
            assert!(!text.contains("plugin_private_tool"));
            assert!(!text.contains("private command content"));
            assert!(!text.contains("wire-call-id"));
            assert!(!text.contains("[missing translation]"));
        }
    }
}
