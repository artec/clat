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
                let blocks = content_blocks(&block["content"])
                    .into_iter()
                    .filter(|block| matches!(block, crate::message::ContentBlock::Image { .. }))
                    .collect();
                let image_parts = content_parts(&block["content"])
                    .into_iter()
                    .filter(|part| matches!(part, crate::model::ContentPart::Image { .. }))
                    .collect();
                items.push((
                    *seq,
                    ModelItem::ToolResult(crate::tool::ToolResult {
                        blocks,
                        image_parts,
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
///（拼接语义不变），image blocks → `ContentPart::Image`。
/// INV-MM2-6（MM-2 W6）：新图块按 attachmentId 产出**相对 ref**
/// `blobs/<attachmentId>`（journal 不再持久化绝对路径）；legacy 块
///（MM-1 桥接期/平铺时代，只有 path）原样透传绝对路径。相对 ref 由
/// `fence_attachment_parts`（唯一持有会话根的解析点）重写为 root 内
/// 绝对路径——live 与回放构造出逐项相等的 items，T1 对拍不需要任何
/// 特判。未知 block 类型跳过（forward-compat）。
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
                let media_type = block.get("mediaType").and_then(Value::as_str);
                let attachment_id = block.get("attachmentId").and_then(Value::as_str);
                let path = block.get("path").and_then(Value::as_str);
                let reference = attachment_id
                    .map(|id| format!("blobs/{id}"))
                    .or_else(|| path.map(str::to_owned));
                if let (Some(reference), Some(media_type)) = (reference, media_type) {
                    parts.push(crate::model::ContentPart::Image {
                        path: reference,
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

/// journal content blocks → 冻结 `ContentBlock`（MM-1A replay 侧）。
/// live 事件（RunStarted/SteeringApplied）与回放（UserMessage）的
/// `content_blocks` 都从 journal 词汇经本函数产出——同一函数是
/// live/replay 逐字段相同的结构性保证，而不是靠两边各写一份解析
/// 碰巧相等。
///
/// image block 的 descriptor 元数据（attachmentId/宽高/字节/显示名）
/// 来自 journal 耐久事实；MM-1A 之前的旧图块没有这些字段，attachmentId
/// 按路径确定性派生（`legacy_attachment_id`）、尺寸/字节记 0 = 未知
///（纯函数、无文件 I/O：旧会话回放不因附件文件被清理而漂移）。
pub(crate) fn content_blocks(blocks: &Value) -> Vec<crate::message::ContentBlock> {
    let mut result = Vec::new();
    let Some(blocks) = blocks.as_array() else {
        return result;
    };
    for block in blocks {
        match block.get("type").and_then(Value::as_str) {
            Some("text") => {
                if let Some(text) = block.get("text").and_then(Value::as_str) {
                    result.push(crate::message::ContentBlock::Text {
                        text: text.to_owned(),
                    });
                }
            }
            Some("image") => {
                let Some(media_type) = block.get("mediaType").and_then(Value::as_str) else {
                    continue;
                };
                let path = block.get("path").and_then(Value::as_str);
                let Some(attachment_id) = block
                    .get("attachmentId")
                    .and_then(Value::as_str)
                    .map(str::to_owned)
                    .or_else(|| path.map(crate::message::legacy_attachment_id))
                else {
                    // 无 path 也无 attachmentId 的畸形块不产出 descriptor。
                    continue;
                };
                let number = |field: &str| block.get(field).and_then(Value::as_u64).unwrap_or(0);
                let display_name = block
                    .get("displayName")
                    .and_then(Value::as_str)
                    .map(str::to_owned)
                    .or_else(|| {
                        path.and_then(|path| {
                            std::path::Path::new(path)
                                .file_name()
                                .map(|name| name.to_string_lossy().into_owned())
                        })
                    });
                result.push(crate::message::ContentBlock::Image {
                    attachment: crate::message::AttachmentDescriptor {
                        attachment_id,
                        media_type: media_type.to_owned(),
                        width: number("width"),
                        height: number("height"),
                        bytes: number("bytes"),
                        display_name,
                        original_width: block.get("originalWidth").and_then(Value::as_u64),
                        original_height: block.get("originalHeight").and_then(Value::as_u64),
                    },
                });
            }
            _ => {}
        }
    }
    result
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
    /// MM-1A（INV-M1A-5/6）：`content_blocks` 是 journal 词汇的统一
    /// descriptor 解析——
    /// - 新式块（耐久元数据）：字段原样进 descriptor，displayName 缺省
    ///   时按路径文件名兜底；
    /// - 旧式块（MM-1A 前的会话，只有 path+mediaType）：attachmentId
    ///   按路径确定性派生（与 `legacy_attachment_id` 同规则）、尺寸/
    ///   字节记 0 = 未知——纯函数、零文件 I/O，附件文件被清理也不漂移；
    /// - 纯文本消息：blocks = 文本块（与 live 同构）。
    ///
    /// pre-fix（无 content_blocks）本测试红。
    #[test]
    fn content_blocks_parse_durable_metadata_and_derive_legacy_ids() {
        // 新式块：admitted payload 构造 → 解析往返。
        let payload = payloads::admitted_user_message(
            "m-1",
            "look",
            &[crate::message::JournalImage {
                descriptor: crate::message::AttachmentDescriptor {
                    attachment_id: "att-1".into(),
                    media_type: "image/png".into(),
                    width: 1024,
                    height: 768,
                    bytes: 2048,
                    display_name: None,
                    original_width: None,
                    original_height: None,
                },
                path: "/sessions/s1/attachments/att-1.png".into(),
            }],
            Some("client-1"),
            Some("digest-1"),
        );
        let blocks = content_blocks(&payload["content"]);
        assert_eq!(
            blocks,
            vec![
                crate::message::ContentBlock::Text {
                    text: "look".into()
                },
                crate::message::ContentBlock::Image {
                    attachment: crate::message::AttachmentDescriptor {
                        attachment_id: "att-1".into(),
                        media_type: "image/png".into(),
                        width: 1024,
                        height: 768,
                        bytes: 2048,
                        // INV-MM2-6：journal 不再持久化 path——新式块
                        // 无 displayName 时不再有路径兜底来源（生产
                        // 写侧恒持久化 displayName；兜底仅属 legacy
                        // path 块，见下）。
                        display_name: None,
                        original_width: None,
                        original_height: None,
                    },
                },
            ]
        );

        // INV-MM2-6：admitted 载荷是 ref-only——不携带 path；模型项
        // 侧（content_parts）按 attachmentId 产出相对 ref。
        assert!(
            payload["content"][1].get("path").is_none(),
            "the durable image block carries no absolute path"
        );
        let parts = content_parts(&payload["content"]);
        assert_eq!(
            parts,
            vec![
                crate::model::ContentPart::Text("look".into()),
                crate::model::ContentPart::Image {
                    path: "blobs/att-1".into(),
                    media_type: "image/png".into(),
                },
            ]
        );

        // 旧式块：路径派生 id + 零元数据，跨调用稳定。
        let legacy = json!([
            { "type": "text", "text": "old" },
            { "type": "image", "path": "/old/attachments/x.png", "mediaType": "image/png" },
        ]);
        let blocks = content_blocks(&legacy);
        let expected_id = crate::message::legacy_attachment_id("/old/attachments/x.png");
        assert_eq!(
            blocks,
            vec![
                crate::message::ContentBlock::Text { text: "old".into() },
                crate::message::ContentBlock::Image {
                    attachment: crate::message::AttachmentDescriptor {
                        attachment_id: expected_id,
                        media_type: "image/png".into(),
                        width: 0,
                        height: 0,
                        bytes: 0,
                        display_name: Some("x.png".into()),
                        original_width: None,
                        original_height: None,
                    },
                },
            ]
        );
        assert_eq!(
            content_blocks(&legacy),
            blocks,
            "legacy derivation is stable"
        );
        // legacy path 块：模型项侧绝对路径原样透传（栅栏按 root 检查）。
        assert_eq!(
            content_parts(&legacy),
            vec![
                crate::model::ContentPart::Text("old".into()),
                crate::model::ContentPart::Image {
                    path: "/old/attachments/x.png".into(),
                    media_type: "image/png".into(),
                },
            ]
        );

        // 纯文本：文本块直通。
        assert_eq!(
            content_blocks(&json!([{ "type": "text", "text": "hi" }])),
            vec![crate::message::ContentBlock::Text { text: "hi".into() }],
        );
        // 畸形（无 path 无 attachmentId 的 image 块）跳过，不产出。
        assert!(content_blocks(&json!([{ "type": "image", "mediaType": "image/png" }])).is_empty());
    }
}
