//! serve 线形状：重放族、控制族与审批帧载荷的手写映射
//!（docs/todo/serve-rpc.md §4/§5/§6）。
//!
//! 纪律与 PWA-1 的 `wire.rs` 相同：不绑内部 serde、字段名由 serve
//! 词汇拥有、固定 golden 钉字节。RunEvent 实时族**不走本模块**——
//! `envelope_line` 原样过线（INV-S2 零转译），PWA-1 的 golden 自动
//! 成为 serve 下行词汇契约。词汇政策（§5.4）：serve 帧/控制词汇新增
//! = v2；唯一例外 `notice.kind`（开放枚举，客户端忽略未知）。

use crate::event::RunEvent;
use crate::model::Usage;
use crate::permission::PermissionRequest;
use crate::session::replay::{ReplayEvent, ReplayRetryFailure, ReplayTurnEnd};
use crate::tool::ToolEffect;
use crate::{ApplicationEvent, SessionSummary, WorkbenchSnapshot};
use serde_json::{Map, Value, json};

#[cfg(test)]
use crate::permission::PermissionDecision;

#[cfg(test)]
use crate::tool::ToolCall;

// —— 会话摘要（session.list）——————————————————————————————————————

pub(crate) fn session_summary_json(summary: &SessionSummary) -> Value {
    let mut fields = vec![("id", Value::String(summary.id.as_str().to_owned()))];
    if let Some(title) = &summary.title {
        fields.push(("title", Value::String(title.clone())));
    }
    fields.push(("created_at_ms", json!(summary.created_at_ms)));
    fields.push(("last_activity_ms", json!(summary.last_activity_ms)));
    fields.push(("message_count", json!(summary.message_count)));
    fields.push(("turns", json!(summary.turns)));
    object(fields)
}

// —— 工作台轻快照（workbench.info）——————————————————————————————

/// RF-2/RF-8：Application DTO → serve wire 的唯一手写映射。这里明确
/// 枚举字段，避免未来给 core DTO 加字段时把凭据或内部状态顺带上网。
pub(crate) fn workbench_snapshot_json(
    snapshot: &WorkbenchSnapshot,
    active_run: Value,
    methods: &[&str],
) -> Value {
    let model_protocol = match snapshot.model.protocol {
        crate::ModelProtocol::OpenAiResponses => "open_ai_responses",
        crate::ModelProtocol::OpenAiCompatible => "open_ai_compatible",
    };
    let thinking_level = snapshot
        .model
        .thinking_level
        .map(|level| level.label().to_ascii_lowercase());
    let servers = snapshot
        .mcp
        .servers
        .iter()
        .map(|server| {
            object(vec![
                ("name", Value::String(server.name.clone())),
                (
                    "server_version",
                    Value::String(server.server_version.clone()),
                ),
                (
                    "protocol_version",
                    Value::String(server.protocol_version.clone()),
                ),
                ("tools", json!(server.tools)),
                ("transport", Value::String(server.transport.clone())),
            ])
        })
        .collect::<Vec<_>>();

    object(vec![
        (
            "project",
            object(vec![
                (
                    "root",
                    Value::String(snapshot.project.root.to_string_lossy().into_owned()),
                ),
                ("name", Value::String(snapshot.project.name.clone())),
                (
                    "workspace_id",
                    snapshot
                        .project
                        .workspace_id
                        .clone()
                        .map(Value::String)
                        .unwrap_or(Value::Null),
                ),
            ]),
        ),
        (
            "session",
            object(vec![
                (
                    "id",
                    snapshot
                        .session
                        .id
                        .as_ref()
                        .map(|id| Value::String(id.as_str().to_owned()))
                        .unwrap_or(Value::Null),
                ),
                (
                    "title",
                    snapshot
                        .session
                        .title
                        .clone()
                        .map(Value::String)
                        .unwrap_or(Value::Null),
                ),
                (
                    "committed_seq",
                    snapshot
                        .session
                        .committed_seq
                        .map_or(Value::Null, |seq| json!(seq)),
                ),
            ]),
        ),
        (
            "model",
            object(vec![
                ("protocol", Value::String(model_protocol.into())),
                ("model", Value::String(snapshot.model.model.clone())),
                (
                    "preset",
                    snapshot
                        .model
                        .preset
                        .clone()
                        .map(Value::String)
                        .unwrap_or(Value::Null),
                ),
                (
                    "active_profile",
                    snapshot
                        .model
                        .active_profile
                        .clone()
                        .map(Value::String)
                        .unwrap_or(Value::Null),
                ),
                (
                    "thinking_level",
                    thinking_level.map(Value::String).unwrap_or(Value::Null),
                ),
                (
                    "max_context_tokens",
                    snapshot
                        .model
                        .max_context_tokens
                        .map_or(Value::Null, |tokens| json!(tokens)),
                ),
                ("run_token_budget", json!(snapshot.model.run_token_budget)),
            ]),
        ),
        (
            "permission",
            object(vec![
                (
                    "mode",
                    Value::String(snapshot.permission_mode.journal_value().into()),
                ),
                ("label", Value::String(snapshot.permission_mode.to_string())),
            ]),
        ),
        (
            "mcp",
            object(vec![
                ("configured", json!(snapshot.mcp.configured)),
                ("connected", json!(snapshot.mcp.connected)),
                ("connecting", json!(snapshot.mcp.connecting)),
                (
                    "failures",
                    Value::Array(
                        snapshot
                            .mcp
                            .failures
                            .iter()
                            .cloned()
                            .map(Value::String)
                            .collect(),
                    ),
                ),
                ("servers", Value::Array(servers)),
            ]),
        ),
        ("active_run", active_run),
        (
            "methods",
            Value::Array(
                methods
                    .iter()
                    .map(|method| Value::String((*method).into()))
                    .collect(),
            ),
        ),
        (
            "capabilities",
            Value::Array(
                [
                    "session-history",
                    "in-run-steering",
                    "permission-modes",
                    "approval-bridge",
                    "model-summary",
                    "mcp-status",
                ]
                .into_iter()
                .map(|capability| Value::String(capability.into()))
                .collect(),
            ),
        ),
    ])
}

