use super::*;

#[cfg(unix)]
pub(super) struct Fixture {
    root: std::path::PathBuf,
    service: Arc<ProcessService>,
    generation: u64,
}

#[cfg(unix)]
impl Fixture {
    pub(super) fn new() -> Self {
        let root = super::sbx4_tests::root();
        let sandbox = Arc::new(
            SandboxService::new(
                root.clone(),
                SandboxModeSource::Shared(Arc::new(RwLock::new(PermissionMode::FullAccess))),
            )
            .unwrap(),
        );
        let service = Arc::new(ProcessService::new(Project::new(&root), sandbox));
        let generation = service.bind_run("outcomes", CancelToken::new()).unwrap();
        Self {
            root,
            service,
            generation,
        }
    }

    pub(super) fn exec(&self, cmd: &str) -> Value {
        self.exec_args(json!({"cmd":cmd,"yield_time_ms":1000}))
    }
    pub(super) fn exec_pipe(&self, cmd: &str) -> Value {
        self.exec_args(json!({"cmd":cmd,"yield_time_ms":1000,"stdin":"pipe"}))
    }
    fn exec_args(&self, args: Value) -> Value {
        ExecCommandTool {
            service: Arc::clone(&self.service),
        }
        .invoke(&args, &Project::new(&self.root), &CancelToken::new())
        .unwrap()
    }

    pub(super) fn poll(&self, id: &Value, chars: &str) -> Value {
        WriteStdinTool {
            service: Arc::clone(&self.service),
        }
        .invoke(
            &json!({"session_id":id,"chars":chars,"yield_time_ms":1000}),
            &Project::new(&self.root),
            &CancelToken::new(),
        )
        .unwrap()
    }

    pub(super) fn compat(&self, command: &str) -> Value {
        RunCommandTool {
            service: Arc::clone(&self.service),
        }
        .invoke(
            &json!({"command":command}),
            &Project::new(&self.root),
            &CancelToken::new(),
        )
        .unwrap()
    }
}

#[cfg(unix)]
impl Drop for Fixture {
    fn drop(&mut self) {
        self.service.unbind_run(self.generation).unwrap();
        self.service.close().unwrap();
        crate::test_support::cleanup_tree(&self.root);
    }
}

#[test]
#[cfg(unix)]
fn command_outcome_distinguishes_execution_refusal_from_program_failure() {
    use std::os::unix::fs::PermissionsExt;
    let f = Fixture::new();
    std::fs::write(f.root.join("blocked"), "#!/bin/sh\nexit 0\n").unwrap();
    std::fs::set_permissions(
        f.root.join("blocked"),
        std::fs::Permissions::from_mode(0o600),
    )
    .unwrap();
    let denied = f.exec("./blocked");
    assert_eq!(denied["exit_code"], 126);
    assert!(!denied["stderr"].as_str().unwrap().is_empty());
    assert_eq!(denied["command_outcome"]["kind"], "environment_failure");
    assert_eq!(denied["command_outcome"]["reason"], "execution_denied");
    assert_eq!(denied["command_outcome"]["evidence"], "output_signature");
    let program = f.exec("printf 'error[E0308]: mismatched types\n' >&2; exit 101");
    assert_eq!(program["exit_code"], 101);
    assert_eq!(program["command_outcome"]["kind"], "program_failure");
    assert_eq!(program["timed_out"], false);
}

#[test]
#[cfg(unix)]
fn command_outcome_polling_and_compat_use_the_same_classification() {
    let f = Fixture::new();
    let start =
        f.exec_pipe("read answer; printf 'sh: ./blocked: Permission denied\n' >&2; exit 126");
    assert_eq!(start["running"], true);
    assert_eq!(start["command_outcome"]["kind"], "running");
    let done = f.poll(&start["session_id"], "go\n");
    assert_eq!(done["exit_code"], 126);
    assert_eq!(done["command_outcome"]["kind"], "environment_failure");
    let again = f.poll(&start["session_id"], "");
    assert_eq!(again["stderr"], "");
    assert_eq!(again["command_outcome"], done["command_outcome"]);
    let compat = RunCommandTool {
        service: Arc::clone(&f.service),
    }
    .invoke(
        &json!({"command":"printf 'sh: ./blocked: Permission denied\n' >&2; exit 126"}),
        &Project::new(&f.root),
        &CancelToken::new(),
    )
    .unwrap();
    assert_eq!(compat["exit_code"], 126);
    assert_eq!(compat["command_outcome"], done["command_outcome"]);
}

