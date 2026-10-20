use super::*;

fn report(failed: bool) -> Value {
    let (status, actual) = if failed {
        ("unexpected", "failed")
    } else {
        ("expected", "passed")
    };
    json!({"errors":[],"suites":[{"specs":[{"file":"contract.spec.js","line":1,"column":1,"title":"up recalls history","tests":[{
        "projectName":"chromium","expectedStatus":"passed","status":status,
        "results":[{"status":actual,"errors":if failed {json!([{"message":"UP_INVARIANT: recalled the wrong entry"}])} else {json!([])}}]
    }]}]}]})
}

fn fixture(failed: bool) -> (Fixture, Value) {
    let f = Fixture::new("assert_eq!(api::live(),7);");
    let source = f.root.join("runner.rs");
    std::fs::write(
        &source,
        r#"fn main() {
        if let Ok(delay) = std::fs::read_to_string("startup-delay-ms") {
            std::thread::sleep(std::time::Duration::from_millis(delay.parse().unwrap()));
        }
        std::fs::write("ran", "yes").unwrap();
        if std::path::Path::new("hold-until-cleanup").exists() {
            let deadline=std::time::Instant::now()+std::time::Duration::from_secs(60);
            while std::time::Instant::now()<deadline {
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            panic!("owner cleanup did not stop the candidate");
        }
        if std::path::Path::new("mutate").exists() {std::fs::write("bound.bin","changed while running").unwrap();}
        if let Ok(other) = std::fs::read_to_string("mutate-other") {std::fs::write(std::path::Path::new(&other).join("bound.bin"),"cross-arm drift").unwrap();}
        if std::path::Path::new("wait").exists() {std::thread::sleep(std::time::Duration::from_secs(5));}
        print!("{}",std::fs::read_to_string("report.json").unwrap());
        let code=std::fs::read_to_string("exit.code").unwrap();
        std::process::exit(code.parse().unwrap());
    }"#,
    )
    .unwrap();
    let program = f.root.join("reporter");
    crate::process::compile_rust_test_helper(&source, &program).unwrap();
    for (dir, red) in [("candidate", false), ("counterexample", failed)] {
        let root = f.root.join(dir);
        std::fs::write(root.join("package.json"), "{}").unwrap();
        std::fs::write(root.join("contract.spec.js"), "same frozen assertions").unwrap();
        std::fs::write(
            root.join("bound.bin"),
            if red { "mutant" } else { "candidate" },
        )
        .unwrap();
        std::fs::write(root.join("report.json"), report(red).to_string()).unwrap();
        std::fs::write(root.join("exit.code"), if red { "1" } else { "0" }).unwrap();
    }
    let args = json!({"candidate_dir":"candidate","counterexample_dir":"counterexample",
        "test_files":["contract.spec.js"],"production_files":["src/api.rs"],
        "expected_failure":"UP_INVARIANT","sandbox":"auto",
        "playwright":{"argv":[program.to_string_lossy()],"build_files":["package.json"],"artifact_files":["bound.bin"]}});
    (f, args)
}

#[test]
fn ag10_playwright_pair_detects_a_both_green_counterexample() {
    let (f, args) = fixture(false);
    let out = f.invoke(&args).expect(
        "paired verification must support Playwright JSON without Cargo-specific arguments",
    );
    assert_eq!(out["status"], "not_discriminating", "{out}");
    assert_eq!(out["candidate"]["exit_code"], 0);
    assert_eq!(out["counterexample"]["exit_code"], 0);
}

#[test]
fn ag10_playwright_pair_requires_the_intended_failure_and_preserves_bound_artifacts() {
    let (f, args) = fixture(true);
    let out = f.invoke(&args).unwrap();
    assert_eq!(out["status"], "discriminates_declared_defect", "{out}");
    assert_ne!(
        out["inputs"]["candidate_artifacts"]["bound.bin"],
        out["inputs"]["counterexample_artifacts"]["bound.bin"]
    );
    assert_eq!(
        out["candidate"]["test_set_sha256"],
        out["counterexample"]["test_set_sha256"]
    );
    assert_eq!(out["counterexample"]["failure_expectation"], "matched");
    let tool = f
        .manager
        .require(TOOL_SERVICE)
        .unwrap()
        .get("test_discrimination")
        .unwrap();
    let durable = tool.journal_output(&out);
    let serialized = serde_json::to_string(&durable).unwrap();
    assert!(!serialized.contains("UP_INVARIANT"));
    assert!(
        !tool
            .journal_arguments(&args)
            .to_string()
            .contains("reporter")
    );
    assert!(serialized.chars().count() < crate::plugins::THRESHOLD_CHARS);
    assert_eq!(
        std::fs::read_to_string(f.root.join("counterexample/bound.bin")).unwrap(),
        "mutant"
    );
}

