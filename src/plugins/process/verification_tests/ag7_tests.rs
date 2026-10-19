use super::*;
use sha2::{Digest, Sha256};

fn once(f: &Fixture, args: Value) -> Result<Value, ToolError> {
    RunCommandTool {
        service: f.service.clone(),
    }
    .invoke(&args, &Project::new(&f.root), &f.cancel)
}

#[test]
fn ag7_evidence_preserves_first_failure_and_exact_declared_bindings() {
    let f = Fixture::new();
    std::fs::write(f.root.join("input.rs"), "synthetic baseline").unwrap();
    let program = std::path::Path::new(&f.program)
        .file_name()
        .unwrap()
        .to_str()
        .unwrap();
    let evidence =
        json!({"directory":"evidence","files":["stage.log"],"bind_files":["input.rs",program]});
    let first = once(
        &f,
        json!({"argv":[f.program,"ag7-red"],"evidence":evidence}),
    )
    .expect("first failing run must return native evidence");
    assert_eq!(first["exit_code"], 101);
    assert_eq!(first["evidence"]["status"], "recorded");
    let path = first["evidence"]["path"].as_str().unwrap();
    let bytes = std::fs::read(f.root.join(path)).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        assert_eq!(
            std::fs::metadata(f.root.join(path))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }
    assert_eq!(
        format!("{:x}", Sha256::digest(&bytes)),
        first["evidence"]["sha256"]
    );
    let record: Value = serde_json::from_slice(&bytes).unwrap();
    assert!(
        record["files"][0]["content"]
            .as_str()
            .unwrap()
            .contains("SYNTHETIC_INVARIANT")
    );
    assert_eq!(record["bindings"]["before"], record["bindings"]["after"]);
    assert!(
        record["bindings"]["before"][program]["sha256"]
            .as_str()
            .is_some_and(|hash| hash.len() == 64),
        "the declared executable must have an observed content digest"
    );
    let second = once(
        &f,
        json!({"argv":[f.program,"ag7-green"],"evidence":evidence}),
    )
    .unwrap();
    assert_eq!(second["exit_code"], 0);
    assert_ne!(second["evidence"]["path"], first["evidence"]["path"]);
    assert_eq!(
        std::fs::read(f.root.join(path)).unwrap(),
        bytes,
        "a rerun must never replace the first failure evidence"
    );
    assert_eq!(
        process_journal_output(&first)["evidence"],
        first["evidence"]
    );
    assert!(
        !process_journal_output(&first)
            .to_string()
            .contains("SYNTHETIC_INVARIANT")
    );
}

#[test]
fn ag7_failure_context_survives_ring_loss_and_suggests_minimal_checks() {
    let f = Fixture::new();
    let out = once(&f, json!({"argv":[f.program,"ag7-red"]})).unwrap();
    assert_eq!(out["exit_code"], 101);
    let context = &out["failure_context"];
    assert_eq!(
        context["tests"],
        json!(["case::denies_invalid"]),
        "retain failure identity before ring eviction"
    );
    assert!(
        context["diagnostics"]
            .to_string()
            .contains("SYNTHETIC_INVARIANT")
    );
    assert!(
        context["next_action"]
            .as_str()
            .unwrap()
            .contains("failed tests")
    );
    assert_eq!(context["authenticated"], false);
    assert!(out.to_string().chars().count() <= crate::plugins::THRESHOLD_CHARS);
    let journal = process_journal_output(&out);
    assert_eq!(journal["failure_context"]["tests"], context["tests"]);
    assert!(journal["failure_context"].get("diagnostics").is_none());
    let green = once(&f, json!({"argv":[f.program,"ag7-green"]})).unwrap();
    assert!(green.get("failure_context").is_none());
    assert!(green.get("evidence").is_none());
}

#[test]
fn ag7_session_info_uses_current_binding_without_commands_or_directory_scans() {
    let f = Fixture::new();
    let tool = process_tools(f.service.clone())
        .into_iter()
        .find(|t| t.definition().name == "session_info")
        .expect("current session identity must be a registered native tool");
    assert_eq!(tool.definition().effect, ToolEffect::Read);
    assert!(crate::tool::ToolAccessPolicy::plan_mode().allows(&tool.definition()));
    assert!(!crate::tool::ToolAccessPolicy::readonly_child().allows(&tool.definition()));
    let project = Project::new(&f.root);
    let info = tool.invoke(&json!({}), &project, &f.cancel).unwrap();
    assert_eq!(info["session_id"], "verification");
    assert_eq!(info["source"], "active_run_binding");
    assert!(!f.root.join("invocations").exists());
    f.service.unbind_run(f.generation).unwrap();
    assert!(tool.invoke(&json!({}), &project, &f.cancel).is_err());
    let g = f
        .service
        .bind_run("second-session", CancelToken::new())
        .unwrap();
    let info = tool.invoke(&json!({}), &project, &f.cancel).unwrap();
    assert_eq!(info["session_id"], "second-session");
    assert!(!info.to_string().contains("verification"));
    f.service.unbind_run(g).unwrap();
}