// —— 重放族（§7.1：ReplayEvent 8 变体，type-tagged snake_case）——————

pub(crate) fn replay_event_json(event: &ReplayEvent) -> Value {
    match event {
        ReplayEvent::UserMessage {
            turn,
            time_ms,
            text,
            content_blocks,
            client_message_id,
        } => {
            // MM-1A additive：blocks 携带图片时才上网（纯文本消息的
            // wire 字节与 v1 完全一致——`text` 语义不变，仍是文本
            // blocks 拼接）。SSE 只带 descriptor，永不带字节。
            let has_images = content_blocks
                .iter()
                .any(|block| matches!(block, crate::message::ContentBlock::Image { .. }));
            let mut fields = vec![
                ("turn", json!(turn)),
                ("time_ms", json!(time_ms)),
                ("text", Value::String(text.clone())),
            ];
            if has_images {
                fields.push((
                    "content_blocks",
                    Value::Array(
                        content_blocks
                            .iter()
                            .map(crate::wire::content_block_to_json)
                            .collect(),
                    ),
                ));
            }
            if let Some(client_message_id) = client_message_id {
                fields.push((
                    "client_message_id",
                    Value::String(client_message_id.clone()),
                ));
            }
            event_object("user_message", fields)
        }
        ReplayEvent::AssistantMessage {
            turn,
            step,
            time_ms,
            reasoning,
            text,
            tool_calls,
            provider,
            model,
            replay_state,
        } => {
            let mut fields = vec![
                ("turn", json!(turn)),
                ("step", json!(step)),
                ("time_ms", json!(time_ms)),
            ];
            if let Some(reasoning) = reasoning {
                fields.push(("reasoning", Value::String(reasoning.clone())));
            }
            fields.push(("text", Value::String(text.clone())));
            fields.push((
                "tool_calls",
                Value::Array(
                    tool_calls
                        .iter()
                        .map(crate::wire::tool_call_to_json)
                        .collect(),
                ),
            ));
            fields.push(("provider", Value::String(provider.clone())));
            fields.push(("model", Value::String(model.clone())));
            if let Some(state) = replay_state {
                fields.push(("replay_state", state.clone()));
            }
            event_object("assistant_message", fields)
        }
        ReplayEvent::PermissionChecked {
            time_ms,
            tool,
            decision,
        } => event_object(
            "permission_checked",
            vec![
                ("time_ms", json!(time_ms)),
                ("tool", Value::String(tool.clone())),
                (
                    "decision",
                    crate::wire::permission_decision_to_json(decision),
                ),
            ],
        ),
        ReplayEvent::ToolRequested { time_ms, call } => event_object(
            "tool_requested",
            vec![
                ("time_ms", json!(time_ms)),
                ("call", crate::wire::tool_call_to_json(call)),
            ],
        ),
        ReplayEvent::ToolFinished {
            time_ms,
            call_id,
            tool,
            output,
            is_error,
        } => event_object(
            "tool_finished",
            vec![
                ("time_ms", json!(time_ms)),
                ("call_id", Value::String(call_id.clone())),
                ("tool", Value::String(tool.clone())),
                ("output", output.clone()),
                ("is_error", Value::Bool(*is_error)),
            ],
        ),
        ReplayEvent::RetryScheduled {
            turn,
            step,
            time_ms,
            retry,
            max_retries,
            delay_ms,
            failure,
        } => event_object(
            "retry_scheduled",
            vec![
                ("turn", json!(turn)),
                ("step", json!(step)),
                ("time_ms", json!(time_ms)),
                ("retry", json!(retry)),
                ("max_retries", json!(max_retries)),
                ("delay_ms", json!(delay_ms)),
                ("failure", replay_retry_failure_json(failure)),
            ],
        ),
        ReplayEvent::TurnEnded {
            turn,
            time_ms,
            reason,
        } => event_object(
            "turn_ended",
            vec![
                ("turn", json!(turn)),
                ("time_ms", json!(time_ms)),
                ("reason", replay_turn_end_json(reason)),
            ],
        ),
        ReplayEvent::Compaction {
            time_ms,
            summary_text,
        } => event_object(
            "compaction",
            vec![
                ("time_ms", json!(time_ms)),
                ("summary_text", Value::String(summary_text.clone())),
            ],
        ),
    }
}

