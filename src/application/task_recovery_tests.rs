use super::*;
use crate::CommandOutcome;
use crate::model::{
    FinishReason, ModelError, ModelEventSink, ModelItem, ModelRequest, ModelResponse,
};
use crate::task::{Progress, TaskBudget, TaskResume};
use crate::test_support::{
    CountingApprover, SharedEvents, TestBehavior, TestModelScript, TestProviderPlugin,
    configure_test_model, roots,
};
use serde_json::json;
use std::sync::atomic::{AtomicUsize, Ordering};
const TABLE: &str = "name,value\nalpha,2\nbeta,3\n";
const REPORT: &str = "# Report\nTwo records. Total value: 5.\n";
fn budget() -> TaskBudget {
    TaskBudget {
        max_tokens: 1_000_000,
        max_runs: 8,
        max_seconds: 120,
        run_tokens: 200_000,
        run_seconds: 30,
    }
}
fn mount(
    root: &std::path::Path,
    storage: &std::path::Path,
    behavior: TestBehavior,
) -> TrustedProjectApplication {
    BootstrapApplication::open(Project::new(root), storage.into())
        .unwrap()
        .authorize_and_mount_with_provider(Arc::new(TestProviderPlugin { behavior }))
        .unwrap()
}
fn task_request(app: &TrustedProjectApplication) -> TaskResume {
    let c = app.task_check().unwrap();
    TaskResume {
        task_id: c.task.id,
        revision: c.task.revision,
        check_digest: c.digest,
        accept_changes: false,
    }
}
fn run(app: &mut TrustedProjectApplication, resume: TaskResume) -> ApplicationRunResult {
    let (completion, rx) = mpsc::channel();
    let (handle, _) = app
        .start_task_run(
            resume,
            ApplicationRunRequest {
                message: crate::message::PendingMessage::text("ignored caller text"),
                asker: None,
                approver: Arc::new(CountingApprover(Arc::new(AtomicUsize::new(0)))),
                events: Box::new(SharedEvents(Arc::new(Mutex::new(Vec::new())))),
                completion,
            },
        )
        .unwrap();
    let result = rx.recv_timeout(std::time::Duration::from_secs(20)).unwrap();
    handle.join().unwrap();
    result
}
struct ReportScript {
    step: AtomicUsize,
    resumed: bool,
}
impl TestModelScript for ReportScript {
    fn stream(
        &self,
        r: ModelRequest<'_>,
        events: &mut dyn ModelEventSink,
    ) -> Result<ModelResponse, ModelError> {
        if r.tools.is_empty() {
            return Ok(crate::test_support::response(
                "report title",
                FinishReason::Completed,
            ));
        }
        let step = self.step.fetch_add(1, Ordering::SeqCst);
        let instructions = r.instructions.unwrap_or_default();
        assert!(instructions.contains("Task recovery state"));
        let (name, args) = match (self.resumed, step) {
            (false, 0) => ("read_file", json!({"path":"materials.txt"})),
            (false, 1) => ("write_file", json!({"path":"table.csv","content":TABLE})),
            (true, 0) => {
                assert!(instructions.contains("table saved"));
                assert!(instructions.contains("investigate missing chart"));
                ("write_file", json!({"path":"report.md","content":REPORT}))
            }
            (false, 2) | (true, 1) => ("read_task", json!({})),
            (false, 3) | (true, 2) => {
                let state = r
                    .items
                    .iter()
                    .rev()
                    .find_map(|i| match i {
                        ModelItem::ToolResult(x) if x.tool_name == "read_task" => Some(&x.output),
                        _ => None,
                    })
                    .unwrap();
                let op = state["operations"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .rev()
                    .find(|o| o["tool"] == "write_file")
                    .unwrap();
                let (path, content, summary) = if self.resumed {
                    ("report.md", REPORT, "report saved")
                } else {
                    ("table.csv", TABLE, "table saved")
                };
                let progress = json!({"summary":summary,"steps":["read source","write structured table"],"questions":["investigate missing chart"],"next":"review totals", "artifacts":[{"path":path,"summary":summary,"digest":format!("{:x}",<sha2::Sha256 as sha2::Digest>::digest(content.as_bytes())),"evidence":[op["id"]]}]});
                (
                    "update_task",
                    json!({"task_id":state["id"],"expected_revision":state["revision"],"progress":progress}),
                )
            }
            _ => {
                return Ok(crate::test_support::response(
                    "checkpoint saved",
                    FinishReason::Completed,
                ));
            }
        };
        let mut response = crate::test_support::response("", FinishReason::ToolCalls);
        response.tool_calls.push(crate::ToolCall {
            id: format!("report-{step}"),
            name: name.into(),
            arguments: args,
        });
        for call in &response.tool_calls {
            events.emit(crate::ModelEvent::ToolCallCompleted { call: call.clone() });
        }
        Ok(response)
    }
}
#[test]
fn ga1_local_report_exit_reopen_check_explicit_continue_preserves_existing_outputs() {
    let (storage, root) = roots("ga1-report-e2e");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("materials.txt"), "alpha=2\nbeta=3\n").unwrap();
    let first = Arc::new(ReportScript {
        step: AtomicUsize::new(0),
        resumed: false,
    });
    let mut app = mount(&root, &storage, TestBehavior::Scripted(first.clone()));
    configure_test_model(&app);
    app.task_create(
        "read materials, table, report".into(),
        vec!["review total 5".into()],
        budget(),
    )
    .unwrap();
    let req = task_request(&app);
    run(&mut app, req).unwrap();
    let before = app.task_state().unwrap().unwrap();
    assert_eq!(before.progress.summary, "table saved");
    assert_eq!(
        before
            .operations
            .iter()
            .filter(|o| o.tool == "write_file")
            .count(),
        1
    );
    assert!(before.runs[0].settled);
    assert!(before.runs[0].tokens > 0);
    let key = crate::session::key::SessionKey {
        id: app.current_session_id().unwrap(),
        project: app.project_key(),
    };
    app.close().unwrap();
    let table = std::fs::read(root.join("table.csv")).unwrap();
    // Force one cold full-journal rebuild, independent of the saved checkpoint.
    let session_dir = crate::session::path_layout::session_dir(
        &storage.join("sessions"),
        key.project.header_cwd.as_deref(),
        &key.id,
    );
    std::fs::remove_file(session_dir.join("clat-checkpoint.json")).unwrap();
    let second = Arc::new(ReportScript {
        step: AtomicUsize::new(0),
        resumed: true,
    });
    let mut app = crate::test_support::reopen_after_close(|| {
        Ok::<_, ApplicationError>(mount(
            &root,
            &storage,
            TestBehavior::Scripted(second.clone()),
        ))
    });
    assert_eq!(app.task_state().unwrap().unwrap(), before);
    let check = app.task_check().unwrap();
    assert_eq!(check.artifacts[0]["status"], "unchanged");
    assert_eq!(second.step.load(Ordering::SeqCst), 0);
    assert!(!root.join("report.md").exists());
    let stale = task_request(&app);
    let req = stale.clone();
    run(&mut app, req).unwrap();
    assert!(app.task_validate_resume(&stale).is_err());
    let after = app.task_state().unwrap().unwrap();
    assert_eq!(after.runs.len(), 2);
    assert!(after.runs[1].tokens > 0);
    assert_eq!(
        after
            .operations
            .iter()
            .filter(|o| o.tool == "write_file")
            .count(),
        2
    );
    assert_eq!(std::fs::read(root.join("table.csv")).unwrap(), table);
    assert_eq!(
        std::fs::read_to_string(root.join("report.md")).unwrap(),
        REPORT
    );
    assert_eq!(after.progress.questions, ["investigate missing chart"]);
    assert_eq!(first.step.load(Ordering::SeqCst), 5);
    assert_eq!(second.step.load(Ordering::SeqCst), 4);
    app.close().unwrap();
    crate::test_support::cleanup_tree(root.parent().unwrap());
}
#[test]
fn ga1_commands_create_save_check_complete_and_no_implicit_ordinary_task() {
    let (storage, root) = roots("ga1-commands");
    std::fs::create_dir_all(&root).unwrap();
    let mut app = mount(&root, &storage, TestBehavior::Success);
    configure_test_model(&app);
    assert!(app.task_state().unwrap().is_none());
    let spec = json!({"objective":"explicit work","acceptance":["human review"],"budget":budget()});
    assert!(matches!(
        app.dispatch_command(&format!("/task create {spec}"))
            .unwrap(),
        CommandOutcome::ShowTask(_)
    ));
    let c = app.task_check().unwrap();
    let id = c.task.id;
    let rev = c.task.revision;
    assert!(matches!(
        app.dispatch_command(&format!("/task continue {id} {rev} {}", c.digest))
            .unwrap(),
        CommandOutcome::StartTaskRun(_)
    ));
    assert!(matches!(
        app.dispatch_command("/task check").unwrap(),
        CommandOutcome::ShowTask(_)
    ));
    app.task_save(
        &id,
        rev,
        Progress {
            summary: "claim only".into(),
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(app.task_state().unwrap().unwrap().phase, "paused");
    assert!(app.task_finish(&id, rev, "complete").is_err());
    app.task_finish(&id, rev + 1, "complete").unwrap();
    assert!(app.task_validate_resume(&task_request(&app)).is_err());
    app.new_session().unwrap();
    assert!(app.task_state().unwrap().is_none());
    app.close().unwrap();
    crate::test_support::cleanup_tree(root.parent().unwrap());
}

struct CompactTask {
    main: AtomicUsize,
    summaries: AtomicUsize,
}
impl TestModelScript for CompactTask {
    fn stream(
        &self,
        r: ModelRequest<'_>,
        events: &mut dyn ModelEventSink,
    ) -> Result<ModelResponse, ModelError> {
        if r.tools.is_empty() {
            if r.instructions
                .unwrap_or_default()
                .contains("context compaction")
            {
                self.summaries.fetch_add(1, Ordering::SeqCst);
            }
            return Ok(crate::test_support::response(
                "Preserve the material observations and continue the task.",
                FinishReason::Completed,
            ));
        }
        assert!(
            r.instructions
                .unwrap_or_default()
                .contains("do not assume reviewed")
        );
        let n = self.main.fetch_add(1, Ordering::SeqCst);
        let mut response = crate::test_support::response("done", FinishReason::Completed);
        if n < 6 {
            response.finish_reason = FinishReason::ToolCalls;
            response.tool_calls.push(crate::ToolCall {
                id: format!("read-{n}"),
                name: "read_file".into(),
                arguments: json!({"path":"observations.txt","max_bytes":16000}),
            });
        }
        for call in &response.tool_calls {
            events.emit(crate::ModelEvent::ToolCallCompleted { call: call.clone() });
        }
        Ok(response)
    }
}
#[test]
fn ga1_in_run_compaction_preserves_progress_questions_operations_and_budget() {
    let (storage, root) = roots("ga1-compaction");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("observations.txt"), "observation\n".repeat(1300)).unwrap();
    let script = Arc::new(CompactTask {
        main: AtomicUsize::new(0),
        summaries: AtomicUsize::new(0),
    });
    let mut app = mount(&root, &storage, TestBehavior::Scripted(script.clone()));
    crate::test_support::configure_test_model_with_budget(&app, u32::MAX);
    let s = app
        .task_create(
            "read observations".into(),
            vec!["human review".into()],
            budget(),
        )
        .unwrap();
    app.task_save(
        &s.id,
        s.revision,
        Progress {
            summary: "do not assume reviewed".into(),
            questions: vec!["which observations need confirmation?".into()],
            ..Default::default()
        },
    )
    .unwrap();
    let fixed = app.context_snapshot().unwrap().total_estimate + 2000;
    crate::test_support::configure_test_model_with_budget(
        &app,
        u32::try_from((fixed + 6000) * 5 / 4).unwrap(),
    );
    let resume = task_request(&app);
    run(&mut app, resume).unwrap();
    assert_eq!(script.main.load(Ordering::SeqCst), 7);
    assert!(script.summaries.load(Ordering::SeqCst) > 0);
    let s = app.task_state().unwrap().unwrap();
    assert_eq!(s.operations.len(), 6);
    assert!(s.operations.iter().all(|o| o.outcome == "confirmed"));
    assert_eq!(
        s.progress.questions,
        ["which observations need confirmation?"]
    );
    assert_ne!(s.phase, "complete");
    assert!(
        s.runs.last().unwrap().tokens >= 35 + 5 * script.summaries.load(Ordering::SeqCst) as u64
    );
    let project = app.project.clone();
    app.close().unwrap();
    let app = crate::test_support::reopen_after_close(|| {
        BootstrapApplication::open(project.clone(), storage.clone())?
            .authorize_and_mount_with_provider(Arc::new(TestProviderPlugin {
                behavior: TestBehavior::Success,
            }))
    });
    assert_eq!(app.task_state().unwrap().unwrap(), s);
    app.close().unwrap();
    crate::test_support::cleanup_tree(root.parent().unwrap());
}
#[test]
fn ga1_approval_denial_has_no_execution_intent_or_file_effect() {
    struct Deny;
    impl crate::permission::PermissionApprover for Deny {
        fn decide(
            &self,
            _: crate::permission::PermissionRequest,
            _: &crate::CancelToken,
        ) -> crate::permission::PermissionDecision {
            crate::permission::PermissionDecision::Deny {
                reason: "user refused".into(),
            }
        }
    }
    let (storage, root) = roots("ga1-denial");
    std::fs::create_dir_all(&root).unwrap();
    let mut app = mount(&root, &storage, TestBehavior::WriteFile);
    configure_test_model(&app);
    app.task_create("try write".into(), vec![], budget())
        .unwrap();
    let resume = task_request(&app);
    let (completion, rx) = mpsc::channel();
    let (handle, _) = app
        .start_task_run(
            resume,
            ApplicationRunRequest {
                message: crate::message::PendingMessage::text(""),
                asker: None,
                approver: Arc::new(Deny),
                events: Box::new(SharedEvents(Arc::new(Mutex::new(Vec::new())))),
                completion,
            },
        )
        .unwrap();
    let result = rx.recv_timeout(std::time::Duration::from_secs(20)).unwrap();
    handle.join().unwrap();
    result.unwrap();
    assert!(app.task_state().unwrap().unwrap().operations.is_empty());
    assert!(!root.join("written.txt").exists());
    app.close().unwrap();
    crate::test_support::cleanup_tree(root.parent().unwrap());
}
#[test]
fn ga1_task_creation_rejects_invalid_inputs_before_materializing_a_session() {
    let (storage, root) = roots("ga1-invalid-birth");
    std::fs::create_dir_all(&root).unwrap();
    let mut app = mount(&root, &storage, TestBehavior::Success);
    assert!(app.task_create("".into(), vec![], budget()).is_err());
    assert!(app.current_session_id().is_none());
    assert!(app.task_state().unwrap().is_none());
    app.close().unwrap();
    crate::test_support::cleanup_tree(root.parent().unwrap());
}

#[test]
fn ga1_missing_usage_failure_and_retry_never_reset_task_spend() {
    struct NoUsage;
    impl TestModelScript for NoUsage {
        fn stream(
            &self,
            _: ModelRequest<'_>,
            _: &mut dyn ModelEventSink,
        ) -> Result<ModelResponse, ModelError> {
            let mut r = crate::test_support::response("usage unavailable", FinishReason::Completed);
            r.usage = None;
            Ok(r)
        }
    }
    let (storage, root) = roots("ga1-unknown-usage");
    std::fs::create_dir_all(&root).unwrap();
    let mut app = mount(&root, &storage, TestBehavior::Scripted(Arc::new(NoUsage)));
    configure_test_model(&app);
    app.task_create("bounded work".into(), vec![], budget())
        .unwrap();
    let resume = task_request(&app);
    run(&mut app, resume).unwrap();
    let first = app.task_state().unwrap().unwrap();
    assert!(first.runs[0].tokens > 5);
    let resume = task_request(&app);
    run(&mut app, resume).unwrap();
    let second = app.task_state().unwrap().unwrap();
    assert_eq!(second.runs[0], first.runs[0]);
    assert!(second.runs[1].tokens > 0);
    assert!(
        app.task_check().unwrap().remaining_tokens < budget().max_tokens - first.runs[0].tokens
    );
    app.close().unwrap();
    crate::test_support::cleanup_tree(root.parent().unwrap());
}
#[test]
fn ga1_old_check_cannot_start_a_second_run_or_cross_a_session_boundary() {
    let (storage, root) = roots("ga1-concurrent");
    std::fs::create_dir_all(&root).unwrap();
    let mut app = mount(&root, &storage, TestBehavior::Success);
    configure_test_model(&app);
    app.task_create("one owner".into(), vec![], budget())
        .unwrap();
    let resume = task_request(&app);
    let stale = resume.clone();
    run(&mut app, resume).unwrap();
    assert!(app.task_validate_resume(&stale).is_err());
    let fresh = task_request(&app);
    app.new_session().unwrap();
    assert!(app.task_validate_resume(&fresh).is_err());
    assert!(app.task_state().unwrap().is_none());
    app.close().unwrap();
    crate::test_support::cleanup_tree(root.parent().unwrap());
}

#[test]
fn ga1_ordinary_run_cannot_see_or_forge_task_progress_tools() {
    struct Probe(AtomicUsize);
    impl TestModelScript for Probe {
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
            assert!(
                !r.tools
                    .iter()
                    .any(|t| matches!(t.name.as_str(), "read_task" | "update_task"))
            );
            assert!(
                !r.instructions
                    .unwrap_or_default()
                    .contains("Task recovery state")
            );
            if self.0.fetch_add(1, Ordering::SeqCst) == 0 {
                let call = crate::ToolCall {
                    id: "forged-task-read".into(),
                    name: "read_task".into(),
                    arguments: json!({}),
                };
                events.emit(crate::ModelEvent::ToolCallCompleted { call: call.clone() });
                let mut response = crate::test_support::response("", FinishReason::ToolCalls);
                response.tool_calls.push(call);
                return Ok(response);
            }
            assert!(r.items.iter().any(|i|matches!(i,ModelItem::ToolResult(result) if result.tool_name=="read_task"&&result.is_error)));
            Ok(crate::test_support::response(
                "ordinary answer",
                FinishReason::Completed,
            ))
        }
    }
    let (storage, root) = roots("ga1-ordinary-authority");
    std::fs::create_dir_all(&root).unwrap();
    let mut app = mount(
        &root,
        &storage,
        TestBehavior::Scripted(Arc::new(Probe(AtomicUsize::new(0)))),
    );
    configure_test_model(&app);
    let before = app
        .task_create("separate paused task".into(), vec![], budget())
        .unwrap();
    let (completion, rx) = mpsc::channel();
    let handle = app
        .start_run(ApplicationRunRequest {
            message: crate::message::PendingMessage::text("ordinary conversation"),
            asker: None,
            approver: Arc::new(CountingApprover(Arc::new(AtomicUsize::new(0)))),
            events: Box::new(SharedEvents(Arc::new(Mutex::new(Vec::new())))),
            completion,
        })
        .unwrap();
    rx.recv_timeout(std::time::Duration::from_secs(20))
        .unwrap()
        .unwrap();
    handle.join().unwrap();
    assert_eq!(app.task_state().unwrap().unwrap(), before);
    app.close().unwrap();
    crate::test_support::cleanup_tree(root.parent().unwrap());
}