#[test]
#[cfg(unix)]
fn command_outcome_preserves_timeout_and_does_not_guess_oom_or_hidden_failure() {
    let f = Fixture::new();
    let timeout = RunCommandTool {
        service: Arc::clone(&f.service),
    }
    .invoke(
        &json!({"command":"sleep 5","timeout_seconds":1}),
        &Project::new(&f.root),
        &CancelToken::new(),
    )
    .unwrap();
    assert_eq!(timeout["timed_out"], true);
    assert_eq!(timeout["command_outcome"]["kind"], "environment_failure");
    assert_eq!(timeout["command_outcome"]["reason"], "timeout");
    assert_eq!(timeout["command_outcome"]["evidence"], "lifecycle");
    for cmd in ["exit 126", "exit 127", "kill -KILL $$"] {
        assert_eq!(
            f.exec(cmd)["command_outcome"]["kind"],
            "unknown_failure",
            "{cmd}"
        );
    }
    let trailing = f.exec("false; printf ok");
    assert_eq!(trailing["exit_code"], 0);
    assert_eq!(trailing["command_outcome"]["kind"], "success");
    let asserted = f.exec("printf 'assertion failed: permission denied\n' >&2; exit 1");
    assert_eq!(asserted["command_outcome"]["kind"], "program_failure");
}

#[test]
fn command_outcome_survives_journal_projection_without_raw_output() {
    let live = json!({
        "exit_code":126,"stderr":"private path /secret/file: Permission denied",
        "command_outcome":{"kind":"environment_failure","reason":"execution_denied","evidence":"output_signature"},
        "sandbox":{"provider":"none","denied":false}
    });
    let journal = process_journal_output(&live);
    assert_eq!(journal["command_outcome"], live["command_outcome"]);
    assert_eq!(journal["exit_code"], 126);
    assert_eq!(journal["sandbox"], live["sandbox"]);
    assert!(!journal.to_string().contains("/secret/file"));
    let old = process_journal_output(&json!({"exit_code":1,"stderr":"old"}));
    assert!(old.get("command_outcome").is_none());
}

#[cfg(unix)]
struct ErrorProbe;

#[cfg(unix)]
impl crate::Model for ErrorProbe {
    fn provider(&self) -> &str {
        "offline"
    }
    fn model_id(&self) -> &str {
        "command-error-probe"
    }
    fn stream(
        &mut self,
        request: crate::ModelRequest<'_>,
        _events: &mut dyn crate::ModelEventSink,
    ) -> Result<crate::ModelResponse, crate::ModelError> {
        let mut response = crate::ModelResponse {
            text: String::new(),
            tool_calls: vec![],
            finish_reason: crate::FinishReason::Completed,
            usage: None,
            provider_response_id: None,
            provider_state: vec![],
            reasoning: None,
        };
        if let Some(crate::ModelItem::ToolResult(result)) = request.items.last() {
            assert!(result.is_error);
            assert!(
                result.output["error"]
                    .as_str()
                    .unwrap()
                    .contains("not available in this run")
            );
            assert_eq!(result.output["command_outcome"]["kind"], "unknown_failure");
            assert_eq!(result.output["verification_witness"]["status"], "unknown");
            response.text = "observed structured error".into();
        } else {
            response.finish_reason = crate::FinishReason::ToolCalls;
            response.tool_calls.push(crate::ToolCall {
                id: "missing-session".into(),
                name: "write_stdin".into(),
                arguments: json!({"session_id":999}),
            });
        }
        Ok(response)
    }
}

#[test]
#[cfg(unix)]
fn command_outcome_native_errors_reach_model_without_swallowing_error() {
    let f = Fixture::new();
    let tools = crate::ToolRegistry::new();
    let _lease = tools
        .register(
            crate::plugin::PluginOwner::for_test(PluginId::new("test.outcomes")),
            Arc::new(WriteStdinTool {
                service: Arc::clone(&f.service),
            }),
        )
        .unwrap();
    let mut probe = ErrorProbe;
    let project = Project::new(&f.root);
    let output = crate::Run::new(&mut probe, &tools, &crate::permission::AllowAll, &project)
        .execute_with_items(
            vec![crate::ModelItem::user_text("probe absent session")],
            crate::MessageContent::text("probe absent session"),
            None,
            &mut Vec::new(),
        )
        .unwrap();
    assert_eq!(output.text, "observed structured error");
}

