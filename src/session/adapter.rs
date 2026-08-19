//! Surface → CLAT `ModelItem` adapter (plan §11.1): the provider request
//! history is built exclusively from the surface projection; log-only
//! events never reach the model.

use crate::model::ModelItem;
use crate::session::event::SessionEvent;
use crate::session::surface::Surface;
use serde_json::Value;

/// Build the provider-neutral conversation from surface nodes in order.
/// Tool calls ride inside assistant messages (DSH shape) and are unpacked
/// into CLAT's paired `ToolCall` items after their assistant item.
pub(crate) fn surface_to_model_items(
    events: &[SessionEvent],
    surface: &Surface,
) -> Result<Vec<ModelItem>, String> {
    Ok(surface_to_model_items_with_seq(events, surface)?
        .into_iter()
        .map(|(_, item)| item)
        .collect())
}

pub(crate) fn surface_to_model_items_with_seq(
    events: &[SessionEvent],
    surface: &Surface,
) -> Result<Vec<(u64, ModelItem)>, String> {
    let mut items = Vec::new();
    for seq in &surface.nodes {
        let event = events
            .get(*seq as usize)
            .ok_or_else(|| format!("surface node {seq} has no event"))?;
        match event.event_type.as_str() {
            "user/message" => {
                items.push((
                    *seq,
                    ModelItem::User {
                        content: content_parts(&event.data["content"]),
                    },
                ));
            }
            "assistant/message" => {
                let message = &event.data["message"];
                let text = content_text(&message["content"]);
                let reasoning = reasoning_text(&message["content"]);
                let replay_state = replay_state(message);
                items.push((*seq, ModelItem::assistant_with_reasoning(text, reasoning)));
                if let Some(replay_state) = replay_state {
                    let provider = provider_of(message);
                    for state in replay_states(replay_state) {
                        items.push((
                            *seq,
                            ModelItem::ProviderState(crate::model::ProviderState {
                                provider: provider.clone(),
                                data: state,
                            }),
                        ));
                    }
                }
                if let Some(blocks) = message["content"].as_array() {
                    for block in blocks {
                        if block.get("type").and_then(Value::as_str) == Some("tool-call") {
                            items.push((
                                *seq,
                                ModelItem::ToolCall(crate::tool::ToolCall {
                                    id: block
                                        .get("id")
                                        .and_then(Value::as_str)
                                        .ok_or("tool-call block without id")?
                                        .to_owned(),
                                    name: block
                                        .get("name")
                                        .and_then(Value::as_str)
                                        .unwrap_or_default()
                                        .to_owned(),
                                    arguments: serde_json::from_str(
                                        block
                                            .get("arguments")
                                            .and_then(Value::as_str)
                                            .unwrap_or("{}"),
                                    )
                                    .unwrap_or(serde_json::json!({})),
                                }),
                            ));
                        }
                    }
                }
            }
            "tool/result" => {
                let block = &event.data["message"]["content"][0];
                items.push((
                    *seq,
                    ModelItem::ToolResult(crate::tool::ToolResult {
                        call_id: block
                            .get("toolCallId")
                            .and_then(Value::as_str)
                            .ok_or("tool-result block without toolCallId")?
                            .to_owned(),
                        tool_name: String::new(),
                        output: content_text(&block["content"]).into(),
                        is_error: block
                            .get("isError")
                            .and_then(Value::as_bool)
                            .unwrap_or(false),
                    }),
                ));
            }
            other => return Err(format!("surface node {seq} has unexpected type `{other}`")),
        }
    }
    Ok(items)
}

fn replay_states(replay_state: Value) -> Vec<Value> {
    match replay_state {
        Value::Array(states) => states,
        state => vec![state],
    }
}

fn content_text(blocks: &Value) -> String {
    blocks
        .as_array()
        .map(|blocks| {
            blocks
                .iter()
                .filter_map(|block| {
                    (block.get("type").and_then(Value::as_str) == Some("text"))
                        .then(|| block.get("text").and_then(Value::as_str))
                        .flatten()
                        .map(str::to_owned)
                })
                .collect::<Vec<_>>()
                .join("")
        })
        .unwrap_or_default()
}

/// user/message 的 content 重建（M2）：text blocks → `ContentPart::Text`
///（拼接语义不变），image blocks → `ContentPart::Image`（journal 存的
/// 绝对引用原样透传——live 与回放构造出逐项相等的 items，T1 对拍
/// 不需要任何特判）。未知 block 类型跳过（forward-compat）。
fn content_parts(blocks: &Value) -> Vec<crate::model::ContentPart> {
    let mut parts = Vec::new();
    let Some(blocks) = blocks.as_array() else {
        return parts;
    };
    for block in blocks {
        match block.get("type").and_then(Value::as_str) {
            Some("text") => {
                if let Some(text) = block.get("text").and_then(Value::as_str) {
                    parts.push(crate::model::ContentPart::Text(text.to_owned()));
                }
            }
            Some("image") => {
                let path = block.get("path").and_then(Value::as_str);
                let media_type = block.get("mediaType").and_then(Value::as_str);
                if let (Some(path), Some(media_type)) = (path, media_type) {
                    parts.push(crate::model::ContentPart::Image {
                        path: path.to_owned(),
                        media_type: media_type.to_owned(),
                    });
                }
            }
            _ => {}
        }
    }
    if parts.is_empty() {
        // 防御：空 content 的 user/message 不该存在（admission 拒空），
        // 保底一个空文本 part 保持 item 形状。
        parts.push(crate::model::ContentPart::Text(String::new()));
    }
    parts
}