fn replay_retry_failure_json(failure: &ReplayRetryFailure) -> Value {
    let mut fields = vec![
        ("message", Value::String(failure.message.clone())),
        ("code", Value::String(failure.code.clone())),
    ];
    if let Some(retry_after) = failure.provider_retry_after_ms {
        fields.push(("provider_retry_after_ms", json!(retry_after)));
    }
    object(fields)
}

fn replay_turn_end_json(reason: &ReplayTurnEnd) -> Value {
    match reason {
        ReplayTurnEnd::Completed => Value::String("completed".into()),
        ReplayTurnEnd::Blocked => Value::String("blocked".into()),
        ReplayTurnEnd::MaxTokens => Value::String("max_tokens".into()),
        ReplayTurnEnd::Interrupted => Value::String("interrupted".into()),
        ReplayTurnEnd::Aborted { cause } => object(vec![("aborted", Value::String(cause.clone()))]),
        ReplayTurnEnd::Error { message } => object(vec![("error", Value::String(message.clone()))]),
    }
}

// —— 审批请求帧载荷（§6.1）————————————————————————————————————————

/// `approval.requested` 的 ctl 载荷：request 字段是 PermissionRequest
/// 的 serve wire 形状（effect 用机器标签，不是给人看的 Display 文案）。
pub(crate) fn approval_requested_ctl(rpc_id: &str, request: &PermissionRequest) -> Value {
    let mut request_fields = vec![
        ("tool", Value::String(request.tool.clone())),
        ("effect", Value::String(tool_effect_tag(request.effect))),
        ("reason", Value::String(request.reason.clone())),
        ("call_id", Value::String(request.call_id.clone())),
    ];
    // arguments 放最后：长载荷不遮蔽前面的判定字段（TUI 弹窗同款
    // 「fields 行」防藏纪律）。
    request_fields.push(("arguments", request.arguments.clone()));
    object(vec![
        ("rpc_id", Value::String(rpc_id.to_owned())),
        ("request", object(request_fields)),
    ])
}

