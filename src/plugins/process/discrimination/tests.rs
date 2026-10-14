use super::*;
use crate::model::{FinishReason, Model, ModelError, ModelItem, ModelRequest, ModelResponse};
use crate::permission::AllowAll;
use crate::plugin::{PluginManager, ScopeKind};
use crate::plugins::ToolRegistryPlugin;
use crate::plugins::services::TOOL_SERVICE;
use crate::tool::ToolCall;

struct Fixture {
    root: std::path::PathBuf,
    manager: PluginManager,
    generation: u64,
    service: Arc<ProcessService>,
}
impl Fixture {
    fn new(test: &str) -> Self {
        let root = crate::plugins::process::sbx4_tests::root();
        for (dir, value) in [("candidate", 7), ("counterexample", 0)] {
            let src = root.join(dir).join("src");
            std::fs::create_dir_all(&src).unwrap();
            std::fs::write(root.join(dir).join("Cargo.toml"),"[package]\nname='discrimination_fixture'\nversion='0.1.0'\nedition='2021'\n[workspace]\n").unwrap();
            std::fs::write(root.join(dir).join("Cargo.lock"), "version = 3\n[[package]]\nname = \"discrimination_fixture\"\nversion = \"0.1.0\"\n").unwrap();
            std::fs::write(
                src.join("api.rs"),
                format!("pub fn live() -> i32 {{ {value} }}\npub fn legacy() -> i32 {{ 7 }}\n"),
            )
            .unwrap();
            std::fs::write(
                src.join("lib.rs"),
                format!("mod api;\n#[test]\nfn contract() {{ {test} }}\n"),
            )
            .unwrap();
        }
        let mut manager = PluginManager::root(ScopeKind::TrustedProject);
        manager
            .mount_all(vec![
                Arc::new(ToolRegistryPlugin),
                Arc::new(SandboxPlugin {
                    project: Project::new(&root),
                    permission_mode: Some(Arc::new(RwLock::new(PermissionMode::FullAccess))),
                }),
                Arc::new(ProcessServicePlugin {
                    project: Project::new(&root),
                }),
                Arc::new(ExecToolsPlugin),
            ])
            .unwrap();
        let service = manager.require(PROCESS_SERVICE).unwrap();
        let generation = service
            .bind_run("discrimination", CancelToken::new())
            .unwrap();
        Self {
            root,
            manager,
            generation,
            service,
        }
    }
    fn args(&self) -> Value {
        json!({"candidate_dir":"candidate","counterexample_dir":"counterexample",
            "package":"discrimination_fixture","test_filter":"contract",
            "test_files":["src/lib.rs"],"production_files":["src/api.rs"],"sandbox":"auto"})
    }
    fn invoke(&self, args: &Value) -> Result<Value, ToolError> {
        let tools = self.manager.require(TOOL_SERVICE).unwrap();
        let tool = tools
            .get("test_discrimination")
            .expect("native exec plugin must expose experiment");
        tool.invoke(args, &Project::new(&self.root), &CancelToken::new())
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.service.unbind_run(self.generation).unwrap();
        self.manager.close().unwrap();
        crate::test_support::cleanup_tree(&self.root);
    }
}

