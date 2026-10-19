use super::*;

#[test]
fn ag5_playwright_receipt_counts_only_browser_results_and_never_passes_skips_or_flakes() {
    let f = Fixture::new();
    for (mode, status, passed, failed, ignored) in [
        ("pw-green", "passed", 13, 0, 2),
        ("pw-failed", "failed", 12, 1, 0),
        ("pw-zero", "inconclusive", 0, 0, 3),
        ("pw-flaky", "inconclusive", 2, 0, 0),
    ] {
        let out = f.finish(
            f.exec(&json!({"argv":[f.program, mode],"yield_time_ms":1000,
            "verification":{"format":"playwright-summary","minimum_executed":1}}))
                .unwrap(),
        );
        let r = &out["verification_receipt"];
        assert_eq!(r["status"], status, "{out}");
        assert_eq!(r["passed"], passed);
        assert_eq!(r["failed"], failed);
        assert_eq!(r["ignored"], ignored);
        assert_eq!(r["authenticated"], false);
        assert_eq!(r["format"], "playwright-summary");
    }
}

#[test]
fn ag5_one_shot_direct_verification_keeps_child_exit_workdir_and_journal_receipt() {
    let f = Fixture::new();
    std::fs::create_dir(f.root.join("suite")).unwrap();
    let tool = RunCommandTool {
        service: f.service.clone(),
    };
    let out = tool
        .invoke(
            &json!({"argv":[f.program,"pw-failed"],"workdir":"suite",
        "verification":{"format":"playwright-summary","minimum_executed":1}}),
            &Project::new(&f.root),
            &CancelToken::new(),
        )
        .unwrap();
    assert_eq!(out["exit_code"], 1);
    assert_eq!(out["running"], false);
    assert_eq!(out["verification_receipt"]["status"], "failed");
    assert_eq!(out["verification_receipt"]["exit_scope"], "direct_child");
    assert_eq!(
        out["cwd"],
        f.root
            .join("suite")
            .canonicalize()
            .unwrap()
            .to_string_lossy()
            .as_ref()
    );
    assert_eq!(
        tool.journal_output(&out)["verification_receipt"],
        out["verification_receipt"]
    );
    assert_eq!(
        std::fs::read_to_string(f.root.join("suite/invocations")).unwrap(),
        "pw-failed\n"
    );
}

#[test]
fn ag5_wrong_session_error_offers_only_current_run_ids_without_reading_or_redirecting() {
    let f = Fixture::new();
    let start = f.exec(&f.args("recover")).unwrap();
    let id = start["session_id"].as_u64().unwrap();
    let tool = WriteStdinTool {
        service: f.service.clone(),
    };
    let err = tool
        .invoke(
            &json!({"session_id":999999,"chars":"MUST_NOT_WRITE"}),
            &Project::new(&f.root),
            &CancelToken::new(),
        )
        .unwrap_err();
    let result = tool.error_output(&err);
    assert_eq!(result["process_recovery"]["scope"], "current_run");
    assert_eq!(result["process_recovery"]["sessions"][0]["session_id"], id);
    assert_eq!(result["process_recovery"]["sessions"][0]["running"], true);
    assert!(!result.to_string().contains(&f.program));
    let done = f.finish(f.poll(&start["session_id"], "go\n"));
    assert_eq!(done["exit_code"], 0);
    assert_eq!(
        std::fs::read_to_string(f.root.join("invocations")).unwrap(),
        "recover\n"
    );
}

#[test]
fn ag5_one_shot_rejects_ambiguous_inputs_and_cancel_before_spawning() {
    let f = Fixture::new();
    let tool = RunCommandTool {
        service: f.service.clone(),
    };
    for args in [
        json!({"command":"echo BAD","argv":[f.program,"green"]}),
        json!({"command":"echo BAD","verification":{"format":"playwright-summary","minimum_executed":1}}),
        json!({"argv":[f.program,"green"],"workdir":"../outside"}),
    ] {
        assert!(
            tool.invoke(&args, &Project::new(&f.root), &CancelToken::new())
                .is_err()
        );
    }
    f.cancel.cancel();
    assert!(
        tool.invoke(
            &json!({"argv":[f.program,"green"]}),
            &Project::new(&f.root),
            &CancelToken::new()
        )
        .is_err()
    );
    assert!(!f.root.join("invocations").exists());
}

#[test]
fn ag5_process_recovery_is_bounded_does_not_consume_and_forgets_old_run() {
    let mut f = Fixture::new();
    let mut ids = Vec::new();
    for _ in 0..9 {
        let mut args = f.args("recover");
        args["yield_time_ms"] = json!(250);
        let started = f.exec(&args).unwrap();
        assert_eq!(started["running"], true);
        let id = started["session_id"].as_u64().unwrap();
        f.service.write_stdin(id, b"go\n", false, false).unwrap();
        // Polling with Fixture::finish can drain output before the child exits,
        // making the next admission legitimately reclaim this session.
        let completed = f
            .service
            .wait_and_consume(id, Duration::from_secs(20), 0)
            .unwrap();
        assert!(!completed.running);
        assert_eq!(completed.exit_code, Some(0));
        ids.push(id);
    }
    let tool = WriteStdinTool {
        service: f.service.clone(),
    };
    let err = tool
        .invoke(
            &json!({"session_id":999999}),
            &Project::new(&f.root),
            &CancelToken::new(),
        )
        .unwrap_err();
    let out = tool.error_output(&err);
    let rows = out["process_recovery"]["sessions"].as_array().unwrap();
    assert_eq!(rows.len(), 8);
    assert_eq!(out["process_recovery"]["omitted"], 1);
    assert!(rows.iter().all(|r| r["running"] == false));
    assert_eq!(
        tool.error_output(&err)["process_recovery"],
        out["process_recovery"]
    );
    for id in ids {
        let output = f
            .service
            .wait_and_consume(id, Duration::ZERO, 1024)
            .unwrap();
        assert!(
            output.stdout.contains("1 passed"),
            "recovery must not consume output"
        );
    }
    f.service.unbind_run(f.generation).unwrap();
    assert!(tool.error_output(&err).get("process_recovery").is_none());
    f.generation = f.service.bind_run("new-run", CancelToken::new()).unwrap();
    let out = tool.error_output(&err);
    assert_eq!(out["process_recovery"]["sessions"], json!([]));
}