fn tool_effect_tag(effect: ToolEffect) -> String {
    match effect {
        ToolEffect::Pure => "pure".into(),
        ToolEffect::Read => "read".into(),
        ToolEffect::Write => "write".into(),
        ToolEffect::Execute => "execute".into(),
        ToolEffect::Network => "network".into(),
        ToolEffect::ExternalRead => "external_read".into(),
        ToolEffect::Destructive => "destructive".into(),
        ToolEffect::SessionWrite => "session_write".into(),
    }
}

// —— prompt.settled（§5.5：invocation 终审，三 outcome 恰一）—————————

pub(crate) fn settled_completed(output: &str, turns: usize, usage: &Usage) -> Value {
    object(vec![
        ("prompt_rpc_id", Value::Null),
        (
            "outcome",
            event_object("completed", settled_done_fields(output, turns, usage)),
        ),
    ])
}

pub(crate) fn settled_cancelled(turns: usize, usage: &Usage) -> Value {
    object(vec![
        ("prompt_rpc_id", Value::Null),
        (
            "outcome",
            event_object(
                "cancelled",
                vec![
                    ("turns", json!(turns)),
                    ("usage", crate::wire::usage_to_json(usage)),
                ],
            ),
        ),
    ])
}

pub(crate) fn settled_failed(error: &str) -> Value {
    object(vec![
        ("prompt_rpc_id", Value::Null),
        (
            "outcome",
            event_object("failed", vec![("error", Value::String(error.to_owned()))]),
        ),
    ])
}

/// 把 settled 载荷的 prompt_rpc_id 从占位 Null 替换为受理时铸造的 id
///（构造点只有 settler 一处，替换发生在广播前）。
pub(crate) fn with_prompt_rpc_id(mut settled: Value, rpc_id: &str) -> Value {
    if let Some(map) = settled.as_object_mut() {
        map.insert("prompt_rpc_id".into(), Value::String(rpc_id.to_owned()));
    }
    settled
}

/// M-03（审查 2026-08-27）：settled 帧 / prompt.send 响应附带的
/// committed 回执投影（additive：无客户端键的消息不出现该字段）。
pub(crate) fn admission_receipt_value(receipt: &crate::message::AdmissionReceipt) -> Value {
    crate::wire::admission_receipt_to_json(receipt)
}

/// settled 载荷附加回执（settler 的统一出口——完成/取消/失败三态
/// 共用；`None` 不加字段，纯文本无键 run 的 settled 字节不变）。
pub(crate) fn with_admission_receipt(
    mut settled: Value,
    receipt: Option<&crate::message::AdmissionReceipt>,
) -> Value {
    if let (Some(map), Some(receipt)) = (settled.as_object_mut(), receipt) {
        map.insert("receipt".into(), admission_receipt_value(receipt));
    }
    settled
}

fn settled_done_fields(output: &str, turns: usize, usage: &Usage) -> Vec<(&'static str, Value)> {
    vec![
        ("output", Value::String(output.to_owned())),
        ("turns", json!(turns)),
        ("usage", crate::wire::usage_to_json(usage)),
    ]
}

// —— notice（§5.3：ApplicationEvent 转发，kind 开放枚举）———————————