fn write_report(f: &Fixture, dir: &str, value: &Value) {
    std::fs::write(f.root.join(dir).join("report.json"), value.to_string()).unwrap();
}
#[test]
fn ag10_playwright_pair_rejects_wrong_observers_test_sets_and_expected_failures() {
    let mut cases = Vec::new();
    let mut wrong = report(true);
    wrong["suites"][0]["specs"][0]["tests"][0]["results"][0]["errors"][0]["message"] =
        json!("unrelated error");
    wrong["suites"][0]["specs"][0]["tests"][0]["results"][0]["stdout"] =
        json!([{"text":"UP_INVARIANT"}]);
    cases.push(wrong);
    let mut changed = report(true);
    changed["suites"][0]["specs"][0]["title"] = json!("another test");
    cases.push(changed);
    let mut expected = report(true);
    expected["suites"][0]["specs"][0]["tests"][0]["expectedStatus"] = json!("failed");
    cases.push(expected);
    let mut retry = report(true);
    let result = retry["suites"][0]["specs"][0]["tests"][0]["results"][0].clone();
    retry["suites"][0]["specs"][0]["tests"][0]["results"]
        .as_array_mut()
        .unwrap()
        .push(result);
    cases.push(retry);
    let mut timeout = report(true);
    timeout["suites"][0]["specs"][0]["tests"][0]["results"][0]["status"] = json!("timedOut");
    cases.push(timeout);
    let mut global = report(true);
    global["errors"] = json!([{"message":"UP_INVARIANT global setup failed"}]);
    cases.push(global);
    cases.extend([json!({}), json!({"errors":[],"suites":[]})]);
    for value in cases {
        let (f, args) = fixture(true);
        write_report(&f, "counterexample", &value);
        assert_eq!(
            f.invoke(&args).unwrap()["status"],
            "inconclusive",
            "{value}"
        );
    }
}
#[test]
fn ag10_playwright_pair_candidate_failure_zero_and_clipped_output_stop_the_control() {
    for (value, code) in [
        (report(true), "1"),
        (json!({"errors":[],"suites":[]}), "0"),
        (report(false), "7"),
    ] {
        let (f, args) = fixture(true);
        write_report(&f, "candidate", &value);
        std::fs::write(f.root.join("candidate/exit.code"), code).unwrap();
        let out = f.invoke(&args).unwrap();
        assert_eq!(out["status"], "inconclusive");
        assert!(out["counterexample"].is_null());
        assert!(!f.root.join("counterexample/ran").exists());
    }
    let (f, args) = fixture(true);
    let mut big = report(false);
    big["extra"] = json!("x".repeat(40000));
    write_report(&f, "candidate", &big);
    assert_eq!(f.invoke(&args).unwrap()["status"], "inconclusive");
    assert!(!f.root.join("counterexample/ran").exists());
}
#[test]
fn ag10_playwright_pair_refuses_input_drift_before_and_during_execution() {
    for path in ["contract.spec.js", "bound.bin", "src/api.rs"] {
        let (f, args) = fixture(true);
        let tool = f
            .manager
            .require(TOOL_SERVICE)
            .unwrap()
            .get("test_discrimination")
            .unwrap();
        let prepared = tool.prepare_execution(&args).unwrap();
        std::fs::write(f.root.join("counterexample").join(path), "drift").unwrap();
        assert!(
            prepared
                .tool
                .invoke(&args, &Project::new(&f.root), &CancelToken::new())
                .is_err()
        );
        assert!(!f.root.join("candidate/ran").exists());
    }
    let (f, args) = fixture(true);
    std::fs::write(f.root.join("candidate/mutate"), "").unwrap();
    assert!(
        f.invoke(&args)
            .unwrap_err()
            .to_string()
            .contains("changed declared")
    );
    assert!(!f.root.join("counterexample/ran").exists());
}
#[test]
fn ag10_playwright_pair_refuses_cross_run_reuse_cancel_and_timeout() {
    let (mut f, args) = fixture(true);
    let tool = f
        .manager
        .require(TOOL_SERVICE)
        .unwrap()
        .get("test_discrimination")
        .unwrap();
    let prepared = tool.prepare_execution(&args).unwrap();
    f.service.unbind_run(f.generation).unwrap();
    f.generation = f.service.bind_run("reopened", CancelToken::new()).unwrap();
    assert!(
        prepared
            .tool
            .invoke(&args, &Project::new(&f.root), &CancelToken::new())
            .is_err()
    );
    assert!(!f.root.join("candidate/ran").exists());
    let prepared = tool.prepare_execution(&args).unwrap();
    let cancel = CancelToken::new();
    cancel.cancel();
    assert!(
        prepared
            .tool
            .invoke(&args, &Project::new(&f.root), &cancel)
            .is_err()
    );
    assert!(
        prepared
            .tool
            .invoke(&args, &Project::new(&f.root), &CancelToken::new())
            .is_err()
    );
    let (f, mut args) = fixture(true);
    args["timeout_seconds"] = json!(1);
    std::fs::write(f.root.join("candidate/wait"), "").unwrap();
    let out = f.invoke(&args).unwrap();
    assert_eq!(out["status"], "inconclusive");
    assert_eq!(out["candidate"]["timed_out"], true);
    assert!(!f.root.join("counterexample/ran").exists());
}
#[test]
fn ag10_playwright_pair_rejects_unsafe_or_ambiguous_declarations() {
    let (f, args) = fixture(true);
    let mut bad = args.clone();
    bad["candidate_dir"] = json!(".");
    assert!(f.invoke(&bad).is_err());
    let mut bad = args.clone();
    bad["playwright"]["artifact_files"] = json!(["../reporter"]);
    assert!(f.invoke(&bad).is_err());
    let mut bad = args.clone();
    bad["playwright"]["build_files"] = json!(["missing.json"]);
    assert!(f.invoke(&bad).is_err());
    let mut bad = args.clone();
    bad["package"] = json!("accidental cargo options");
    assert!(f.invoke(&bad).is_err());
    let mut bad = args.clone();
    bad["expected_failure"] = json!("");
    assert!(f.invoke(&bad).is_err());
    assert!(!f.root.join("candidate/ran").exists());
    std::os::unix::fs::symlink(
        f.root.join("reporter"),
        f.root.join("candidate/outside.bin"),
    )
    .unwrap();
    let mut bad = args.clone();
    bad["playwright"]["artifact_files"] = json!(["outside.bin"]);
    assert!(f.invoke(&bad).is_err());
}

