//! Scripted discovery -> actual tools -> GA-2; reuses acceptance scaffolding.
use super::*;
#[derive(Default)]
struct DiscoveryState {
    stage: u8,
    id: Value,
    offset: usize,
    schema: String,
}
struct DiscoveryPrelude {
    target: &'static str,
    source: &'static str,
    state: Mutex<DiscoveryState>,
    inner: Sequence,
}
impl DiscoveryPrelude {
    fn new(target: &'static str, steps: Vec<Step>) -> Arc<Self> {
        Arc::new(Self {
            target,
            source: "native",
            state: Mutex::new(DiscoveryState::default()),
            inner: Sequence {
                step: AtomicUsize::new(0),
                steps,
            },
        })
    }
    fn call(name: &str, id: String, args: Value, events: &mut dyn ModelEventSink) -> ModelResponse {
        let call = crate::ToolCall {
            id,
            name: name.into(),
            arguments: args,
        };
        events.emit(crate::ModelEvent::ToolCallCompleted { call: call.clone() });
        let mut response = crate::test_support::response("", FinishReason::ToolCalls);
        response.tool_calls.push(call);
        response
    }
    fn previous(r: &ModelRequest<'_>, name: &str) -> Value {
        let result = r
            .items
            .iter()
            .rev()
            .find_map(|i| match i {
                ModelItem::ToolResult(t) if t.tool_name == name => Some(t),
                _ => None,
            })
            .unwrap();
        assert!(!result.is_error, "{name}: {:?}", result.output);
        result.output.clone()
    }
}
impl TestModelScript for DiscoveryPrelude {
    fn stream(
        &self,
        r: ModelRequest<'_>,
        events: &mut dyn ModelEventSink,
    ) -> Result<ModelResponse, ModelError> {
        if r.tools.is_empty() {
            return self.inner.stream(r, events);
        }
        let mut state = self.state.lock().unwrap();
        let actual = r.tools.iter().find(|t| t.name == self.target).unwrap();
        assert!(!actual.description.contains("opaquecredential123"));
        assert!(
            !actual
                .input_schema
                .to_string()
                .contains("opaquecredential123")
        );
        match state.stage {
            0 => {
                state.stage = 1;
                return Ok(Self::call(
                    "query_capabilities",
                    "ga3-query".into(),
                    json!({"name":self.target}),
                    events,
                ));
            }
            1 => {
                let page = Self::previous(&r, "query_capabilities");
                assert_eq!(page["candidates"].as_array().unwrap().len(), 1);
                let tool = &page["candidates"][0];
                assert_eq!(tool["name"], self.target);
                assert_eq!(tool["source"], self.source);
                assert_eq!(page["confirmed_path"], false);
                assert_eq!(tool["checks"]["end_to_end"], "unknown");
                state.id = tool["id"].clone();
                state.stage = 2;
            }
            2 => {
                let detail = Self::previous(&r, "describe_capability");
                assert_eq!(detail["tool"]["id"], state.id);
                assert_eq!(detail["tool"]["name"], self.target);
                state
                    .schema
                    .push_str(detail["schema"]["text"].as_str().unwrap());
                if let Some(next) = detail["schema"]["next_offset"].as_u64() {
                    state.offset = next as usize;
                } else {
                    assert_eq!(
                        serde_json::from_str::<Value>(&state.schema).unwrap(),
                        actual.input_schema
                    );
                    state.stage = 3;
                }
            }
            _ => {}
        }
        if state.stage == 2 {
            return Ok(Self::call(
                "describe_capability",
                format!("ga3-detail-{}", state.offset),
                json!({"id":state.id,"schema_offset":state.offset}),
                events,
            ));
        }
        drop(state);
        self.inner.stream(r, events)
    }
}
fn discovered_app(
    label: &str,
    script: Arc<DiscoveryPrelude>,
) -> (TrustedProjectApplication, PathBuf, PathBuf) {
    let (storage, root) = roots(label);
    std::fs::create_dir_all(&root).unwrap();
    std::fs::create_dir_all(&storage).unwrap();
    std::fs::write(root.join("input.json"), INPUT).unwrap();
    // Deterministic missing dependency, not a query-triggered probe.
    std::fs::write(
        storage.join("mcp.json"),
        json!({"missing":{"command":storage.join("absent-parser")},"unconfigured":{"command":""}})
            .to_string(),
    )
    .unwrap();
    let mut app = mount_checked(&root, &storage, TestBehavior::Scripted(script));
    configure_test_model(&app);
    app.set_permission_mode(crate::permission::PermissionMode::FullAccess)
        .unwrap();
    assert!(
        app.mcp_status
            .wait_until_settled(std::time::Duration::from_secs(5))
    );
    let status = app.capability_query(&json!({"source":"missing"})).unwrap();
    assert_eq!(status["candidates"], json!([]));
    assert_eq!(
        status["sources"]["observed"]["items"][0]["reason"],
        "missing_dependency"
    );
    let status = app
        .capability_query(&json!({"source":"unconfigured"}))
        .unwrap();
    assert_eq!(
        status["sources"]["observed"]["items"][0]["reason"],
        "missing_configuration"
    );
    app.task_create(
        "discover actual tool, produce output, independently verify".into(),
        vec![],
        budget(),
    )
    .unwrap();
    (app, root, storage)
}
#[test]
fn ga3_noncoding_discovers_text_json_candidate_then_real_outputs_and_acceptance() {
    let script = DiscoveryPrelude::new(
        "write_file",
        vec![
            Step::Call("read_file", json!({"path":"input.json"})),
            write(
                "table.json",
                &TABLE.replace(r#""total":5"#, r#""total":999"#),
            ),
            write("report.md", REPORT),
            read(),
            Step::Verify("failed"),
            Step::Candidate,
            write("table.json", TABLE),
            read(),
            Step::Verify("passed"),
            Step::Candidate,
            Step::Done,
        ],
    );
    let (mut app, root, storage) = discovered_app("ga3-report-e2e", script.clone());
    activate(&app, records_contract(true));
    let before = app.task_state().unwrap().unwrap();
    for _ in 0..3 {
        let p = app.capability_query(&json!({"name":"write_file"})).unwrap();
        assert_eq!(p["candidates"][0]["outputs"], json!(["file"]));
        assert_eq!(app.task_state().unwrap().unwrap(), before);
    }
    let r = task_request(&app);
    run(&mut app, r).unwrap();
    let done = app.task_state().unwrap().unwrap();
    assert_eq!(done.phase, "complete");
    assert_eq!(
        done.acceptance_state
            .as_ref()
            .unwrap()
            .reports
            .iter()
            .map(|r| r.status.as_str())
            .collect::<Vec<_>>(),
        ["failed", "passed"]
    );
    assert_eq!(
        done.operations.len(),
        4,
        "query/detail must not create Task operations"
    );
    assert_eq!(script.inner.step.load(Ordering::SeqCst), 11);
    assert_eq!(script.state.lock().unwrap().stage, 3);
    assert!(done.runs[0].settled && done.runs[0].tokens > 0);
    assert_eq!(
        std::fs::read_to_string(root.join("report.md")).unwrap(),
        REPORT
    );
    let old = app.capability_query(&json!({"name":"write_file"})).unwrap();
    let no_path = app
        .capability_query(&json!({"output":"xlsx","scope":"local","limit":1}))
        .unwrap();
    assert_eq!(no_path["confirmed_path"], false);
    assert!(!no_path.to_string().contains("available: xlsx"));
    app.close().unwrap();
    let mut app = crate::test_support::reopen_after_close(|| {
        Ok::<_, ApplicationError>(mount_checked(&root, &storage, TestBehavior::Success))
    });
    assert!(
        app.capability_query(&json!({"id":old["candidates"][0]["id"]}))
            .is_err()
    );
    assert_eq!(app.task_state().unwrap().unwrap(), done);
    let p = app.capability_query(&json!({"name":"write_file"})).unwrap();
    app.new_session().unwrap();
    assert!(
        app.capability_query(&json!({"id":p["candidates"][0]["id"]}))
            .is_err()
    );
    finish(app, &root);
}
#[test]
fn ga3_programming_discovers_receipt_and_runs_real_rust_then_requires_explicit_recheck() {
    let binary = if cfg!(windows) {
        "ga3-tests.exe"
    } else {
        "ga3-tests"
    };
    let executable = if cfg!(windows) {
        "ga3-tests.exe"
    } else {
        "./ga3-tests"
    };
    let code = r#"#[test] fn invariant(){let n=std::fs::read_to_string("calls").ok().and_then(|v|v.parse::<u64>().ok()).unwrap_or(0);std::fs::write("calls",(n+1).to_string()).unwrap();assert_eq!(2+3,5);}"#;
    let invoke = json!([executable]);
    let script = DiscoveryPrelude::new(
        "run_command",
        vec![
            write("case.rs", code),
            process(rust_test_argv("case.rs", binary), false),
            process(invoke.clone(), true),
            read(),
            Step::Verify("passed"),
            Step::Done,
        ],
    );
    let (mut app, root, storage) = discovered_app("ga3-rust-e2e", script.clone());
    activate(
        &app,
        AcceptanceContract {
            version: 1,
            allow_model_completion: true,
            clauses: vec![command_clause(
                "unit",
                invoke.clone(),
                "rust-test-summary",
                vec!["case.rs", binary],
            )],
        },
    );
    let r = task_request(&app);
    run(&mut app, r).unwrap();
    assert_eq!(std::fs::read_to_string(root.join("calls")).unwrap(), "1");
    let initial = app.task_state().unwrap().unwrap();
    assert_eq!(initial.operations.len(), 3);
    let old_id = app
        .capability_query(&json!({"name":"run_command"}))
        .unwrap()["candidates"][0]["id"]
        .clone();
    app.close().unwrap();
    let blocked = DiscoveryPrelude::new(
        "run_command",
        vec![process(invoke.clone(), true), read(), Step::Done],
    );
    let mut app = crate::test_support::reopen_after_close(|| {
        Ok::<_, ApplicationError>(mount_checked(
            &root,
            &storage,
            TestBehavior::Scripted(blocked.clone()),
        ))
    });
    let r = task_request(&app);
    assert!(run(&mut app, r).is_err());
    assert_eq!(blocked.inner.step.load(Ordering::SeqCst), 1);
    assert_eq!(std::fs::read_to_string(root.join("calls")).unwrap(), "1");
    assert!(app.capability_query(&json!({"id":old_id})).is_err());
    app.close().unwrap();
    let granted = DiscoveryPrelude::new(
        "run_command",
        vec![
            process(invoke, true),
            read(),
            Step::Verify("passed"),
            Step::Candidate,
            Step::Done,
        ],
    );
    let mut app = crate::test_support::reopen_after_close(|| {
        Ok::<_, ApplicationError>(mount_checked(
            &root,
            &storage,
            TestBehavior::Scripted(granted.clone()),
        ))
    });
    let s = app.task_state().unwrap().unwrap();
    app.task_authorize_recheck(&s.id, s.revision, "unit")
        .unwrap();
    let r = task_request(&app);
    run(&mut app, r).unwrap();
    let done = app.task_state().unwrap().unwrap();
    assert_eq!(done.phase, "complete");
    assert_eq!(std::fs::read_to_string(root.join("calls")).unwrap(), "2");
    assert!(done.runs.iter().all(|r| r.settled && r.tokens > 0));
    let facts: Vec<_> = done
        .operations
        .iter()
        .filter(|o| o.command_fact.is_some())
        .collect();
    assert_eq!(facts.len(), 2);
    assert_ne!(facts[0].id, facts[1].id);
    assert!(
        done.acceptance_state.as_ref().unwrap().grants[0]
            .consumed_by
            .is_some()
    );
    finish(app, &root);
}

#[test]
fn ga3_ready_host_handler_query_never_preapproves_and_denial_executes_nothing() {
    let script = DiscoveryPrelude::new(
        "write_file",
        vec![
            Step::Reject(
                "write_file",
                json!({"path":"denied.txt","content":"must not write"}),
            ),
            read(),
            Step::Done,
        ],
    );
    let (mut app, root, storage) = discovered_app("ga3-deny", script.clone());
    app.set_permission_mode(crate::permission::PermissionMode::ReadOnly)
        .unwrap();
    struct Deny {
        calls: Arc<AtomicUsize>,
        script: Arc<DiscoveryPrelude>,
    }
    impl crate::permission::PermissionApprover for Deny {
        fn decide(
            &self,
            _: crate::permission::PermissionRequest,
            _: &crate::CancelToken,
        ) -> crate::permission::PermissionDecision {
            assert_eq!(
                self.script.state.lock().unwrap().stage,
                3,
                "query and every detail page must bypass approver"
            );
            self.calls.fetch_add(1, Ordering::SeqCst);
            crate::permission::PermissionDecision::Deny {
                reason: "test user denial".into(),
            }
        }
    }
    let calls = Arc::new(AtomicUsize::new(0));
    let page = app.capability_query(&json!({"name":"write_file"})).unwrap();
    assert!(
        page["candidates"][0]["readiness"]
            .as_str()
            .unwrap()
            .contains("available: host handler only")
    );
    let (tx, rx) = mpsc::channel();
    let resume = task_request(&app);
    let (h, _) = app
        .start_task_run(
            resume,
            ApplicationRunRequest {
                message: crate::message::PendingMessage::text("continue"),
                asker: None,
                approver: Arc::new(Deny {
                    calls: calls.clone(),
                    script,
                }),
                events: Box::new(crate::test_support::SharedEvents(Arc::new(Mutex::new(
                    vec![],
                )))),
                completion: tx,
            },
        )
        .unwrap();
    rx.recv_timeout(std::time::Duration::from_secs(20))
        .unwrap()
        .unwrap();
    h.join().unwrap();
    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "replay: {:?}",
        crate::test_support::load_replay(&storage)
    );
    assert!(!root.join("denied.txt").exists());
    let s = app.task_state().unwrap().unwrap();
    assert_eq!(
        s.operations.len(),
        0,
        "permission denial precedes Task intent; discovery must not create one either"
    );
    assert_ne!(s.phase, "complete");
    finish(app, &root);
}

