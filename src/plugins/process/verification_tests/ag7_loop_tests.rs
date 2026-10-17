use super::*;
use crate::model::{FinishReason, Model, ModelError, ModelItem, ModelRequest, ModelResponse};
use crate::plugins::services::{TOOL_PIPELINE_SERVICE, TOOL_SERVICE};

struct EvidenceModel {
    stage: usize,
    calls: usize,
    program: String,
}

impl EvidenceModel {
    fn next(&mut self, request: ModelRequest<'_>) -> (&'static str, Option<(&'static str, Value)>) {
        assert!(request.tools.iter().any(|t| t.name == "session_info"));
        if self.stage == 0 {
            self.stage = 1;
            return ("", Some(("session_info", json!({}))));
        }
        let Some(ModelItem::ToolResult(result)) = request.items.last() else {
            panic!("native tool result required")
        };
        if self.stage == 1 {
            assert_eq!(result.output["session_id"], "ag7-loop");
            self.stage = 2;
            return (
                "",
                Some((
                    "run_command",
                    json!({"argv":[self.program,"ag7-red"],
                "evidence":{"directory":"records","files":["stage.log"],"bind_files":["probe.rs"]}}),
                )),
            );
        }
        assert!(!result.is_error, "{}", result.output);
        assert!(
            result.output.get("clat_truncated").is_none(),
            "critical context pruned"
        );
        if result.output["running"] == true {
            return (
                "",
                Some((
                    "write_stdin",
                    json!({"session_id":result.output["session_id"],"yield_time_ms":1000}),
                )),
            );
        }
        match self.stage {
            2 => {
                assert_eq!(result.output["exit_code"], 101);
                assert_eq!(result.output["evidence"]["status"], "recorded");
                assert_eq!(
                    result.output["failure_context"]["tests"],
                    json!(["case::denies_invalid"])
                );
                self.stage = 3;
                (
                    "",
                    Some((
                        "exec_command",
                        json!({"argv":[self.program,"ag7-inspect",result.output["evidence"]["path"]],"yield_time_ms":1000}),
                    )),
                )
            }
            3 => {
                assert!(
                    result.output["stdout"]
                        .as_str()
                        .unwrap()
                        .contains("preserved-first-failure")
                );
                self.stage = 4;
                (
                    "",
                    Some((
                        "run_command",
                        json!({"argv":[self.program,"ag7-targeted","case::denies_invalid"],
                    "verification":{"format":"rust-test-summary","minimum_executed":1}}),
                    )),
                )
            }
            4 => {
                assert_eq!(result.output["verification_receipt"]["status"], "passed");
                assert_eq!(result.output["verification_receipt"]["executed"], 1);
                ("first failure inspected; only failed test rechecked", None)
            }
            _ => unreachable!(),
        }
    }
}

impl Model for EvidenceModel {
    fn provider(&self) -> &str {
        "offline-engineering-probe"
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
        assert!(self.calls <= 12, "no repeated verification loops");
        let (text, call) = self.next(request);
        let tool_calls: Vec<_> = call
            .into_iter()
            .map(|(name, arguments)| crate::ToolCall {
                id: format!("ag7-{}", self.calls),
                name: name.into(),
                arguments,
            })
            .collect();
        Ok(ModelResponse {
            text: text.into(),
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

fn journey(denied: bool) {
    let f = Fixture::new();
    let mut manager = f.manager();
    let service = manager.require(PROCESS_SERVICE).unwrap();
    let generation = service.bind_run("ag7-loop", CancelToken::new()).unwrap();
    let tools = manager.require(TOOL_SERVICE).unwrap();
    let pipeline = manager.require(TOOL_PIPELINE_SERVICE).unwrap();
    let mut model = EvidenceModel {
        stage: 0,
        calls: 0,
        program: f.program.clone(),
    };
    let policy: &dyn crate::permission::PermissionPolicy = if denied {
        &crate::permission::SafeByDefault
    } else {
        &crate::permission::AllowAll
    };
    let outcome = crate::run::Run::new(&mut model, &tools, policy, &Project::new(&f.root))
        .with_tool_pipeline(&pipeline)
        .execute_with_items(
            vec![ModelItem::user_text("inspect the current run and verify")],
            crate::message::MessageContent::text("inspect the current run and verify"),
            None,
            &mut Vec::new(),
        );
    if denied {
        assert!(
            outcome
                .unwrap_err()
                .to_string()
                .contains("permission required")
        );
        assert!(!f.root.join("invocations").exists());
        assert!(!f.root.join("records").exists());
        assert_eq!(model.calls, 2);
    } else {
        let result = outcome.unwrap();
        assert_eq!(
            result.text,
            "first failure inspected; only failed test rechecked"
        );
        assert_eq!(
            std::fs::read_to_string(f.root.join("invocations")).unwrap(),
            "ag7-red\nag7-inspect\nag7-targeted\n"
        );
        assert_eq!(
            std::fs::read_to_string(f.root.join("targeted-filter")).unwrap(),
            "case::denies_invalid"
        );
        let records = std::fs::read_dir(f.root.join("records"))
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|n| n.starts_with("command-") && n.ends_with(".json"))
            .count();
        assert_eq!(
            records, 1,
            "one explicit request produces one immutable record"
        );
        assert_eq!(model.calls, 5);
    }
    service.unbind_run(generation).unwrap();
    manager.close().unwrap();
}

#[test]
fn ag7_production_loop_retains_failure_reads_record_and_runs_only_the_failed_filter() {
    journey(false);
}

#[test]
fn ag7_production_loop_permission_denial_has_no_process_or_evidence_side_effect() {
    journey(true);
}