#[test]
fn discrimination_detects_stale_test_target_via_real_agent_loop() {
    let f = Fixture::new("assert_eq!(api::legacy(),7);");
    let tools = f.manager.require(TOOL_SERVICE).unwrap();
    let mut model = ProbeModel {
        calls: 0,
        args: f.args(),
    };
    let mut events = Vec::new();
    let output = crate::run::Run::new(&mut model, &tools, &AllowAll, &Project::new(&f.root))
        .execute_with_items(
            vec![ModelItem::user_text("Verify the changed live API contract")],
            crate::message::MessageContent::text("Verify the changed live API contract"),
            None,
            &mut events,
        )
        .unwrap();
    assert_eq!(
        output.text,
        "inspect production entry instead of claiming coverage"
    );
    assert_eq!(model.calls, 3, "tool must not add model requests itself");
}
struct ProbeModel {
    calls: usize,
    args: Value,
}
impl Model for ProbeModel {
    fn provider(&self) -> &str {
        "scripted-discrimination"
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
        let (text, calls) = match self.calls {
            1 => (
                "",
                vec![ToolCall {
                    id: "paired".into(),
                    name: if r.tools.iter().any(|d| d.name == "test_discrimination") {
                        "test_discrimination"
                    } else {
                        "run_command"
                    }
                    .into(),
                    arguments: if r.tools.iter().any(|d| d.name == "test_discrimination") {
                        self.args.clone()
                    } else {
                        json!({"command":"cd candidate && cargo test --offline --locked --lib contract -- --exact"})
                    },
                }],
            ),
            2 => {
                let Some(ModelItem::ToolResult(result)) = r.items.last() else {
                    panic!("missing real tool result")
                };
                assert!(!result.is_error, "{}", result.output);
                assert_eq!(
                    result.output["status"], "not_discriminating",
                    "{}",
                    result.output
                );
                assert_eq!(result.output["candidate"]["exit_code"], 0);
                assert_eq!(result.output["counterexample"]["exit_code"], 0);
                (
                    "",
                    vec![ToolCall {
                        id: "inspect".into(),
                        name: "run_command".into(),
                        arguments: json!({"command":"cat candidate/src/lib.rs"}),
                    }],
                )
            }
            _ => {
                let Some(ModelItem::ToolResult(result)) = r.items.last() else {
                    panic!("missing follow-up read")
                };
                assert!(
                    result.output["stdout"]
                        .as_str()
                        .unwrap()
                        .contains("api::legacy()")
                );
                (
                    "inspect production entry instead of claiming coverage",
                    vec![],
                )
            }
        };
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
#[test]
fn discrimination_accepts_effective_test_and_keeps_source_and_journal_clean() {
    let f = Fixture::new("assert_eq!(api::live(),7);");
    let source = f.root.join("counterexample/src/api.rs");
    let before = std::fs::read(&source).unwrap();
    let output = f.invoke(&f.args()).unwrap();
    assert_eq!(
        output["status"], "discriminates_declared_defect",
        "{output}"
    );
    assert_eq!(output["counterexample"]["exit_code"], 101);
    assert_eq!(std::fs::read(source).unwrap(), before);
    let tool = f
        .manager
        .require(TOOL_SERVICE)
        .unwrap()
        .get("test_discrimination")
        .unwrap();
    let durable = tool.journal_output(&output);
    assert!(durable["candidate"].get("stdout").is_none());
    assert!(durable["counterexample"].get("stderr").is_none());
    assert_eq!(durable["status"], output["status"]);
}
#[test]
fn discrimination_zero_filter_is_inconclusive_and_does_not_start_control() {
    let f = Fixture::new("assert_eq!(api::live(),7);");
    let mut args = f.args();
    args["test_filter"] = json!("missing_test");
    let out = f.invoke(&args).unwrap();
    assert_eq!(out["status"], "inconclusive");
    assert!(out["counterexample"].is_null());
    assert!(!f.root.join("counterexample/target").exists());
}
#[test]
fn discrimination_refuses_changed_tests_aliases_and_changed_prepared_inputs() {
    let f = Fixture::new("assert_eq!(api::live(),7);");
    let args = f.args();
    let tool = f
        .manager
        .require(TOOL_SERVICE)
        .unwrap()
        .get("test_discrimination")
        .unwrap();
    let prepared = tool.prepare_execution(&args).unwrap();
    std::fs::write(
        f.root.join("counterexample/src/api.rs"),
        "pub fn live()->i32{9}\n",
    )
    .unwrap();
    assert!(
        prepared
            .tool
            .invoke(&args, &Project::new(&f.root), &CancelToken::new())
            .unwrap_err()
            .to_string()
            .contains("changed after preparation")
    );
    assert!(
        !f.root.join("candidate/target").exists(),
        "all declared inputs must be checked before either execution"
    );
    let mut same = args.clone();
    same["counterexample_dir"] = json!("candidate");
    assert!(f.invoke(&same).is_err());
    let mut injected = args.clone();
    injected["test_filter"] = json!("contract; touch marker");
    assert!(f.invoke(&injected).is_err());
    std::fs::write(
        f.root.join("counterexample/src/lib.rs"),
        "#[test] fn other(){}\n",
    )
    .unwrap();
    assert!(f.invoke(&args).is_err());
}
#[test]
fn discrimination_respects_denial_without_executing_either_copy() {
    let f = Fixture::new("assert_eq!(api::live(),7);");
    let tools = f.manager.require(TOOL_SERVICE).unwrap();
    struct DeniedModel {
        calls: usize,
        args: Value,
    }
    impl Model for DeniedModel {
        fn provider(&self) -> &str {
            "scripted"
        }
        fn model_id(&self) -> &str {
            "denied"
        }
        fn stream(
            &mut self,
            r: ModelRequest<'_>,
            _: &mut dyn crate::model::ModelEventSink,
        ) -> Result<ModelResponse, ModelError> {
            self.calls += 1;
            let calls = if self.calls == 1 {
                vec![ToolCall {
                    id: "denied".into(),
                    name: "test_discrimination".into(),
                    arguments: self.args.clone(),
                }]
            } else {
                let Some(ModelItem::ToolResult(result)) = r.items.last() else {
                    panic!("result")
                };
                assert!(result.is_error);
                vec![]
            };
            Ok(ModelResponse {
                text: "done".into(),
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
    let mut model = DeniedModel {
        calls: 0,
        args: f.args(),
    };
    crate::run::Run::new(
        &mut model,
        &tools,
        &crate::permission::SafeByDefault,
        &Project::new(&f.root),
    )
    .execute_with_items(
        vec![ModelItem::user_text("verify")],
        crate::message::MessageContent::text("verify"),
        None,
        &mut Vec::new(),
    )
    .unwrap_err();
    assert!(!f.root.join("candidate/target").exists());
    assert!(!f.root.join("counterexample/target").exists());
}

#[test]
fn discrimination_compile_failure_is_not_a_counterexample_test_failure() {
    let f = Fixture::new("assert_eq!(api::live(),7);");
    std::fs::write(f.root.join("counterexample/src/api.rs"), "not Rust source").unwrap();
    let out = f.invoke(&f.args()).unwrap();
    assert_eq!(out["status"], "inconclusive");
    assert_eq!(out["counterexample"]["exit_code"], 101);
    assert_eq!(out["counterexample"]["test_summaries"], json!([]));
}

#[test]
fn discrimination_cancelled_call_does_not_execute_and_prepared_call_is_single_use() {
    let f = Fixture::new("assert_eq!(api::live(),7);");
    let tool = f
        .manager
        .require(TOOL_SERVICE)
        .unwrap()
        .get("test_discrimination")
        .unwrap();
    let prepared = tool.prepare_execution(&f.args()).unwrap();
    let cancel = CancelToken::new();
    cancel.cancel();
    assert!(
        prepared
            .tool
            .invoke(&f.args(), &Project::new(&f.root), &cancel)
            .unwrap_err()
            .to_string()
            .contains("cancelled")
    );
    assert!(!f.root.join("candidate/target").exists());
    assert!(!f.root.join("counterexample/target").exists());
    assert!(
        prepared
            .tool
            .invoke(&f.args(), &Project::new(&f.root), &CancelToken::new())
            .unwrap_err()
            .to_string()
            .contains("consumed")
    );
}

#[test]
fn discrimination_long_diagnostics_keep_model_evidence_below_pruner_threshold() {
    let f =
        Fixture::new("eprintln!(\"{}\", \"\\u{0000}\".repeat(2000)); assert_eq!(api::live(),7);");
    let out = f.invoke(&f.args()).unwrap();
    assert_eq!(out["status"], "discriminates_declared_defect", "{out}");
    assert_eq!(out["counterexample"]["diagnostic_excerpt_truncated"], true);
    assert!(serde_json::to_string(&out).unwrap().chars().count() < 8192);
}
