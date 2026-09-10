use super::*;

/// 解析并解包 Run 事件（exec 终态走各自的专项测试）。
fn parse_run(line: &str) -> RunEvent {
    match parse_envelope_line(line).expect("roundtrip parse") {
        WireEvent::Run(event) => event,
        other => panic!("expected a run event, got {other:?}"),
    }
}

// INV-J2 编译期钉：下方两个 witness 对全部变体穷举 match、无通配
// 臂——新增 RunEvent/ModelEvent 变体时这里缺臂即编译失败，作者必须
// 同时补 wire 支持与下方样本。往返测试对每个样本调用 witness。
fn run_event_witness(event: &RunEvent) {
    match event {
        RunEvent::RunStarted { .. } => {}
        RunEvent::ModelRequested { .. } => {}
        RunEvent::ModelStream { .. } => {}
        RunEvent::ModelResponded { .. } => {}
        RunEvent::ToolRequested { .. } => {}
        RunEvent::PermissionChecked { .. } => {}
        RunEvent::PermissionDenied { .. } => {}
        RunEvent::ToolStarted { .. } => {}
        RunEvent::ToolFinished { .. } => {}
        RunEvent::SteeringApplied { .. } => {}
        RunEvent::RunCompleted { .. } => {}
        RunEvent::RunCancelled { .. } => {}
        RunEvent::RunFailed { .. } => {}
    }
}

fn model_event_witness(event: &ModelEvent) {
    match event {
        ModelEvent::ResponseStarted { .. } => {}
        ModelEvent::TextDelta { .. } => {}
        ModelEvent::RefusalDelta { .. } => {}
        ModelEvent::ToolCallStarted { .. } => {}
        ModelEvent::ToolArgumentsDelta { .. } => {}
        ModelEvent::ToolCallCompleted { .. } => {}
        ModelEvent::ReasoningDelta { .. } => {}
        ModelEvent::ReasoningSummaryDelta { .. } => {}
        ModelEvent::Usage(_) => {}
        ModelEvent::ResponseCompleted { .. } => {}
        ModelEvent::RetryScheduled { .. } => {}
        ModelEvent::RetryStarted { .. } => {}
        ModelEvent::ProviderEvent { .. } => {}
    }
}

fn sample_tool_call() -> ToolCall {
    ToolCall {
        id: "call-1".into(),
        name: "write_file".into(),
        arguments: json!({"path": "notes.txt", "content": "hi"}),
    }
}

/// MM-1A：样例 descriptor——宽高/字节/显示名齐全，进 golden 的
/// 唯一图片形状（INV-M1A-2：descriptor 之外无字节/路径字段）。
fn sample_image_block() -> crate::message::ContentBlock {
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
    }
}

fn sample_tool_result() -> ToolResult {
    ToolResult {
        blocks: Vec::new(),
        image_parts: Vec::new(),
        call_id: "call-1".into(),
        tool_name: "write_file".into(),
        output: json!({"ok": true}),
        is_error: false,
    }
}

fn sample_retry_failure() -> RetryFailure {
    RetryFailure {
        message: "upstream 503".into(),
        code: "server".into(),
        status: Some(503),
        provider_retry_after_ms: Some(1200),
    }
}

fn sample_usage() -> Usage {
    Usage {
        input_tokens: 12,
        output_tokens: 34,
        cached_input_tokens: Some(5),
        reasoning_tokens: Some(6),
    }
}

