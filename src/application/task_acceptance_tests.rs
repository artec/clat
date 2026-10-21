use super::task_recovery_tests::{budget, run, task_request};
use super::*;
use crate::model::{
    FinishReason, ModelError, ModelEventSink, ModelItem, ModelRequest, ModelResponse,
};
use crate::task::{AcceptanceContract, Clause};
use crate::test_support::{TestBehavior, TestModelScript, configure_test_model, roots};
use serde::Deserialize;
use serde_json::json;
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicUsize, Ordering};
const INPUT: &str = r#"[{"name":"alpha","amount":2},{"name":"beta","amount":3}]"#;
const TABLE: &str =
    r#"{"rows":[{"name":"alpha","amount":2},{"name":"beta","amount":3}],"row_count":2,"total":5}"#;
const REPORT: &str = "# Report\n## Results\nTotal: 5\n";
#[derive(Clone)]
enum Step {
    Call(&'static str, Value),
    Verify(&'static str),
    Candidate,
    Poll,
    ForgeProgress,
    Reject(&'static str, Value),
    Done,
    Cancel,
    Panic,
}
struct Sequence {
    step: AtomicUsize,
    steps: Vec<Step>,
}
impl TestModelScript for Sequence {
    fn stream(
        &self,
        r: ModelRequest<'_>,
        events: &mut dyn ModelEventSink,
    ) -> Result<ModelResponse, ModelError> {
        if r.tools.is_empty() {
            return Ok(crate::test_support::response(
                "title",
                FinishReason::Completed,
            ));
        }
        let i = self.step.fetch_add(1, Ordering::SeqCst);
        let step = self
            .steps
            .get(i)
            .unwrap_or_else(|| panic!("unexpected acceptance model step {i}"));
        if i > 0
            && let Step::Verify(expected) = &self.steps[i - 1]
        {
            let s = latest_state(&r);
            if *expected != "any" {
                assert_eq!(
                    s.acceptance_state
                        .as_ref()
                        .unwrap()
                        .reports
                        .last()
                        .unwrap()
                        .status,
                    *expected,
                    "report={:?}; facts={:?}",
                    s.acceptance_state.as_ref().unwrap().reports.last(),
                    s.operations
                        .iter()
                        .filter_map(|o| o.command_fact.as_ref())
                        .collect::<Vec<_>>()
                );
            }
        }
        if i > 0 {
            let previous = r
                .items
                .iter()
                .rev()
                .find_map(|item| match item {
                    ModelItem::ToolResult(result)
                        if result.call_id == format!("acceptance-{}", i - 1) =>
                    {
                        Some(result)
                    }
                    _ => None,
                })
                .expect("previous tool result");
            match &self.steps[i - 1] {
                Step::Call(_, _) | Step::Verify(_) | Step::Poll => assert!(
                    !previous.is_error,
                    "unexpected tool error: {:?}",
                    previous.output
                ),
                Step::ForgeProgress | Step::Reject(_, _) => assert!(
                    previous.is_error,
                    "forged acceptance authority was accepted: {:?}",
                    previous.output
                ),
                Step::Candidate => {
                    let state = latest_state(&r);
                    let passed = state
                        .acceptance_state
                        .as_ref()
                        .unwrap()
                        .reports
                        .last()
                        .unwrap()
                        .status
                        == "passed";
                    assert_eq!(
                        previous.is_error, !passed,
                        "failed acceptance must reject candidate; passed candidate must remain pending: {:?}",
                        previous.output
                    );
                }
                Step::Done | Step::Cancel | Step::Panic => unreachable!(),
            }
        }
        let (name, args) = match step {
            Step::Call(name, args) => (*name, args.clone()),
            Step::Poll => {
                let id = r
                    .items
                    .iter()
                    .rev()
                    .find_map(|i| match i {
                        ModelItem::ToolResult(t) if t.tool_name == "exec_command" => {
                            t.output["session_id"].as_u64()
                        }
                        _ => None,
                    })
                    .expect("actual native exec session");
                (
                    "write_stdin",
                    json!({"session_id":id,"chars":"","yield_time_ms":1000}),
                )
            }
            Step::Reject(name, args) => (*name, args.clone()),
            Step::ForgeProgress => {
                let s = latest_state(&r);
                (
                    "update_task",
                    json!({"task_id":s.id,"expected_revision":s.revision,"progress":{},"acceptance_state":{"contract":{"version":999,"allow_model_completion":true,"clauses":[]},"reports":[{"status":"passed"}]}}),
                )
            }
            Step::Verify(expected) => {
                let s = latest_state(&r);
                assert_ne!(s.phase, "complete");
                if *expected != "any" {
                    assert!(matches!(*expected, "failed" | "passed" | "inconclusive"));
                }
                let evidence = command_refs(&s);
                (
                    "verify_task_acceptance",
                    json!({"task_id":s.id,"expected_revision":s.revision,"evidence":evidence}),
                )
            }
            Step::Candidate => {
                let s = latest_state(&r);
                let a = s.acceptance_state.as_ref().unwrap();
                (
                    "propose_task_completion",
                    json!({"task_id":s.id,"expected_revision":s.revision,"report_id":a.reports.last().unwrap().id}),
                )
            }
            Step::Cancel => {
                return Ok(crate::test_support::response(
                    "cancelled",
                    FinishReason::Cancelled,
                ));
            }
            Step::Panic => panic!("injected acceptance final model panic"),
            Step::Done => {
                assert_ne!(
                    latest_state(&r).phase,
                    "complete",
                    "model must only observe pending completion inside the Run"
                );
                return Ok(crate::test_support::response(
                    "candidate submitted; final gate remains pending",
                    FinishReason::Completed,
                ));
            }
        };
        let call = crate::ToolCall {
            id: format!("acceptance-{i}"),
            name: name.into(),
            arguments: args,
        };
        events.emit(crate::ModelEvent::ToolCallCompleted { call: call.clone() });
        let mut response = crate::test_support::response("", FinishReason::ToolCalls);
        response.tool_calls.push(call);
        Ok(response)
    }
}
#[derive(Deserialize)]
struct ModelView {
    id: String,
    revision: u64,
    phase: String,
    operations: Vec<ModelOperation>,
    acceptance_state: Option<ModelAcceptance>,
}
#[derive(Deserialize)]
struct ModelOperation {
    id: String,
    command_fact: Option<ModelCommand>,
}
#[derive(Deserialize, Debug)]
struct ModelCommand {
    clauses: Vec<String>,
}
#[derive(Deserialize)]
struct ModelAcceptance {
    reports: Vec<ModelReport>,
}
#[derive(Deserialize, Debug)]
struct ModelReport {
    id: String,
    status: String,
}
fn latest_state(r: &ModelRequest<'_>) -> ModelView {
    r.items
        .iter()
        .rev()
        .find_map(|item| match item {
            ModelItem::ToolResult(result)
                if !result.is_error
                    && matches!(
                        result.tool_name.as_str(),
                        "read_task" | "verify_task_acceptance" | "propose_task_completion"
                    ) =>
            {
                serde_json::from_value(result.output.clone()).ok()
            }
            _ => None,
        })
        .expect("a real task-tool state is required")
}
fn command_refs(s: &ModelView) -> BTreeMap<String, String> {
    let mut refs = BTreeMap::new();
    for op in &s.operations {
        if let Some(f) = &op.command_fact {
            for clause in &f.clauses {
                refs.insert(clause.clone(), op.id.clone());
            }
        }
    }
    refs
}
pub(super) fn mount_checked(
    root: &std::path::Path,
    storage: &std::path::Path,
    behavior: TestBehavior,
) -> TrustedProjectApplication {
    BootstrapApplication::open(crate::Project::new(root), storage.into())
        .unwrap()
        .with_permission_modes()
        .authorize_and_mount_with_provider(Arc::new(crate::test_support::TestProviderPlugin {
            behavior,
        }))
        .unwrap()
}
fn fresh(
    label: &str,
    steps: Vec<Step>,
) -> (TrustedProjectApplication, Arc<Sequence>, PathBuf, PathBuf) {
    let (storage, root) = roots(label);
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("input.json"), INPUT).unwrap();
    let script = Arc::new(Sequence {
        step: AtomicUsize::new(0),
        steps,
    });
    let mut app = mount_checked(&root, &storage, TestBehavior::Scripted(script.clone()));
    configure_test_model(&app);
    app.set_permission_mode(crate::permission::PermissionMode::FullAccess)
        .unwrap();
    app.task_create(
        "inspect declared inputs and complete deliverables".into(),
        vec![],
        budget(),
    )
    .unwrap();
    (app, script, root, storage)
}
fn clause(id: &str, checker: &str, files: Vec<&str>, parameters: Value) -> Clause {
    Clause {
        id: id.into(),
        required: true,
        checker: checker.into(),
        checker_version: 1,
        source: if checker == "human" {
            "human"
        } else if checker == "command" {
            "command_operation"
        } else {
            "project_files"
        }
        .into(),
        files: files.into_iter().map(Into::into).collect(),
        parameters,
    }
}
fn records_contract(allow: bool) -> AcceptanceContract {
    AcceptanceContract {
        version: 1,
        allow_model_completion: allow,
        clauses: vec![
            clause(
                "records",
                "records",
                vec!["input.json", "table.json", "report.md"],
                json!({"input":"input.json","table":"table.json","report":"report.md","value_field":"amount","total_label":"Total:"}),
            ),
            clause(
                "sections",
                "text",
                vec!["report.md"],
                json!({"path":"report.md","contains":["# Report","## Results"]}),
            ),
            clause(
                "fields",
                "json",
                vec!["table.json"],
                json!({"path":"table.json","checks":[{"id":"rows","op":"equals","pointer":"/row_count","expected":2}]}),
            ),
        ],
    }
}
fn activate(app: &TrustedProjectApplication, c: AcceptanceContract) {
    let s = app.task_state().unwrap().unwrap();
    app.task_set_acceptance(&s.id, s.revision, c).unwrap();
}
fn write(path: &str, content: &str) -> Step {
    Step::Call(
        "write_file",
        json!({"path":path,"content":content,"overwrite":true}),
    )
}
fn read() -> Step {
    Step::Call("read_task", json!({}))
}
fn finish(app: TrustedProjectApplication, root: &std::path::Path) {
    app.close().unwrap();
    crate::test_support::cleanup_tree(root.parent().unwrap());
}
#[test]
fn ga2_records_report_rejects_complete_but_wrong_numbers_then_finishes_after_run_and_reopens() {
    let bad = TABLE.replace("\"total\":5", "\"total\":6");
    let steps = vec![
        write("table.json", &bad),
        write("report.md", &REPORT.replace("Total: 5", "Total: 6")),
        read(),
        Step::Verify("failed"),
        Step::Candidate,
        write("table.json", TABLE),
        write("report.md", REPORT),
        read(),
        Step::Verify("passed"),
        Step::Candidate,
        Step::Done,
    ];
    let (mut app, script, root, storage) = fresh("ga2-records-e2e", steps);
    activate(&app, records_contract(true));
    let resume = task_request(&app);
    run(&mut app, resume).unwrap();
    let s = app.task_state().unwrap().unwrap();
    let a = s.acceptance_state.as_ref().unwrap();
    assert_eq!(s.phase, "complete");
    assert_eq!(a.reports.len(), 2);
    assert_eq!(a.reports[0].status, "failed");
    assert_eq!(a.reports[1].status, "passed");
    assert_eq!(a.completion.as_ref().unwrap().kind, "verified");
    assert!(s.runs[0].settled && s.runs[0].tokens > 0);
    assert_eq!(
        std::fs::read_to_string(root.join("table.json")).unwrap(),
        TABLE
    );
    assert_eq!(
        std::fs::read_to_string(root.join("report.md")).unwrap(),
        REPORT
    );
    assert_eq!(script.step.load(Ordering::SeqCst), 11);
    app.close().unwrap();
    let app = crate::test_support::reopen_after_close(|| {
        Ok::<_, ApplicationError>(mount_checked(&root, &storage, TestBehavior::Success))
    });
    assert_eq!(app.task_state().unwrap().unwrap(), s);
    assert_eq!(
        app.task_verification_status(None).unwrap()["current_applicability"]["applicable"],
        true
    );
    std::fs::write(root.join("input.json"), "[]").unwrap();
    let view = app.task_verification_status(None).unwrap();
    assert_eq!(view["current_applicability"]["applicable"], false);
    assert_eq!(app.task_state().unwrap().unwrap(), s);
    finish(app, &root);
}
#[test]
fn ga2_current_bytes_contract_version_and_human_provenance_gate_all_completion_paths() {
    let (app, _script, root, _storage) = fresh("ga2-current-identity", vec![]);
    std::fs::write(root.join("table.json"), TABLE).unwrap();
    std::fs::write(root.join("report.md"), REPORT).unwrap();
    activate(&app, records_contract(false));
    let s = app.task_state().unwrap().unwrap();
    let s = app
        .task_save_verification(&s.id, s.revision, &BTreeMap::new())
        .unwrap();
    let report = s.acceptance_state.as_ref().unwrap().reports[0].id.clone();
    assert!(app.task_finish(&s.id, s.revision, "complete").is_err());
    std::fs::write(
        root.join("input.json"),
        INPUT.replace("\"amount\":3", "\"amount\":8"),
    )
    .unwrap();
    assert!(
        app.task_complete_verified(&s.id, s.revision, &report)
            .is_err()
    );
    assert_eq!(app.task_state().unwrap().unwrap(), s);
    std::fs::write(root.join("input.json"), INPUT).unwrap();
    let mut c = records_contract(false);
    c.version = 2;
    app.task_set_acceptance(&s.id, s.revision, c).unwrap();
    let next = app.task_state().unwrap().unwrap();
    assert!(
        app.task_complete_verified(&s.id, next.revision, &report)
            .is_err()
    );
    let done = app
        .task_complete_override(
            &s.id,
            next.revision,
            "user reviewed changed contract and accepts manually",
        )
        .unwrap();
    assert_eq!(
        done.acceptance_state
            .as_ref()
            .unwrap()
            .completion
            .as_ref()
            .unwrap()
            .kind,
        "human_override"
    );
    assert_eq!(
        done.acceptance_state.as_ref().unwrap().reports[0].status,
        "passed"
    );
    finish(app, &root);
}
#[test]
fn ga2_records_checks_compute_inputs_and_reject_invalid_integer_semantics_and_missing_text() {
    let (app, _, root, _) = fresh("ga2-record-counterexamples", vec![]);
    activate(&app, records_contract(false));
    std::fs::write(root.join("table.json"), TABLE).unwrap();
    std::fs::write(root.join("report.md"), REPORT).unwrap();
    for (input, status) in [
        (INPUT.to_string(), "passed"),
        (INPUT.replace("\"amount\":3", "\"amount\":8"), "failed"),
        (
            "[{\"amount\":9223372036854775807},{\"amount\":1}]".into(),
            "inconclusive",
        ),
        ("[{\"amount\":1.5}]".into(), "inconclusive"),
    ] {
        std::fs::write(root.join("input.json"), input).unwrap();
        let report = app.task_verify(&BTreeMap::new()).unwrap();
        assert_eq!(report.results[0].status, status);
    }
    std::fs::write(root.join("input.json"), INPUT).unwrap();
    std::fs::write(root.join("report.md"), "# Report\nTotal: 5\n").unwrap();
    let r = app.task_verify(&BTreeMap::new()).unwrap();
    assert_eq!(r.status, "failed");
    assert_eq!(r.results[1].status, "failed");
    std::fs::remove_file(root.join("input.json")).unwrap();
    assert_eq!(
        app.task_verify(&BTreeMap::new()).unwrap().results[0].status,
        "inconclusive"
    );
    finish(app, &root);
}
#[test]
fn ga2_unknown_checker_and_unconfirmed_human_clauses_never_vacuously_pass() {
    let (app, _, root, _) = fresh("ga2-unknown-human", vec![]);
    let mut c = records_contract(false);
    c.clauses = vec![
        clause("unknown", "no-checker", vec![], json!({})),
        clause(
            "human",
            "human",
            vec![],
            json!({"text":"User inspects the report"}),
        ),
    ];
    activate(&app, c);
    let r = app.task_verify(&BTreeMap::new()).unwrap();
    assert_eq!(r.results.len(), 2);
    assert!(r.results.iter().all(|r| r.status == "inconclusive"));
    let s = app.task_state().unwrap().unwrap();
    app.task_attest(
        &s.id,
        s.revision,
        "human",
        "User actually checked stated condition",
    )
    .unwrap();
    let r = app.task_verify(&BTreeMap::new()).unwrap();
    assert_eq!(r.status, "inconclusive");
    assert_eq!(r.results[1].status, "passed");
    assert_eq!(r.results[1].source, "human");
    finish(app, &root);
}

fn process(argv: Value, verification: bool) -> Step {
    let mut args = json!({"argv":argv,"workdir":".","timeout_seconds":30,"sandbox":"off"});
    if verification {
        args["verification"] = json!({"format":"rust-test-summary","minimum_executed":1});
    }
    Step::Call("run_command", args)
}
// Keep real compiled fixtures small enough for the public 250-ms verification
// budget. Linux's bundled standard-library symbols alone can add several MiB;
// they are irrelevant to the test invariant but are still hashed as evidence.
fn rust_test_argv(source: &str, binary: &str) -> Value {
    json!([
        "rustc",
        "--test",
        source,
        "-C",
        "strip=symbols",
        "-o",
        binary
    ])
}
fn command_clause(id: &str, argv: Value, format: &str, files: Vec<&str>) -> Clause {
    clause(
        id,
        "command",
        files,
        json!({"argv":argv,"cwd":".","format":format,"minimum_executed":1}),
    )
}
#[test]
fn ga2_programming_real_rust_harness_zero_failure_and_pass_bound_to_runtime_input_and_completion() {
    let ignored = "#[test]\n#[ignore]\nfn invariant(){assert_eq!(2+3,5);}\n";
    let failed = "#[test]\nfn invariant(){assert_eq!(2+3,6);}\n";
    let passed = "#[test]\nfn invariant(){assert_eq!(2+3,5);}\n";
    let executable = if cfg!(windows) {
        "case-tests.exe"
    } else {
        "./case-tests"
    };
    let binary = if cfg!(windows) {
        "case-tests.exe"
    } else {
        "case-tests"
    };
    let compile = rust_test_argv("case.rs", binary);
    let invoke = json!([executable]);
    let mut steps = Vec::new();
    for (source, status) in [
        (ignored, "inconclusive"),
        (failed, "failed"),
        (passed, "passed"),
    ] {
        steps.extend([
            write("case.rs", source),
            process(compile.clone(), false),
            process(invoke.clone(), true),
            read(),
            Step::Verify(status),
            Step::Candidate,
        ]);
    }
    steps.push(Step::Done);
    let (mut app, script, root, storage) = fresh("ga2-rust-e2e", steps);
    activate(
        &app,
        AcceptanceContract {
            version: 1,
            allow_model_completion: true,
            clauses: vec![command_clause(
                "unit",
                invoke,
                "rust-test-summary",
                vec!["case.rs", binary],
            )],
        },
    );
    let resume = task_request(&app);
    run(&mut app, resume).unwrap();
    let s = app.task_state().unwrap().unwrap();
    let a = s.acceptance_state.as_ref().unwrap();
    assert_eq!(s.phase, "complete");
    assert_eq!(
        a.reports
            .iter()
            .map(|r| r.status.as_str())
            .collect::<Vec<_>>(),
        ["inconclusive", "failed", "passed"]
    );
    let facts: Vec<_> = s
        .operations
        .iter()
        .filter_map(|o| o.command_fact.as_ref())
        .collect();
    assert_eq!(facts.len(), 3);
    assert_eq!(facts[0].receipt.as_ref().unwrap()["executed"], 0);
    assert_ne!(facts[1].lifecycle["exit_code"], 0);
    assert_eq!(facts[2].lifecycle["exit_code"], 0);
    assert!(s.runs[0].settled && s.runs[0].tokens > 0);
    assert_eq!(script.step.load(Ordering::SeqCst), 19);
    assert_ne!(
        a.reports[0].results[0].operation,
        a.reports[2].results[0].operation
    );
    app.close().unwrap();
    let mut app = crate::test_support::reopen_after_close(|| {
        Ok::<_, ApplicationError>(mount_checked(&root, &storage, TestBehavior::Success))
    });
    assert_eq!(app.task_state().unwrap().unwrap(), s);
    assert!(
        app.task_verification_status(Some(&a.reports[1].id))
            .unwrap()["current_applicability"]["applicable"]
            == false
    );
    std::fs::write(root.join("case.rs"), passed.replace("2+3,5", "2+3,6")).unwrap();
    assert_eq!(
        app.task_verification_status(None).unwrap()["current_applicability"]["applicable"],
        false
    );
    assert_eq!(app.task_state().unwrap().unwrap(), s);
    let next = app
        .task_create(
            "separate task cannot reuse prior command identity".into(),
            vec![],
            budget(),
        )
        .unwrap();
    app.task_set_acceptance(&next.id, next.revision, a.contract.clone().unwrap())
        .unwrap();
    let evidence = BTreeMap::from([(
        "unit".into(),
        a.reports[2].results[0].operation.clone().unwrap(),
    )]);
    let report = app.task_verify(&evidence).unwrap();
    assert_eq!(report.status, "inconclusive");
    assert!(report.results[0].operation.is_none());
    finish(app, &root);
}

#[test]
fn ga2_same_command_recovery_replay_stays_blocked_but_explicit_single_use_recheck_creates_new_evidence()
 {
    let binary = if cfg!(windows) {
        "recheck-tests.exe"
    } else {
        "recheck-tests"
    };
    let executable = if cfg!(windows) {
        "recheck-tests.exe"
    } else {
        "./recheck-tests"
    };
    let code = r#"#[test] fn invariant(){let n=std::fs::read_to_string("execution-count").ok().and_then(|v|v.parse::<u64>().ok()).unwrap_or(0);std::fs::write("execution-count",(n+1).to_string()).unwrap();assert_eq!(2+3,5);}"#;
    let compile = rust_test_argv("recheck.rs", binary);
    let invoke = json!([executable]);
    let steps = vec![
        write("recheck.rs", code),
        process(compile, false),
        process(invoke.clone(), true),
        read(),
        Step::Verify("passed"),
        Step::Done,
    ];
    let (mut app, _, root, storage) = fresh("ga2-explicit-recheck", steps);
    activate(
        &app,
        AcceptanceContract {
            version: 1,
            allow_model_completion: true,
            clauses: vec![command_clause(
                "unit",
                invoke.clone(),
                "rust-test-summary",
                vec!["recheck.rs", binary],
            )],
        },
    );
    let resume = task_request(&app);
    run(&mut app, resume).unwrap();
    let first = app.task_state().unwrap().unwrap();
    assert_eq!(first.phase, "paused");
    assert_eq!(
        std::fs::read_to_string(root.join("execution-count")).unwrap(),
        "1"
    );
    app.close().unwrap();
    let blocked = Arc::new(Sequence {
        step: AtomicUsize::new(0),
        steps: vec![Step::Call("run_command", {
            let Step::Call(_, mut args) = process(invoke.clone(), true) else {
                unreachable!()
            };
            args["timeout_seconds"] = json!(31);
            args
        })],
    });
    let mut app = crate::test_support::reopen_after_close(|| {
        Ok::<_, ApplicationError>(mount_checked(
            &root,
            &storage,
            TestBehavior::Scripted(blocked.clone()),
        ))
    });
    let resume = task_request(&app);
    assert!(run(&mut app, resume).is_err());
    assert_eq!(blocked.step.load(Ordering::SeqCst), 1);
    assert_eq!(
        std::fs::read_to_string(root.join("execution-count")).unwrap(),
        "1"
    );
    app.close().unwrap();
    let script = Arc::new(Sequence {
        step: AtomicUsize::new(0),
        steps: vec![
            process(invoke, true),
            read(),
            Step::Verify("passed"),
            Step::Candidate,
            Step::Done,
        ],
    });
    let mut app = crate::test_support::reopen_after_close(|| {
        Ok::<_, ApplicationError>(mount_checked(
            &root,
            &storage,
            TestBehavior::Scripted(script.clone()),
        ))
    });
    let s = app.task_state().unwrap().unwrap();
    app.task_authorize_recheck(&s.id, s.revision, "unit")
        .unwrap();
    let resume = task_request(&app);
    run(&mut app, resume).unwrap();
    let done = app.task_state().unwrap().unwrap();
    let a = done.acceptance_state.as_ref().unwrap();
    assert_eq!(done.phase, "complete");
    assert_eq!(done.runs.len(), 3);
    assert!(done.runs.iter().all(|r| r.settled && r.tokens > 0));
    assert_eq!(
        std::fs::read_to_string(root.join("execution-count")).unwrap(),
        "2"
    );
    assert_eq!(a.grants.len(), 1);
    let consumed = a.grants[0].consumed_by.as_ref().unwrap();
    assert_ne!(
        Some(consumed),
        first
            .operations
            .iter()
            .find(|o| o.command_fact.is_some())
            .map(|o| &o.id)
    );
    assert_eq!(
        a.reports.last().unwrap().results[0].operation.as_ref(),
        Some(consumed)
    );
    finish(app, &root);
}

pub(super) fn pending_completion_app(label: &str) -> (TrustedProjectApplication, PathBuf) {
    let (app, _, root, _) = fresh(
        label,
        vec![read(), Step::Verify("passed"), Step::Candidate, Step::Done],
    );
    std::fs::write(root.join("table.json"), TABLE).unwrap();
    std::fs::write(root.join("report.md"), REPORT).unwrap();
    activate(&app, records_contract(true));
    (app, root)
}
#[test]
fn ga2_model_candidate_needs_user_opt_in_and_cancel_or_panic_cannot_complete() {
    for ending in [Step::Done, Step::Cancel, Step::Panic] {
        let is_done = matches!(ending, Step::Done);
        let (mut app, _, root, _) = fresh(
            "ga2-candidate-termination",
            vec![read(), Step::Verify("passed"), Step::Candidate, ending],
        );
        std::fs::write(root.join("table.json"), TABLE).unwrap();
        std::fs::write(root.join("report.md"), REPORT).unwrap();
        activate(&app, records_contract(!is_done));
        let resume = task_request(&app);
        let result = run(&mut app, resume);
        let s = app.task_state().unwrap().unwrap();
        let a = s.acceptance_state.as_ref().unwrap();
        assert_ne!(s.phase, "complete");
        assert!(a.completion.is_none());
        assert!(s.runs[0].settled);
        if is_done {
            result.unwrap();
            assert_eq!(a.candidate.as_ref().unwrap().state, "pending");
            let report = a.reports.last().unwrap().id.clone();
            let done = app
                .task_complete_verified(&s.id, s.revision, &report)
                .unwrap();
            assert_eq!(
                done.acceptance_state.unwrap().completion.unwrap().authority,
                "explicit_user"
            );
        } else {
            assert!(result.is_err() || result.is_ok_and(|done| done.cancelled));
            assert_eq!(a.candidate.as_ref().unwrap().state, "rejected");
        }
        finish(app, &root);
    }
}
#[test]
fn ga2_candidate_then_changed_output_fails_final_gate_and_unrelated_run_cannot_revive_candidate() {
    let (mut app, _, root, storage) = fresh(
        "ga2-late-output",
        vec![
            read(),
            Step::Verify("passed"),
            Step::Candidate,
            write("report.md", "# Report\n## Results\nTotal: 50\n"),
            Step::Done,
        ],
    );
    std::fs::write(root.join("table.json"), TABLE).unwrap();
    std::fs::write(root.join("report.md"), REPORT).unwrap();
    activate(&app, records_contract(true));
    let resume = task_request(&app);
    let failure = run(&mut app, resume).unwrap_err();
    assert!(failure.error.contains("task completion gate failed"));
    let before = app.task_state().unwrap().unwrap();
    assert_eq!(before.phase, "paused");
    assert!(
        before
            .acceptance_state
            .as_ref()
            .unwrap()
            .completion
            .is_none()
    );
    app.close().unwrap();
    std::fs::write(root.join("report.md"), REPORT).unwrap();
    let mut app = crate::test_support::reopen_after_close(|| {
        Ok::<_, ApplicationError>(mount_checked(&root, &storage, TestBehavior::Success))
    });
    let (completion, rx) = mpsc::channel();
    let handle = app
        .start_run(ApplicationRunRequest {
            message: crate::message::PendingMessage::text("ordinary unrelated message"),
            asker: None,
            approver: Arc::new(crate::test_support::CountingApprover(Arc::new(
                AtomicUsize::new(0),
            ))),
            events: Box::new(crate::test_support::SharedEvents(Arc::new(Mutex::new(
                Vec::new(),
            )))),
            completion,
        })
        .unwrap();
    rx.recv_timeout(std::time::Duration::from_secs(15))
        .unwrap()
        .unwrap();
    handle.join().unwrap();
    assert_eq!(app.task_state().unwrap().unwrap(), before);
    finish(app, &root);
}
#[test]
fn ga2_durable_report_and_completion_failures_never_publish_success_and_checks_remain_read_only() {
    let (app, script, root, storage) = fresh("ga2-persistence", vec![]);
    std::fs::write(root.join("table.json"), TABLE).unwrap();
    std::fs::write(root.join("report.md"), REPORT).unwrap();
    activate(&app, records_contract(false));
    fn files(root: &std::path::Path) -> BTreeMap<PathBuf, Vec<u8>> {
        fn walk(root: &std::path::Path, out: &mut BTreeMap<PathBuf, Vec<u8>>) {
            for entry in std::fs::read_dir(root).unwrap() {
                let p = entry.unwrap().path();
                if p.is_dir() {
                    walk(&p, out)
                } else {
                    out.insert(p.clone(), std::fs::read(p).unwrap());
                }
            }
        }
        let mut out = BTreeMap::new();
        walk(root, &mut out);
        out
    }
    let before = files(&storage);
    let checked = app.task_verify(&BTreeMap::new()).unwrap();
    assert_eq!(checked.status, "passed");
    app.task_verification_status(None).unwrap();
    assert_eq!(files(&storage), before);
    assert_eq!(script.step.load(Ordering::SeqCst), 0);
    let s = app.task_state().unwrap().unwrap();
    app.sessions
        .inject_persistence_faults(crate::session::persistence::FaultHooks {
            fail_batch_write: true,
            ..Default::default()
        });
    assert!(
        app.task_save_verification(&s.id, s.revision, &BTreeMap::new())
            .is_err()
    );
    assert_eq!(app.task_state().unwrap().unwrap(), s);
    let s = app
        .task_save_verification(&s.id, s.revision, &BTreeMap::new())
        .unwrap();
    let report = s.acceptance_state.as_ref().unwrap().reports[0].id.clone();
    app.sessions
        .inject_persistence_faults(crate::session::persistence::FaultHooks {
            fail_batch_write: true,
            ..Default::default()
        });
    assert!(
        app.task_complete_verified(&s.id, s.revision, &report)
            .is_err()
    );
    assert_eq!(app.task_state().unwrap().unwrap(), s);
    let done = app
        .task_complete_verified(&s.id, s.revision, &report)
        .unwrap();
    assert_eq!(done.phase, "complete");
    finish(app, &root);
}

#[test]
fn ga2_in_run_compaction_keeps_confirmed_contract_and_saved_results_outside_model_summary() {
    let script = Arc::new(super::task_recovery_tests::CompactTask {
        main: AtomicUsize::new(0),
        summaries: AtomicUsize::new(0),
        expected_items: Default::default(),
    });
    let (storage, root) = roots("ga2-compaction");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("input.json"), INPUT).unwrap();
    let mut app = mount_checked(&root, &storage, TestBehavior::Scripted(script.clone()));
    configure_test_model(&app);
    app.task_create("compact verified state".into(), vec![], budget())
        .unwrap();
    std::fs::write(root.join("table.json"), TABLE).unwrap();
    std::fs::write(root.join("report.md"), REPORT).unwrap();
    std::fs::write(root.join("observations.txt"), "observation\n".repeat(1300)).unwrap();
    activate(&app, records_contract(false));
    let s = app.task_state().unwrap().unwrap();
    let s = app
        .task_save(
            &s.id,
            s.revision,
            crate::task::Progress {
                summary: "do not assume reviewed".into(),
                ..Default::default()
            },
        )
        .unwrap();
    let s = app
        .task_save_verification(&s.id, s.revision, &BTreeMap::new())
        .unwrap();
    let report = s.acceptance_state.as_ref().unwrap().reports[0].id.clone();
    *script.expected_items.lock().unwrap() = vec![
        serde_json::to_string(
            s.acceptance_state
                .as_ref()
                .unwrap()
                .contract
                .as_ref()
                .unwrap(),
        )
        .unwrap(),
        report.clone(),
    ];
    crate::test_support::configure_test_model_with_budget(&app, u32::MAX);
    let fixed = app.context_snapshot().unwrap().total_estimate + 2000;
    crate::test_support::configure_test_model_with_budget(
        &app,
        u32::try_from((fixed + 6000) * 5 / 4).unwrap(),
    );
    let resume = task_request(&app);
    run(&mut app, resume).unwrap();
    assert_eq!(script.main.load(Ordering::SeqCst), 7);
    assert!(script.summaries.load(Ordering::SeqCst) > 0);
    let next = app.task_state().unwrap().unwrap();
    assert_eq!(next.acceptance_state, s.acceptance_state);
    assert!(next.runs[0].settled && next.runs[0].tokens > 0);
    app.task_complete_verified(&next.id, next.revision, &report)
        .unwrap();
    finish(app, &root);
}

#[test]
fn ga2_model_cannot_weaken_contract_submit_fake_reports_or_access_user_override() {
    let (mut app, _, root, _) = fresh(
        "ga2-forged-authority",
        vec![
            read(),
            Step::ForgeProgress,
            read(),
            Step::Reject(
                "verify_task_acceptance",
                json!({"task_id":"forged","expected_revision":1,"report":{"status":"passed"},"authority":"explicit_user"}),
            ),
            read(),
            Step::Reject(
                "propose_task_completion",
                json!({"task_id":"forged","expected_revision":1,"report_id":"forged","override":true}),
            ),
            read(),
            Step::Done,
        ],
    );
    activate(&app, records_contract(true));
    let before = app.task_state().unwrap().unwrap();
    let resume = task_request(&app);
    run(&mut app, resume).unwrap();
    let after = app.task_state().unwrap().unwrap();
    assert_eq!(after.acceptance_state, before.acceptance_state);
    assert!(after.operations.is_empty());
    assert_ne!(after.phase, "complete");
    assert!(after.runs[0].settled && after.runs[0].tokens > 0);
    finish(app, &root);
}

#[test]
fn ga2_actual_run_journal_failure_after_candidate_cannot_publish_verified_completion() {
    struct FailCandidateJournal(
        Arc<crate::session::use_cases::SessionService>,
        Arc<Mutex<Vec<crate::RunEvent>>>,
    );
    impl crate::EventSink for FailCandidateJournal {
        fn emit(&mut self, event: crate::RunEvent) {
            self.1.lock().unwrap().push(event.clone());
            if matches!(event,crate::RunEvent::ToolFinished{result} if result.tool_name=="propose_task_completion"&&!result.is_error)
            {
                self.0
                    .inject_persistence_faults(crate::session::persistence::FaultHooks {
                        fail_batch_write: true,
                        ..Default::default()
                    });
            }
        }
    }
    let (mut app, root) = pending_completion_app("ga2-journal-after-candidate");
    let resume = task_request(&app);
    let observed = Arc::new(Mutex::new(Vec::new()));
    let (completion, rx) = std::sync::mpsc::channel();
    let (handle, _) = app
        .start_task_run(
            resume,
            ApplicationRunRequest {
                message: crate::message::PendingMessage::text(""),
                asker: None,
                approver: Arc::new(crate::test_support::CountingApprover(Arc::new(
                    AtomicUsize::new(0),
                ))),
                events: Box::new(FailCandidateJournal(app.sessions.clone(), observed.clone())),
                completion,
            },
        )
        .unwrap();
    assert!(
        rx.recv_timeout(std::time::Duration::from_secs(20))
            .unwrap()
            .is_err()
    );
    handle.join().unwrap();
    app.sessions
        .inject_persistence_faults(crate::session::persistence::FaultHooks::default());
    assert_task_terminal_history(&mut app, &observed, "error");
    let s = app.task_state().unwrap().unwrap();
    assert_ne!(s.phase, "complete");
    let a = s.acceptance_state.unwrap();
    assert!(a.completion.is_none());
    assert_ne!(a.candidate.as_ref().unwrap().state, "accepted");
    assert_eq!(a.reports.last().unwrap().status, "passed");
    finish(app, &root);
}

#[test]
fn ga2_rework_final_gate_failure_emits_failed_terminal() {
    let (mut app, _, root, _) = fresh(
        "ga2-audit-terminal",
        vec![
            read(),
            Step::Verify("passed"),
            Step::Candidate,
            write("report.md", "# Report\n## Results\nTotal: 50\n"),
            Step::Done,
        ],
    );
    std::fs::write(root.join("table.json"), TABLE).unwrap();
    std::fs::write(root.join("report.md"), REPORT).unwrap();
    activate(&app, records_contract(true));
    let resume = task_request(&app);
    let observed = Arc::new(Mutex::new(Vec::new()));
    let (completion, rx) = mpsc::channel();
    let (handle, _) = app
        .start_task_run(
            resume,
            ApplicationRunRequest {
                message: crate::message::PendingMessage::text("continue"),
                asker: None,
                approver: Arc::new(crate::test_support::CountingApprover(Arc::new(
                    AtomicUsize::new(0),
                ))),
                events: Box::new(crate::test_support::SharedEvents(observed.clone())),
                completion,
            },
        )
        .unwrap();
    let result = rx.recv_timeout(std::time::Duration::from_secs(20)).unwrap();
    handle.join().unwrap();
    assert!(
        result
            .unwrap_err()
            .error
            .contains("task completion gate failed")
    );
    let events = observed.lock().unwrap();
    let success = events
        .iter()
        .filter(|e| matches!(e, crate::RunEvent::RunCompleted { .. }))
        .count();
    let failed = events
        .iter()
        .filter(|e| matches!(e, crate::RunEvent::RunFailed { .. }))
        .count();
    drop(events);
    assert_task_terminal_history(&mut app, &observed, "error");
    finish(app, &root);
    assert_eq!(
        (success, failed),
        (0, 1),
        "completion channel failure must agree with exactly one terminal event"
    );
}

#[test]
fn ga2_rework_command_alias_cannot_bypass_recheck_grant() {
    let binary = if cfg!(windows) {
        "alias-tests.exe"
    } else {
        "alias-tests"
    };
    let exe = if cfg!(windows) {
        "alias-tests.exe"
    } else {
        "./alias-tests"
    };
    let code = r#"#[test] fn invariant(){let n=std::fs::read_to_string("execution-count").ok().and_then(|v|v.parse::<u64>().ok()).unwrap_or(0);std::fs::write("execution-count",(n+1).to_string()).unwrap();}"#;
    let argv = json!([exe]);
    let (mut app, _, root, storage) = fresh(
        "ga2-audit-alias",
        vec![
            write("alias.rs", code),
            process(rust_test_argv("alias.rs", binary), false),
            process(argv.clone(), true),
            read(),
            Step::Done,
        ],
    );
    activate(
        &app,
        AcceptanceContract {
            version: 1,
            allow_model_completion: false,
            clauses: vec![command_clause(
                "unit",
                argv.clone(),
                "rust-test-summary",
                vec!["alias.rs", binary],
            )],
        },
    );
    let request = task_request(&app);
    run(&mut app, request).unwrap();
    assert_eq!(
        std::fs::read_to_string(root.join("execution-count")).unwrap(),
        "1"
    );
    app.close().unwrap();
    let script = Arc::new(Sequence {
        step: AtomicUsize::new(0),
        steps: vec![
            Step::Call(
                "exec_command",
                json!({"argv":argv,"tty":false,"sandbox":"off","yield_time_ms":1000,"verification":{"format":"rust-test-summary","minimum_executed":1}}),
            ),
            read(),
            Step::Done,
        ],
    });
    let mut app = crate::test_support::reopen_after_close(|| {
        Ok::<_, ApplicationError>(mount_checked(
            &root,
            &storage,
            TestBehavior::Scripted(script.clone()),
        ))
    });
    let request = task_request(&app);
    let result = run(&mut app, request);
    eprintln!("alias Run result: {result:?}");
    let count = std::fs::read_to_string(root.join("execution-count")).unwrap();
    finish(app, &root);
    assert_eq!(
        count, "1",
        "switching command wrapper must not repeat a confirmed verification command without explicit recheck grant"
    );
}

pub(super) fn assert_task_terminal_history(
    app: &mut TrustedProjectApplication,
    observed: &Arc<Mutex<Vec<crate::RunEvent>>>,
    expected: &str,
) {
    let events = observed.lock().unwrap();
    let kinds: Vec<_> = events
        .iter()
        .filter_map(|e| match e {
            crate::RunEvent::RunCompleted { .. } => Some("completed"),
            crate::RunEvent::RunFailed { .. } => Some("error"),
            crate::RunEvent::RunCancelled { .. } => Some("aborted"),
            _ => None,
        })
        .collect();
    assert_eq!(
        kinds,
        [expected],
        "exactly one terminal must agree with application result"
    );
    drop(events);
    let snapshot = app.snapshot().unwrap();
    let ends: Vec<_> = snapshot
        .replay
        .iter()
        .filter_map(|e| match e {
            crate::session::replay::ReplayEvent::TurnEnded { reason, .. } => Some(reason),
            _ => None,
        })
        .collect();
    assert_eq!(ends.len(), 1, "one replayable journal terminal");
    let actual = match ends[0] {
        crate::session::replay::ReplayTurnEnd::Completed => "completed",
        crate::session::replay::ReplayTurnEnd::Error { .. } => "error",
        crate::session::replay::ReplayTurnEnd::Aborted { .. } => "aborted",
        other => panic!("unexpected replay terminal {other:?}"),
    };
    assert_eq!(actual, expected);
}
fn run_observed(
    app: &mut TrustedProjectApplication,
) -> (ApplicationRunResult, Arc<Mutex<Vec<crate::RunEvent>>>) {
    let observed = Arc::new(Mutex::new(Vec::new()));
    let (completion, rx) = mpsc::channel();
    let request = task_request(app);
    let (handle, _) = app
        .start_task_run(
            request,
            ApplicationRunRequest {
                message: crate::message::PendingMessage::text("continue"),
                asker: None,
                approver: Arc::new(crate::test_support::CountingApprover(Arc::new(
                    AtomicUsize::new(0),
                ))),
                events: Box::new(crate::test_support::SharedEvents(observed.clone())),
                completion,
            },
        )
        .unwrap();
    let result = rx.recv_timeout(std::time::Duration::from_secs(20)).unwrap();
    handle.join().unwrap();
    (result, observed)
}
#[test]
fn ga2_rework_success_cancel_and_panic_have_one_matching_durable_and_live_terminal() {
    for (ending, expected) in [
        (Step::Done, "completed"),
        (Step::Cancel, "aborted"),
        (Step::Panic, "error"),
    ] {
        let (mut app, _, root, storage) = fresh(
            "ga2-terminal-matrix",
            vec![read(), Step::Verify("passed"), Step::Candidate, ending],
        );
        std::fs::write(root.join("table.json"), TABLE).unwrap();
        std::fs::write(root.join("report.md"), REPORT).unwrap();
        activate(&app, records_contract(true));
        let (result, events) = run_observed(&mut app);
        match expected {
            "completed" => assert!(result.is_ok_and(|d| !d.cancelled)),
            "aborted" => assert!(result.is_ok_and(|d| d.cancelled)),
            _ => assert!(result.is_err()),
        }
        assert_task_terminal_history(&mut app, &events, expected);
        let state = app.task_state().unwrap().unwrap();
        assert_eq!(state.phase == "complete", expected == "completed");
        let raw = super::tests::load_events(&storage);
        let ends: Vec<_> = raw.iter().filter(|e| e.event_type == "turn/end").collect();
        assert_eq!(ends.len(), 1);
        assert_eq!(ends[0].data["reason"]["kind"], expected);
        if expected == "completed" {
            let complete = raw
                .iter()
                .find(|e| e.event_type == "clat/task" && e.data["phase"] == "complete")
                .unwrap();
            assert_eq!(
                complete.seq + 1,
                ends[0].seq,
                "Task completion and terminal share the final atomic group"
            );
        }
        app.close().unwrap();
        let mut reopened = crate::test_support::reopen_after_close(|| {
            Ok::<_, ApplicationError>(mount_checked(&root, &storage, TestBehavior::Success))
        });
        assert_eq!(reopened.task_state().unwrap().unwrap(), state);
        assert_task_terminal_history(&mut reopened, &events, expected);
        finish(reopened, &root);
    }
}

fn wrapper(argv: Value, exec: bool) -> Step {
    if exec {
        Step::Call(
            "exec_command",
            json!({"argv":argv,"tty":false,"sandbox":"off","workdir":".","yield_time_ms":30000,"max_output_tokens":1024,"verification":{"format":"rust-test-summary","minimum_executed":1}}),
        )
    } else {
        process(argv, true)
    }
}
#[test]
fn ga2_rework_wrappers_bidirectionally_require_one_grant_and_keep_new_runtime_evidence() {
    for first_exec in [false, true] {
        let binary = if cfg!(windows) { "dual.exe" } else { "dual" };
        let exe = if cfg!(windows) { "dual.exe" } else { "./dual" };
        let argv = json!([exe]);
        let code = r#"#[test] fn invariant(){let n=std::fs::read_to_string("count").ok().and_then(|s|s.parse::<u32>().ok()).unwrap_or(0);std::fs::write("count",(n+1).to_string()).unwrap();}"#;
        let (mut app, _, root, storage) = fresh(
            "ga2-dual-wrapper",
            vec![
                write("dual.rs", code),
                process(rust_test_argv("dual.rs", binary), false),
                wrapper(argv.clone(), first_exec),
                read(),
                Step::Verify("passed"),
                Step::Done,
            ],
        );
        activate(
            &app,
            AcceptanceContract {
                version: 1,
                allow_model_completion: true,
                clauses: vec![command_clause(
                    "unit",
                    argv.clone(),
                    "rust-test-summary",
                    vec!["dual.rs", binary],
                )],
            },
        );
        let request = task_request(&app);
        let result = run(&mut app, request);
        assert!(
            result.is_ok(),
            "first_exec={first_exec}; result={result:?}; task={:?}",
            app.task_state().unwrap()
        );
        let first = app.task_state().unwrap().unwrap();
        let old = first
            .operations
            .iter()
            .find(|o| o.command_fact.is_some())
            .unwrap()
            .clone();
        assert!(old.execution_identity.is_some());
        let mut legacy = first.clone();
        let op = legacy
            .operations
            .iter_mut()
            .find(|o| o.id == old.id)
            .unwrap();
        op.execution_identity = None;
        op.command_fact.as_mut().unwrap().invocation_digest =
            crate::task::state::digest(&serde_json::to_vec(&(argv.clone(), ".")).unwrap());
        assert!(
            crate::plugins::prior_command_matches(
                op,
                &first,
                old.execution_identity.as_ref().unwrap()
            ),
            "older relative-digest fact retains wrapper fence"
        );
        let legacy_op = op.clone();
        legacy
            .acceptance_state
            .as_mut()
            .unwrap()
            .contract
            .as_mut()
            .unwrap()
            .version += 1;
        assert!(
            crate::plugins::prior_command_matches(
                &legacy_op,
                &legacy,
                old.execution_identity.as_ref().unwrap()
            ),
            "unavailable old contract fails closed instead of authorizing replay"
        );

        app.close().unwrap();
        let blocked = Arc::new(Sequence {
            step: AtomicUsize::new(0),
            steps: vec![wrapper(argv.clone(), !first_exec), read(), Step::Done],
        });
        let mut app = crate::test_support::reopen_after_close(|| {
            Ok::<_, ApplicationError>(mount_checked(
                &root,
                &storage,
                TestBehavior::Scripted(blocked.clone()),
            ))
        });
        assert_eq!(app.task_state().unwrap().unwrap(), first);
        let request = task_request(&app);
        assert!(run(&mut app, request).is_err());
        assert_eq!(std::fs::read_to_string(root.join("count")).unwrap(), "1");
        app.close().unwrap();
        let script = Arc::new(Sequence {
            step: AtomicUsize::new(0),
            steps: vec![
                wrapper(argv.clone(), !first_exec),
                read(),
                Step::Verify("passed"),
                Step::Done,
            ],
        });
        let mut app = crate::test_support::reopen_after_close(|| {
            Ok::<_, ApplicationError>(mount_checked(
                &root,
                &storage,
                TestBehavior::Scripted(script.clone()),
            ))
        });
        let s = app.task_state().unwrap().unwrap();
        app.task_authorize_recheck(&s.id, s.revision, "unit")
            .unwrap();
        let request = task_request(&app);
        run(&mut app, request).unwrap();
        assert_eq!(std::fs::read_to_string(root.join("count")).unwrap(), "2");
        let done = app.task_state().unwrap().unwrap();
        assert_eq!(done.phase, "paused");
        assert_eq!(done.runs.len(), 3);
        assert!(done.runs.iter().all(|r| r.settled && r.tokens > 0));
        let a = done.acceptance_state.as_ref().unwrap();
        let id = a.grants[0].consumed_by.as_ref().unwrap();
        assert_ne!(id, &old.id);
        let op = done.operations.iter().find(|o| &o.id == id).unwrap();
        assert_eq!(op.execution_identity, old.execution_identity);
        assert_ne!(op.run, old.run);
        assert!(op.command_fact.as_ref().unwrap().receipt.is_some());
        assert_eq!(
            a.reports.last().unwrap().results[0].operation.as_ref(),
            Some(id)
        );
        app.close().unwrap();
        let replay = Arc::new(Sequence {
            step: AtomicUsize::new(0),
            steps: vec![wrapper(argv.clone(), first_exec), read(), Step::Done],
        });
        let mut app = crate::test_support::reopen_after_close(|| {
            Ok::<_, ApplicationError>(mount_checked(
                &root,
                &storage,
                TestBehavior::Scripted(replay.clone()),
            ))
        });
        let request = task_request(&app);
        assert!(run(&mut app, request).is_err());
        assert_eq!(
            std::fs::read_to_string(root.join("count")).unwrap(),
            "2",
            "a consumed grant cannot authorize another Run"
        );
        let state = app.task_state().unwrap().unwrap();
        assert_eq!(state.runs.len(), 4);
        assert!(state.runs.iter().all(|r| r.settled && r.tokens > 0));
        assert_eq!(state.acceptance_state.as_ref().unwrap().grants, a.grants);
        app.task_complete_verified(&state.id, state.revision, &a.reports.last().unwrap().id)
            .unwrap();
        finish(app, &root);
    }
}

#[test]
fn ga2_rework_shell_alias_and_resolved_workdir_do_not_grant_a_new_execution() {
    let (mut app, _, root, storage) = fresh(
        "ga2-shell-alias",
        vec![
            Step::Call(
                "run_command",
                json!({"command":"echo executed >> count","workdir":"nested","sandbox":"off"}),
            ),
            read(),
            Step::Done,
        ],
    );
    std::fs::create_dir(root.join("nested")).unwrap();
    let request = task_request(&app);
    run(&mut app, request).unwrap();
    assert_eq!(
        std::fs::read_to_string(root.join("nested/count"))
            .unwrap()
            .lines()
            .count(),
        1
    );
    app.close().unwrap();
    let script = Arc::new(Sequence {
        step: AtomicUsize::new(0),
        steps: vec![
            Step::Call(
                "exec_command",
                json!({"cmd":"echo executed >> count","workdir":"nested/.","tty":false,"sandbox":"off","yield_time_ms":1000}),
            ),
            read(),
            Step::Done,
        ],
    });
    let mut app = crate::test_support::reopen_after_close(|| {
        Ok::<_, ApplicationError>(mount_checked(
            &root,
            &storage,
            TestBehavior::Scripted(script.clone()),
        ))
    });
    let request = task_request(&app);
    assert!(run(&mut app, request).is_err());
    assert_eq!(
        std::fs::read_to_string(root.join("nested/count"))
            .unwrap()
            .lines()
            .count(),
        1
    );
    finish(app, &root);
}

#[test]
fn ga2_rework_actual_settlement_failure_has_no_success_terminal_and_no_completed_task() {
    struct Fault {
        sessions: Arc<crate::session::use_cases::SessionService>,
        events: Arc<Mutex<Vec<crate::RunEvent>>>,
    }
    impl crate::EventSink for Fault {
        fn emit(&mut self, event: crate::RunEvent) {
            if matches!(
                event,
                crate::RunEvent::ModelResponded {
                    finish_reason: FinishReason::Completed,
                    ..
                }
            ) {
                self.sessions
                    .inject_persistence_faults(crate::session::persistence::FaultHooks {
                        fail_batch_write: true,
                        ..Default::default()
                    });
            }
            self.events.lock().unwrap().push(event);
        }
    }
    let (mut app, root) = pending_completion_app("ga2-settlement-events");
    let request = task_request(&app);
    let observed = Arc::new(Mutex::new(Vec::new()));
    let (completion, rx) = mpsc::channel();
    let (handle, _) = app
        .start_task_run(
            request,
            ApplicationRunRequest {
                message: crate::message::PendingMessage::text("continue"),
                asker: None,
                approver: Arc::new(crate::test_support::CountingApprover(Arc::new(
                    AtomicUsize::new(0),
                ))),
                events: Box::new(Fault {
                    sessions: app.sessions.clone(),
                    events: observed.clone(),
                }),
                completion,
            },
        )
        .unwrap();
    let failure = rx
        .recv_timeout(std::time::Duration::from_secs(20))
        .unwrap()
        .unwrap_err();
    handle.join().unwrap();
    assert!(
        failure.error.contains("task progress settlement failed"),
        "{}",
        failure.error
    );
    app.sessions
        .inject_persistence_faults(crate::session::persistence::FaultHooks::default());
    assert_task_terminal_history(&mut app, &observed, "error");
    let s = app.task_state().unwrap().unwrap();
    assert_ne!(s.phase, "complete");
    assert!(!s.runs.last().unwrap().settled);
    assert!(s.acceptance_state.unwrap().completion.is_none());
    finish(app, &root);
}

#[test]
fn ga2_rework_running_exec_receipt_and_poll_result_cannot_be_used_as_completed_evidence() {
    let binary = if cfg!(windows) { "slow.exe" } else { "slow" };
    let exe = if cfg!(windows) { "slow.exe" } else { "./slow" };
    let argv = json!([exe]);
    let code = r#"#[test] fn invariant(){std::fs::write("started","1").unwrap();std::thread::sleep(std::time::Duration::from_secs(5));}"#;
    let (mut app, _, root, _) = fresh(
        "ga2-running-exec",
        vec![
            write("slow.rs", code),
            process(rust_test_argv("slow.rs", binary), false),
            Step::Call(
                "exec_command",
                json!({"argv":argv,"tty":false,"sandbox":"off","yield_time_ms":250,"verification":{"format":"rust-test-summary","minimum_executed":1}}),
            ),
            Step::Poll,
            read(),
            Step::Verify("inconclusive"),
            Step::Candidate,
            Step::Done,
        ],
    );
    activate(
        &app,
        AcceptanceContract {
            version: 1,
            allow_model_completion: true,
            clauses: vec![command_clause(
                "unit",
                argv,
                "rust-test-summary",
                vec!["slow.rs", binary],
            )],
        },
    );
    let request = task_request(&app);
    run(&mut app, request).unwrap();
    let s = app.task_state().unwrap().unwrap();
    assert_eq!(s.phase, "paused");
    let op = s
        .operations
        .iter()
        .find(|o| o.command_fact.is_some())
        .unwrap();
    let fact = op.command_fact.as_ref().unwrap();
    assert_eq!(fact.lifecycle["running"], true);
    assert_eq!(
        s.acceptance_state.as_ref().unwrap().reports[0].status,
        "inconclusive"
    );
    let wrong = BTreeMap::from([(
        "unit".into(),
        s.operations
            .iter()
            .find(|o| o.tool == "write_stdin")
            .unwrap()
            .id
            .clone(),
    )]);
    assert_eq!(app.task_verify(&wrong).unwrap().status, "inconclusive");
    assert!(s.runs[0].settled && s.runs[0].tokens > 0);
    finish(app, &root);
}

#[path = "capability_acceptance_tests.rs"]
mod capability_tests;
