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
        // 无载荷的纯思考轮（无正文、无调用、无结果，且 reasoning 不
        // 回放）序列化后是完全空的 assistant 消息——严格校验端点拒绝
        // （Moonshot 系 2026-11-21 Kimi K3.1 实测 400 "must not be
        // empty"）。整条跳过：空消息不携带任何信息。注：空正文 +
        // tool_calls 的形状同日实测 200，下方“空串不坈 null”的既有
        // 决定不受影响；DeepSeek/GLM 的 reasoning 回放（replay_plain）
        // 会给消息带上 reasoning_content 载荷，同样不跳。
        let reasoning_replayed =
            replay_plain && pending.reasoning.as_deref().is_some_and(|v| !v.is_empty());
        let empty_content = pending.content.as_deref().is_none_or(str::is_empty);
        if !has_calls && empty_content && pending.results.is_empty() && !reasoning_replayed {
            return;
        }
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
        // Moonshot 系端点要求 assistant 的每个 tool_call_id 都有配对的
        // tool 响应消息（2026-11-21 Kimi K3.1 实测 400 "did not have
        // response messages"）——取消/中断的调用在会话日志里可能没有
        // 记录结果。为缺失的调用补一条诚实占位：协议配对完整、历史
        // 无损，占位文本明确标注结果未记录（不发明数据）。配对是
        // OpenAI 标准要求，其它厂商只是不校验。先在移动 results 前
        // 算出未应答 id 列表（拥有所有权，避开 use-after-move）。
        let unanswered: Vec<String> = pending
            .calls
            .iter()
            .filter_map(|call| call["id"].as_str())
            .filter(|id| {
                !pending
                    .results
                    .iter()
                    .any(|result| result["tool_call_id"].as_str() == Some(*id))
            })
            .map(str::to_owned)
            .collect();
        messages.extend(pending.results);
        for id in unanswered {
            messages.push(json!({
                "role": "tool",
                "tool_call_id": id,
                "content": "[no recorded result]",
            }));
        }
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
