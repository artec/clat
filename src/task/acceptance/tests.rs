use super::*;
use crate::task::acceptance::{Clause, Completion};
fn configured() -> Fixture {
    let mut f = Fixture::new();
    f.task
        .install_checkers(crate::plugins::task_checkers())
        .unwrap();
    std::fs::write(f.root.join("report.md"), b"Results\nTotal: 5\n").unwrap();
    let s = f.task.current().unwrap().unwrap();
    let mut c = super::acceptance_contract_tests::contract(1);
    c.clauses.push(Clause {
        id: "text".into(),
        required: true,
        checker: "text".into(),
        checker_version: 1,
        source: "project_files".into(),
        files: vec!["report.md".into()],
        parameters: json!({"path":"report.md","contains":["Results","Total: 5"]}),
    });
    let s = f.task.set_acceptance(&s.id, s.revision, c).unwrap();
    f.task
        .attest(
            &s.id,
            s.revision,
            "legacy-1",
            "User checked original condition",
        )
        .unwrap();
    f
}
fn save(f: &Fixture) -> TaskState {
    let s = f.task.current().unwrap().unwrap();
    f.task
        .save_verification(
            &s.id,
            s.revision,
            &Default::default(),
            false,
            &CancelToken::new(),
        )
        .unwrap()
}
fn reject_change(s: &TaskState, mut n: TaskState) {
    n.revision = s.revision + 1;
    let mut old = Some(s.clone());
    assert!(fold_change(&mut old, &serde_json::to_value(n).unwrap()).is_err());
    assert_eq!(old, Some(s.clone()));
}
#[test]
fn ga2_admission_refuses_forged_aggregate_cross_task_combined_completion_and_missing_producer() {
    let f = configured();
    let s = save(&f);
    let r = s.acceptance_state.as_ref().unwrap().reports[0].clone();
    let mut forged_birth = s.clone();
    forged_birth.revision = 1;
    assert!(
        fold_change(&mut None, &serde_json::to_value(forged_birth).unwrap()).is_err(),
        "birth cannot import user authority or pre-approved reports"
    );
    for kind in 0..6 {
        let mut n = s.clone();
        let a = n.acceptance_state.as_mut().unwrap();
        let mut fake = r.clone();
        fake.id = "forged".into();
        match kind {
            0 => fake.task_id = "another-task".into(),
            1 => {
                fake.results[0].status = "failed".into();
            }
            2 => fake.results.pop().map(|_| ()).unwrap(),
            3 => {
                fake.results[1].operation = Some("other-operation".into());
            }
            4 => {
                fake.contract_version += 1;
            }
            _ => {
                a.attestations[0].note = "changed human claim".into();
            }
        }
        a.reports.push(fake);
        reject_change(&s, n);
    }
    let mut n = s.clone();
    let a = n.acceptance_state.as_mut().unwrap();
    let mut new = r;
    new.id = "not-previously-durable".into();
    a.reports.push(new.clone());
    a.completion = Some(Completion {
        kind: "mixed_verified".into(),
        authority: "explicit_user".into(),
        report: Some(new.id),
        note: "forged combined commit".into(),
    });
    n.phase = "complete".into();
    reject_change(&s, n);
}
#[test]
fn ga2_user_cas_content_replacement_and_normal_settlement_have_distinct_semantics() {
    let f = configured();
    let s = save(&f);
    let report = s.acceptance_state.as_ref().unwrap().reports[0].id.clone();
    assert!(
        f.task
            .save_verification(
                &s.id,
                s.revision - 1,
                &Default::default(),
                false,
                &CancelToken::new()
            )
            .is_err()
    );
    f.begin(1000);
    f.task.settle(7).unwrap();
    let settled = f.task.current().unwrap().unwrap();
    assert!(settled.revision > s.revision);
    assert_eq!(
        f.task.verification_status(None).unwrap()["current_applicability"]["applicable"],
        true
    );
    assert!(f.task.finish_verified(&s.id, s.revision, &report).is_err());
    std::fs::remove_file(f.root.join("report.md")).unwrap();
    std::fs::write(f.root.join("report.md"), "Results\nTotal: 6\n").unwrap();
    assert!(
        f.task
            .finish_verified(&s.id, settled.revision, &report)
            .is_err()
    );
    assert_eq!(f.task.current().unwrap().unwrap(), settled);
    std::fs::write(f.root.join("report.md"), "Results\nTotal: 5\n").unwrap();
    let done = f
        .task
        .finish_verified(&s.id, settled.revision, &report)
        .unwrap();
    assert_eq!(
        done.acceptance_state.unwrap().completion.unwrap().kind,
        "mixed_verified"
    );
}
#[test]
fn ga2_read_bounds_cancel_invalid_utf8_unknown_version_and_symlinks_are_inconclusive() {
    let f = configured();
    for content in [vec![255], vec![b'x'; 8 * 1024 * 1024 + 1]] {
        std::fs::write(f.root.join("report.md"), content).unwrap();
        assert_eq!(
            f.task
                .check_acceptance(&Default::default(), &CancelToken::new())
                .unwrap()
                .status,
            "inconclusive"
        );
    }
    std::fs::write(f.root.join("report.md"), "Results\nTotal: 5\n").unwrap();
    let cancel = CancelToken::new();
    cancel.cancel();
    assert_eq!(
        f.task
            .check_acceptance(&Default::default(), &cancel)
            .unwrap()
            .status,
        "inconclusive"
    );
    #[cfg(unix)]
    {
        std::fs::remove_file(f.root.join("report.md")).unwrap();
        std::os::unix::fs::symlink("../outside", f.root.join("report.md")).unwrap();
        assert_eq!(
            f.task
                .check_acceptance(&Default::default(), &CancelToken::new())
                .unwrap()
                .status,
            "inconclusive"
        );
        std::fs::remove_file(f.root.join("report.md")).unwrap();
    }
    let s = f.task.current().unwrap().unwrap();
    let mut c = s.acceptance_state.unwrap().contract.unwrap();
    c.version = 2;
    c.clauses[1].checker_version = 99;
    f.task.set_acceptance(&s.id, s.revision, c).unwrap();
    let report = f
        .task
        .check_acceptance(&Default::default(), &CancelToken::new())
        .unwrap();
    assert_eq!(report.results.len(), 2);
    assert!(report.results.iter().all(|r| r.status == "inconclusive"));
}
#[test]
fn ga2_deep_duplicate_cross_root_contracts_are_refused_without_publication() {
    let f = configured();
    let s = f.task.current().unwrap().unwrap();
    let c = s
        .acceptance_state
        .as_ref()
        .unwrap()
        .contract
        .as_ref()
        .unwrap();
    for kind in 0..4 {
        let mut n = c.clone();
        n.version = 2;
        match kind {
            0 => n.clauses.push(n.clauses[1].clone()),
            1 => n.clauses[1].files[0] = "../outside".into(),
            2 => {
                let mut v = json!(null);
                for _ in 0..34 {
                    v = json!([v]);
                }
                n.clauses[1].parameters = v;
            }
            _ => n.clauses[1].parameters = json!({"path":"report.md","contains":[]}),
        }
        assert!(f.task.set_acceptance(&s.id, s.revision, n).is_err());
        assert_eq!(f.task.current().unwrap().unwrap(), s);
    }
}
#[test]
fn ga2_task_tools_refuse_user_only_authority_and_checks_are_revision_read_only() {
    let mut f = configured();
    f.begin(1000);
    let s = f.task.current().unwrap().unwrap();
    let replacement = TaskService::new(f.task.sessions.clone(), f.task.project.clone()).unwrap();
    let service = Arc::new(std::mem::replace(&mut f.task, replacement));
    let tools = crate::task::acceptance::interface::tools(service.clone());
    for args in [
        json!({"report":{"status":"passed"}}),
        json!({"authority":"explicit_user"}),
        json!({"attestations":["legacy-1"]}),
    ] {
        assert!(
            tools[0]
                .invoke(&args, &service.project, &CancelToken::new())
                .is_err()
        );
        assert_eq!(service.current().unwrap().unwrap(), s);
    }
    let result = tools[0]
        .invoke(&json!({}), &service.project, &CancelToken::new())
        .unwrap();
    assert_eq!(result["status"], "passed");
    assert_eq!(service.current().unwrap().unwrap(), s);
    assert!(tools[1].invoke(&json!({"task_id":s.id,"expected_revision":s.revision,"report":{"status":"passed"}}),&service.project,&CancelToken::new()).is_err());
    service.settle(3).unwrap();
    assert!(tools[2].invoke(&json!({"task_id":s.id,"expected_revision":s.revision,"report_id":"forged","override":true}),&service.project,&CancelToken::new()).is_err());
}