pub(crate) fn notice_ctl(event: &ApplicationEvent) -> Value {
    match event {
        ApplicationEvent::MonitorUpdated(status) => object(vec![
            ("kind", Value::String("monitor".into())),
            (
                "payload",
                match status {
                    Some(text) => Value::String(text.clone()),
                    None => Value::Null,
                },
            ),
        ]),
        ApplicationEvent::CompactionUpdated(status) => {
            let (state, note, succeeded) = match status {
                crate::CompactionStatus::Started => ("started".to_owned(), None, None),
                crate::CompactionStatus::Finished { note, succeeded } => {
                    ("finished".to_owned(), Some(note.clone()), Some(*succeeded))
                }
            };
            let mut payload = vec![("status", Value::String(state))];
            if let Some(note) = note {
                payload.push(("note", Value::String(note)));
            }
            if let Some(succeeded) = succeeded {
                payload.push(("succeeded", Value::Bool(succeeded)));
            }
            object(vec![
                ("kind", Value::String("compaction".into())),
                ("payload", object(payload)),
            ])
        }
        ApplicationEvent::TitleUpdated { title } => object(vec![
            ("kind", Value::String("title".into())),
            (
                "payload",
                object(vec![("title", Value::String(title.clone()))]),
            ),
        ]),
        ApplicationEvent::McpStartupNotice { failures } => object(vec![
            ("kind", Value::String("mcp_startup".into())),
            ("payload", object(vec![("failures", json!(failures))])),
        ]),
        ApplicationEvent::LanguageIntelligenceNotice { message } => object(vec![
            ("kind", Value::String("language_intelligence".into())),
            (
                "payload",
                object(vec![("message", Value::String(message.clone()))]),
            ),
        ]),
        ApplicationEvent::ProcessFinished {
            session_id,
            exit_code,
            signal,
            timed_out,
            cancelled,
            terminated,
        } => object(vec![
            ("kind", Value::String("process_finished".into())),
            (
                "payload",
                object(vec![
                    ("session_id", json!(session_id)),
                    ("exit_code", json!(exit_code)),
                    ("signal", json!(signal)),
                    ("timed_out", json!(timed_out)),
                    ("cancelled", json!(cancelled)),
                    ("terminated", json!(terminated)),
                ]),
            ),
        ]),
    }
}

// —— 通用构造 ——————————————————————————————————————————————————————

fn object(fields: Vec<(&str, Value)>) -> Value {
    let mut map = Map::new();
    for (name, value) in fields {
        map.insert(name.to_string(), value);
    }
    Value::Object(map)
}

fn event_object(tag: &str, fields: Vec<(&str, Value)>) -> Value {
    object(
        std::iter::once(("type", Value::String(tag.to_string())))
            .chain(fields)
            .collect(),
    )
}

// —— 供 sse/protocol 组帧的便捷封装 ————————————————————————————————

/// 实时族帧 data：RunEvent 经 `envelope_line` 去掉行尾换行的原文
///（INV-S2 零转译——这里不做任何再构造）。
pub(crate) fn realtime_data(event: &RunEvent) -> String {
    let line = crate::wire::envelope_line(event);
    line.strip_suffix('\n')
        .expect("envelope_line ends with newline")
        .to_owned()
}

/// 重放族帧 data：`{"v":1,"replay":{...}}`。
pub(crate) fn replay_data(event: &ReplayEvent) -> String {
    json!({
        "v": crate::wire::WIRE_VERSION,
        "replay": replay_event_json(event),
    })
    .to_string()
}

