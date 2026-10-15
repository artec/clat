use super::*;

const EXPECTED: &str = "the live result must equal seven";

#[test]
fn expected_failure_rejects_an_unrelated_panic_through_the_agent_loop() {
    // Printing the expectation is deliberately insufficient: it must occur
    // in the failed assertion, not in arbitrary test stdout.
    let f = Fixture::new(&format!(
        "println!({EXPECTED:?}); assert!(api::live()!=0,\"unrelated fixture failure\");"
    ));
    let tools = f.manager.require(TOOL_SERVICE).unwrap();
    let pipeline = crate::tool::ToolExecutionPipeline::default();
    let _pruner = pipeline
        .register_result_transformer(
            crate::plugin::PluginOwner::for_test(crate::plugin::PluginId::new("test.expectation")),
            Arc::new(crate::plugins::ResultPruner),
        )
        .unwrap();
    let mut args = f.args();
    args["expected_failure"] = json!(EXPECTED);
    let mut model = ExpectedFailureModel { args, calls: 0 };
    let output = crate::run::Run::new(&mut model, &tools, &AllowAll, &Project::new(&f.root))
        .with_tool_pipeline(&pipeline)
        .execute_with_items(
            vec![ModelItem::user_text("Check the declared live contract")],
            crate::message::MessageContent::text("Check the declared live contract"),
            None,
            &mut Vec::new(),
        )
        .unwrap();
    assert_eq!(
        output.text,
        "inspect the unrelated failure before claiming discrimination"
    );
    assert_eq!(model.calls, 3, "the tool does not add model requests");
}

struct ExpectedFailureModel {
    args: Value,
    calls: usize,
}

impl Model for ExpectedFailureModel {
    fn provider(&self) -> &str {
        "scripted-expectation"
    }
    fn model_id(&self) -> &str {
        "offline"
    }
    fn stream(
        &mut self,
        r: ModelRequest<'_>,
        _: &mut dyn crate::model::ModelEventSink,
    ) -> Result<ModelResponse, ModelError> {
        self.calls += 1;
        let calls = match self.calls {
            1 => vec![ToolCall {
                id: "pair".into(),
                name: "test_discrimination".into(),
                arguments: self.args.clone(),
            }],
            2 => {
                let Some(ModelItem::ToolResult(result)) = r.items.last() else {
                    panic!("missing tool feedback")
                };
                assert!(!result.is_error, "{}", result.output);
                assert!(result.output.get("clat_truncated").is_none());
                assert_eq!(
                    result.output["status"], "inconclusive",
                    "unrelated red cannot prove the declared invariant"
                );
                assert_eq!(
                    result.output["counterexample"]["failure_expectation"],
                    "mismatched"
                );
                assert!(
                    result.output["next_action"]
                        .as_str()
                        .unwrap()
                        .contains("failure reason")
                );
                vec![ToolCall {
                    id: "inspect".into(),
                    name: "run_command".into(),
                    arguments: json!({"command":"cat counterexample/src/api.rs"}),
                }]
            }
            3 => {
                let Some(ModelItem::ToolResult(result)) = r.items.last() else {
                    panic!("missing inspection")
                };
                assert!(result.output["stdout"].as_str().unwrap().contains("{ 0 }"));
                vec![]
            }
            _ => panic!("unexpected extra model request"),
        };
        Ok(ModelResponse {
            text: if calls.is_empty() {
                "inspect the unrelated failure before claiming discrimination".into()
            } else {
                String::new()
            },
            finish_reason: if calls.is_empty() {
                FinishReason::Completed
            } else {
                FinishReason::ToolCalls
            },
            tool_calls: calls,
            usage: None,
            provider_response_id: None,
            provider_state: vec![],
            reasoning: None,
        })
    }
}

#[test]
fn expected_failure_accepts_the_declared_invariant_and_keeps_journal_bounded() {
    let f = Fixture::new(&format!("assert_eq!(api::live(),7,{EXPECTED:?});"));
    let mut args = f.args();
    args["expected_failure"] = json!(EXPECTED);
    let output = f.invoke(&args).unwrap();
    assert_eq!(
        output["status"], "discriminates_declared_defect",
        "{output}"
    );
    assert_eq!(output["counterexample"]["failure_expectation"], "matched");
    let tool = f
        .manager
        .require(TOOL_SERVICE)
        .unwrap()
        .get("test_discrimination")
        .unwrap();
    let durable = tool.journal_output(&output);
    assert!(durable["counterexample"].get("stdout").is_none());
    assert_eq!(durable["counterexample"]["failure_expectation"], "matched");
    assert!(serde_json::to_string(&durable).unwrap().len() < 8192);
}

#[test]
fn invalid_expectation_is_rejected_before_any_build() {
    let f = Fixture::new("assert_eq!(api::live(),7);");
    for expected in [String::new(), " ".into(), "x".repeat(513), "中".repeat(200)] {
        let mut args = f.args();
        args["expected_failure"] = json!(expected);
        assert!(f.invoke(&args).is_err());
    }
    assert!(!f.root.join("candidate/target").exists());
    assert!(!f.root.join("counterexample/target").exists());
}

#[test]
fn expected_failure_without_a_panic_diagnostic_is_inconclusive() {
    let f = Fixture::new(
        "if api::live()==0 { std::panic::set_hook(Box::new(|_| {})); panic!(\"hidden contract\"); }",
    );
    let mut args = f.args();
    args["expected_failure"] = json!("hidden contract");
    let out = f.invoke(&args).unwrap();
    assert_eq!(out["counterexample"]["test_summaries"][0]["failed"], 1);
    assert_eq!(out["counterexample"]["failure_expectation"], "unavailable");
    assert_eq!(out["status"], "inconclusive");
}

#[test]
fn expected_failure_is_checked_before_diagnostic_excerpting() {
    let f = Fixture::new(&format!(
        "assert!(api::live()==7,\"{{}}{EXPECTED}{{}}\",\"x\".repeat(600),\"y\".repeat(600));"
    ));
    let mut args = f.args();
    args["expected_failure"] = json!(EXPECTED);
    let out = f.invoke(&args).unwrap();
    assert_eq!(out["status"], "discriminates_declared_defect", "{out}");
    assert!(
        !out["counterexample"]["stdout"]
            .as_str()
            .unwrap()
            .contains(EXPECTED)
    );
    assert_eq!(out["counterexample"]["failure_expectation"], "matched");
    assert!(serde_json::to_string(&out).unwrap().chars().count() < 8192);
}