#[test]
fn ag7_evidence_discloses_changed_inputs_missing_files_and_lossy_capped_logs() {
    let f = Fixture::new();
    std::fs::write(f.root.join("input.rs"), "before").unwrap();
    let mut log = vec![b'x'; 300000];
    log[0] = 0xff;
    std::fs::write(f.root.join("large.log"), log).unwrap();
    let out = once(&f, json!({"argv":[f.program,"ag7-change"],"evidence":{
        "directory":"records","bind_files":["input.rs","missing"],"files":["large.log","missing"]}})).unwrap();
    let bytes = std::fs::read(f.root.join(out["evidence"]["path"].as_str().unwrap())).unwrap();
    let record: Value = serde_json::from_slice(&bytes).unwrap();
    assert_ne!(
        record["bindings"]["before"]["input.rs"],
        record["bindings"]["after"]["input.rs"]
    );
    assert_eq!(
        record["bindings"]["before"]["missing"]["status"],
        "unavailable"
    );
    assert_eq!(record["files"][0]["truncated"], true);
    assert_eq!(record["files"][0]["encoding"], "utf8-lossy");
    assert_eq!(record["files"][1]["status"], "unavailable");
    assert_eq!(
        out["exit_code"], 0,
        "evidence observations do not rewrite the real exit"
    );
}

#[test]
fn ag7_evidence_validates_paths_frozen_ownership_and_cancel_before_side_effects() {
    let mut f = Fixture::new();
    for directory in ["../escape", "/outside", "a/../b", ""] {
        assert!(
            once(
                &f,
                json!({"argv":[f.program,"green"],"evidence":{"directory":directory}})
            )
            .is_err()
        );
    }
    let tool = RunCommandTool {
        service: f.service.clone(),
    };
    let args = json!({"argv":[f.program,"green"],"evidence":{"directory":"records"}});
    let prepared = tool.prepare_execution(&args).unwrap();
    f.service.unbind_run(f.generation).unwrap();
    f.generation = f.service.bind_run("new-owner", f.cancel.clone()).unwrap();
    assert!(
        prepared
            .tool
            .invoke(&args, &Project::new(&f.root), &f.cancel)
            .is_err()
    );
    assert!(!f.root.join("records").exists());
    f.cancel.cancel();
    assert!(once(&f, args).is_err());
    assert!(!f.root.join("records").exists());
    assert!(!f.root.join("invocations").exists());
}

#[test]
fn ag7_readonly_cannot_record_evidence_even_if_execute_is_requested() {
    let f = Fixture::new();
    *f.mode.write().unwrap() = PermissionMode::ReadOnly;
    assert!(
        once(
            &f,
            json!({"argv":[f.program,"green"],"evidence":{"directory":"records"}})
        )
        .is_err()
    );
    assert!(!f.root.join("records").exists());
    assert!(!f.root.join("invocations").exists());
}

#[test]
fn ag7_evidence_records_timeout_without_duplicating_raw_output_in_edit_recovery() {
    let f = Fixture::new();
    let storage = f.root.join("recovery");
    std::fs::create_dir(&storage).unwrap();
    let project = Project::new(&f.root).with_file_review(&storage).unwrap();
    let review = project.file_review().unwrap();
    review.begin("synthetic-review", 1).unwrap();
    let out = RunCommandTool {
        service: f.service.clone(),
    }
    .invoke(
        &json!({"argv":[f.program,"wait"],"timeout_seconds":1,"evidence":{"directory":"records"}}),
        &project,
        &f.cancel,
    )
    .unwrap();
    assert_eq!(out["timed_out"], true);
    assert_eq!(out["evidence"]["status"], "recorded");
    let record: Value = serde_json::from_slice(
        &std::fs::read(f.root.join(out["evidence"]["path"].as_str().unwrap())).unwrap(),
    )
    .unwrap();
    assert_eq!(record["timed_out"], true);
    // A deadline may kill the child before its main is scheduled. Do not make
    // artifact correctness depend on the machine scheduling it within 1s.
    let calls = match std::fs::read_to_string(f.root.join("invocations")) {
        Ok(calls) => calls,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => panic!("read synthetic invocation witness: {e}"),
    };
    assert!(
        matches!(calls.as_str(), "" | "wait\n"),
        "no duplicate side effects"
    );
    assert!(
        review
            .load("synthetic-review", 1)
            .unwrap()
            .unwrap()
            .files
            .is_empty(),
        "raw command artifacts are not duplicated in file recovery"
    );
    review.end();
}