/// 控制族帧 data：`{"v":1,"ctl":{...}}`。
pub(crate) fn ctl_data(ctl: &Value) -> String {
    json!({
        "v": crate::wire::WIRE_VERSION,
        "ctl": ctl,
    })
    .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// §12 验收 4：serve 帧形状的固定 golden——字段名/顺序/省略形态是
    /// v1 契约本身（serve 词汇新增 = v2，词汇政策 §5.4-2）；删映射
    /// 字段或改字段名在此即红。
    #[test]
    fn serve_shapes_match_golden_bytes() {
        let summary = SessionSummary {
            id: crate::SessionId::new("0f8c2a4e-1111-2222-3333-444455556666"),
            title: Some("review the diff".into()),
            created_at_ms: 1_755_900_000_000,
            last_activity_ms: 1_755_900_999_000,
            message_count: 12,
            turns: 3,
        };
        assert_eq!(
            session_summary_json(&summary).to_string(),
            r#"{"id":"0f8c2a4e-1111-2222-3333-444455556666","title":"review the diff","created_at_ms":1755900000000,"last_activity_ms":1755900999000,"message_count":12,"turns":3}"#
        );

        let workbench = WorkbenchSnapshot {
            project: crate::WorkbenchProjectSnapshot {
                root: std::path::PathBuf::from("/work/repo"),
                name: "repo".into(),
                workspace_id: Some("workspace-1".into()),
            },
            session: crate::WorkbenchSessionSnapshot {
                id: Some(crate::SessionId::new("session-1")),
                title: Some("active work".into()),
                committed_seq: Some(42),
            },
            model: crate::WorkbenchModelSnapshot {
                protocol: crate::ModelProtocol::OpenAiResponses,
                model: "deepseek-v3".into(),
                preset: Some("deepseek".into()),
                active_profile: Some("daily".into()),
                thinking_level: Some(crate::ThinkingLevel::High),
                max_context_tokens: Some(128_000),
                run_token_budget: 10_000_000,
            },
            permission_mode: crate::PermissionMode::ReadOnly,
            mcp: crate::McpStatusDto {
                configured: 1,
                connected: 1,
                connecting: 0,
                failures: vec!["stale server warning".into()],
                servers: vec![crate::McpServerInfoDto {
                    name: "repo-tools".into(),
                    server_version: "1.2.3".into(),
                    protocol_version: "2025-06-18".into(),
                    tools: 4,
                    transport: "stdio".into(),
                }],
            },
        };
        assert_eq!(
            workbench_snapshot_json(
                &workbench,
                json!({"prompt_rpc_id": "prompt-1", "started": 99}),
                &["workbench.info", "permission.set"],
            )
            .to_string(),
            r#"{"project":{"root":"/work/repo","name":"repo","workspace_id":"workspace-1"},"session":{"id":"session-1","title":"active work","committed_seq":42},"model":{"protocol":"open_ai_responses","model":"deepseek-v3","preset":"deepseek","active_profile":"daily","thinking_level":"high","max_context_tokens":128000,"run_token_budget":10000000},"permission":{"mode":"read-only","label":"Read Only"},"mcp":{"configured":1,"connected":1,"connecting":0,"failures":["stale server warning"],"servers":[{"name":"repo-tools","server_version":"1.2.3","protocol_version":"2025-06-18","tools":4,"transport":"stdio"}]},"active_run":{"prompt_rpc_id":"prompt-1","started":99},"methods":["workbench.info","permission.set"],"capabilities":["session-history","in-run-steering","permission-modes","approval-bridge","model-summary","mcp-status"]}"#
        );

        let replay: Vec<(&str, &str)> = vec![
            (
                "user",
                r#"{"type":"user_message","turn":1,"time_ms":1000,"text":"hi"}"#,
            ),
            (
                "assistant",
                r#"{"type":"assistant_message","turn":1,"step":0,"time_ms":2000,"text":"done","tool_calls":[{"id":"c1","name":"read_file","arguments":{"path":"a"}}],"provider":"glm","model":"glm-5.3"}"#,
            ),
            (
                "assistant_reasoning",
                r#"{"type":"assistant_message","turn":1,"step":1,"time_ms":2500,"reasoning":"why","text":"","tool_calls":[],"provider":"glm","model":"glm-5.3","replay_state":{"items":[]}}"#,
            ),
            (
                "permission",
                r#"{"type":"permission_checked","time_ms":2600,"tool":"write_file","decision":{"deny":"too risky"}}"#,
            ),
            (
                "tool_requested",
                r#"{"type":"tool_requested","time_ms":2610,"call":{"id":"c1","name":"write_file","arguments":{"path":"notes.txt"}}}"#,
            ),
            (
                "tool_finished",
                r#"{"type":"tool_finished","time_ms":2620,"call_id":"c1","tool":"write_file","output":{"ok":true},"is_error":false}"#,
            ),
            (
                "retry",
                r#"{"type":"retry_scheduled","turn":2,"step":0,"time_ms":3000,"retry":1,"max_retries":3,"delay_ms":500,"failure":{"message":"upstream 503","code":"server","provider_retry_after_ms":1200}}"#,
            ),
            (
                "turn_completed",
                r#"{"type":"turn_ended","turn":2,"time_ms":3100,"reason":"completed"}"#,
            ),
            (
                "turn_aborted",
                r#"{"type":"turn_ended","turn":3,"time_ms":3200,"reason":{"aborted":"user interrupt"}}"#,
            ),
            (
                "turn_error",
                r#"{"type":"turn_ended","turn":4,"time_ms":3300,"reason":{"error":"model failed"}}"#,
            ),
            (
                "compaction",
                r#"{"type":"compaction","time_ms":4000,"summary_text":"earlier turns summarized"}"#,
            ),
            // MM-1A additive：含图 user 消息的 golden——blocks 只带
            // descriptor（无字节/路径），幂等键随行；纯文本形态见
            // 上面 "user" 行（零新字段）。
            (
                "user_image",
                r#"{"type":"user_message","turn":5,"time_ms":5000,"text":"look","content_blocks":[{"type":"text","text":"look"},{"type":"image","attachment":{"attachment_id":"0f8c2a4e11112222","media_type":"image/png","width":1024,"height":768,"bytes":2048,"display_name":"shot.png"}}],"client_message_id":"client-9"}"#,
            ),
        ];
        let samples: Vec<ReplayEvent> = vec![
            ReplayEvent::UserMessage {
                turn: 1,
                time_ms: 1000,
                text: "hi".into(),
                content_blocks: Vec::new(),
                client_message_id: None,
            },
            ReplayEvent::AssistantMessage {
                turn: 1,
                step: 0,
                time_ms: 2000,
                reasoning: None,
                text: "done".into(),
                tool_calls: vec![ToolCall {
                    id: "c1".into(),
                    name: "read_file".into(),
                    arguments: json!({"path": "a"}),
                }],
                provider: "glm".into(),
                model: "glm-5.3".into(),
                replay_state: None,
            },
            ReplayEvent::AssistantMessage {
                turn: 1,
                step: 1,
                time_ms: 2500,
                reasoning: Some("why".into()),
                text: String::new(),
                tool_calls: Vec::new(),
                provider: "glm".into(),
                model: "glm-5.3".into(),
                replay_state: Some(json!({"items": []})),
            },
            ReplayEvent::PermissionChecked {
                time_ms: 2600,
                tool: "write_file".into(),
                decision: PermissionDecision::Deny {
                    reason: "too risky".into(),
                },
            },
            ReplayEvent::ToolRequested {
                time_ms: 2610,
                call: ToolCall {
                    id: "c1".into(),
                    name: "write_file".into(),
                    arguments: json!({"path": "notes.txt"}),
                },
            },
            ReplayEvent::ToolFinished {
                time_ms: 2620,
                call_id: "c1".into(),
                tool: "write_file".into(),
                output: json!({"ok": true}),
                is_error: false,
            },
            ReplayEvent::RetryScheduled {
                turn: 2,
                step: 0,
                time_ms: 3000,
                retry: 1,
                max_retries: 3,
                delay_ms: 500,
                failure: ReplayRetryFailure {
                    message: "upstream 503".into(),
                    code: "server".into(),
                    provider_retry_after_ms: Some(1200),
                },
            },
            ReplayEvent::TurnEnded {
                turn: 2,
                time_ms: 3100,
                reason: ReplayTurnEnd::Completed,
            },
            ReplayEvent::TurnEnded {
                turn: 3,
                time_ms: 3200,
                reason: ReplayTurnEnd::Aborted {
                    cause: "user interrupt".into(),
                },
            },
            ReplayEvent::TurnEnded {
                turn: 4,
                time_ms: 3300,
                reason: ReplayTurnEnd::Error {
                    message: "model failed".into(),
                },
            },
            ReplayEvent::Compaction {
                time_ms: 4000,
                summary_text: "earlier turns summarized".into(),
            },
            ReplayEvent::UserMessage {
                turn: 5,
                time_ms: 5000,
                text: "look".into(),
                content_blocks: vec![
                    crate::message::ContentBlock::Text {
                        text: "look".into(),
                    },
                    crate::message::ContentBlock::Image {
                        attachment: crate::message::AttachmentDescriptor {
                            attachment_id: "0f8c2a4e11112222".into(),
                            media_type: "image/png".into(),
                            width: 1024,
                            height: 768,
                            bytes: 2048,
                            display_name: Some("shot.png".into()),
                            original_width: None,
                            original_height: None,
                        },
                    },
                ],
                client_message_id: Some("client-9".into()),
            },
        ];
        for ((label, golden), sample) in replay.iter().zip(samples) {
            assert_eq!(
                replay_event_json(&sample).to_string(),
                *golden,
                "golden drift in replay variant `{label}`"
            );
        }

        // 审批请求帧：effect 是机器标签，arguments 在最后。
        let request = PermissionRequest {
            tool: "write_file".into(),
            effect: ToolEffect::Write,
            reason: "tool `write_file` can cause side effects".into(),
            arguments: json!({"path": "notes.txt"}),
            call_id: "c9".into(),
        };
        assert_eq!(
            approval_requested_ctl("rpc-1", &request).to_string(),
            r#"{"rpc_id":"rpc-1","request":{"tool":"write_file","effect":"write","reason":"tool `write_file` can cause side effects","call_id":"c9","arguments":{"path":"notes.txt"}}}"#
        );

        // settled 三 outcome。
        let usage = Usage {
            input_tokens: 10,
            output_tokens: 5,
            cached_input_tokens: Some(2),
            reasoning_tokens: None,
        };
        assert_eq!(
            with_prompt_rpc_id(settled_completed("done", 2, &usage), "prompt-1").to_string(),
            r#"{"prompt_rpc_id":"prompt-1","outcome":{"type":"completed","output":"done","turns":2,"usage":{"input_tokens":10,"output_tokens":5,"cached_input_tokens":2}}}"#
        );
        assert_eq!(
            with_prompt_rpc_id(settled_cancelled(1, &usage), "prompt-1").to_string(),
            r#"{"prompt_rpc_id":"prompt-1","outcome":{"type":"cancelled","turns":1,"usage":{"input_tokens":10,"output_tokens":5,"cached_input_tokens":2}}}"#
        );
        assert_eq!(
            with_prompt_rpc_id(settled_failed("model error"), "prompt-1").to_string(),
            r#"{"prompt_rpc_id":"prompt-1","outcome":{"type":"failed","error":"model error"}}"#
        );

        // notice kind 是开放枚举，已实现形状进入 golden。
        assert_eq!(
            notice_ctl(&ApplicationEvent::MonitorUpdated(Some("GLM 12%".into()))).to_string(),
            r#"{"kind":"monitor","payload":"GLM 12%"}"#
        );
        assert_eq!(
            notice_ctl(&ApplicationEvent::MonitorUpdated(None)).to_string(),
            r#"{"kind":"monitor","payload":null}"#
        );
        assert_eq!(
            notice_ctl(&ApplicationEvent::CompactionUpdated(
                crate::CompactionStatus::Finished {
                    note: "compacted 30 turns".into(),
                    succeeded: true,
                }
            ))
            .to_string(),
            r#"{"kind":"compaction","payload":{"status":"finished","note":"compacted 30 turns","succeeded":true}}"#
        );
        assert_eq!(
            notice_ctl(&ApplicationEvent::TitleUpdated {
                title: "new title".into()
            })
            .to_string(),
            r#"{"kind":"title","payload":{"title":"new title"}}"#
        );
        assert_eq!(
            notice_ctl(&ApplicationEvent::McpStartupNotice { failures: 2 }).to_string(),
            r#"{"kind":"mcp_startup","payload":{"failures":2}}"#
        );
        assert_eq!(
            notice_ctl(&ApplicationEvent::ProcessFinished {
                session_id: 7,
                exit_code: Some(0),
                signal: None,
                timed_out: false,
                cancelled: false,
                terminated: false,
            })
            .to_string(),
            r#"{"kind":"process_finished","payload":{"session_id":7,"exit_code":0,"signal":null,"timed_out":false,"cancelled":false,"terminated":false}}"#
        );
    }
}