fn reasoning_text(blocks: &Value) -> Option<String> {
    let joined = blocks
        .as_array()?
        .iter()
        .filter_map(|block| {
            (block.get("type").and_then(Value::as_str) == Some("reasoning"))
                .then(|| block.get("text").and_then(Value::as_str))
                .flatten()
                .map(str::to_owned)
        })
        .collect::<Vec<_>>()
        .join("");
    (!joined.is_empty()).then_some(joined)
}

fn replay_state(message: &Value) -> Option<Value> {
    message
        .get("source")
        .and_then(|source| source.get("replayState"))
        .cloned()
}

fn provider_of(message: &Value) -> String {
    message
        .pointer("/source/provider")
        .and_then(Value::as_str)
        .unwrap_or("dsh-compatible")
        .to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::event::{SessionEvent, TurnEndReason, payloads};
    use crate::session::projection::ProjectionRegistry;
    use serde_json::json;

    #[test]
    fn model_history_comes_from_surface_only_with_tool_pairs() {
        let events = vec![
            SessionEvent::new("turn/start", 0, 1, payloads::turn_start(1)),
            SessionEvent::new("user/message", 1, 2, payloads::user_message("read it"))
                .append(Vec::new()),
            SessionEvent::new(
                "assistant/message",
                2,
                3,
                json!({
                    "turn": 1, "step": 0,
                    "message": {
                        "id": "m1", "role": "assistant",
                        "content": [
                            { "type": "reasoning", "text": "thinking..." },
                            { "type": "tool-call", "id": "call-1", "name": "read_file", "arguments": "{\"path\":\"x\"}" },
                        ],
                        "source": {
                            "kind": "model", "provider": "openai", "model": "gpt",
                            "replayState": [
                                { "id": "reasoning-item-1" },
                                { "id": "reasoning-item-2" }
                            ],
                        },
                    },
                }),
            )
            .append(Vec::new()),
            SessionEvent::new(
                "tool/result",
                3,
                4,
                json!({
                    "turn": 1, "step": 0,
                    "message": {
                        "id": "m2", "role": "user",
                        "content": [{ "type": "tool-result", "toolCallId": "call-1", "isError": false,
                                      "content": [{ "type": "text", "text": "file body" }] }],
                        "source": { "kind": "tool", "callId": "call-1" },
                    },
                }),
            )
            .append(Vec::new()),
            SessionEvent::new("turn/end", 4, 5, payloads::turn_end(1, &TurnEndReason::Completed)),
        ];
        let mut registry = ProjectionRegistry::clat();
        registry.fold_all(&events).expect("fold");
        let rows = registry
            .checkpoint(
                crate::session::projection::CheckpointIdentity {
                    created_at: 0,
                    cwd: None,
                },
                0,
            )
            .rows;
        // Rebuild the surface from the checkpointed state.
        let mut surface = Surface::default();
        let surface_events: Vec<SessionEvent> = rows["surface"].val["events"]
            .as_array()
            .expect("events")
            .iter()
            .map(|value| serde_json::from_value(value.clone()).expect("event"))
            .collect();
        for event in &surface_events {
            surface
                .apply_public(event, &surface_events[..event.seq as usize])
                .expect("apply");
        }

        let items = surface_to_model_items(&events, &surface).expect("items");
        let shapes: Vec<&str> = items
            .iter()
            .map(|item| match item {
                ModelItem::User { .. } => "user",
                ModelItem::Assistant { .. } => "assistant",
                ModelItem::ToolCall(_) => "call",
                ModelItem::ToolResult(_) => "result",
                ModelItem::ProviderState(_) => "state",
            })
            .collect();
        // reasoning text is replayable, replayState travels as provider
        // state, tool calls unpack after their assistant item.
        assert_eq!(
            shapes,
            vec!["user", "assistant", "state", "state", "call", "result"]
        );
        assert!(matches!(
            &items[1],
            ModelItem::Assistant { reasoning: Some(reasoning), .. } if reasoning == "thinking..."
        ));
        assert!(matches!(
            &items[2],
            ModelItem::ProviderState(state) if state.provider == "openai"
        ));
        assert!(matches!(
            &items[3],
            ModelItem::ProviderState(state) if state.data["id"] == "reasoning-item-2"
        ));
        assert!(matches!(&items[5], ModelItem::ToolResult(result) if !result.is_error));
        let seqs: Vec<u64> = surface_to_model_items_with_seq(&events, &surface)
            .expect("items with seq")
            .into_iter()
            .map(|(seq, _)| seq)
            .collect();
        assert_eq!(seqs, vec![1, 2, 2, 2, 2, 3]);
    }
}
