use super::state::*;
use super::*;
use crate::session::event::SessionEvent;
use crate::session::key::ProjectKey;
use crate::session::persistence::{FaultHooks, JsonlCompression};
use crate::session::projection::{CheckpointIdentity, ProjectionRegistry};
use crate::session::use_cases::SessionService;
use crate::tool::{ToolInvocation, ToolMiddleware, ToolNext};
use crate::{CancelToken, Project, Tool, ToolDefinition, ToolEffect, ToolError};
use serde_json::{Value, json};
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
fn budget() -> TaskBudget {
    TaskBudget {
        max_tokens: 10000,
        max_runs: 8,
        max_seconds: 60,
        run_tokens: 1000,
        run_seconds: 10,
    }
}
struct Fixture {
    task: TaskService,
    root: std::path::PathBuf,
}
impl Fixture {
    fn new() -> Self {
        let (storage, root) = crate::test_support::roots("ga1-service");
        std::fs::create_dir_all(&root).unwrap();
        let root = root.canonicalize().unwrap();
        std::fs::create_dir_all(storage.join("sessions")).unwrap();
        let sessions = Arc::new(
            SessionService::new(storage.join("sessions"), JsonlCompression::Zstd).unwrap(),
        );
        sessions
            .new_session(&ProjectKey::from_cwd(&root.to_string_lossy()))
            .unwrap();
        let task = TaskService::new(sessions, Project::new(&root)).unwrap();
        task.create(
            "finish report".into(),
            vec!["human checks content".into()],
            budget(),
        )
        .unwrap();
        Self { task, root }
    }
    fn resume(&self) -> TaskResume {
        let c = self.task.check("read-only".into(), &[]).unwrap();
        TaskResume {
            task_id: c.task.id,
            revision: c.task.revision,
            check_digest: c.digest,
            accept_changes: false,
        }
    }
    fn begin(&self, cap: u64) {
        let c = self.task.check("read-only".into(), &[]).unwrap();
        self.task.begin(&self.resume(), &c, Some(cap)).unwrap();
        self.task.admit(1).unwrap();
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = self.task.sessions.quiesce_active();
        crate::test_support::cleanup_tree(self.root.parent().unwrap());
    }
}
struct Effect {
    count: AtomicUsize,
    cancel: bool,
    faults: Option<Arc<SessionService>>,
}
impl Tool for Effect {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "effect".into(),
            description: "test".into(),
            input_schema: json!({}),
            effect: ToolEffect::Write,
            strict: false,
        }
    }
    fn invoke(&self, _: &Value, _: &Project, c: &CancelToken) -> Result<Value, ToolError> {
        self.count.fetch_add(1, Ordering::SeqCst);
        if self.cancel {
            c.cancel();
        }
        if let Some(s) = &self.faults {
            s.inject_persistence_faults(FaultHooks {
                fail_batch_write: true,
                ..Default::default()
            });
        }
        Ok(json!({"saved":true}))
    }
}
struct Invoke;
impl ToolNext for Invoke {
    fn execute(&self, i: &ToolInvocation<'_>) -> Result<Value, ToolError> {
        i.tool.invoke(i.arguments, i.project, i.cancel)
    }
}
fn invoke(f: &Fixture, tool: &Effect, cancel: &CancelToken) -> Result<Value, ToolError> {
    f.task.execute(
        &ToolInvocation {
            call_id: None,
            tool,
            arguments: &json!({"private":"not retained"}),
            project: &f.task.project,
            cancel,
        },
        &Invoke,
    )
}
#[test]
fn ga1_intent_and_result_commit_windows_are_conservative() {
    let f = Fixture::new();
    f.begin(1000);
    let effect = Effect {
        count: AtomicUsize::new(0),
        cancel: false,
        faults: None,
    };
    let cancelled = CancelToken::new();
    cancelled.cancel();
    assert!(invoke(&f, &effect, &cancelled).is_err());
    assert!(f.task.current().unwrap().unwrap().operations.is_empty());
    invoke(&f, &effect, &CancelToken::new()).unwrap();
    let state = f.task.current().unwrap().unwrap();
    assert_eq!(state.operations[0].outcome, "confirmed");
    assert_eq!(effect.count.load(Ordering::SeqCst), 1);
    assert!(
        !serde_json::to_string(&state)
            .unwrap()
            .contains("not retained")
    );
    f.task.settle(33).unwrap();
    assert_eq!(f.task.current().unwrap().unwrap().remaining().0, 9967);
}
#[test]
fn ga1_intent_persistence_failure_never_invokes() {
    let f = Fixture::new();
    f.begin(1000);
    f.task.sessions.inject_persistence_faults(FaultHooks {
        fail_batch_write: true,
        ..Default::default()
    });
    let e = Effect {
        count: AtomicUsize::new(0),
        cancel: false,
        faults: None,
    };
    assert!(invoke(&f, &e, &CancelToken::new()).unwrap_err().is_fatal());
    assert_eq!(e.count.load(Ordering::SeqCst), 0);
    f.task
        .sessions
        .inject_persistence_faults(FaultHooks::default());
    assert!(f.task.current().unwrap().unwrap().operations.is_empty());
}
#[test]
fn ga1_result_persistence_failure_retains_unknown_not_failure() {
    let f = Fixture::new();
    f.begin(1000);
    let e = Effect {
        count: AtomicUsize::new(0),
        cancel: false,
        faults: Some(f.task.sessions.clone()),
    };
    assert!(invoke(&f, &e, &CancelToken::new()).unwrap_err().is_fatal());
    assert_eq!(e.count.load(Ordering::SeqCst), 1);
    f.task
        .sessions
        .inject_persistence_faults(FaultHooks::default());
    let s = f.task.current().unwrap().unwrap();
    assert_eq!(s.operations[0].outcome, "unknown");
    f.task.settle(7).unwrap();
    let c = f.task.check("read-only".into(), &[]).unwrap();
    assert!(f.task.verify_check(&f.resume(), &c).is_err());
    let s = f.task.current().unwrap().unwrap();
    f.task
        .resolve(
            &s.id,
            s.revision,
            &s.operations[0].id,
            "confirmed",
            "checked actual result",
        )
        .unwrap();
    assert_eq!(e.count.load(Ordering::SeqCst), 1);
}
#[test]
fn ga1_cancel_during_or_after_effect_keeps_unknown() {
    let f = Fixture::new();
    f.begin(1000);
    let e = Effect {
        count: AtomicUsize::new(0),
        cancel: true,
        faults: None,
    };
    invoke(&f, &e, &CancelToken::new()).unwrap();
    assert_eq!(
        f.task.current().unwrap().unwrap().operations[0].outcome,
        "unknown"
    );
    assert_eq!(e.count.load(Ordering::SeqCst), 1);
}
#[test]
fn ga1_progress_commit_failure_and_stale_cas_never_publish() {
    let f = Fixture::new();
    let s = f.task.current().unwrap().unwrap();
    let p = Progress {
        summary: "claim only".into(),
        ..Default::default()
    };
    f.task.sessions.inject_persistence_faults(FaultHooks {
        fail_batch_write: true,
        ..Default::default()
    });
    assert!(f.task.save(&s.id, s.revision, p.clone(), false).is_err());
    f.task
        .sessions
        .inject_persistence_faults(FaultHooks::default());
    assert_eq!(f.task.current().unwrap().unwrap(), s);
    let changed = f.task.save(&s.id, s.revision, p.clone(), false).unwrap();
    assert_eq!(changed.phase, "paused");
    assert!(f.task.save(&s.id, s.revision, p, false).is_err());
    assert_eq!(f.task.current().unwrap().unwrap(), changed);
}
#[test]
fn ga1_changed_missing_restricted_and_stale_recovery_are_distinct() {
    let f = Fixture::new();
    let s = f.task.current().unwrap().unwrap();
    for name in ["good", "changed", "gone"] {
        std::fs::write(f.root.join(name), b"original").unwrap();
    }
    let artifacts = ["good", "changed", "gone"]
        .iter()
        .map(|p| Artifact {
            path: (*p).into(),
            summary: "result".into(),
            digest: digest(b"original"),
            evidence: vec![],
        })
        .collect();
    f.task
        .save(
            &s.id,
            s.revision,
            Progress {
                artifacts,
                ..Default::default()
            },
            false,
        )
        .unwrap();
    let old = f.resume();
    std::fs::write(f.root.join("changed"), b"new").unwrap();
    std::fs::remove_file(f.root.join("gone")).unwrap();
    let c = f.task.check("read-only".into(), &[]).unwrap();
    assert_eq!(
        c.artifacts
            .iter()
            .map(|a| a["status"].as_str().unwrap())
            .collect::<Vec<_>>(),
        vec!["unchanged", "changed", "missing"]
    );
    assert!(f.task.verify_check(&old, &c).is_err());
    let mut request = f.resume();
    assert!(f.task.verify_check(&request, &c).is_err());
    request.accept_changes = true;
    f.task.verify_check(&request, &c).unwrap();
    let mode = f.task.check("full-access".into(), &[]).unwrap();
    assert!(f.task.verify_check(&request, &mode).is_err());
}
#[cfg(unix)]
#[test]
fn ga1_symlink_replacement_and_parent_escape_never_read_other_projects() {
    use std::os::unix::fs::symlink;
    let f = Fixture::new();
    std::fs::write(f.root.join("file"), b"original").unwrap();
    let s = f.task.current().unwrap().unwrap();
    f.task
        .save(
            &s.id,
            s.revision,
            Progress {
                artifacts: vec![Artifact {
                    path: "file".into(),
                    summary: "x".into(),
                    digest: digest(b"original"),
                    evidence: vec![],
                }],
                ..Default::default()
            },
            false,
        )
        .unwrap();
    std::fs::remove_file(f.root.join("file")).unwrap();
    symlink("/etc/passwd", f.root.join("file")).unwrap();
    assert_eq!(
        f.task.check("mode".into(), &[]).unwrap().artifacts[0]["status"],
        "restricted"
    );
    symlink("/etc", f.root.join("linked")).unwrap();
    assert!(f.task.read_artifact("linked/passwd").is_err());
    assert!(f.task.read_artifact("../outside").is_err());
}
#[test]
fn ga1_full_fold_checkpoint_tail_and_legacy_no_task_are_equal() {
    let f = Fixture::new();
    let s = f.task.current().unwrap().unwrap();
    let mut p = ProjectionRegistry::clat();
    let event = SessionEvent::new("clat/task", 0, 1, serde_json::to_value(&s).unwrap());
    p.fold_one(&event).unwrap();
    let identity = CheckpointIdentity {
        created_at: 1,
        cwd: Some(s.project.clone()),
    };
    let record = p.checkpoint(identity.clone(), 1);
    let mut restored = ProjectionRegistry::clat();
    restored.restore(&record, &[], 1).unwrap();
    let mut next = s.clone();
    next.revision += 1;
    next.progress.questions.push("verify report totals".into());
    let event = SessionEvent::new("clat/task", 1, 2, serde_json::to_value(&next).unwrap());
    p.fold_one(&event).unwrap();
    restored.fold_one(&event).unwrap();
    assert_eq!(p.state_snapshot("task"), restored.state_snapshot("task"));
    assert_eq!(
        ProjectionRegistry::clat().state_snapshot("task"),
        Some(Value::Null)
    );
    next.revision += 2;
    assert!(
        p.fold_one(&SessionEvent::new(
            "clat/task",
            2,
            3,
            serde_json::to_value(next).unwrap()
        ))
        .is_err()
    );
}
#[test]
fn ga1_crash_reservations_are_not_zeroed_and_no_local_ownership_is_revived() {
    let f = Fixture::new();
    f.begin(1000);
    let s = f.task.current().unwrap().unwrap();
    // Lose only the process-local binding, as on restart; the journal stays authoritative.
    *f.task.live.lock().unwrap() = None;
    let c = f.task.check("mode".into(), &[]).unwrap();
    assert!(c.unknown_consumption);
    assert_eq!(c.remaining_tokens, 9000);
    assert_eq!(c.remaining_ms, 50000);
    let resume = TaskResume {
        task_id: c.task.id.clone(),
        revision: c.task.revision,
        check_digest: c.digest.clone(),
        accept_changes: false,
    };
    f.task.verify_check(&resume, &c).unwrap();
    assert!(f.task.cap().is_none());
    assert_eq!(f.task.current().unwrap().unwrap(), s);
}
#[test]
fn ga1_external_jobs_query_only_the_recorded_identity() {
    struct Query;
    impl JobQuery for Query {
        fn query(
            &self,
            j: &JobReference,
            deadline: std::time::Instant,
            c: &CancelToken,
        ) -> JobObservation {
            assert!(!c.is_cancelled());
            assert!(std::time::Instant::now() < deadline);
            assert_eq!((&*j.provider, &*j.scope), ("fake", "local-test"));
            match j.id.as_str() {
                "run" => JobObservation::Running,
                "done" => JobObservation::Complete {
                    result_ref: j.result_ref.clone(),
                },
                "fail" => JobObservation::Failed,
                _ => JobObservation::Expired,
            }
        }
    }
    let f = Fixture::new();
    let s = f.task.current().unwrap().unwrap();
    let jobs = ["run", "done", "fail", "old"]
        .iter()
        .map(|id| JobReference {
            provider: "fake".into(),
            scope: "local-test".into(),
            id: (*id).into(),
            result_ref: "results/report".into(),
            last_observation: "queued".into(),
        })
        .collect();
    f.task
        .save(
            &s.id,
            s.revision,
            Progress {
                jobs,
                ..Default::default()
            },
            false,
        )
        .unwrap();
    assert!(
        f.task
            .check("mode".into(), &[])
            .unwrap()
            .jobs
            .iter()
            .all(|j| *j == JobObservation::Unavailable)
    );
    f.task.set_query(Some(Arc::new(Query)));
    assert_eq!(
        f.task.check("mode".into(), &[]).unwrap().jobs,
        vec![
            JobObservation::Running,
            JobObservation::Complete {
                result_ref: "results/report".into()
            },
            JobObservation::Failed,
            JobObservation::Expired
        ]
    );
}
#[test]
fn ga1_task_identity_session_terminal_and_capacity_guards() {
    let f = Fixture::new();
    let s = f.task.current().unwrap().unwrap();
    assert!(
        f.task
            .save("other", s.revision, Progress::default(), false)
            .is_err()
    );
    assert!(
        f.task
            .save(&s.id, s.revision, Progress::default(), true)
            .is_err()
    );
    assert!(
        f.task
            .save(
                &s.id,
                s.revision,
                Progress {
                    questions: vec!["x".into(); 33],
                    ..Default::default()
                },
                false
            )
            .is_err()
    );
    f.task.finish(&s.id, s.revision, "complete").unwrap();
    assert!(
        f.task
            .verify_check(&f.resume(), &f.task.check("mode".into(), &[]).unwrap())
            .is_err()
    );
    let new = f
        .task
        .create("new explicit task".into(), vec![], budget())
        .unwrap();
    assert_ne!(s.id, new.id);
    f.task.sessions.quiesce_active().unwrap();
    f.task
        .sessions
        .new_session(&ProjectKey::from_cwd(&f.root.to_string_lossy()))
        .unwrap();
    assert!(f.task.current().unwrap().is_none());
    assert!(
        f.task
            .save(&new.id, new.revision, Progress::default(), false)
            .is_err()
    );
}