#[test]
fn ga3_in_run_compaction_queries_current_view_again_without_reviving_history_authority() {
    struct Requery {
        inner: super::super::task_recovery_tests::CompactTask,
        observed: AtomicUsize,
        pending: std::sync::atomic::AtomicBool,
        queries: AtomicUsize,
    }
    impl TestModelScript for Requery {
        fn stream(
            &self,
            r: ModelRequest<'_>,
            events: &mut dyn ModelEventSink,
        ) -> Result<ModelResponse, ModelError> {
            if r.tools.is_empty() {
                return self.inner.stream(r, events);
            }
            if self.pending.swap(false, Ordering::SeqCst) {
                let p = DiscoveryPrelude::previous(&r, "query_capabilities");
                assert_eq!(p["domain"], "active-run");
                let d = r.tools.iter().find(|d| d.name == "read_file").unwrap();
                assert_eq!(p["candidates"][0]["name"], d.name);
                assert!(!p["candidates"][0]["id"].is_null());
            }
            let generation = self.inner.summaries.load(Ordering::SeqCst) + 1;
            if self.observed.swap(generation, Ordering::SeqCst) != generation {
                let n = self.queries.fetch_add(1, Ordering::SeqCst);
                assert!(n < 20);
                self.pending.store(true, Ordering::SeqCst);
                return Ok(DiscoveryPrelude::call(
                    "query_capabilities",
                    format!("ga3-requery-{n}"),
                    json!({"name":"read_file"}),
                    events,
                ));
            }
            self.inner.stream(r, events)
        }
    }
    let script = Arc::new(Requery {
        inner: super::super::task_recovery_tests::CompactTask {
            main: AtomicUsize::new(0),
            summaries: AtomicUsize::new(0),
            expected_items: Default::default(),
        },
        observed: AtomicUsize::new(0),
        pending: Default::default(),
        queries: AtomicUsize::new(0),
    });
    let (storage, root) = roots("ga3-compaction");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("observations.txt"), "observation\n".repeat(1300)).unwrap();
    let mut app = mount_checked(&root, &storage, TestBehavior::Scripted(script.clone()));
    configure_test_model(&app);
    let s = app
        .task_create("query after compaction".into(), vec![], budget())
        .unwrap();
    app.task_save(
        &s.id,
        s.revision,
        crate::task::Progress {
            summary: "do not assume reviewed".into(),
            ..Default::default()
        },
    )
    .unwrap();
    crate::test_support::configure_test_model_with_budget(&app, u32::MAX);
    let fixed = app.context_snapshot().unwrap().total_estimate + 2000;
    crate::test_support::configure_test_model_with_budget(
        &app,
        u32::try_from((fixed + 6000) * 5 / 4).unwrap(),
    );
    let request = task_request(&app);
    run(&mut app, request).unwrap();
    assert!(script.inner.summaries.load(Ordering::SeqCst) > 0);
    assert!(script.queries.load(Ordering::SeqCst) > 1);
    let done = app.task_state().unwrap().unwrap();
    assert_eq!(done.operations.len(), 6);
    assert!(done.runs[0].settled && done.runs[0].tokens > 0);
    assert!(
        app.tools
            .query_capabilities(&json!({"name":"read_file"}), &crate::CancelToken::new())
            .is_err()
    );
    finish(app, &root);
}