#[test]
fn ga2_versioned_golden_catalog_full_fold_and_checkpoint_tail_preserve_completion_provenance() {
    let raw = include_str!("../../../tests/fixtures/task-acceptance-v1.json");
    let states: Vec<TaskState> = serde_json::from_str(raw).unwrap();
    assert_eq!(
        format!("{}\n", serde_json::to_string_pretty(&states).unwrap()),
        raw
    );
    let mut full = ProjectionRegistry::clat();
    let mut tail = ProjectionRegistry::clat();
    let mut events = Vec::new();
    for (i, s) in states.iter().enumerate() {
        let mut event =
            SessionEvent::new("clat/task", i as u64, 1, serde_json::to_value(s).unwrap());
        event.ignorable = Some(true);
        crate::session::catalog::event_spec("clat/task")
            .unwrap()
            .validate(&event, 4)
            .unwrap();
        full.fold_one(&event).unwrap();
        events.push(event.clone());
        if i == 2 {
            let checkpoint = full.checkpoint(
                CheckpointIdentity {
                    created_at: 1,
                    cwd: Some(s.project.clone()),
                },
                3,
            );
            let floor = checkpoint
                .restore_floor(&ProjectionRegistry::clat())
                .unwrap_or(0);
            tail.restore(&checkpoint, &events[floor as usize..], floor)
                .unwrap();
        } else if i > 2 {
            tail.fold_one(&event).unwrap();
        }
    }
    assert_eq!(full.state_snapshot("task"), tail.state_snapshot("task"));
    assert_eq!(
        full.state_snapshot("task"),
        Some(serde_json::to_value(states.last().unwrap()).unwrap())
    );
    assert_eq!(
        states
            .last()
            .unwrap()
            .acceptance_state
            .as_ref()
            .unwrap()
            .completion
            .as_ref()
            .unwrap()
            .kind,
        "mixed_verified"
    );
}

