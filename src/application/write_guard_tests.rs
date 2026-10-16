//! Production catalog → permission → file tools → pruner → next ModelRequest.
use super::*;
use crate::model::{
    FinishReason, ModelError, ModelEventSink, ModelItem, ModelRequest, ModelResponse,
};

const ORIGINAL: &str = "// existing tests must survive\nfn before() {}\n// append here\n";
const EXTRA: &str = "fn added() {}\n";

struct AppendRecoveryScript;

pub(super) fn response(tool: Option<(&str, serde_json::Value)>, text: &str) -> ModelResponse {
    let has_tool = tool.is_some();
    ModelResponse {
        text: text.into(),
        tool_calls: tool
            .map(|(name, arguments)| {
                vec![crate::ToolCall {
                    id: format!("guard-{name}"),
                    name: name.into(),
                    arguments,
                }]
            })
            .unwrap_or_default(),
        finish_reason: if has_tool {
            FinishReason::ToolCalls
        } else {
            FinishReason::Completed
        },
        usage: None,
        provider_response_id: None,
        provider_state: Vec::new(),
        reasoning: None,
    }
}

impl TestModelScript for AppendRecoveryScript {
    fn stream(
        &self,
        request: ModelRequest<'_>,
        _events: &mut dyn ModelEventSink,
    ) -> Result<ModelResponse, ModelError> {
        let last = request.items.iter().rev().find_map(|item| match item {
            ModelItem::ToolResult(result) => Some(result),
            _ => None,
        });
        match last {
            None => Ok(response(
                Some(("write_file", json!({"path":"suite.rs","content":EXTRA}))),
                "",
            )),
            Some(result) if result.tool_name == "write_file" && result.is_error => {
                let output = result.output.to_string();
                assert!(output.contains("overwrite") && output.contains("edit_file"));
                assert!(!output.contains("clat_truncated") && output.len() < 2048);
                let schema = request
                    .tools
                    .iter()
                    .find(|tool| tool.name == "write_file")
                    .unwrap();
                assert_eq!(
                    schema.input_schema["properties"]["overwrite"]["type"],
                    "boolean"
                );
                Ok(response(
                    Some((
                        "edit_file",
                        json!({"path":"suite.rs","old_str":"// append here\n","new_str":format!("// append here\n{EXTRA}")}),
                    )),
                    "",
                ))
            }
            Some(result) if result.tool_name == "edit_file" => {
                assert!(
                    !result.is_error,
                    "the precise recovery must succeed: {result:?}"
                );
                Ok(response(None, "recovered with precise edit"))
            }
            Some(_) => Ok(response(None, "ambiguous write succeeded without recovery")),
        }
    }
}

#[test]
fn write_guard_application_loop_preserves_old_tests_and_recovers_with_edit() {
    let (storage, root) = roots("ag4-write-guard-loop");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("suite.rs"), ORIGINAL).unwrap();
    let mut app = mount(
        &Project::new(&root),
        &storage,
        TestBehavior::Scripted(Arc::new(AppendRecoveryScript)),
    );
    configure_test_model(&app);
    let done = run(&mut app, "append the additional test to suite.rs").unwrap();
    app.close().unwrap();
    assert_eq!(
        std::fs::read_to_string(root.join("suite.rs")).unwrap(),
        format!("{ORIGINAL}{EXTRA}"),
        "the final artifact must preserve existing tests and add the requested test"
    );
    assert_eq!(done.output, "recovered with precise edit");
    let events = load_events(&storage);
    let calls: Vec<_> = events
        .iter()
        .filter(|e| e.event_type == "tool/call")
        .map(|e| e.data["name"].as_str().unwrap())
        .collect();
    assert_eq!(calls, ["write_file", "edit_file"]);
    let results: Vec<_> = events
        .iter()
        .filter(|e| e.event_type == "tool/result")
        .collect();
    assert_eq!(results.len(), 2);
    assert_eq!(results[0].data["message"]["isError"], true);
    assert!(
        results[0].data.to_string().len() < 3000,
        "bounded error remains bounded in journal"
    );
    crate::test_support::cleanup_tree(&storage);
    crate::test_support::cleanup_tree(&root);
}

struct ExplicitWriteScript {
    overwrite: bool,
    expect_error: bool,
    requests: std::sync::atomic::AtomicUsize,
}
impl TestModelScript for ExplicitWriteScript {
    fn stream(
        &self,
        request: ModelRequest<'_>,
        _events: &mut dyn ModelEventSink,
    ) -> Result<ModelResponse, ModelError> {
        let n = self
            .requests
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        assert!(n < 2, "a normal write needs exactly one tool round-trip");
        if let Some(result) = request.items.iter().rev().find_map(|item| match item {
            ModelItem::ToolResult(result) => Some(result),
            _ => None,
        }) {
            assert_eq!(result.is_error, self.expect_error);
            Ok(response(None, "write processed"))
        } else {
            let mut arguments = json!({"path":"suite.rs","content":"replacement"});
            if self.overwrite {
                arguments["overwrite"] = json!(true);
            }
            Ok(response(Some(("write_file", arguments)), ""))
        }
    }
}

#[test]
fn write_guard_explicit_overwrite_still_obeys_production_permission_denial() {
    let (storage, root) = roots("ag4-write-guard-denied");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("suite.rs"), ORIGINAL).unwrap();
    let script = Arc::new(ExplicitWriteScript {
        overwrite: true,
        expect_error: true,
        requests: Default::default(),
    });
    let mut app = mount(
        &Project::new(&root),
        &storage,
        TestBehavior::Scripted(script.clone()),
    );
    configure_test_model(&app);
    let done = run_with_approver(
        &mut app,
        "replace suite.rs",
        Arc::new(
            |_request: crate::PermissionRequest, _cancel: &crate::model::CancelToken| {
                crate::PermissionDecision::Deny {
                    reason: "write not authorized".into(),
                }
            },
        ),
    )
    .unwrap();
    app.close().unwrap();
    assert_eq!(done.output, "write processed");
    assert_eq!(script.requests.load(std::sync::atomic::Ordering::SeqCst), 2);
    assert_eq!(
        std::fs::read_to_string(root.join("suite.rs")).unwrap(),
        ORIGINAL
    );
    assert!(
        !load_events(&storage)
            .iter()
            .any(|event| event.event_type == "tool/call"),
        "permission rejection precedes invocation"
    );
    crate::test_support::cleanup_tree(&storage);
    crate::test_support::cleanup_tree(&root);
}

#[test]
fn write_guard_normal_creation_and_explicit_replacement_need_no_extra_model_turn() {
    for overwrite in [false, true] {
        let (storage, root) = roots("ag4-write-guard-normal");
        std::fs::create_dir_all(&root).unwrap();
        if overwrite {
            std::fs::write(root.join("suite.rs"), ORIGINAL).unwrap();
        }
        let script = Arc::new(ExplicitWriteScript {
            overwrite,
            expect_error: false,
            requests: Default::default(),
        });
        let mut app = mount(
            &Project::new(&root),
            &storage,
            TestBehavior::Scripted(script.clone()),
        );
        configure_test_model(&app);
        assert_eq!(
            run(&mut app, "write the complete file").unwrap().output,
            "write processed"
        );
        app.close().unwrap();
        assert_eq!(script.requests.load(std::sync::atomic::Ordering::SeqCst), 2);
        assert_eq!(
            std::fs::read_to_string(root.join("suite.rs")).unwrap(),
            "replacement"
        );
        crate::test_support::cleanup_tree(&storage);
        crate::test_support::cleanup_tree(&root);
    }
}