fn user_schema(app: &TrustedProjectApplication, name: &str, secret: &str) -> String {
    let page = app.capability_query(&json!({"name":name})).unwrap();
    let mut offset = 0;
    let mut idle_schema = String::new();
    loop {
        let detail = app
            .capability_query(&json!({"id":page["candidates"][0]["id"],"schema_offset":offset}))
            .unwrap();
        assert!(!detail.to_string().contains(secret));
        idle_schema.push_str(detail["schema"]["text"].as_str().unwrap());
        if detail["schema"]["next_offset"].is_null() {
            break;
        }
        let next = detail["schema"]["next_offset"].as_u64().unwrap();
        assert!(next > offset && next < 65536);
        offset = next;
    }
    idle_schema
}

#[test]
fn ga3_rework_schema_publication_is_shared_by_idle_model_results_and_journal() {
    struct LegacySchema;
    impl crate::Tool for LegacySchema {
        fn definition(&self) -> crate::ToolDefinition {
            crate::ToolDefinition {
                name: "legacy_schema".into(),
                description: "fixture".into(),
                input_schema: json!({"type":"object","description":format!("{}API_KEY=supersecretvalue", "中文".repeat(1800))}),
                effect: crate::ToolEffect::Pure,
                strict: false,
            }
        }
        fn capability_metadata(&self) -> crate::ToolCapability {
            crate::ToolCapability::native(&[], &[], &[], "fixture")
        }
        fn invoke(
            &self,
            _: &Value,
            _: &crate::Project,
            _: &crate::CancelToken,
        ) -> Result<Value, crate::ToolError> {
            panic!("schema disclosure must never execute the tool")
        }
    }
    let script = DiscoveryPrelude::new("legacy_schema", vec![read(), Step::Done]);
    let (mut app, root, storage) = discovered_app("ga3-schema-journal", script.clone());
    app.tools.reopen_for_plugin_reload().unwrap();
    let _lease = app
        .tools
        .register(
            crate::plugin::PluginOwner::for_test(crate::plugin::PluginId::new("fixture.schema")),
            Arc::new(LegacySchema),
        )
        .unwrap();
    app.tools.freeze().unwrap();
    let idle_schema = user_schema(&app, "legacy_schema", "supersecretvalue");
    let request = task_request(&app);
    run(&mut app, request).unwrap();
    let model_schema = script.state.lock().unwrap().schema.clone();
    assert_eq!(idle_schema, model_schema);
    assert!(model_schema.contains("[REDACTED]"));
    assert!(!model_schema.contains("supersecretvalue"));
    app.close().unwrap();
    let replay = format!("{:?}", crate::test_support::load_replay(&storage));
    assert!(replay.contains("describe_capability"));
    assert!(replay.contains("[REDACTED]"));
    assert!(!replay.contains("supersecretvalue"));
    crate::test_support::cleanup_tree(root.parent().unwrap());
}