#[test]
fn ga2_human_only_contract_completes_with_human_attested_source() {
    let f = Fixture::new();
    let s = f.task.current().unwrap().unwrap();
    let s = f
        .task
        .set_acceptance(
            &s.id,
            s.revision,
            super::acceptance_contract_tests::contract(1),
        )
        .unwrap();
    f.task
        .attest(
            &s.id,
            s.revision,
            "legacy-1",
            "Human checked all requirements",
        )
        .unwrap();
    let s = save(&f);
    let r = &s.acceptance_state.as_ref().unwrap().reports[0].id;
    let done = f.task.finish_verified(&s.id, s.revision, r).unwrap();
    assert_eq!(
        done.acceptance_state.unwrap().completion.unwrap().kind,
        "human_attested"
    );
}

struct FaultyChecker {
    path: std::path::PathBuf,
    mode: u8,
}
impl crate::task::acceptance::AcceptanceChecker for FaultyChecker {
    fn identity(&self) -> (&'static str, u32) {
        ("faulty", 1)
    }
    fn validate(&self, _: &Clause) -> Result<(), String> {
        Ok(())
    }
    fn evaluate(
        &self,
        _: &Clause,
        _: &TaskState,
        _: &crate::task::acceptance::CheckSnapshot,
        _: Option<&str>,
    ) -> crate::task::acceptance::CheckerOutcome {
        match self.mode {
            0 => panic!("injected checker panic"),
            1 => std::fs::write(&self.path, b"replaced during verification").unwrap(),
            _ => std::thread::sleep(std::time::Duration::from_millis(260)),
        }
        crate::task::acceptance::CheckerOutcome::new("passed", "untrusted premature result")
    }
}
#[test]
fn ga2_checker_panic_changed_during_read_and_deadline_never_return_pass() {
    for mode in 0..3 {
        let mut f = configured();
        f.task
            .install_checkers(vec![Arc::new(FaultyChecker {
                path: f.root.join("report.md"),
                mode,
            })])
            .unwrap();
        let s = f.task.current().unwrap().unwrap();
        let mut c = s.acceptance_state.unwrap().contract.unwrap();
        c.version = 2;
        c.clauses[1].checker = "faulty".into();
        f.task.set_acceptance(&s.id, s.revision, c).unwrap();
        let report = f
            .task
            .check_acceptance(&Default::default(), &CancelToken::new())
            .unwrap();
        assert_eq!(report.results[1].status, "inconclusive");
        assert_ne!(report.status, "passed");
    }
}