#[path = "playwright_loop_tests.rs"]
mod loop_tests;

#[test]
fn ag10_playwright_pair_owner_cleanup_interrupts_candidate_without_starting_control() {
    owner_cleanup_case(0);
}

#[test]
fn ag10_owner_cleanup_waits_for_slow_start_without_confusing_it_with_cancellation() {
    owner_cleanup_case(4000);
}

fn owner_cleanup_case(startup_delay_ms: u64) {
    let (mut f, args) = fixture(true);
    std::fs::write(f.root.join("candidate/hold-until-cleanup"), "").unwrap();
    std::fs::write(
        f.root.join("candidate/startup-delay-ms"),
        startup_delay_ms.to_string(),
    )
    .unwrap();
    let tool = f
        .manager
        .require(TOOL_SERVICE)
        .unwrap()
        .get("test_discrimination")
        .unwrap();
    let prepared = tool.prepare_execution(&args).unwrap();
    let a = args.clone();
    let project = Project::new(&f.root);
    let job = std::thread::spawn(move || prepared.tool.invoke(&a, &project, &CancelToken::new()));
    let job = await_candidate_ready(&f.root, job);
    let cancelled_at = std::time::Instant::now();
    f.service.unbind_run(f.generation).unwrap();
    let outcome = job.join().unwrap();
    assert!(
        cancelled_at.elapsed() < Duration::from_secs(5),
        "owner cleanup must interrupt the held candidate, not wait for its 60-second deadline"
    );
    assert!(outcome.is_err() || outcome.as_ref().unwrap()["status"] == "inconclusive");
    assert!(!f.root.join("counterexample/ran").exists());
    f.generation = f
        .service
        .bind_run("after-cleanup", CancelToken::new())
        .unwrap();
}

fn await_candidate_ready(
    root: &std::path::Path,
    job: std::thread::JoinHandle<Result<Value, ToolError>>,
) -> std::thread::JoinHandle<Result<Value, ToolError>> {
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    loop {
        if matches!(
            std::fs::read_to_string(root.join("candidate/ran")).as_deref(),
            Ok("yes")
        ) {
            return job;
        }
        if job.is_finished() {
            panic!(
                "candidate exited before readiness: {:?}",
                job.join().unwrap()
            );
        }
        assert!(
            std::time::Instant::now() < deadline,
            "candidate must actually start before cancellation (readiness deadline exceeded)"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn ag10_playwright_pair_rejects_fifo_artifacts_without_blocking() {
    let (f, mut args) = fixture(true);
    let file = f.root.join("candidate/fifo");
    let name = std::ffi::CString::new(file.to_string_lossy().as_bytes()).unwrap();
    assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
    args["playwright"]["artifact_files"] = json!(["fifo"]);
    assert!(f.invoke(&args).is_err());
    assert!(!f.root.join("candidate/ran").exists());
}

#[test]
fn ag10_playwright_pair_rejects_cross_arm_input_mutation() {
    for (actor, other) in [
        ("candidate", "counterexample"),
        ("counterexample", "candidate"),
    ] {
        let (f, args) = fixture(true);
        std::fs::write(
            f.root.join(actor).join("mutate-other"),
            format!("../{other}"),
        )
        .unwrap();
        let error = f
            .invoke(&args)
            .expect_err("neither arm may change the other frozen input set");
        assert!(error.to_string().contains("inputs changed"), "{error}");
        assert_eq!(
            std::fs::read_to_string(f.root.join(other).join("bound.bin")).unwrap(),
            "cross-arm drift"
        );
        if actor == "candidate" {
            assert!(!f.root.join("counterexample/ran").exists());
        }
    }
}