#[test]
#[ignore = "local python3 fixture; run explicitly with --ignored"]
fn ga3_mcp_env_credential_is_private_in_user_model_chunks_and_journal() {
    let (storage, root) = roots("ga3-env-schema-journal");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::create_dir_all(&storage).unwrap();
    let requests = root.join("requests");
    std::fs::write(
        storage.join("mcp.json"),
        crate::test_support::mcp_discovery::config(&requests, true).to_string(),
    )
    .unwrap();
    let mut script = DiscoveryPrelude::new("mcp_fixture_echo", vec![read(), Step::Done]);
    Arc::get_mut(&mut script).unwrap().source = "mcp";
    let mut app = mount_checked(&root, &storage, TestBehavior::Scripted(script.clone()));
    configure_test_model(&app);
    assert!(
        app.mcp_status
            .wait_until_settled(std::time::Duration::from_secs(15))
    );
    assert_eq!(app.mcp_status.snapshot().connected, 1);
    app.task_create("inspect actual MCP public schema".into(), vec![], budget())
        .unwrap();
    let before = std::fs::read(&requests).unwrap();
    let idle_schema = user_schema(&app, "mcp_fixture_echo", "opaquecredential123");
    let definitions = app.tools.definitions();
    let definition = definitions
        .iter()
        .find(|d| d.name == "mcp_fixture_echo")
        .unwrap();
    assert!(!definition.description.contains("opaquecredential123"));
    assert!(
        !definition
            .input_schema
            .to_string()
            .contains("opaquecredential123")
    );
    assert!(app.tools.get("mcp_fixture_unsafe_env").is_none());
    assert_eq!(
        serde_json::from_str::<Value>(&idle_schema).unwrap(),
        definition.input_schema
    );
    let request = task_request(&app);
    run(&mut app, request).unwrap();
    let model_schema = script.state.lock().unwrap().schema.clone();
    assert_eq!(model_schema, idle_schema);
    assert!(model_schema.contains("[REDACTED]"));
    assert!(!model_schema.contains("opaquecredential123"));
    assert!(!model_schema.contains("raw-private-config-value"));
    assert_eq!(
        std::fs::read(&requests).unwrap(),
        before,
        "discovery sends zero MCP messages"
    );
    app.close().unwrap();
    let replay = format!("{:?}", crate::test_support::load_replay(&storage));
    assert!(replay.contains("describe_capability"));
    assert!(replay.contains("[REDACTED]"));
    assert!(!replay.contains("opaquecredential123"));
    assert!(!replay.contains("raw-private-config-value"));
    crate::test_support::cleanup_tree(root.parent().unwrap());
}