#[test]
fn ga2_bounded_model_pages_preserve_revision_all_operation_and_report_identities() {
    let f = configured();
    let mut s = save(&f);
    s.progress.summary = "x".repeat(8192);
    for i in 0..128 {
        s.operations.push(Operation {
            id: format!("op-{i}"),
            run: "run".into(),
            tool: "run_command".into(),
            arguments_digest: "0".repeat(64),
            call_id: None,
            outcome: "confirmed".into(),
            result_digest: None,
            result_kind: None,
            resolution: None,
            command_fact: None,
            execution_identity: None,
        });
    }
    for i in 1..8 {
        let a = s.acceptance_state.as_mut().unwrap();
        let mut r = a.reports[0].clone();
        r.id = format!("check-{i}");
        a.reports.push(r);
    }
    let summary = TaskService::model_view(&s, &json!({}));
    assert!(summary.to_string().len() <= 4096);
    assert_eq!(summary["id"], s.id);
    assert_eq!(summary["revision"], s.revision);
    assert_eq!(summary["acceptance_state"]["reports"][0]["id"], "check-7");
    for (section, total) in [("operations", 128), ("reports", 16), ("contract", 2)] {
        let mut offset = 0;
        let mut rows = Vec::new();
        loop {
            let page = TaskService::model_view(&s, &json!({"section":section,"offset":offset}));
            assert!(page.to_string().len() <= 4096, "{page}");
            assert_eq!(page["total"], total);
            let key = if section == "operations" {
                "operations"
            } else {
                "items"
            };
            rows.extend(page[key].as_array().unwrap().clone());
            match page["next_offset"].as_u64() {
                Some(n) => {
                    assert!(n > offset);
                    offset = n;
                }
                None => break,
            }
        }
        assert_eq!(rows.len(), total);
        if section == "operations" {
            assert_eq!(
                rows.iter()
                    .map(|r| r["id"].as_str().unwrap())
                    .collect::<Vec<_>>(),
                s.operations
                    .iter()
                    .map(|o| o.id.as_str())
                    .collect::<Vec<_>>()
            );
        }
    }
}

#[test]
fn ga2_failed_task_settlement_cannot_promote_a_pending_candidate() {
    let f = configured();
    f.begin(1000);
    let s = f.task.current().unwrap().unwrap();
    let s = f
        .task
        .save_verification(
            &s.id,
            s.revision,
            &Default::default(),
            true,
            &CancelToken::new(),
        )
        .unwrap();
    let report = s.acceptance_state.as_ref().unwrap().reports[0].id.clone();
    let pending = f
        .task
        .propose_completion(&s.id, s.revision, &report)
        .unwrap();
    let run = pending.runs.last().unwrap().id.clone();
    f.task.sessions.inject_persistence_faults(FaultHooks {
        fail_batch_write: true,
        ..Default::default()
    });
    assert!(f.task.settle(9).is_err());
    f.task
        .sessions
        .inject_persistence_faults(FaultHooks::default());
    f.task
        .finalize_candidate(Some(&run), true, &CancelToken::new())
        .unwrap();
    let s = f.task.current().unwrap().unwrap();
    assert_eq!(s, pending);
    assert_ne!(s.phase, "complete");
    assert!(!s.runs.last().unwrap().settled);
    assert!(f.task.finish_verified(&s.id, s.revision, &report).is_err());
}

#[test]
fn ga2_json_adapter_uses_explicit_observation_assertions_and_bounded_inputs() {
    let f = configured();
    let s = f.task.current().unwrap().unwrap();
    let mut c = s.acceptance_state.unwrap().contract.unwrap();
    c.version = 2;
    c.clauses[1].checker = "json".into();
    c.clauses[1].files = vec!["result.json".into()];
    c.clauses[1].parameters = json!({"path":"result.json","checks":[{"op":"equals","id":"count","pointer":"/count","expected":2},{"op":"is_true","id":"ready","pointer":"/ready"},{"op":"all_before","id":"order","pointer":"/trace","before":"save","after":"close"}]});
    f.task.set_acceptance(&s.id, s.revision, c).unwrap();
    for (value, status) in [
        (
            json!({"count":2,"ready":true,"trace":["save","close"]}),
            "passed",
        ),
        (
            json!({"count":3,"ready":true,"trace":["save","close"]}),
            "failed",
        ),
        (
            json!({"count":2,"ready":false,"trace":["save","close"]}),
            "failed",
        ),
        (
            json!({"count":2,"ready":true,"trace":["close","save"]}),
            "failed",
        ),
        (json!({"count":2,"ready":true}), "inconclusive"),
    ] {
        std::fs::write(f.root.join("result.json"), value.to_string()).unwrap();
        let report = f
            .task
            .check_acceptance(&Default::default(), &CancelToken::new())
            .unwrap();
        assert_eq!(report.results[1].status, status);
    }
    std::fs::write(
        f.root.join("result.json"),
        json!({"padding":"x".repeat(256*1024)}).to_string(),
    )
    .unwrap();
    assert_eq!(
        f.task
            .check_acceptance(&Default::default(), &CancelToken::new())
            .unwrap()
            .results[1]
            .status,
        "inconclusive"
    );
}