#[test]
fn ag7_failure_context_is_cumulative_across_split_lines_polling_and_empty_polls() {
    let f = Fixture::new();
    let start = f
        .exec(&json!({"argv":[f.program,"ag7-poll"],"stdin":"pipe","yield_time_ms":250}))
        .unwrap();
    let id = &start["session_id"];
    let mut middle = f.poll(id, "continue\n");
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while !middle["failure_context"]["diagnostics"]
        .to_string()
        .contains("SYNTHETIC_POLL_FAILURE")
        && std::time::Instant::now() < deadline
    {
        middle = f.poll(id, "");
    }
    assert_eq!(middle["failure_context"]["tests"], json!(["case::polled"]));
    let done = f.finish(f.poll(id, "finish\n"));
    assert_eq!(done["exit_code"], 101);
    let empty = f.poll(id, "");
    assert_eq!(empty["stdout"], "");
    assert_eq!(empty["failure_context"], done["failure_context"]);
    assert!(
        empty["failure_context"]["diagnostics"]
            .to_string()
            .contains("SYNTHETIC_POLL_FAILURE")
    );
}

#[cfg(unix)]
#[test]
fn ag7_session_metadata_does_not_panic_or_invent_a_non_utf8_project_path() {
    use std::os::unix::ffi::OsStringExt as _;
    let f = Fixture::new();
    let root = f.root.join(std::ffi::OsString::from_vec(
        b"synthetic-non-utf8-\xff".to_vec(),
    ));
    // This is a metadata-encoding unit control: APFS rejects such directory
    // names. No filesystem operation or command uses this synthetic path.
    let sandbox = Arc::new(
        SandboxService::new(f.root.clone(), SandboxModeSource::Shared(f.mode.clone())).unwrap(),
    );
    let service = Arc::new(ProcessService::new(Project::new(&root), sandbox));
    let generation = service
        .bind_run("synthetic-non-utf8-session", CancelToken::new())
        .unwrap();
    let tool = process_tools(service.clone())
        .into_iter()
        .find(|t| t.definition().name == "session_info")
        .unwrap();
    let info = tool
        .invoke(&json!({}), &Project::new(&root), &CancelToken::new())
        .unwrap();
    assert_eq!(info["session_id"], "synthetic-non-utf8-session");
    assert!(
        info["project_root"].is_null(),
        "unrepresentable paths must not be invented"
    );
    let error = ExecCommandTool {
        service: service.clone(),
    }
    .error_output(&prepared::process_error(
        crate::process::ProcessError::WorkdirOutOfScope,
    ));
    assert!(error["invocation_recovery"]["project_root"].is_null());
    assert_eq!(error["invocation_recovery"]["command_started"], false);
    service.unbind_run(generation).unwrap();
    service.close().unwrap();
}

#[cfg(unix)]
#[test]
fn ag7_evidence_rejects_symlink_escapes_and_never_blocks_on_special_files() {
    use std::os::unix::fs::symlink;
    let f = Fixture::new();
    let outside = super::super::sbx4_tests::root();
    symlink(&outside, f.root.join("outside")).unwrap();
    assert!(
        once(
            &f,
            json!({"argv":[f.program,"green"],"evidence":{"directory":"outside/records"}})
        )
        .is_err()
    );
    assert!(!f.root.join("invocations").exists());
    std::fs::write(outside.join("private"), "SYNTHETIC_EXTERNAL").unwrap();
    symlink(outside.join("private"), f.root.join("link")).unwrap();
    let fifo = std::ffi::CString::new(f.root.join("pipe").to_str().unwrap()).unwrap();
    assert_eq!(unsafe { libc::mkfifo(fifo.as_ptr(), 0o600) }, 0);
    let out = once(
        &f,
        json!({"argv":[f.program,"green"],"evidence":{
        "directory":"records","files":["link","pipe"],"bind_files":["link","pipe"]}}),
    )
    .unwrap();
    let text =
        std::fs::read_to_string(f.root.join(out["evidence"]["path"].as_str().unwrap())).unwrap();
    let record: Value = serde_json::from_str(&text).unwrap();
    assert!(!text.contains("SYNTHETIC_EXTERNAL"));
    assert!(
        record["files"]
            .as_array()
            .unwrap()
            .iter()
            .all(|r| r["status"] == "unavailable")
    );
    crate::test_support::cleanup_tree(&outside);
}