#[test]
#[cfg(unix)]
fn command_outcome_launch_failure_retains_original_error_and_never_runs_command() {
    let f = Fixture::new();
    let work = f.root.join("vanishing-cwd");
    std::fs::create_dir(&work).unwrap();
    let tool = ExecCommandTool {
        service: Arc::clone(&f.service),
    };
    let args = json!({"cmd":"touch should-never-exist","workdir":"vanishing-cwd"});
    let prepared = tool.prepare_execution(&args).unwrap();
    std::fs::remove_dir(&work).unwrap();
    let error = prepared
        .tool
        .invoke(&args, &Project::new(&f.root), &CancelToken::new())
        .unwrap_err();
    let output = prepared.tool.error_output(&error);
    assert_eq!(output["error"], error.to_string());
    assert!(error.to_string().starts_with("process spawn failed:"));
    assert_eq!(output["command_outcome"]["reason"], "spawn_failed");
    assert_eq!(output["command_outcome"]["evidence"], "operation_context");
    assert!(!f.root.join("should-never-exist").exists());
    let resource = tool.error_output(&ToolError::new(
        "process active limit reached (8); terminate an existing session",
    ));
    assert_eq!(resource["command_outcome"]["reason"], "session_limit");
}

#[test]
fn command_outcome_catalog_checkpoint_and_replay_preserve_recorded_labels() {
    use crate::session::event::{SessionEvent, payloads};
    use crate::session::projection::{CheckpointIdentity, ProjectionRegistry};
    let samples = [
        (
            json!({"exit_code":126,"stderr":"transient diagnostic",
            "command_outcome":{"kind":"environment_failure","reason":"execution_denied","evidence":"output_signature"}}),
            false,
        ),
        (
            process_error_output(&ToolError::new(
                "process active limit reached (8); terminate an existing session",
            )),
            true,
        ),
        (json!({"exit_code":1,"stderr":"legacy output"}), false),
        (
            json!({"exit_code":0,"stdout":"transient diagnostic", "verification_witness":{
                "scope":"consumed_output_bytes","evidence":"output_signature","authenticated":false,
                "target_exit_code":null,"status":"observed","output_incomplete":false,
                "observations_truncated":false,"observations":[{"format":"rust_test_summary", "stream":"stdout",
                "status":"zero_matches","matched":0,"passed":0,"failed":0,"ignored":0,"filtered_out":2}]
            },"verification_receipt":{"version":1,"status":"inconclusive","exit_scope":"direct_child",
                "authenticated":false,"executed":0,"owner_generation":1,"invocation_sha256":"fixture"}}),
            false,
        ),
    ];
    for (live, is_error) in samples {
        let durable = process_journal_output(&live);
        let event = SessionEvent::new(
            "tool/result",
            0,
            1,
            payloads::tool_result(
                1,
                1,
                "command-1",
                payloads::tool_result_content(&durable),
                is_error,
            ),
        )
        .append(Vec::new());
        let seat = crate::session::catalog::event_spec("tool/result").unwrap();
        assert!(seat.surface);
        assert!(matches!(
            seat.replay,
            crate::session::catalog::ReplayKind::ToolResult
        ));
        seat.validate(&event, 4).unwrap();
        let mut replay = crate::session::replay::ReplayAdapter::new();
        let mut replay_events = Vec::new();
        replay.push(&event, &mut replay_events);
        let [
            crate::session::replay::ReplayEvent::ToolFinished {
                output,
                is_error: replay_error,
                ..
            },
        ] = replay_events.as_slice()
        else {
            panic!("tool/result must actually replay")
        };
        assert_eq!(output, &durable);
        assert_eq!(*replay_error, is_error);
        let mut original = ProjectionRegistry::clat();
        original.fold_one(&event).unwrap();
        let checkpoint = original.checkpoint(
            CheckpointIdentity {
                created_at: 0,
                cwd: None,
            },
            0,
        );
        let checkpoint = serde_json::from_value(serde_json::to_value(checkpoint).unwrap()).unwrap();
        let mut restored = ProjectionRegistry::clat();
        restored.restore(&checkpoint, &[], 1).unwrap();
        let items = restored.surface_nodes().unwrap();
        assert_eq!(items, original.surface_nodes().unwrap());
        let crate::ModelItem::ToolResult(result) = &items[0].1 else {
            panic!("missing tool result")
        };
        let json: Value = serde_json::from_str(result.output.as_str().unwrap()).unwrap();
        assert_eq!(json["command_outcome"], live["command_outcome"]);
        assert_eq!(json["verification_witness"], live["verification_witness"]);
        assert_eq!(json["verification_receipt"], live["verification_receipt"]);
        assert_eq!(result.is_error, is_error);
        assert!(!json.to_string().contains("transient diagnostic"));
        assert!(!json.to_string().contains("legacy output"));
    }
}