#[test]
fn ga2_file_count_exhaustion_retains_every_clause_as_inconclusive() {
    let f = configured();
    let s = f.task.current().unwrap().unwrap();
    let mut c = s.acceptance_state.unwrap().contract.unwrap();
    c.version = 2;
    for i in 0..9 {
        c.clauses.push(Clause {
            id: format!("extra-{i}"),
            required: true,
            checker: "unavailable".into(),
            checker_version: 1,
            source: "project_files".into(),
            files: (0..8).map(|j| format!("data-{i}-{j}")).collect(),
            parameters: json!({}),
        });
    }
    f.task.set_acceptance(&s.id, s.revision, c).unwrap();
    let r = f
        .task
        .check_acceptance(&Default::default(), &CancelToken::new())
        .unwrap();
    assert_eq!(r.results.len(), 11);
    assert_eq!(r.status, "inconclusive");
    assert!(r.results.iter().all(|r| r.status == "inconclusive"));
}

#[test]
fn ga2_competing_clients_save_and_complete_at_most_one_cas() {
    let f = configured();
    let initial = f.task.current().unwrap().unwrap();
    let results = std::thread::scope(|scope| {
        let handles: Vec<_> = (0..2)
            .map(|_| {
                scope.spawn(|| {
                    f.task.save_verification(
                        &initial.id,
                        initial.revision,
                        &Default::default(),
                        false,
                        &CancelToken::new(),
                    )
                })
            })
            .collect();
        handles
            .into_iter()
            .map(|h| h.join().unwrap())
            .collect::<Vec<_>>()
    });
    assert_eq!(results.iter().filter(|r| r.is_ok()).count(), 1);
    let saved = f.task.current().unwrap().unwrap();
    assert_eq!(saved.revision, initial.revision + 1);
    assert_eq!(saved.acceptance_state.as_ref().unwrap().reports.len(), 1);
    let report = &saved.acceptance_state.as_ref().unwrap().reports[0].id;
    let results = std::thread::scope(|scope| {
        let handles: Vec<_> = (0..2)
            .map(|_| scope.spawn(|| f.task.finish_verified(&saved.id, saved.revision, report)))
            .collect();
        handles
            .into_iter()
            .map(|h| h.join().unwrap())
            .collect::<Vec<_>>()
    });
    assert_eq!(results.iter().filter(|r| r.is_ok()).count(), 1);
    let done = f.task.current().unwrap().unwrap();
    assert_eq!(done.revision, saved.revision + 1);
    assert_eq!(done.phase, "complete");
}

#[test]
fn ga2_rework_execution_identity_golden_admission_checkpoint_and_immutability() {
    let raw = include_str!("../../../tests/fixtures/task-execution-identity-v1.json");
    let states: Vec<TaskState> = serde_json::from_str(raw).unwrap();
    assert_eq!(
        format!("{}\n", serde_json::to_string_pretty(&states).unwrap()),
        raw
    );
    let mut full = ProjectionRegistry::clat();
    let mut tail = ProjectionRegistry::clat();
    let mut events = Vec::new();
    for (i, s) in states.iter().enumerate() {
        let mut event =
            SessionEvent::new("clat/task", i as u64, 1, serde_json::to_value(s).unwrap());
        event.ignorable = Some(true);
        crate::session::catalog::event_spec("clat/task")
            .unwrap()
            .validate(&event, 4)
            .unwrap();
        full.fold_one(&event).unwrap();
        events.push(event.clone());
        if i == 3 {
            let record = full.checkpoint(
                CheckpointIdentity {
                    created_at: 1,
                    cwd: Some(s.project.clone()),
                },
                4,
            );
            let floor = record
                .restore_floor(&ProjectionRegistry::clat())
                .unwrap_or(0);
            tail.restore(&record, &events[floor as usize..], floor)
                .unwrap();
        } else if i > 3 {
            tail.fold_one(&event).unwrap();
        }
    }
    assert_eq!(full.state_snapshot("task"), tail.state_snapshot("task"));
    assert_eq!(
        full.state_snapshot("task"),
        Some(serde_json::to_value(states.last().unwrap()).unwrap())
    );
    let mut forged = states.last().unwrap().clone();
    forged.operations[0].execution_identity = Some("3".repeat(64));
    reject_change(states.last().unwrap(), forged);
}