#[test]
fn ga1_known_effect_is_not_replayed_in_a_later_run() {
    let f = Fixture::new();
    f.begin(1000);
    let e = Effect {
        count: AtomicUsize::new(0),
        cancel: false,
        faults: None,
    };
    invoke(&f, &e, &CancelToken::new()).unwrap();
    f.task.settle(5).unwrap();
    let c = f.task.check("read-only".into(), &[]).unwrap();
    f.task.begin(&f.resume(), &c, Some(1000)).unwrap();
    f.task.admit(2).unwrap();
    assert!(invoke(&f, &e, &CancelToken::new()).unwrap_err().is_fatal());
    assert_eq!(e.count.load(Ordering::SeqCst), 1);
}
#[test]
fn ga1_result_failure_poisons_nested_followup_even_after_io_recovers() {
    let f = Fixture::new();
    f.begin(1000);
    let e = Effect {
        count: AtomicUsize::new(0),
        cancel: false,
        faults: Some(f.task.sessions.clone()),
    };
    assert!(invoke(&f, &e, &CancelToken::new()).is_err());
    f.task
        .sessions
        .inject_persistence_faults(FaultHooks::default());
    let later = Effect {
        count: AtomicUsize::new(0),
        cancel: false,
        faults: None,
    };
    assert!(
        invoke(&f, &later, &CancelToken::new())
            .unwrap_err()
            .is_fatal()
    );
    assert_eq!(later.count.load(Ordering::SeqCst), 0);
}
#[test]
fn ga1_cancel_after_durable_intent_before_actual_invoke_is_not_executed() {
    struct CancelAtIntent {
        cancel: CancelToken,
        definitions: AtomicUsize,
        effects: AtomicUsize,
    }
    impl Tool for CancelAtIntent {
        fn definition(&self) -> ToolDefinition {
            // First definition is effect classification; second is inside intent creation.
            if self.definitions.fetch_add(1, Ordering::SeqCst) == 1 {
                self.cancel.cancel();
            }
            ToolDefinition {
                name: "intent-cancel".into(),
                description: "test".into(),
                effect: ToolEffect::Write,
                input_schema: json!({}),
                strict: false,
            }
        }
        fn invoke(&self, _: &Value, _: &Project, _: &CancelToken) -> Result<Value, ToolError> {
            self.effects.fetch_add(1, Ordering::SeqCst);
            Ok(json!({}))
        }
    }
    let f = Fixture::new();
    f.begin(1000);
    let cancel = CancelToken::new();
    let tool = CancelAtIntent {
        cancel: cancel.clone(),
        definitions: AtomicUsize::new(0),
        effects: AtomicUsize::new(0),
    };
    assert!(
        f.task
            .execute(
                &ToolInvocation {
                    call_id: None,
                    tool: &tool,
                    arguments: &json!({}),
                    project: &f.task.project,
                    cancel: &cancel
                },
                &Invoke
            )
            .is_err()
    );
    assert_eq!(tool.effects.load(Ordering::SeqCst), 0);
    assert_eq!(
        f.task.current().unwrap().unwrap().operations[0].outcome,
        "not_executed"
    );
}
#[test]
fn ga1_budget_exhaustion_and_failed_settlement_keep_durable_debits() {
    let f = Fixture::new();
    f.begin(1000);
    f.task.sessions.inject_persistence_faults(FaultHooks {
        fail_batch_write: true,
        ..Default::default()
    });
    assert!(f.task.settle(3).is_err());
    f.task
        .sessions
        .inject_persistence_faults(FaultHooks::default());
    let c = f.task.check("read-only".into(), &[]).unwrap();
    assert!(c.unknown_consumption);
    assert_eq!(c.remaining_tokens, 9000);
    let request = TaskResume {
        task_id: c.task.id.clone(),
        revision: c.task.revision,
        check_digest: c.digest.clone(),
        accept_changes: false,
    };
    f.task.begin(&request, &c, Some(1000)).unwrap();
    f.task.admit(2).unwrap();
    f.task.settle(9000).unwrap();
    let c = f.task.check("read-only".into(), &[]).unwrap();
    assert_eq!(c.remaining_tokens, 0);
    assert!(f.task.verify_check(&f.resume(), &c).is_err());
}
#[test]
fn ga1_golden_catalog_admission_fold_and_checkpoint_use_one_payload() {
    let raw = include_str!("../../tests/fixtures/task-state.json");
    let state = decode_change(&serde_json::from_str(raw).unwrap()).unwrap();
    assert_eq!(
        format!("{}\n", serde_json::to_string_pretty(&state).unwrap()),
        raw
    );
    let mut event = SessionEvent::new("clat/task", 0, 1, serde_json::to_value(&state).unwrap());
    event.ignorable = Some(true);
    crate::session::catalog::event_spec("clat/task")
        .unwrap()
        .validate(&event, 4)
        .unwrap();
    let mut p = ProjectionRegistry::clat();
    p.fold_one(&event).unwrap();
    assert_eq!(p.state_snapshot("task"), Some(event.data));
    let mut invalid = state;
    invalid.runs.push(super::state::TaskRun {
        id: "forged".into(),
        turn: Some(1),
        tokens: 0,
        elapsed_ms: 0,
        settled: true,
    });
    invalid.revision += 1;
    assert!(
        p.fold_one(&SessionEvent::new(
            "clat/task",
            1,
            2,
            serde_json::to_value(invalid).unwrap()
        ))
        .is_err()
    );
}

