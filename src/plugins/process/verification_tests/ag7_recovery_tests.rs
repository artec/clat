use super::*;
use crate::model::{FinishReason, Model, ModelError, ModelItem, ModelRequest, ModelResponse};
use crate::plugins::services::{TOOL_PIPELINE_SERVICE, TOOL_SERVICE};

struct RecoveryModel {
    calls: usize,
    args: Value,
    tool: &'static str,
    reason: &'static str,
    root: String,
    program: String,
}

impl Model for RecoveryModel {
    fn provider(&self) -> &str {
        "offline-recovery-probe"
    }
    fn model_id(&self) -> &str {
        "scripted-not-capability-evidence"
    }
    fn stream(
        &mut self,
        request: ModelRequest<'_>,
        _: &mut dyn crate::model::ModelEventSink,
    ) -> Result<ModelResponse, ModelError> {
        self.calls += 1;
        assert!(self.calls <= 3, "one correction, no automatic retries");
        if self.calls == 2 {
            let Some(ModelItem::ToolResult(result)) = request.items.last() else {
                panic!("error result")
            };
            assert!(result.is_error);
            let hint = &result.output["invocation_recovery"];
            assert_eq!(
                hint["reason"], self.reason,
                "the next model request needs actionable admission facts"
            );
            assert_eq!(hint["command_started"], false);
            if self.reason == "workdir_out_of_scope" {
                assert_eq!(hint["project_root"], self.root);
                self.args["workdir"] = json!(".");
            } else if self.reason == "verification_requires_argv" {
                assert_eq!(hint["required"], json!({"command_form":"argv","tty":false}));
                self.args.as_object_mut().unwrap().remove("command");
                self.args["argv"] = json!([self.program, "green"]);
                if self.tool == "exec_command" {
                    self.args["tty"] = json!(false);
                }
            } else {
                self.args.as_object_mut().unwrap().remove("command");
            }
        }
        let tool_calls = if self.calls <= 2 {
            vec![crate::ToolCall {
                id: format!("recovery-{}", self.calls),
                name: self.tool.into(),
                arguments: self.args.clone(),
            }]
        } else {
            let Some(ModelItem::ToolResult(result)) = request.items.last() else {
                panic!("corrected result")
            };
            assert!(!result.is_error, "{}", result.output);
            assert_eq!(result.output["verification_receipt"]["status"], "passed");
            vec![]
        };
        Ok(ModelResponse {
            text: if tool_calls.is_empty() {
                "literal invocation recovered".into()
            } else {
                String::new()
            },
            finish_reason: if tool_calls.is_empty() {
                FinishReason::Completed
            } else {
                FinishReason::ToolCalls
            },
            tool_calls,
            usage: None,
            provider_response_id: None,
            provider_state: vec![],
            reasoning: None,
        })
    }
}

fn recovery_journey(mode: u8) {
    let workdir = mode == 0;
    let f = Fixture::new();
    let mut manager = f.manager();
    let service = manager.require(PROCESS_SERVICE).unwrap();
    let generation = service
        .bind_run("ag7-recovery", CancelToken::new())
        .unwrap();
    let tools = manager.require(TOOL_SERVICE).unwrap();
    let pipeline = manager.require(TOOL_PIPELINE_SERVICE).unwrap();
    let mut args = json!({"argv":[f.program,"green"],"verification":{"format":"rust-test-summary","minimum_executed":1}});
    if mode == 3 {
        args["tty"] = json!(true);
        args["yield_time_ms"] = json!(30000);
    } else if workdir {
        args["workdir"] = json!(f.root.to_string_lossy());
        args["yield_time_ms"] = json!(30000);
    } else {
        args["command"] = json!("exit 99");
        if mode == 2 {
            args.as_object_mut().unwrap().remove("argv");
        }
    }
    let mut model = RecoveryModel {
        calls: 0,
        args,
        tool: if workdir || mode == 3 {
            "exec_command"
        } else {
            "run_command"
        },
        reason: if workdir {
            "workdir_out_of_scope"
        } else if mode >= 2 {
            "verification_requires_argv"
        } else {
            "ambiguous_command_input"
        },
        root: f.root.to_string_lossy().into_owned(),
        program: f.program.clone(),
    };
    let result = crate::run::Run::new(
        &mut model,
        &tools,
        &crate::permission::AllowAll,
        &Project::new(&f.root),
    )
    .with_tool_pipeline(&pipeline)
    .execute_with_items(
        vec![ModelItem::user_text("verify literal invocation")],
        crate::message::MessageContent::text("verify literal invocation"),
        None,
        &mut Vec::new(),
    )
    .unwrap();
    assert_eq!(result.text, "literal invocation recovered");
    assert_eq!(
        std::fs::read_to_string(f.root.join("invocations")).unwrap(),
        "green\n"
    );
    assert_eq!(model.calls, 3);
    service.unbind_run(generation).unwrap();
    manager.close().unwrap();
}

#[test]
fn ag7_recovery_keeps_literal_verification_after_workdir_rejection() {
    recovery_journey(0);
}

#[test]
fn ag7_recovery_resolves_argument_conflict_without_running_the_rejected_command() {
    recovery_journey(1);
}

#[test]
fn ag8_recovery_keeps_verification_after_shell_form_rejection() {
    recovery_journey(2);
}

#[test]
fn ag8_recovery_never_relabels_permission_or_runtime_errors_as_not_started() {
    let f = Fixture::new();
    for message in [
        "permission denied",
        "cancelled",
        "process spawn failed",
        "unknown verification format",
    ] {
        let out = super::super::invocation_recovery::annotate(&f.service, &ToolError::new(message));
        assert!(out.get("invocation_recovery").is_none());
    }
}

#[test]
fn ag8_recovery_keeps_verification_after_pty_form_rejection() {
    recovery_journey(3);
}