fn run_event_samples() -> Vec<RunEvent> {
    vec![
        RunEvent::RunStarted {
            project: PathBuf::from("/tmp/repo"),
            message: crate::message::MessageContent::text("do it"),
            client_message_id: None,
        },
        RunEvent::ModelRequested {
            turn: 1,
            provider: "application-test".into(),
            model: "deterministic".into(),
        },
        RunEvent::ModelStream {
            turn: 1,
            event: ModelEvent::TextDelta {
                delta: "hel\u{1b}lo".into(),
            },
        },
        RunEvent::ModelResponded {
            turn: 1,
            outcome: ModelOutcome {
                has_text: true,
                tool_calls: 1,
            },
            finish_reason: FinishReason::Completed,
            provider_replay: Some(json!({"items": [{"type": "reasoning"}]})),
        },
        RunEvent::ToolRequested {
            call: sample_tool_call(),
        },
        RunEvent::PermissionChecked {
            tool: "write_file".into(),
            decision: PermissionDecision::Unavailable {
                reason: "non-interactive run denied `write_file`".into(),
            },
        },
        RunEvent::PermissionDenied {
            tool: "write_file".into(),
            reason: "denied".into(),
        },
        RunEvent::ToolStarted {
            call_id: "call-1".into(),
            tool: "write_file".into(),
        },
        RunEvent::ToolFinished {
            result: sample_tool_result(),
        },
        RunEvent::SteeringApplied {
            message: crate::message::MessageContent::from_blocks(vec![
                crate::message::ContentBlock::Text {
                    text: "steer mid-run".into(),
                },
                sample_image_block(),
            ]),
            client_message_id: Some("client-7".into()),
            request_digest: None,
            receipt: None,
        },
        RunEvent::RunCompleted {
            output: "done".into(),
            turns: 2,
            usage: sample_usage(),
        },
        RunEvent::RunCancelled {
            turns: 1,
            usage: Usage::default(),
        },
        RunEvent::RunFailed {
            message: "model error: boom".into(),
        },
    ]
}

fn model_event_samples() -> Vec<ModelEvent> {
    vec![
        ModelEvent::ResponseStarted {
            response_id: Some("resp-1".into()),
        },
        ModelEvent::ResponseStarted { response_id: None },
        ModelEvent::TextDelta {
            delta: "hello".into(),
        },
        ModelEvent::RefusalDelta { delta: "no".into() },
        ModelEvent::ToolCallStarted {
            call_id: "call-1".into(),
            name: Some("write_file".into()),
        },
        ModelEvent::ToolCallStarted {
            call_id: "call-1".into(),
            name: None,
        },
        ModelEvent::ToolArgumentsDelta {
            call_id: "call-1".into(),
            delta: "{\"pa".into(),
        },
        ModelEvent::ToolCallCompleted {
            call: sample_tool_call(),
        },
        ModelEvent::ReasoningDelta {
            delta: "thinking".into(),
        },
        ModelEvent::ReasoningSummaryDelta {
            delta: "summary".into(),
        },
        ModelEvent::Usage(Usage {
            input_tokens: 1,
            output_tokens: 2,
            cached_input_tokens: None,
            reasoning_tokens: None,
        }),
        ModelEvent::ResponseCompleted {
            finish_reason: FinishReason::Unknown("vendor-stop-9".into()),
        },
        ModelEvent::ResponseCompleted {
            finish_reason: FinishReason::ToolCalls,
        },
        ModelEvent::RetryScheduled {
            retry: 1,
            max_retries: 3,
            delay_ms: 500,
            failure: sample_retry_failure(),
        },
        ModelEvent::RetryStarted { retry: 1 },
        ModelEvent::ProviderEvent {
            name: "server_event".into(),
        },
    ]
}