#[test]
fn ga1_job_queries_share_an_absolute_deadline_and_external_cancellation() {
    struct SlowQuery(AtomicUsize);
    impl JobQuery for SlowQuery {
        fn query(
            &self,
            _: &JobReference,
            deadline: std::time::Instant,
            cancel: &CancelToken,
        ) -> JobObservation {
            self.0.fetch_add(1, Ordering::SeqCst);
            while std::time::Instant::now() < deadline && !cancel.is_cancelled() {
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
            assert!(cancel.is_cancelled());
            JobObservation::Unavailable
        }
    }
    let f = Fixture::new();
    let s = f.task.current().unwrap().unwrap();
    let jobs = vec![
        JobReference {
            provider: "fake".into(),
            scope: "test".into(),
            id: "running".into(),
            result_ref: String::new(),
            last_observation: "running".into()
        };
        2
    ];
    let mut jobs = jobs;
    jobs[1].id = "other-running".into();
    f.task
        .save(
            &s.id,
            s.revision,
            Progress {
                jobs,
                ..Default::default()
            },
            false,
        )
        .unwrap();
    let query = Arc::new(SlowQuery(AtomicUsize::new(0)));
    f.task.set_query(Some(query.clone()));
    let cancel = CancelToken::new();
    cancel.cancel();
    assert!(
        f.task
            .check_with_cancel("mode".into(), &[], &cancel)
            .unwrap()
            .jobs
            .iter()
            .all(|j| *j == JobObservation::Unavailable)
    );
    assert_eq!(query.0.load(Ordering::SeqCst), 0);
    f.task.check("mode".into(), &[]).unwrap();
    assert_eq!(
        query.0.load(Ordering::SeqCst),
        1,
        "one deadline covers all queries"
    );
}

#[test]
fn ga1_two_continuations_cannot_both_claim_one_task_revision() {
    let f = Fixture::new();
    let check = f.task.check("read-only".into(), &[]).unwrap();
    let request = f.resume();
    let released = std::sync::atomic::AtomicBool::new(false);
    let results = std::thread::scope(|scope| {
        let spawn = || {
            scope.spawn(|| {
                let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
                while !released.load(Ordering::SeqCst) && std::time::Instant::now() < deadline {
                    std::thread::sleep(std::time::Duration::from_millis(1));
                }
                assert!(released.load(Ordering::SeqCst));
                f.task.begin(&request, &check, Some(1000)).is_ok()
            })
        };
        let first = spawn();
        let second = spawn();
        released.store(true, Ordering::SeqCst);
        [first.join().unwrap(), second.join().unwrap()]
    });
    assert_eq!(results.iter().filter(|ok| **ok).count(), 1);
    assert_eq!(f.task.current().unwrap().unwrap().runs.len(), 1);
}

#[cfg(unix)]
#[test]
fn ga1_replaced_project_root_cannot_rebind_old_task_even_with_identical_files() {
    let f = Fixture::new();
    let moved = f.root.with_extension("moved");
    std::fs::rename(&f.root, &moved).unwrap();
    std::fs::create_dir(&f.root).unwrap();
    assert!(f.task.current().is_err());
    let another = TaskService::new(f.task.sessions.clone(), Project::new(&f.root)).unwrap();
    assert!(another.current().is_err());
    std::fs::remove_dir(&f.root).unwrap();
    std::fs::rename(&moved, &f.root).unwrap();
    assert!(f.task.current().unwrap().is_some());
}

#[test]
fn ga1_show_and_recovery_check_do_not_rewrite_journal_or_checkpoint_files() {
    fn files(
        root: &std::path::Path,
    ) -> std::collections::BTreeMap<std::path::PathBuf, (Vec<u8>, std::time::SystemTime)> {
        fn visit(
            path: &std::path::Path,
            all: &mut std::collections::BTreeMap<
                std::path::PathBuf,
                (Vec<u8>, std::time::SystemTime),
            >,
        ) {
            for entry in std::fs::read_dir(path).unwrap() {
                let entry = entry.unwrap();
                let metadata = entry.metadata().unwrap();
                if metadata.is_dir() {
                    visit(&entry.path(), all);
                } else if metadata.is_file() {
                    all.insert(
                        entry.path(),
                        (
                            std::fs::read(entry.path()).unwrap(),
                            metadata.modified().unwrap(),
                        ),
                    );
                }
            }
        }
        let mut all = std::collections::BTreeMap::new();
        visit(root, &mut all);
        all
    }
    let f = Fixture::new();
    let before = files(f.root.parent().unwrap());
    f.task.current().unwrap();
    f.task.check("read-only".into(), &[]).unwrap();
    assert_eq!(
        before,
        files(f.root.parent().unwrap()),
        "task reads must not publish a new checkpoint generation"
    );
}

#[test]
fn ga1_oversized_artifact_attempts_consume_the_shared_read_budget() {
    let f = Fixture::new();
    std::fs::write(f.root.join("oversized"), b"abc").unwrap();
    let mut remaining = 2;
    assert!(
        f.task
            .read_artifact_budget("oversized", &mut remaining)
            .is_err()
    );
    assert_eq!(
        remaining, 0,
        "failed/oversized reads must not get a fresh allowance for the next reference"
    );
    assert!(
        f.task
            .read_artifact_budget("oversized", &mut remaining)
            .is_err()
    );
}

#[test]
fn ga1_rework_session_admission_is_a_separate_durable_unique_binding() {
    let f = Fixture::new();
    let mut snapshots = vec![f.task.current().unwrap().unwrap()];
    let c = f.task.check("read-only".into(), &[]).unwrap();
    f.task.begin(&f.resume(), &c, Some(1000)).unwrap();
    snapshots.push(f.task.current().unwrap().unwrap());
    assert_eq!(snapshots[1].runs[0].turn, None);
    let mut premature = snapshots[1].clone();
    premature.runs[0].turn = Some(1);
    assert!(
        super::state::fold_change(
            &mut Some(snapshots[0].clone()),
            &serde_json::to_value(premature).unwrap()
        )
        .is_err(),
        "a reservation cannot claim future session admission"
    );
    let mut forged = snapshots[1].clone();
    forged.revision += 1;
    forged.phase = "paused".into();
    forged.runs[0].turn = Some(1);
    forged.runs[0].settled = true;
    assert!(
        super::state::fold_change(
            &mut Some(snapshots[1].clone()),
            &serde_json::to_value(forged).unwrap()
        )
        .is_err(),
        "admission cannot be smuggled into settlement"
    );
    assert!(f.task.admit(0).is_err());
    f.task.sessions.inject_persistence_faults(FaultHooks {
        fail_batch_write: true,
        ..Default::default()
    });
    assert!(f.task.admit(1).is_err());
    assert_eq!(f.task.current().unwrap().unwrap(), snapshots[1]);
    f.task
        .sessions
        .inject_persistence_faults(FaultHooks::default());
    f.task.admit(1).unwrap();
    snapshots.push(f.task.current().unwrap().unwrap());
    assert!(f.task.admit(2).is_err());
    f.task.settle(7).unwrap();
    snapshots.push(f.task.current().unwrap().unwrap());
    let c = f.task.check("read-only".into(), &[]).unwrap();
    f.task.begin(&f.resume(), &c, Some(1000)).unwrap();
    snapshots.push(f.task.current().unwrap().unwrap());
    assert!(
        f.task.admit(1).is_err(),
        "an admitted session turn cannot be reused"
    );
    f.task.admit(2).unwrap();
    snapshots.push(f.task.current().unwrap().unwrap());
    let mut full = ProjectionRegistry::clat();
    let mut tail = ProjectionRegistry::clat();
    let mut events = Vec::new();
    for (i, s) in snapshots.iter().enumerate() {
        let event = SessionEvent::new("clat/task", i as u64, 1, serde_json::to_value(s).unwrap());
        full.fold_one(&event).unwrap();
        events.push(event.clone());
        if i == 2 {
            let record = full.checkpoint(
                CheckpointIdentity {
                    created_at: 1,
                    cwd: Some(s.project.clone()),
                },
                3,
            );
            let floor = record
                .restore_floor(&ProjectionRegistry::clat())
                .unwrap_or(0);
            tail.restore(&record, &events[floor as usize..], floor)
                .unwrap();
        } else if i > 2 {
            tail.fold_one(&event).unwrap();
        }
    }
    assert_eq!(full.state_snapshot("task"), tail.state_snapshot("task"));
    assert_eq!(
        full.state_snapshot("task"),
        Some(serde_json::to_value(snapshots.last().unwrap()).unwrap())
    );
}
#[test]
fn ga1_rework_unadmitted_reservation_cannot_execute_effects() {
    let f = Fixture::new();
    let c = f.task.check("read-only".into(), &[]).unwrap();
    f.task.begin(&f.resume(), &c, Some(1000)).unwrap();
    let effect = Effect {
        count: AtomicUsize::new(0),
        cancel: false,
        faults: None,
    };
    assert!(
        invoke(&f, &effect, &CancelToken::new())
            .unwrap_err()
            .is_fatal()
    );
    assert_eq!(effect.count.load(Ordering::SeqCst), 0);
    assert!(f.task.current().unwrap().unwrap().operations.is_empty());
}
