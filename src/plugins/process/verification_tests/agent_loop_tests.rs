use super::*;
use crate::model::{FinishReason, Model, ModelError, ModelItem, ModelRequest, ModelResponse};
use crate::plugin::{PluginManager, ScopeKind};
use crate::plugins::services::{TOOL_PIPELINE_SERVICE, TOOL_SERVICE};

#[test]
fn verification_model_sees_failure_after_pruning_and_actually_inspects_the_contract() {
    let f = Fixture::new();
    std::fs::write(
        f.root.join("contract.txt"),
        "contract: preserve every source byte",
    )
    .unwrap();
    let mut manager = PluginManager::root(ScopeKind::TrustedProject);
    manager
        .mount_all(vec![
            Arc::new(crate::plugins::ToolRegistryPlugin),
            Arc::new(crate::plugins::ToolPipelinePlugin),
            Arc::new(crate::plugins::ToolResultPrunerPlugin),
            Arc::new(SandboxPlugin {
                project: Project::new(&f.root),
                permission_mode: Some(Arc::new(RwLock::new(PermissionMode::FullAccess))),
            }),
            Arc::new(ProcessServicePlugin {
                project: Project::new(&f.root),
            }),
            Arc::new(ExecToolsPlugin),
        ])
        .unwrap();
    let service = manager.require(PROCESS_SERVICE).unwrap();
    let generation = service
        .bind_run("verification-loop", CancelToken::new())
        .unwrap();
    let tools = manager.require(TOOL_SERVICE).unwrap();
    let pipeline = manager.require(TOOL_PIPELINE_SERVICE).unwrap();
    let mut model = ProbeModel {
        calls: 0,
        program: f.program.clone(),
        args: f.args("noisy"),
        inspecting: false,
    };
    let mut events = Vec::new();
    let output = crate::run::Run::new(
        &mut model,
        &tools,
        &crate::permission::AllowAll,
        &Project::new(&f.root),
    )
    .with_tool_pipeline(&pipeline)
    .execute_with_items(
        vec![ModelItem::user_text("verify the contract")],
        crate::message::MessageContent::text("verify the contract"),
        None,
        &mut events,
    )
    .unwrap();
    assert_eq!(
        output.text,
        "failure inspected; no unsupported success claim"
    );
    assert_eq!(
        model.calls,
        events
            .iter()
            .filter(|e| matches!(e, crate::RunEvent::ToolStarted { .. }))
            .count()
            + 1
    );
    assert_eq!(
        std::fs::read_to_string(f.root.join("invocations")).unwrap(),
        "noisy\ninspect\n"
    );
    service.unbind_run(generation).unwrap();
    manager.close().unwrap();
}

struct ProbeModel {
    calls: usize,
    program: String,
    args: Value,
    inspecting: bool,
}
impl ProbeModel {
    fn next(&mut self, request: ModelRequest<'_>) -> (&'static str, Option<(&'static str, Value)>) {
        if self.calls == 1 {
            return ("", Some(("exec_command", self.args.clone())));
        }
        let Some(ModelItem::ToolResult(result)) = request.items.last() else {
            panic!("missing production result")
        };
        assert!(!result.is_error, "{}", result.output);
        assert!(
            result.output.get("clat_truncated").is_none(),
            "verification envelope was lost: {}",
            result.output
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
        if self.inspecting {
            assert_eq!(result.output["exit_code"], 0);
            assert!(
                result.output["stdout"]
                    .as_str()
                    .unwrap()
                    .contains("preserve every source byte")
            );
            eprintln!("VERIFICATION_FOLLOWUP_VIEW {}", result.output["stdout"]);
            return ("failure inspected; no unsupported success claim", None);
        }
        let receipt = &result.output["verification_receipt"];
        eprintln!("VERIFICATION_MODEL_RECEIPT {receipt}");
        assert_eq!(receipt["status"], "failed", "{}", result.output);
        assert_eq!(receipt["failed"], 1);
        assert_eq!(receipt["exit_code"], 101);
        assert!(receipt["next_action"].as_str().unwrap().contains("Inspect"));
        assert!(
            serde_json::to_string(&result.output)
                .unwrap()
                .chars()
                .count()
                <= crate::plugins::THRESHOLD_CHARS
        );
        self.inspecting = true;
        (
            "",
            Some((
                "exec_command",
                json!({"argv":[self.program,"inspect","contract.txt"],"yield_time_ms":1000}),
            )),
        )
    }
}
impl Model for ProbeModel {
    fn provider(&self) -> &str {
        "offline-verification"
    }
    fn model_id(&self) -> &str {
        "scripted"
    }
    fn stream(
        &mut self,
        request: ModelRequest<'_>,
        _: &mut dyn crate::model::ModelEventSink,
    ) -> Result<ModelResponse, ModelError> {
        self.calls += 1;
        assert!(self.calls <= 32, "unexpected verification loop");
        let (text, call) = self.next(request);
        let calls = call
            .into_iter()
            .map(|(name, arguments)| crate::ToolCall {
                id: format!("verify-{}", self.calls),
                name: name.into(),
                arguments,
            })
            .collect::<Vec<_>>();
        Ok(ModelResponse {
            text: text.into(),
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