#[test]
fn roundtrip_every_run_event_variant() {
    for event in run_event_samples() {
        run_event_witness(&event);
        let line = envelope_line(&event);
        assert!(line.ends_with('\n'), "line must be newline-terminated");
        assert!(
            !line[..line.len() - 1].contains('\n'),
            "one event per line, no embedded newlines"
        );
        // INV-J3：信封形态钉——v 在前、event 在后、type 标签开头。
        assert!(
            line.starts_with(r#"{"v":1,"event":{"type":""#),
            "envelope shape: {line}"
        );
        assert_eq!(parse_run(&line), event);
    }
}

#[test]
fn roundtrip_every_model_event_variant() {
    for event in model_event_samples() {
        model_event_witness(&event);
        let wrapped = RunEvent::ModelStream {
            turn: 2,
            event: event.clone(),
        };
        match parse_run(&envelope_line(&wrapped)) {
            RunEvent::ModelStream { turn, event: inner } => {
                assert_eq!(turn, 2);
                assert_eq!(inner, event);
            }
            other => panic!("expected model_stream, got {other:?}"),
        }
    }
}

#[test]
fn optional_fields_are_omitted_when_none() {
    // Usage 的 None 字段省略（amend-only：缺席即 None）。
    let event = RunEvent::ModelStream {
        turn: 1,
        event: ModelEvent::Usage(Usage::default()),
    };
    let line = envelope_line(&event);
    assert!(!line.contains("cached_input_tokens"));
    assert!(!line.contains("reasoning_tokens"));
    let parsed = parse_run(&line);
    assert_eq!(parsed, event);

    // response_id/name 为 None 时同样省略。
    let event = RunEvent::ModelStream {
        turn: 1,
        event: ModelEvent::ToolCallStarted {
            call_id: "call-1".into(),
            name: None,
        },
    };
    let line = envelope_line(&event);
    assert!(!line.contains(r#""name""#));
    assert_eq!(parse_run(&line), event);

    // provider_replay 为 None 时省略。
    let event = RunEvent::ModelResponded {
        turn: 1,
        outcome: ModelOutcome {
            has_text: true,
            tool_calls: 0,
        },
        finish_reason: FinishReason::Completed,
        provider_replay: None,
    };
    let line = envelope_line(&event);
    assert!(!line.contains("provider_replay"));
    assert_eq!(parse_run(&line), event);
}

#[test]
fn control_characters_stay_escaped() {
    // FP-10：字符串载荷里的 C0/DEL 经 serde 转义，结构性字节全为
    // 可打印 ASCII——事件行不能把终端转义序列带进显示流。
    let event = RunEvent::SteeringApplied {
        message: crate::message::MessageContent::text("x\u{1b}]52;c;base64\u{7f}y"),
        client_message_id: None,
        request_digest: None,
        receipt: None,
    };
    let line = envelope_line(&event);
    assert!(
        line.chars().all(|c| c == '\n' || !c.is_control()),
        "structural bytes must be printable: {line:?}"
    );
    assert!(!line.contains('\u{1b}'));
    assert_eq!(parse_run(&line), event);
}

#[test]
fn finish_reason_and_permission_decision_forms_roundtrip() {
    let reasons = [
        FinishReason::Completed,
        FinishReason::ToolCalls,
        FinishReason::MaxTokens,
        FinishReason::Refusal,
        FinishReason::Cancelled,
        FinishReason::Incomplete,
        FinishReason::Error,
        FinishReason::Unknown("rate_limited".into()),
    ];
    for reason in reasons {
        let event = RunEvent::ModelStream {
            turn: 1,
            event: ModelEvent::ResponseCompleted {
                finish_reason: reason.clone(),
            },
        };
        assert_eq!(parse_run(&envelope_line(&event)), event);
    }
    let decisions = [
        PermissionDecision::Allow,
        PermissionDecision::Ask {
            reason: "needs review".into(),
        },
        PermissionDecision::Deny {
            reason: "too risky".into(),
        },
        PermissionDecision::Unavailable {
            reason: "no approver".into(),
        },
    ];
    for decision in decisions {
        let event = RunEvent::PermissionChecked {
            tool: "run_command".into(),
            decision: decision.clone(),
        };
        assert_eq!(parse_run(&envelope_line(&event)), event);
    }
}

#[test]
fn unknown_fields_are_tolerated() {
    // v1 amend-only：读取方容忍未知字段（未来修订可能新增可选字段）。
    let line =
        r#"{"v":1,"future_field":true,"event":{"type":"run_failed","message":"boom","extra":1}}"#;
    assert_eq!(
        parse_run(line),
        RunEvent::RunFailed {
            message: "boom".into()
        }
    );
}

#[test]
fn malformed_lines_fail_closed() {
    assert_eq!(
        parse_envelope_line("not json"),
        Err(WireError::Malformed("line is not valid JSON"))
    );
    assert_eq!(
        parse_envelope_line(r#"{"event":{"type":"run_failed","message":"x"}}"#),
        Err(WireError::Malformed(
            "envelope `v` is missing or not a number"
        ))
    );
    assert_eq!(
        parse_envelope_line(r#"{"v":1}"#),
        Err(WireError::Malformed("envelope has no `event`"))
    );
    assert_eq!(
        parse_envelope_line(r#"{"v":2,"event":{"type":"run_failed","message":"x"}}"#),
        Err(WireError::Version(2))
    );
    assert_eq!(
        parse_envelope_line(r#"{"v":1,"event":{"type":"quantum_collapsed","message":"x"}}"#),
        Err(WireError::UnknownType("quantum_collapsed".into()))
    );
    assert_eq!(
        parse_envelope_line(r#"{"v":1,"event":{"type":"run_failed"}}"#),
        Err(WireError::Field {
            event: "run_failed",
            field: "message"
        })
    );
    assert_eq!(
        parse_envelope_line(
            r#"{"v":1,"event":{"type":"run_completed","output":"x","turns":"many","usage":{"input_tokens":1,"output_tokens":1}}}"#
        ),
        Err(WireError::Field {
            event: "run_completed",
            field: "turns"
        })
    );
}

/// PWA1-02：v1 政策是「新增 type = 词汇变更 = v2」——顶层与内嵌
/// ModelEvent 词汇同样钉死，读侧对未知 type fail-closed 与政策
/// 自洽（顶层腿见 malformed_lines_fail_closed）。
#[test]
fn nested_unknown_model_event_type_fails_closed() {
    let line = r#"{"v":1,"event":{"type":"model_stream","turn":1,"event":{"type":"context_compacted","tokens":42}}}"#;
    assert_eq!(
        parse_envelope_line(line),
        Err(WireError::UnknownType("context_compacted".into()))
    );
}

/// PWA1-01（wire 半）：exec 终态是唯一携带进程退出码的事件，往返
/// 与 fail-closed 纪律与其他 typed 事件一致（行序由 exec 集成测试
/// 钉：恒为最后一行）。
#[test]
fn exec_finals_roundtrip_and_carry_exit_codes() {
    assert_eq!(
        parse_envelope_line(&exec_completed_line(0)).expect("parse"),
        WireEvent::ExecCompleted { exit_code: 0 }
    );
    assert_eq!(
        parse_envelope_line(&exec_failed_line(130, "cancelled after 2 turns")).expect("parse"),
        WireEvent::ExecFailed {
            exit_code: 130,
            message: "cancelled after 2 turns".into(),
        }
    );
    assert_eq!(
        parse_envelope_line(r#"{"v":1,"event":{"type":"exec_failed","exit_code":1}}"#),
        Err(WireError::Field {
            event: "exec_failed",
            field: "message"
        })
    );
}

/// PWA1-03：固定 JSON golden——字段名/顺序/省略形态是 v1 契约本身，
/// 不是当前实现的副产物。任何词汇或 nested schema 变更（按政策应
/// 升 v2）都在这里红；内部 `ToolCall`/`ToolResult` 的 serde 演进
/// 不再能静默改写 wire（写侧经 wire 拥有的显式映射）。
#[test]
fn v1_golden_lines_never_drift() {
    let golden: Vec<(&str, WireEvent)> = vec![
        (
            r#"{"v":1,"event":{"type":"run_started","project":"/repo","prompt":"hi"}}"#,
            WireEvent::Run(RunEvent::RunStarted {
                project: PathBuf::from("/repo"),
                message: crate::message::MessageContent::text("hi"),
                client_message_id: None,
            }),
        ),
        (
            r#"{"v":1,"event":{"type":"model_requested","turn":1,"provider":"glm","model":"glm-5.3"}}"#,
            WireEvent::Run(RunEvent::ModelRequested {
                turn: 1,
                provider: "glm".into(),
                model: "glm-5.3".into(),
            }),
        ),
        (
            r#"{"v":1,"event":{"type":"model_stream","turn":1,"event":{"type":"response_started","response_id":"resp-1"}}}"#,
            WireEvent::Run(RunEvent::ModelStream {
                turn: 1,
                event: ModelEvent::ResponseStarted {
                    response_id: Some("resp-1".into()),
                },
            }),
        ),
        (
            r#"{"v":1,"event":{"type":"model_stream","turn":1,"event":{"type":"response_started"}}}"#,
            WireEvent::Run(RunEvent::ModelStream {
                turn: 1,
                event: ModelEvent::ResponseStarted { response_id: None },
            }),
        ),
        (
            r#"{"v":1,"event":{"type":"model_stream","turn":1,"event":{"type":"text_delta","delta":"done"}}}"#,
            WireEvent::Run(RunEvent::ModelStream {
                turn: 1,
                event: ModelEvent::TextDelta {
                    delta: "done".into(),
                },
            }),
        ),
        (
            r#"{"v":1,"event":{"type":"model_stream","turn":1,"event":{"type":"refusal_delta","delta":"no"}}}"#,
            WireEvent::Run(RunEvent::ModelStream {
                turn: 1,
                event: ModelEvent::RefusalDelta { delta: "no".into() },
            }),
        ),
        (
            r#"{"v":1,"event":{"type":"model_stream","turn":1,"event":{"type":"tool_call_started","call_id":"c1","name":"read_file"}}}"#,
            WireEvent::Run(RunEvent::ModelStream {
                turn: 1,
                event: ModelEvent::ToolCallStarted {
                    call_id: "c1".into(),
                    name: Some("read_file".into()),
                },
            }),
        ),
        (
            r#"{"v":1,"event":{"type":"model_stream","turn":1,"event":{"type":"tool_call_started","call_id":"c1"}}}"#,
            WireEvent::Run(RunEvent::ModelStream {
                turn: 1,
                event: ModelEvent::ToolCallStarted {
                    call_id: "c1".into(),
                    name: None,
                },
            }),
        ),
        (
            r#"{"v":1,"event":{"type":"model_stream","turn":1,"event":{"type":"tool_arguments_delta","call_id":"c1","delta":"{\"pa"}}}"#,
            WireEvent::Run(RunEvent::ModelStream {
                turn: 1,
                event: ModelEvent::ToolArgumentsDelta {
                    call_id: "c1".into(),
                    delta: "{\"pa".into(),
                },
            }),
        ),
        (
            r#"{"v":1,"event":{"type":"model_stream","turn":1,"event":{"type":"tool_call_completed","call":{"id":"c1","name":"read_file","arguments":{"path":"a"}}}}}"#,
            WireEvent::Run(RunEvent::ModelStream {
                turn: 1,
                event: ModelEvent::ToolCallCompleted {
                    call: ToolCall {
                        id: "c1".into(),
                        name: "read_file".into(),
                        arguments: json!({"path": "a"}),
                    },
                },
            }),
        ),
        (
            r#"{"v":1,"event":{"type":"model_stream","turn":1,"event":{"type":"reasoning_delta","delta":"thinking"}}}"#,
            WireEvent::Run(RunEvent::ModelStream {
                turn: 1,
                event: ModelEvent::ReasoningDelta {
                    delta: "thinking".into(),
                },
            }),
        ),
        (
            r#"{"v":1,"event":{"type":"model_stream","turn":1,"event":{"type":"reasoning_summary_delta","delta":"summary"}}}"#,
            WireEvent::Run(RunEvent::ModelStream {
                turn: 1,
                event: ModelEvent::ReasoningSummaryDelta {
                    delta: "summary".into(),
                },
            }),
        ),
        (
            r#"{"v":1,"event":{"type":"model_stream","turn":1,"event":{"type":"usage","input_tokens":10,"output_tokens":5}}}"#,
            WireEvent::Run(RunEvent::ModelStream {
                turn: 1,
                event: ModelEvent::Usage(Usage {
                    input_tokens: 10,
                    output_tokens: 5,
                    cached_input_tokens: None,
                    reasoning_tokens: None,
                }),
            }),
        ),
        (
            r#"{"v":1,"event":{"type":"model_stream","turn":1,"event":{"type":"response_completed","finish_reason":"completed"}}}"#,
            WireEvent::Run(RunEvent::ModelStream {
                turn: 1,
                event: ModelEvent::ResponseCompleted {
                    finish_reason: FinishReason::Completed,
                },
            }),
        ),
        (
            r#"{"v":1,"event":{"type":"model_stream","turn":1,"event":{"type":"response_completed","finish_reason":{"unknown":"vendor-stop"}}}}"#,
            WireEvent::Run(RunEvent::ModelStream {
                turn: 1,
                event: ModelEvent::ResponseCompleted {
                    finish_reason: FinishReason::Unknown("vendor-stop".into()),
                },
            }),
        ),
        (
            r#"{"v":1,"event":{"type":"model_stream","turn":1,"event":{"type":"retry_scheduled","retry":1,"max_retries":3,"delay_ms":500,"failure":{"message":"upstream 503","code":"server","status":503,"provider_retry_after_ms":1200}}}}"#,
            WireEvent::Run(RunEvent::ModelStream {
                turn: 1,
                event: ModelEvent::RetryScheduled {
                    retry: 1,
                    max_retries: 3,
                    delay_ms: 500,
                    failure: RetryFailure {
                        message: "upstream 503".into(),
                        code: "server".into(),
                        status: Some(503),
                        provider_retry_after_ms: Some(1200),
                    },
                },
            }),
        ),
        (
            r#"{"v":1,"event":{"type":"model_stream","turn":1,"event":{"type":"retry_started","retry":1}}}"#,
            WireEvent::Run(RunEvent::ModelStream {
                turn: 1,
                event: ModelEvent::RetryStarted { retry: 1 },
            }),
        ),
        (
            r#"{"v":1,"event":{"type":"model_stream","turn":1,"event":{"type":"provider_event","name":"ping"}}}"#,
            WireEvent::Run(RunEvent::ModelStream {
                turn: 1,
                event: ModelEvent::ProviderEvent {
                    name: "ping".into(),
                },
            }),
        ),
        (
            r#"{"v":1,"event":{"type":"model_responded","turn":1,"outcome":{"has_text":true,"tool_calls":1},"finish_reason":"completed"}}"#,
            WireEvent::Run(RunEvent::ModelResponded {
                turn: 1,
                outcome: ModelOutcome {
                    has_text: true,
                    tool_calls: 1,
                },
                finish_reason: FinishReason::Completed,
                provider_replay: None,
            }),
        ),
        (
            r#"{"v":1,"event":{"type":"model_responded","turn":1,"outcome":{"has_text":false,"tool_calls":0},"finish_reason":"tool_calls","provider_replay":{"items":[]}}}"#,
            WireEvent::Run(RunEvent::ModelResponded {
                turn: 1,
                outcome: ModelOutcome {
                    has_text: false,
                    tool_calls: 0,
                },
                finish_reason: FinishReason::ToolCalls,
                provider_replay: Some(json!({"items": []})),
            }),
        ),
        (
            r#"{"v":1,"event":{"type":"tool_requested","call":{"id":"c1","name":"read_file","arguments":{"path":"a"}}}}"#,
            WireEvent::Run(RunEvent::ToolRequested {
                call: ToolCall {
                    id: "c1".into(),
                    name: "read_file".into(),
                    arguments: json!({"path": "a"}),
                },
            }),
        ),
        (
            r#"{"v":1,"event":{"type":"permission_checked","tool":"read_file","decision":"allow"}}"#,
            WireEvent::Run(RunEvent::PermissionChecked {
                tool: "read_file".into(),
                decision: PermissionDecision::Allow,
            }),
        ),
        (
            r#"{"v":1,"event":{"type":"permission_checked","tool":"write_file","decision":{"unavailable":"no approver"}}}"#,
            WireEvent::Run(RunEvent::PermissionChecked {
                tool: "write_file".into(),
                decision: PermissionDecision::Unavailable {
                    reason: "no approver".into(),
                },
            }),
        ),
        (
            r#"{"v":1,"event":{"type":"permission_denied","tool":"write_file","reason":"denied"}}"#,
            WireEvent::Run(RunEvent::PermissionDenied {
                tool: "write_file".into(),
                reason: "denied".into(),
            }),
        ),
        (
            r#"{"v":1,"event":{"type":"tool_started","call_id":"c1","tool":"write_file"}}"#,
            WireEvent::Run(RunEvent::ToolStarted {
                call_id: "c1".into(),
                tool: "write_file".into(),
            }),
        ),
        (
            r#"{"v":1,"event":{"type":"tool_finished","result":{"call_id":"c1","tool_name":"read_file","output":{"ok":true},"is_error":false}}}"#,
            WireEvent::Run(RunEvent::ToolFinished {
                result: ToolResult {
                    blocks: Vec::new(),
                    image_parts: Vec::new(),
                    call_id: "c1".into(),
                    tool_name: "read_file".into(),
                    output: json!({"ok": true}),
                    is_error: false,
                },
            }),
        ),
        (
            r#"{"v":1,"event":{"type":"steering_applied","text":"steer"}}"#,
            WireEvent::Run(RunEvent::SteeringApplied {
                message: crate::message::MessageContent::text("steer"),
                client_message_id: None,
                request_digest: None,
                receipt: None,
            }),
        ),
        (
            r#"{"v":1,"event":{"type":"run_completed","output":"done","turns":2,"usage":{"input_tokens":12,"output_tokens":34,"cached_input_tokens":5,"reasoning_tokens":6}}}"#,
            WireEvent::Run(RunEvent::RunCompleted {
                output: "done".into(),
                turns: 2,
                usage: Usage {
                    input_tokens: 12,
                    output_tokens: 34,
                    cached_input_tokens: Some(5),
                    reasoning_tokens: Some(6),
                },
            }),
        ),
        (
            r#"{"v":1,"event":{"type":"run_cancelled","turns":1,"usage":{"input_tokens":0,"output_tokens":0}}}"#,
            WireEvent::Run(RunEvent::RunCancelled {
                turns: 1,
                usage: Usage::default(),
            }),
        ),
        (
            r#"{"v":1,"event":{"type":"run_failed","message":"model error"}}"#,
            WireEvent::Run(RunEvent::RunFailed {
                message: "model error".into(),
            }),
        ),
        (
            r#"{"v":1,"event":{"type":"exec_completed","exit_code":0}}"#,
            WireEvent::ExecCompleted { exit_code: 0 },
        ),
        (
            r#"{"v":1,"event":{"type":"exec_failed","exit_code":130,"message":"cancelled after 2 turns"}}"#,
            WireEvent::ExecFailed {
                exit_code: 130,
                message: "cancelled after 2 turns".into(),
            },
        ),
    ];
    for (line, event) in golden {
        let produced = match &event {
            WireEvent::Run(run_event) => envelope_line(run_event),
            WireEvent::ExecCompleted { exit_code } => exec_completed_line(*exit_code),
            WireEvent::ExecFailed { exit_code, message } => exec_failed_line(*exit_code, message),
        };
        assert_eq!(
            produced,
            format!("{line}\n"),
            "writer must produce the golden bytes exactly"
        );
        assert_eq!(
            parse_envelope_line(line).expect("golden must parse back"),
            event,
            "reader must read the golden bytes back"
        );
    }
}

/// PWA1-04：v1 的 `project` 是 UTF-8 display path——非 UTF-8 路径
/// lossy 转写但**必须显式打标**，绝不静默替换。修复前本测试红
///（无 `project_utf8_lossy` 字段）。
#[cfg(unix)]
#[test]
fn non_utf8_project_path_is_marked_lossy_explicitly() {
    use std::ffi::OsString;
    use std::os::unix::ffi::OsStringExt;
    let event = RunEvent::RunStarted {
        project: PathBuf::from(OsString::from_vec(b"/tmp/\xffbad".to_vec())),
        message: crate::message::MessageContent::text("hi"),
        client_message_id: None,
    };
    let line = envelope_line(&event);
    assert!(
        line.contains(r#""project_utf8_lossy":true"#),
        "lossy transcription must be explicit, never silent: {line}"
    );
    assert!(
        line.contains('\u{fffd}'),
        "display path is the lossy transcription: {line}"
    );
    // 读回得到 lossy 形态（无损往返的域是 UTF-8 路径）。
    match parse_run(&line) {
        RunEvent::RunStarted { project, .. } => {
            assert_eq!(project, PathBuf::from("/tmp/\u{fffd}bad"));
        }
        other => panic!("expected run_started, got {other:?}"),
    }
}

/// MM-1A（INV-M1A-1/2/6）：含图消息的固定 JSON golden——
/// `content_blocks` 的字段名/顺序/省略形态、descriptor 的 wire 面
///（snake_case、无字节/路径字段）、`client_message_id` 的位置都是
/// v1 契约本身。pre-fix（无 content_blocks 投影）本测试红。
#[test]
fn mm1a_content_blocks_golden_lines_never_drift() {
    let image = sample_image_block();
    let descriptor_json = r#"{"attachment_id":"0f8c2a4e11112222","media_type":"image/png","width":1024,"height":768,"bytes":2048,"display_name":"shot.png"}"#;
    // run_started：prompt 保持文本投影，blocks 追加，幂等键在尾。
    let run_started = RunEvent::RunStarted {
        project: PathBuf::from("/repo"),
        message: crate::message::MessageContent::from_blocks(vec![
            crate::message::ContentBlock::Text {
                text: "look".into(),
            },
            image.clone(),
        ]),
        client_message_id: Some("client-1".into()),
    };
    let line = envelope_line(&run_started);
    assert_eq!(
        line,
        format!(
            "{{\"v\":1,\"event\":{{\"type\":\"run_started\",\"project\":\"/repo\",\"prompt\":\"look\",\"content_blocks\":[{{\"type\":\"text\",\"text\":\"look\"}},{{\"type\":\"image\",\"attachment\":{descriptor_json}}}],\"client_message_id\":\"client-1\"}}}}\n"
        )
    );
    assert_eq!(parse_run(&line), run_started);
    // steering_applied：同构。
    let steering = RunEvent::SteeringApplied {
        message: crate::message::MessageContent::from_blocks(vec![
            crate::message::ContentBlock::Text {
                text: "and this".into(),
            },
            image.clone(),
        ]),
        client_message_id: Some("client-2".into()),
        request_digest: Some("digest-2".into()),
        receipt: Some(Box::new(crate::message::AdmissionReceipt::committed(
            "client-2".into(),
            "message-2".into(),
            vec!["attachment-1".into()],
        ))),
    };
    let line = envelope_line(&steering);
    assert_eq!(
        line,
        format!(
            "{{\"v\":1,\"event\":{{\"type\":\"steering_applied\",\"text\":\"and this\",\"content_blocks\":[{{\"type\":\"text\",\"text\":\"and this\"}},{{\"type\":\"image\",\"attachment\":{descriptor_json}}}],\"client_message_id\":\"client-2\",\"request_digest\":\"digest-2\",\"receipt\":{{\"client_message_id\":\"client-2\",\"state\":\"committed\",\"committed_message_id\":\"message-2\",\"attachment_ids\":[\"attachment-1\"],\"retryable\":false}}}}}}\n"
        )
    );
    assert_eq!(parse_run(&line), steering);
    // tool_finished：result 内的 content_blocks（blocks 空则省略——
    // 既有 golden 已钉纯文本形状）。
    let tool_finished = RunEvent::ToolFinished {
        result: ToolResult {
            call_id: "c1".into(),
            tool_name: "view_image".into(),
            output: json!({"noted": true}),
            is_error: false,
            blocks: vec![image],
            image_parts: Vec::new(),
        },
    };
    let line = envelope_line(&tool_finished);
    assert_eq!(
        line,
        format!(
            "{{\"v\":1,\"event\":{{\"type\":\"tool_finished\",\"result\":{{\"call_id\":\"c1\",\"tool_name\":\"view_image\",\"output\":{{\"noted\":true}},\"is_error\":false,\"content_blocks\":[{{\"type\":\"image\",\"attachment\":{descriptor_json}}}]}}}}}}\n"
        )
    );
    assert_eq!(parse_run(&line), tool_finished);
}

/// MM-1A（INV-M1A-6）：纯文本消息的 wire 字节与 v1 完全一致——
/// `content_blocks`/`client_message_id` 缺省省略；descriptor 的
/// 可选字段（original_*）省略形态由 golden 上面的样本锁定。
#[test]
fn mm1a_text_only_messages_omit_the_additive_fields_entirely() {
    for event in [
        RunEvent::RunStarted {
            project: PathBuf::from("/repo"),
            message: crate::message::MessageContent::text("plain"),
            client_message_id: None,
        },
        RunEvent::SteeringApplied {
            message: crate::message::MessageContent::text("plain"),
            client_message_id: None,
            request_digest: None,
            receipt: None,
        },
    ] {
        let line = envelope_line(&event);
        assert!(!line.contains("content_blocks"), "no blocks field: {line}");
        assert!(!line.contains("client_message_id"), "no id field: {line}");
    }
    let tool = RunEvent::ToolFinished {
        result: ToolResult {
            call_id: "c1".into(),
            tool_name: "read_file".into(),
            output: json!("ok"),
            is_error: false,
            blocks: Vec::new(),
            image_parts: Vec::new(),
        },
    };
    let line = envelope_line(&tool);
    assert!(!line.contains("content_blocks"), "no blocks field: {line}");
}

/// MM-1A：读回的 fail-closed 面——坏 block 形状/未知 block type/
/// 坏幂等键类型按 v1 政策拒绝（新 type = 未知 type fail-closed 的
/// 既有纪律延伸到嵌套 blocks 词汇）。
#[test]
fn mm1a_malformed_content_blocks_fail_closed() {
    let cases = [
        // content_blocks 非数组。
        r#"{"v":1,"event":{"type":"run_started","project":"/repo","prompt":"x","content_blocks":"nope"}}"#,
        // 未知 block type。
        r#"{"v":1,"event":{"type":"run_started","project":"/repo","prompt":"x","content_blocks":[{"type":"video","attachment":{}}]}}"#,
        // 缺 text 字段。
        r#"{"v":1,"event":{"type":"steering_applied","text":"x","content_blocks":[{"type":"text"}]}}"#,
        // descriptor 缺 attachment_id。
        r#"{"v":1,"event":{"type":"run_started","project":"/repo","prompt":"x","content_blocks":[{"type":"image","attachment":{"media_type":"image/png","width":1,"height":1,"bytes":1}}]}}"#,
        // client_message_id 类型错误。
        r#"{"v":1,"event":{"type":"run_started","project":"/repo","prompt":"x","client_message_id":7}}"#,
    ];
    for line in cases {
        assert!(
            parse_envelope_line(line).is_err(),
            "malformed content must fail closed: {line}"
        );
    }
}
