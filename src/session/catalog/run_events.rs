//! Runtime projection seats owned by the event catalog. Journal vocabulary and
//! RunEvent are not one-to-one: a seat may write zero or many durable events.
//! Bodies expand in their owning module: recorder state stays private and wire
//! helpers remain private. Adding a variant requires all three projections here;
//! generated exhaustive matches preserve the compiler's missing-variant check.
macro_rules! run_event_seats {
    ($consumer:ident, $this:ident, $input:ident, $object:ident) => {
        $consumer! { $this, $input, $object;
RunStarted {
            project,
            message,
            client_message_id,
        } { .. } => "run_started";
record {}
wire { {
            let mut fields = Vec::new();
            match project.to_str() {
                Some(path) => fields.push(("project", Value::String(path.to_owned()))),
                None => {
                    // PWA1-04：v1 的 project 是 UTF-8 display path。非
                    // UTF-8 路径 lossy 转写并显式打标——绝不静默替换；
                    // 无损往返只承诺 UTF-8 域。
                    fields.push((
                        "project",
                        Value::String(project.to_string_lossy().into_owned()),
                    ));
                    fields.push(("project_utf8_lossy", Value::Bool(true)));
                }
            }
            // MM-1A：`prompt` 保持文本投影语义；图片存在时附加
            // content_blocks 与客户端幂等键（additive，INV-M1A-6）。
            fields.extend(message_text_fields(message, "prompt"));
            if let Some(client_message_id) = client_message_id {
                fields.push((
                    "client_message_id",
                    Value::String(client_message_id.clone()),
                ));
            }
            event_object("run_started", fields)
        } }
parse { Ok(RunEvent::RunStarted {
            // PWA1-04：`project_utf8_lossy` 是写侧的显式 lossy 标记，
            // 读侧按未知可选字段容忍（RunEvent 无处安放；无损往返的
            // 域是 UTF-8 路径）。
            project: PathBuf::from(string_field($object, "run_started", "project")?),
            message: message_from_wire($object, "run_started", "prompt")?,
            client_message_id: client_message_id_from_wire($object, "run_started")?,
        }) }

ModelRequested {
            turn,
            provider,
            model,
        } { .. } => "model_requested";
record {
                $this.refresh_dynamic_instructions();
                $this.close_open_step();
                $this.open_step();
            }
wire { event_object(
            "model_requested",
            vec![
                ("turn", json!(turn)),
                ("provider", Value::String(provider.clone())),
                ("model", Value::String(model.clone())),
            ],
        ) }
parse { Ok(RunEvent::ModelRequested {
            turn: usize_field($object, "model_requested", "turn")?,
            provider: string_field($object, "model_requested", "provider")?,
            model: string_field($object, "model_requested", "model")?,
        }) }

ModelStream { turn, event } { event: stream, .. } => "model_stream";
record {
                match stream {
                    ModelEvent::RetryScheduled {
                        retry,
                        max_retries,
                        delay_ms,
                        failure,
                    } => {
                        // v2 把没有成为最终 assistant/message 的一次模型
                        // 请求也作为独立 attempt 留在同一条流里；供应商在
                        // 首字节前失败时，finish 仍明确记下失败边界与原因。
                        if let Some(usage) = $this.stream_usage.as_ref() {
                            $this.assistant_stream.push(
                                crate::session::event::now_ms(),
                                serde_json::json!({
                                    "type": "usage",
                                    "usage": usage_value(usage),
                                }),
                            );
                        }
                        let mut attempt_failure = serde_json::json!({
                            "message": failure.message,
                            "code": failure.code,
                        });
                        if let Some(status) = failure.status {
                            attempt_failure["status"] = serde_json::json!(status);
                        }
                        if let Some(retry_after) = failure.provider_retry_after_ms {
                            attempt_failure["providerRetryAfterMs"] =
                                serde_json::json!(retry_after);
                        }
                        $this.assistant_stream.push(
                            crate::session::event::now_ms(),
                            serde_json::json!({
                                "type": "finish",
                                "reason": { "kind": "error", "failure": attempt_failure },
                            }),
                        );
                        $this.assistant_attempt();
                        // FP-01：失败 attempt 已烧掉的保守成本兑现（预留
                        // 保留给同请求的下一次 attempt，最终成功 attempt
                        // 由 ModelResponded 的 reconcile 替换）。
                        if let Some(ledger) = &$this.budget.ledger {
                            ledger.commit_retry_attempt();
                        }
                        // llm/retry (catalog §2.3): a retryable failure with
                        // its backoff, durable before the wait.
                        let (turn, step) = $this.state();
                        let failure = serde_json::json!({
                            "message": failure.message,
                            "code": failure.code,
                            "providerRetryAfterMs": failure.provider_retry_after_ms,
                        });
                        let retry_id = uuid::Uuid::new_v4().to_string();
                        $this.pending_retry_id = Some(retry_id.clone());
                        $this.append_quietly(
                            NewSessionEvent::new(
                                "llm/retry",
                                payloads::llm_retry(
                                    &retry_id,
                                    turn,
                                    step,
                                    &$this.provider,
                                    *retry,
                                    *max_retries,
                                    *delay_ms,
                                    failure,
                                ),
                            )
                            .log_only(),
                        );
                        $this.flush_quietly();
                    }
                    ModelEvent::RetryStarted { retry } => {
                        let (turn, step) = $this.state();
                        let retry_id = $this
                            .pending_retry_id
                            .take()
                            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
                        $this.append_quietly(
                            NewSessionEvent::new(
                                "llm/retry-started",
                                payloads::llm_retry_started(&retry_id, turn, step, *retry),
                            )
                            .log_only(),
                        );
                    }
                    _ if $this.step_open => {
                        $this.handle_stream_chunk(stream.clone());
                    }
                    _ => {}
                }
            }
wire { event_object(
            "model_stream",
            vec![("turn", json!(turn)), ("event", model_event_to_json(event))],
        ) }
parse { Ok(RunEvent::ModelStream {
            turn: usize_field($object, "model_stream", "turn")?,
            event: model_event_from_json(required($object, "model_stream", "event")?)?,
        }) }

ModelResponded {
            turn,
            outcome,
            finish_reason,
            provider_replay,
        } {
                finish_reason,
                provider_replay,
                ..
            } => "model_responded";
record {
                $this.replay_state = provider_replay
                    .as_ref()
                    .filter(|replay| !replay.is_null())
                    .cloned();
                $this.assistant_message(finish_reason);
                $this.message_emitted = true;
            }
wire { {
            let mut fields = vec![
                ("turn", json!(turn)),
                ("outcome", model_outcome_to_json(outcome)),
                ("finish_reason", finish_reason_to_json(finish_reason)),
            ];
            if let Some(replay) = provider_replay {
                fields.push(("provider_replay", replay.clone()));
            }
            event_object("model_responded", fields)
        } }
parse { Ok(RunEvent::ModelResponded {
            turn: usize_field($object, "model_responded", "turn")?,
            outcome: model_outcome_from_json(
                required($object, "model_responded", "outcome")?,
                "model_responded",
            )?,
            finish_reason: finish_reason_from_json(
                required($object, "model_responded", "finish_reason")?,
                "model_responded",
            )?,
            provider_replay: opt_value_field($object, "provider_replay"),
        }) }

ToolRequested { call } { call } => "tool_requested";
record {
                let index = 2 + $this.next_block_index();
                let arguments = $this
                    .tool_registry
                    .as_ref()
                    .and_then(|registry| registry.get(&call.name))
                    .map_or_else(
                        || call.arguments.clone(),
                        |tool| tool.journal_arguments(&call.arguments),
                    );
                let pending = PendingCall {
                    id: call.id.clone(),
                    name: call.name.clone(),
                    arguments,
                    index,
                };
                if let Ok(mut shared) = $this.shared.lock() {
                    shared.stash(call.id.clone(), pending);
                }
            }
wire { {
            event_object("tool_requested", vec![("call", tool_call_to_json(call))])
        } }
parse { Ok(RunEvent::ToolRequested {
            call: tool_call_from_json(
                required($object, "tool_requested", "call")?,
                "tool_requested",
            )?,
        }) }

PermissionChecked { tool, decision } { .. } => "permission_checked";
record {}
wire { event_object(
            "permission_checked",
            vec![
                ("tool", Value::String(tool.clone())),
                ("decision", permission_decision_to_json(decision)),
            ],
        ) }
parse { Ok(RunEvent::PermissionChecked {
            tool: string_field($object, "permission_checked", "tool")?,
            decision: permission_decision_from_json(
                required($object, "permission_checked", "decision")?,
                "permission_checked",
            )?,
        }) }

PermissionDenied { tool, reason } { tool, .. } => "permission_denied";
record {
                let denial = $this.shared.lock().ok().and_then(|mut shared| {
                    let call_id = shared.unjournaled_by_name(tool)?;
                    let call = shared.take(&call_id)?;
                    Some((call, shared.turn, shared.step))
                });
                if let Some((call, turn, step)) = denial {
                    // Policy-level denial (no approval round-trip): only the
                    // error tool/result lands durably — no tool/call, the
                    // same shape recovery synthesizes (catalog §3).
                    let result_text = format!("permission denied for tool `{tool}`");
                    let event = NewSessionEvent::new(
                        "tool/result",
                        payloads::tool_result(
                            turn,
                            step,
                            &call.id,
                            payloads::tool_result_content(&Value::String(result_text)),
                            true,
                        ),
                    )
                    .append(Vec::new());
                    $this.append_quietly(event);
                    $this.flush_quietly();
                }
            }
wire { event_object(
            "permission_denied",
            vec![
                ("tool", Value::String(tool.clone())),
                ("reason", Value::String(reason.clone())),
            ],
        ) }
parse { Ok(RunEvent::PermissionDenied {
            tool: string_field($object, "permission_denied", "tool")?,
            reason: string_field($object, "permission_denied", "reason")?,
        }) }

ToolStarted { call_id, tool } { call_id, tool } => "tool_started";
record {
                let (already, call, turn, step) = match $this.shared.lock() {
                    Ok(shared) => {
                        let already = shared.is_journaled(call_id);
                        let call = shared.get(call_id).cloned();
                        (already, call, shared.turn, shared.step)
                    }
                    Err(_) => {
                        $this.forwarded.push($input);
                        return;
                    }
                };
                if !already {
                    let arguments = call
                        .as_ref()
                        .map(|pending| pending.arguments.clone())
                        .unwrap_or_else(|| Value::Object(Default::default()));
                    let event = NewSessionEvent::new(
                        "tool/call",
                        payloads::tool_call(turn, step, call_id, tool, &arguments),
                    )
                    .log_only();
                    $this.append_quietly(event);
                    // Pre-execution durability barrier: a crash between
                    // `tool/call` and `tool/result` must synthesize an
                    // outcome-unknown result (recovery.rs).
                    $this.flush_quietly();
                }
            }
wire { event_object(
            "tool_started",
            vec![
                ("call_id", Value::String(call_id.clone())),
                ("tool", Value::String(tool.clone())),
            ],
        ) }
parse { Ok(RunEvent::ToolStarted {
            call_id: string_field($object, "tool_started", "call_id")?,
            tool: string_field($object, "tool_started", "tool")?,
        }) }

ToolFinished { result } { result } => "tool_finished";
record {
                let (turn, step) = $this.state();
                let output = $this
                    .tool_registry
                    .as_ref()
                    .and_then(|registry| registry.get(&result.tool_name))
                    .map_or_else(
                        || result.output.clone(),
                        |tool| tool.journal_output(&result.output),
                    );
                let event = NewSessionEvent::new(
                    "tool/result",
                    payloads::tool_result(
                        turn,
                        step,
                        &result.call_id,
                        payloads::tool_result_content_with_blocks(&output, &result.blocks),
                        result.is_error,
                    ),
                )
                .append(Vec::new());
                $this.append_quietly(event);
            }
wire { event_object(
            "tool_finished",
            vec![("result", tool_result_to_json(result))],
        ) }
parse { Ok(RunEvent::ToolFinished {
            result: tool_result_from_json(
                required($object, "tool_finished", "result")?,
                "tool_finished",
            )?,
        }) }

SteeringApplied {
            message,
            client_message_id,
            request_digest,
            receipt,
        } { .. } => "steering_applied";
record {
 let RunEvent::SteeringApplied {
                message,
                client_message_id,
                request_digest,
                ..
            } = $input else { unreachable!("catalog seat") };

                // Claim is committed here, not in the queue or Run. Close the
                // prior step, append+flush the typed user event, then upgrade the
                // forwarded event from Reserved to Committed. If durability
                // fails no receipt is emitted; recorder.finish turns the run into
                // the authoritative failure.
                $this.close_open_step();
                let message_id = uuid::Uuid::new_v4().to_string();
                let digest = client_message_id
                    .as_ref()
                    .map(|_| request_digest.unwrap_or_else(|| message.request_digest()));
                // RunEvent intentionally carries descriptors only. The
                // journal payload helper ignores `path`; rebuild its internal
                // adapter shape without letting provider-visible absolute
                // paths escape the queued PendingMessage.
                let images = message
                    .image_descriptors()
                    .into_iter()
                    .map(|descriptor| crate::message::JournalImage {
                        descriptor: descriptor.clone(),
                        path: String::new(),
                    })
                    .collect::<Vec<_>>();
                let payload = payloads::admitted_user_message(
                    &message_id,
                    &message.plain_text(),
                    &images,
                    client_message_id.as_deref(),
                    digest.as_deref(),
                );
                let appended = $this
                    .append_quietly(
                        NewSessionEvent::new("user/message", payload).append(Vec::new()),
                    )
                    .is_some();
                $this.flush_quietly();
                let receipt = (appended && $this.journal_error.is_none())
                    .then(|| {
                        client_message_id.clone().map(|client_message_id| {
                            Box::new(crate::message::AdmissionReceipt::committed(
                                client_message_id,
                                message_id,
                                message.attachment_ids(),
                            ))
                        })
                    })
                    .flatten();
                $this.forwarded.push(RunEvent::SteeringApplied {
                    message,
                    client_message_id,
                    request_digest: digest,
                    receipt,
                });
                return;

}
wire { {
            let mut fields = message_text_fields(message, "text");
            if let Some(client_message_id) = client_message_id {
                fields.push((
                    "client_message_id",
                    Value::String(client_message_id.clone()),
                ));
            }
            if let Some(request_digest) = request_digest {
                fields.push(("request_digest", Value::String(request_digest.clone())));
            }
            if let Some(receipt) = receipt {
                fields.push(("receipt", admission_receipt_to_json(receipt)));
            }
            event_object("steering_applied", fields)
        } }
parse { Ok(RunEvent::SteeringApplied {
            message: message_from_wire($object, "steering_applied", "text")?,
            client_message_id: client_message_id_from_wire($object, "steering_applied")?,
            request_digest: opt_string_field($object, "steering_applied", "request_digest")?,
            receipt: admission_receipt_from_wire($object, "steering_applied")?,
        }) }

RunCompleted {
            output,
            turns,
            usage,
        } { .. } => "run_completed";
record {
                $this.terminal = Some($input);
                return;
            }
wire { event_object(
            "run_completed",
            vec![
                ("output", Value::String(output.clone())),
                ("turns", json!(turns)),
                ("usage", usage_to_json(usage)),
            ],
        ) }
parse { Ok(RunEvent::RunCompleted {
            output: string_field($object, "run_completed", "output")?,
            turns: usize_field($object, "run_completed", "turns")?,
            usage: usage_from_map(
                object_of(
                    required($object, "run_completed", "usage")?,
                    "`usage` is not an object",
                )?,
                "run_completed",
            )?,
        }) }

RunCancelled { turns, usage } { .. } => "run_cancelled";
record {
                $this.terminal = Some($input);
                return;
            }
wire { event_object(
            "run_cancelled",
            vec![("turns", json!(turns)), ("usage", usage_to_json(usage))],
        ) }
parse { Ok(RunEvent::RunCancelled {
            turns: usize_field($object, "run_cancelled", "turns")?,
            usage: usage_from_map(
                object_of(
                    required($object, "run_cancelled", "usage")?,
                    "`usage` is not an object",
                )?,
                "run_cancelled",
            )?,
        }) }

RunFailed { message } { message } => "run_failed";
record {
                // Model/loop failures also fail the journal close below;
                // keep the first journal error authoritative.
                let _ = message;
                $this.terminal = Some($input);
                return;
            }
wire { event_object(
            "run_failed",
            vec![("message", Value::String(message.clone()))],
        ) }
parse { Ok(RunEvent::RunFailed {
            message: string_field($object, "run_failed", "message")?,
        }) }
        }
    };
}
pub(crate) use run_event_seats;