#[test]
fn ga1_real_run_stops_before_another_model_request_when_intent_commit_fails() {
    struct WriteOnce(AtomicUsize);
    impl TestModelScript for WriteOnce {
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
            let n = self.0.fetch_add(1, Ordering::SeqCst);
            let mut response = crate::test_support::response("stop", FinishReason::Completed);
            if n == 0 {
                let call = crate::ToolCall {
                    id: "intent-failure".into(),
                    name: "write_file".into(),
                    arguments: json!({"path":"must-not-exist","content":"effect"}),
                };
                events.emit(crate::ModelEvent::ToolCallCompleted { call: call.clone() });
                response.tool_calls.push(call);
                response.finish_reason = FinishReason::ToolCalls;
            }
            Ok(response)
        }
    }
    struct FailAfterApproval(Arc<crate::session::use_cases::SessionService>);
    impl crate::EventSink for FailAfterApproval {
        fn emit(&mut self, event: crate::RunEvent) {
            if matches!(event, crate::RunEvent::ToolStarted { .. }) {
                self.0
                    .inject_persistence_faults(crate::session::persistence::FaultHooks {
                        fail_batch_write: true,
                        ..Default::default()
                    });
            }
        }
    }
    let (storage, root) = roots("ga1-real-intent-failure");
    std::fs::create_dir_all(&root).unwrap();
    let script = Arc::new(WriteOnce(AtomicUsize::new(0)));
    let mut app = mount(&root, &storage, TestBehavior::Scripted(script.clone()));
    configure_test_model(&app);
    app.task_create("one effect".into(), vec![], budget())
        .unwrap();
    let resume = task_request(&app);
    let (completion, rx) = mpsc::channel();
    let (handle, _) = app
        .start_task_run(
            resume,
            ApplicationRunRequest {
                message: crate::message::PendingMessage::text(""),
                asker: None,
                approver: Arc::new(CountingApprover(Arc::new(AtomicUsize::new(0)))),
                events: Box::new(FailAfterApproval(app.sessions.clone())),
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
    assert_eq!(
        script.0.load(Ordering::SeqCst),
        1,
        "a fatal task journal failure must not become another model turn"
    );
    assert!(!root.join("must-not-exist").exists());
    let s = app.task_state().unwrap().unwrap();
    assert!(s.operations.is_empty());
    assert!(s.runs[0].settled);
    assert!(s.runs[0].tokens > 0);
    app.close().unwrap();
    crate::test_support::cleanup_tree(root.parent().unwrap());
}

#[test]
fn ga1_ordinary_prompt_cannot_bypass_an_unresolved_task_operation() {
    let (storage, root) = roots("ga1-ordinary-unknown");
    std::fs::create_dir_all(&root).unwrap();
    let mut app = mount(&root, &storage, TestBehavior::Success);
    configure_test_model(&app);
    app.task_create("uncertain work".into(), vec![], budget())
        .unwrap();
    make_task_uncertain(&app);
    let (completion, _rx) = mpsc::channel();
    let start = app.start_run(ApplicationRunRequest {
        message: crate::message::PendingMessage::text("continue by normal conversation"),
        asker: None,
        approver: Arc::new(CountingApprover(Arc::new(AtomicUsize::new(0)))),
        events: Box::new(SharedEvents(Arc::new(Mutex::new(Vec::new())))),
        completion,
    });
    assert!(
        start.is_err(),
        "ordinary prompt admission must not bypass task recovery uncertainty"
    );
    app.close().unwrap();
    crate::test_support::cleanup_tree(root.parent().unwrap());
}

#[test]
fn ga1_rework_stale_skill_pre_admission_failure_does_not_strand_task() {
    let (storage, root) = roots("ga1-audit-start-failure");
    let skill = root.join(".clat/skills/audit-once/SKILL.md");
    std::fs::create_dir_all(skill.parent().unwrap()).unwrap();
    std::fs::write(
        &skill,
        "---\nname: audit-once\ndescription: audit fixture\n---\nCheck the task.\n",
    )
    .unwrap();
    let mut app = mount(&root, &storage, TestBehavior::Success);
    configure_test_model(&app);
    app.task_create("recover from failed start".into(), vec![], budget())
        .unwrap();
    app.arm_skill("audit-once").unwrap();
    std::fs::remove_file(&skill).unwrap();
    let resume = task_request(&app);
    let (completion, _rx) = mpsc::channel();
    let first = app.start_task_run(
        resume,
        ApplicationRunRequest {
            message: crate::message::PendingMessage::text("continue"),
            asker: None,
            approver: Arc::new(CountingApprover(Arc::new(AtomicUsize::new(0)))),
            events: Box::new(SharedEvents(Arc::new(Mutex::new(Vec::new())))),
            completion,
        },
    );
    let first_error = first
        .err()
        .expect("removed armed skill must reject before admission");
    eprintln!("first expected admission error: {first_error}");
    assert_eq!(app.sessions.active_turns().unwrap(), 0);
    let reserved = app.task_state().unwrap().unwrap().runs[0].clone();
    assert!(reserved.settled);
    assert_eq!(reserved.turn, None);
    assert_eq!(reserved.tokens, 0);
    let resume = task_request(&app);
    let (completion, rx) = mpsc::channel();
    let second = app.start_task_run(
        resume,
        ApplicationRunRequest {
            message: crate::message::PendingMessage::text("continue"),
            asker: None,
            approver: Arc::new(CountingApprover(Arc::new(AtomicUsize::new(0)))),
            events: Box::new(SharedEvents(Arc::new(Mutex::new(Vec::new())))),
            completion,
        },
    );
    let second_error = match second {
        Ok((h, _)) => {
            rx.recv_timeout(std::time::Duration::from_secs(10))
                .unwrap()
                .unwrap();
            h.join().unwrap();
            let state = app.task_state().unwrap().unwrap();
            assert_eq!(state.runs[0], reserved);
            assert_eq!(state.runs[1].turn, Some(1));
            assert_ne!(state.runs[1].id, reserved.id);
            assert!(state.runs[1].tokens > 0);
            assert_eq!(app.sessions.active_turns().unwrap(), 1);
            None
        }
        Err(e) => Some(e.to_string()),
    };
    app.close().unwrap();
    crate::test_support::cleanup_tree(root.parent().unwrap());
    assert!(
        second_error.is_none(),
        "recoverable pre-admission failure permanently stranded task: {second_error:?}"
    );
}

#[test]
fn ga1_rework_recovery_check_recognizes_verified_view_image() {
    let (storage, root) = roots("ga1-audit-vision");
    std::fs::create_dir_all(&root).unwrap();
    let mut app = mount(&root, &storage, TestBehavior::Success);
    configure_test_model(&app);
    let (config, _) = app.model_state().unwrap();
    assert!(config.capabilities.accepts_image_input());
    let access = crate::tool::ToolAccessPolicy::all().with_view_image(true);
    assert!(
        app.tools
            .definitions_for(&access)
            .iter()
            .any(|d| d.name == "view_image")
    );
    let s = app
        .task_create("inspect image".into(), vec![], budget())
        .unwrap();
    app.task_save(
        &s.id,
        s.revision,
        Progress {
            required_tools: vec!["view_image".into()],
            ..Default::default()
        },
    )
    .unwrap();
    let check = app.task_check().unwrap();
    app.close().unwrap();
    crate::test_support::cleanup_tree(root.parent().unwrap());
    assert!(
        check.missing_tools.is_empty(),
        "verified visual tool incorrectly marked missing: {:?}",
        check.missing_tools
    );
}

#[test]
fn ga1_rework_goal_cannot_bypass_an_unresolved_task_operation() {
    let (storage, root) = roots("ga1-ordinary-unknown");
    std::fs::create_dir_all(&root).unwrap();
    let mut app = mount(&root, &storage, TestBehavior::Success);
    configure_test_model(&app);
    app.task_create("uncertain work".into(), vec![], budget())
        .unwrap();
    make_task_uncertain(&app);
    let goal = app.goal_create(
        "continue through Goal",
        crate::goal::GoalAcceptance::User,
        crate::goal::GoalLimits {
            max_rounds: 1,
            ..Default::default()
        },
        true,
    );
    eprintln!(
        "goal creation while task unknown: {:?}",
        goal.as_ref().map(|g| &g.state.phase)
    );
    let mut bypass = false;
    if goal.is_ok() {
        let (completion, rx) = mpsc::channel();
        let started = app.start_goal_run(ApplicationRunRequest {
            message: crate::message::PendingMessage::text("continue through Goal"),
            asker: None,
            approver: Arc::new(CountingApprover(Arc::new(AtomicUsize::new(0)))),
            events: Box::new(SharedEvents(Arc::new(Mutex::new(Vec::new())))),
            completion,
        });
        if let Ok((h, _)) = started {
            let result = rx.recv_timeout(std::time::Duration::from_secs(10)).unwrap();
            h.join().unwrap();
            eprintln!("Goal Run executed despite unresolved operation: {result:?}");
            bypass = true;
        }
    }
    app.close().unwrap();
    crate::test_support::cleanup_tree(root.parent().unwrap());
    assert!(
        !bypass,
        "Goal bypassed unknown-operation recovery and started a model Run"
    );
}

fn make_task_uncertain(app: &TrustedProjectApplication) {
    use crate::tool::{ToolInvocation, ToolMiddleware, ToolNext};
    struct UncertainEffect;
    impl crate::Tool for UncertainEffect {
        fn definition(&self) -> crate::ToolDefinition {
            crate::ToolDefinition {
                name: "uncertain-effect".into(),
                description: "test".into(),
                effect: crate::ToolEffect::Write,
                input_schema: json!({}),
                strict: false,
            }
        }
        fn invoke(
            &self,
            _: &serde_json::Value,
            _: &Project,
            c: &crate::CancelToken,
        ) -> Result<serde_json::Value, crate::ToolError> {
            c.cancel();
            Ok(json!({"effect_started":true}))
        }
    }
    struct Invoke;
    impl ToolNext for Invoke {
        fn execute(&self, i: &ToolInvocation<'_>) -> Result<serde_json::Value, crate::ToolError> {
            i.tool.invoke(i.arguments, i.project, i.cancel)
        }
    }
    let c = app.task_check().unwrap();
    app.task.begin(&task_request(app), &c, None).unwrap();
    app.task.admit(1).unwrap();
    app.task
        .execute(
            &ToolInvocation {
                call_id: None,
                tool: &UncertainEffect,
                arguments: &json!({}),
                project: &app.project,
                cancel: &crate::CancelToken::new(),
            },
            &Invoke,
        )
        .unwrap();
    app.task.settle(5).unwrap();
    assert_eq!(
        app.task_state().unwrap().unwrap().operations[0].outcome,
        "unknown"
    );
}

struct ReworkCalls(AtomicUsize);
impl TestModelScript for ReworkCalls {
    fn stream(
        &self,
        _: ModelRequest<'_>,
        _: &mut dyn ModelEventSink,
    ) -> Result<ModelResponse, ModelError> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(crate::test_support::response(
            "done",
            FinishReason::Completed,
        ))
    }
}
fn rework_request() -> ApplicationRunRequest {
    let (completion, _rx) = mpsc::channel();
    ApplicationRunRequest {
        message: crate::message::PendingMessage::text("must not execute"),
        asker: None,
        approver: Arc::new(CountingApprover(Arc::new(AtomicUsize::new(0)))),
        events: Box::new(SharedEvents(Arc::new(Mutex::new(Vec::new())))),
        completion,
    }
}
#[test]
fn ga1_rework_unfinished_task_rejects_both_goal_creation_modes_before_any_run() {
    let (storage, root) = roots("ga1-rework-goal-create");
    std::fs::create_dir_all(&root).unwrap();
    let calls = Arc::new(ReworkCalls(AtomicUsize::new(0)));
    let mut app = mount(&root, &storage, TestBehavior::Scripted(calls.clone()));
    configure_test_model(&app);
    let task = app
        .task_create("unfinished task".into(), vec![], budget())
        .unwrap();
    for arm in [false, true] {
        assert!(
            app.goal_create(
                "competing goal",
                crate::goal::GoalAcceptance::User,
                crate::goal::GoalLimits::default(),
                arm
            )
            .is_err()
        );
        assert!(app.goal().unwrap().is_none());
        assert_eq!(app.task_state().unwrap().unwrap(), task);
    }
    assert_eq!(app.sessions.active_turns().unwrap(), 0);
    assert_eq!(calls.0.load(Ordering::SeqCst), 0);
    app.task_finish(&task.id, task.revision, "complete")
        .unwrap();
    assert!(
        app.goal_create(
            "allowed after task completion",
            crate::goal::GoalAcceptance::User,
            crate::goal::GoalLimits::default(),
            false
        )
        .is_ok()
    );
    let goal = app.goal().unwrap().unwrap();
    assert!(
        app.task_create("cannot replace unfinished Goal".into(), vec![], budget())
            .is_err()
    );
    app.goal_arm(goal.state.revision).unwrap();
    assert!(
        app.task_create("still excluded while armed".into(), vec![], budget())
            .is_err()
    );
    app.goal_clear(goal.state.revision).unwrap();
    assert!(
        app.task_create("allowed after Goal clear".into(), vec![], budget())
            .is_ok()
    );
    app.close().unwrap();
    crate::test_support::cleanup_tree(root.parent().unwrap());
}
#[test]
fn ga1_rework_cold_conflicting_goal_cannot_resume_arm_or_execute() {
    for paused in [false, true] {
        let (storage, root) = roots("ga1-rework-old-goal");
        std::fs::create_dir_all(&root).unwrap();
        let calls = Arc::new(ReworkCalls(AtomicUsize::new(0)));
        let mut app = mount(&root, &storage, TestBehavior::Scripted(calls.clone()));
        configure_test_model(&app);
        app.task_create("uncertain task".into(), vec![], budget())
            .unwrap();
        make_task_uncertain(&app);
        // Seed the durable mixed state accepted by the pre-fix Application.
        // Production callers cannot bypass the Application exclusion this way.
        let mut goal = app
            .goal
            .create(
                "old conflicting goal",
                crate::goal::GoalAcceptance::User,
                crate::goal::GoalLimits::default(),
                false,
            )
            .unwrap();
        if paused {
            goal = app.goal.pause(goal.state.revision).unwrap();
        }
        app.close().unwrap();
        let mut app = crate::test_support::reopen_after_close(|| {
            Ok::<_, ApplicationError>(mount(
                &root,
                &storage,
                TestBehavior::Scripted(calls.clone()),
            ))
        });
        let before = app.task_state().unwrap().unwrap();
        assert!(!app.goal().unwrap().unwrap().armed);
        assert!(app.check_goal_activation().is_err());
        assert!(app.goal_resume(goal.state.revision).is_err());
        assert!(app.goal_arm(goal.state.revision).is_err());
        assert!(app.start_goal_run(rework_request()).is_err());
        assert_eq!(app.task_state().unwrap().unwrap(), before);
        assert_eq!(app.sessions.active_turns().unwrap(), 0);
        assert_eq!(calls.0.load(Ordering::SeqCst), 0);
        // Removing uncertainty alone must not remove workflow exclusion.
        let op = &before.operations[0];
        app.task_resolve(
            &before.id,
            before.revision,
            &op.id,
            "not_executed",
            "fixture inspected",
        )
        .unwrap();
        assert!(app.goal_resume(goal.state.revision).is_err());
        assert!(app.goal_arm(goal.state.revision).is_err());
        if !paused {
            // Even stale process-local arming cannot grant authority over a task.
            app.goal.arm(goal.state.revision).unwrap();
        }
        assert!(app.start_goal_run(rework_request()).is_err());
        assert_eq!(calls.0.load(Ordering::SeqCst), 0);
        app.close().unwrap();
        crate::test_support::cleanup_tree(root.parent().unwrap());
    }
}
#[test]
fn ga1_rework_crash_between_reservation_and_binding_keeps_budget_and_retries_new_identity() {
    for admitted in [false, true] {
        let (storage, root) = roots("ga1-rework-reservation-crash");
        std::fs::create_dir_all(&root).unwrap();
        let mut app = mount(&root, &storage, TestBehavior::Success);
        configure_test_model(&app);
        app.task_create("survive admission gap".into(), vec![], budget())
            .unwrap();
        let check = app.task_check().unwrap();
        app.task.begin(&task_request(&app), &check, None).unwrap();
        if admitted {
            use crate::session::{event::payloads, run_journal::NewSessionEvent};
            // Commit the same admission facts, then drop the app before task binding.
            app.sessions
                .journal()
                .unwrap()
                .append_atomic_durable(&[
                    NewSessionEvent::new("turn/start", payloads::turn_start(1)),
                    NewSessionEvent::new(
                        "user/message",
                        payloads::user_message("committed before crash"),
                    )
                    .append(Vec::new()),
                ])
                .unwrap();
        }
        let old = app.task_state().unwrap().unwrap();
        assert_eq!(old.runs[0].turn, None);
        assert!(!old.runs[0].settled);
        // The live stats count completed turns, not the unclosed admission.
        assert_eq!(app.sessions.active_turns().unwrap(), 0);
        app.close().unwrap();
        let mut app = crate::test_support::reopen_after_close(|| {
            Ok::<_, ApplicationError>(mount(&root, &storage, TestBehavior::Success))
        });
        let check = app.task_check().unwrap();
        assert_eq!(app.sessions.active_turns().unwrap(), u64::from(admitted));
        assert_eq!(check.task, old);
        assert!(check.unknown_consumption);
        assert_eq!(
            check.remaining_tokens,
            old.budget.max_tokens - old.runs[0].tokens
        );
        let resume = task_request(&app);
        run(&mut app, resume).unwrap();
        let after = app.task_state().unwrap().unwrap();
        assert_eq!(after.runs[0], old.runs[0]);
        assert_ne!(after.runs[0].id, after.runs[1].id);
        assert_eq!(after.runs[1].turn, Some(1 + u64::from(admitted)));
        assert!(after.runs[1].settled && after.runs[1].tokens > 0);
        assert_eq!(
            app.sessions.active_turns().unwrap(),
            1 + u64::from(admitted)
        );
        assert_eq!(
            after.remaining().0,
            after.budget.max_tokens - old.runs[0].tokens - after.runs[1].tokens
        );
        app.close().unwrap();
        crate::test_support::cleanup_tree(root.parent().unwrap());
    }
}
#[test]
fn ga1_rework_model_switch_invalidates_check_and_matches_actual_tool_visibility() {
    let (storage, root) = roots("ga1-rework-visibility");
    std::fs::create_dir_all(&root).unwrap();
    let mut app = mount(&root, &storage, TestBehavior::Success);
    configure_test_model(&app);
    let (vision, credentials) = app.model_state().unwrap();
    let s = app
        .task_create("inspect image".into(), vec![], budget())
        .unwrap();
    app.task_save(
        &s.id,
        s.revision,
        Progress {
            required_tools: vec!["view_image".into()],
            ..Default::default()
        },
    )
    .unwrap();
    let mut unverified = vision.clone();
    unverified.capabilities.image_input_verified = false;
    let mut text = vision.clone();
    text.capabilities = crate::model::ModelCapabilities::default();
    let mut no_result = vision.clone();
    no_result.capabilities.tool_result_modalities = vec![crate::model::Modality::Text];
    let mut other_vision = vision.clone();
    other_vision.model = "different-verified-visual-model".into();
    for config in [unverified, text, no_result, other_vision] {
        app.save_model_state(&vision, &credentials).unwrap();
        let old = task_request(&app);
        app.task_validate_resume(&old).unwrap();
        app.save_model_state(&config, &credentials).unwrap();
        let check = app.task_check().unwrap();
        let skills = app.skills.snapshot().unwrap();
        let context = app.run_context_snapshot(
            &config,
            skills,
            None,
            app.memory.injection("inspection").unwrap(),
            app.goal.injection().unwrap(),
            Some("task".into()),
        );
        let available = context
            .tool_definitions
            .iter()
            .any(|t| t.name == "view_image");
        assert_eq!(available, config.capabilities.accepts_image_tool_results());
        assert_eq!(check.missing_tools.is_empty(), available);
        assert!(
            app.task_validate_resume(&old).is_err(),
            "model changes must invalidate prior authorization"
        );
        let new = task_request(&app);
        assert_eq!(app.task_validate_resume(&new).is_ok(), available);
    }
    assert_eq!(app.sessions.active_turns().unwrap(), 0);
    app.close().unwrap();
    crate::test_support::cleanup_tree(root.parent().unwrap());
}
