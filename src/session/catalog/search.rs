//! Search's message classification follows the existing catalog projection kind.
use super::*;
use crate::session::event::SessionEvent;
use serde_json::Value;
pub(crate) struct SearchDocument {
    pub kind: &'static str,
    pub text: String,
}
pub(crate) fn document(event: &SessionEvent) -> Option<SearchDocument> {
    let (kind, blocks) = match replay_kind(&event.event_type) {
        ReplayKind::UserMessage => ("user", &event.data["content"]),
        ReplayKind::AssistantMessage => ("assistant", &event.data["message"]["content"]),
        ReplayKind::ToolResult => {
            let message = &event.data["message"];
            (
                "tool",
                if message["role"] == "tool" {
                    &message["content"]
                } else {
                    &message["content"][0]["content"]
                },
            )
        }
        _ => return None,
    };
    let text = blocks
        .as_array()?
        .iter()
        .filter(|part| part["type"] == "text")
        .filter_map(|part| part["text"].as_str())
        .map(|s| {
            if kind == "tool" {
                search_tool_text(s)
            } else {
                crate::redact::redact_secrets(s)
            }
        })
        .collect::<Vec<_>>()
        .join("\n");
    (!text.is_empty()).then_some(SearchDocument { kind, text })
}
fn search_tool_text(text: &str) -> String {
    let Ok(value) = serde_json::from_str::<Value>(text) else {
        return crate::redact::redact_secrets(text);
    };
    let mut parts = Vec::new();
    readable(&value, &mut parts);
    crate::redact::redact_secrets(&parts.join("\n"))
}
fn readable(value: &Value, out: &mut Vec<String>) {
    match value {
        Value::String(s) => out.push(s.clone()),
        Value::Array(values) => {
            for value in values {
                readable(value, out);
            }
        }
        Value::Object(object) => {
            for (key, value) in object {
                if matches!(
                    key.as_str(),
                    "text"
                        | "body"
                        | "message"
                        | "summary"
                        | "title"
                        | "snippet"
                        | "content"
                        | "output"
                        | "sources"
                        | "result"
                ) {
                    readable(value, out);
                }
            }
        }
        _ => {}
    }
}
pub(crate) fn preserves_history(event: &SessionEvent) -> bool {
    matches!(replay_kind(&event.event_type), ReplayKind::Compaction)
}
pub(crate) fn begins_step(event: &SessionEvent) -> bool {
    event_spec(&event.event_type).is_some_and(|spec| spec.starts_step)
}
