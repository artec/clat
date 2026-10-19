use super::*;

struct ModelProbe {
    calls: usize,
    args: Value,
    denied: bool,
}
impl Model for ModelProbe {
    fn provider(&self) -> &str {
        "scripted-offline"
    }
    fn model_id(&self) -> &str {
        "wiring-not-capability"
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
                    panic!("production result missing")
                };
                if self.denied {
                    assert!(result.is_error);
                    vec![]
                } else {
                    assert!(!result.is_error, "{}", result.output);
                    assert!(result.output.get("clat_truncated").is_none());
                    assert_eq!(result.output["status"], "not_discriminating");
                    assert!(
                        result.output["next_action"]
                            .as_str()
                            .unwrap()
                            .contains("production entry")
                    );
                    vec![ToolCall {
                        id: "inspect".into(),
                        name: "run_command".into(),
                        arguments: json!({"argv":["cat","candidate/contract.spec.js"]}),
                    }]
                }
            }
            3 => {
                let Some(ModelItem::ToolResult(result)) = r.items.last() else {
                    panic!("inspection missing")
                };
                assert!(
                    result.output["stdout"]
                        .as_str()
                        .unwrap()
                        .contains("same frozen assertions")
                );
                vec![]
            }
            _ => panic!("tool added an unexpected model round"),
        };
        Ok(ModelResponse {
            text: if calls.is_empty() {
                "inspect test instead of claiming protection".into()
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
fn journey(denied: bool) {
    let (f, args) = fixture(false);
    let tools = f.manager.require(TOOL_SERVICE).unwrap();
    let pipeline = crate::tool::ToolExecutionPipeline::default();
    let _pruner = pipeline
        .register_result_transformer(
            crate::plugin::PluginOwner::for_test(crate::plugin::PluginId::new("test.pw-pair")),
            Arc::new(crate::plugins::ResultPruner),
        )
        .unwrap();
    let mut model = ModelProbe {
        calls: 0,
        args,
        denied,
    };
    let policy: &dyn crate::permission::PermissionPolicy = if denied {
        &crate::permission::SafeByDefault
    } else {
        &AllowAll
    };
    let output = crate::run::Run::new(&mut model, &tools, policy, &Project::new(&f.root))
        .with_tool_pipeline(&pipeline)
        .execute_with_items(
            vec![ModelItem::user_text("verify an actual counterexample")],
            crate::message::MessageContent::text("verify an actual counterexample"),
            None,
            &mut Vec::new(),
        );
    if denied {
        assert!(output.is_err());
        assert!(!f.root.join("candidate/ran").exists());
        assert!(!f.root.join("counterexample/ran").exists());
        assert_eq!(model.calls, 1, "denial ends before another model request");
    } else {
        assert_eq!(
            output.unwrap().text,
            "inspect test instead of claiming protection"
        );
        assert_eq!(model.calls, 3);
        assert_eq!(
            std::fs::read_to_string(f.root.join("candidate/ran")).unwrap(),
            "yes"
        );
        assert_eq!(
            std::fs::read_to_string(f.root.join("counterexample/ran")).unwrap(),
            "yes"
        );
    }
}
#[test]
fn ag10_playwright_pair_reaches_next_model_request_and_changes_the_followup() {
    journey(false);
}
#[test]
fn ag10_playwright_pair_obeys_normal_execute_denial_before_either_arm() {
    journey(true);
}
