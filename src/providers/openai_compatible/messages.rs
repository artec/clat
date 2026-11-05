//! Provider-specific reasoning replay; the neutral history remains lossless.
use super::*;

/// Decide from the effective request, after config and per-request overrides.
pub(super) fn replay_plain_reasoning(endpoint: &str, body: &Map<String, Value>) -> bool {
    if body
        .get("thinking")
        .and_then(|v| v.get("type"))
        .and_then(Value::as_str)
        == Some("disabled")
    {
        return false;
    }
    let Ok(url) = url::Url::parse(endpoint) else {
        return false;
    };
    match url.host_str().unwrap_or_default() {
        "api.deepseek.com" => body
            .get("tools")
            .and_then(Value::as_array)
            .is_some_and(|tools| !tools.is_empty()),
        "open.bigmodel.cn" | "api.z.ai" => body
            .get("thinking")
            .and_then(|v| v.get("clear_thinking"))
            .and_then(Value::as_bool)
            .map(|clear| !clear)
            .unwrap_or_else(|| url.path().starts_with("/api/coding/")),
        _ => false,
    }
}

#[derive(Default)]
struct PendingAssistant {
    content: Option<String>,
    reasoning: Option<String>,
    calls: Vec<Value>,
    results: Vec<Value>,
}
impl PendingAssistant {
    fn flush(messages: &mut Vec<Value>, pending: &mut Option<Self>, replay_plain: bool) {
        let Some(pending) = pending.take() else {
            return;
        };
        let has_calls = !pending.calls.is_empty();
        // Empty tool-call content stays an empty string, never null.
        let mut message =
            json!({"role":"assistant", "content":pending.content.unwrap_or_default()});
        if has_calls {
            message["tool_calls"] = json!(pending.calls);
        }
        if (has_calls || replay_plain)
            && let Some(reasoning) = pending.reasoning.filter(|value| !value.is_empty())
        {
            message["reasoning_content"] = json!(reasoning);
        }
        messages.push(message);
        messages.extend(pending.results);
    }
}

/// Group each assistant's calls and results without changing their order.
pub(super) fn map_messages(
    instructions: Option<&str>,
    items: &[ModelItem],
    policy: &crate::model::ImageRequestPolicy,
    replay_plain: bool,
) -> Result<Vec<Value>, ModelError> {
    let mut messages = Vec::new();
    if let Some(instructions) = instructions {
        messages.push(json!({"role": "system", "content": instructions}));
    }
    let mut pending: Option<PendingAssistant> = None;
    let protected_start = items
        .iter()
        .rposition(|item| matches!(item, ModelItem::User { .. }))
        .unwrap_or(items.len());
    for (index, item) in items.iter().enumerate() {
        let image_failure = if index >= protected_start {
            ImageFailureMode::RejectCurrent
        } else {
            ImageFailureMode::DegradeHistory
        };
        match item {
            ModelItem::User { content } => {
                PendingAssistant::flush(&mut messages, &mut pending, replay_plain);
                messages.push(json!({
                    "role": "user",
                    "content": user_content_with_mode(content, policy, image_failure)?,
                }));
            }
            ModelItem::Assistant { content, reasoning } => {
                PendingAssistant::flush(&mut messages, &mut pending, replay_plain);
                pending = Some(PendingAssistant {
                    content: Some(content_text(content)),
                    reasoning: reasoning.clone(),
                    calls: Vec::new(),
                    results: Vec::new(),
                });
            }
            ModelItem::ToolCall(call) => {
                let calls = &mut pending.get_or_insert_with(PendingAssistant::default).calls;
                calls.push(json!({
                    "id": call.id,
                    "type": "function",
                    "function": {
                        "name": call.name,
                        "arguments": serde_json::to_string(&call.arguments)
                            .map_err(|error| ModelError::request(format!("invalid tool arguments: {error}")))?,
                    }
                }));
            }
            ModelItem::ToolResult(result) => {
                let result_message = json!({
                    "role": "tool",
                    "tool_call_id": result.call_id,
                    "content": tool_result_content(result, policy, image_failure)?,
                });
                match &mut pending {
                    // The result belongs to the turn that is still open; it
                    // is emitted right after the turn's assistant message.
                    Some(turn) => turn.results.push(result_message),
                    // Defensive: an orphan result (no open turn) is emitted
                    // as-is.
                    None => messages.push(result_message),
                }
            }
            ModelItem::ProviderState(_) => {}
        }
    }
    PendingAssistant::flush(&mut messages, &mut pending, replay_plain);
    Ok(messages)
}
