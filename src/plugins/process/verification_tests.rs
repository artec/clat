use super::*;
mod ag5_tests;
mod agent_loop_tests;

const PROBE: &str = r#"
use std::io::{self, Read, Write};
fn main() {
    let args: Vec<_> = std::env::args().skip(1).collect();
    writeln!(std::fs::OpenOptions::new().create(true).append(true).open("invocations").unwrap(), "{}", args[0]).unwrap();
    match args[0].as_str() {
        "recover" => { let mut s=String::new(); io::stdin().read_line(&mut s).unwrap(); assert_eq!(s,"go\n"); println!("  1 passed (0.1s)"); }
        "pw-green" => { println!("test result: ok. 999 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.01s\n\x1b[32m  13 passed (1.2s)\x1b[0m\n  2 skipped"); }
        "pw-failed" => { println!("  1 failed\n  12 passed (2.0s)"); std::process::exit(1); }
        "pw-zero" => println!("  3 skipped"),
        "pw-flaky" => println!("  1 flaky\n  2 passed (1.0s)"),
        "literal" => { std::fs::write("received-args", format!("{:?}", &args[1..])).unwrap(); println!("{}", args[1..].join("\n")); std::process::exit(7); }
        "wait" => std::thread::sleep(std::time::Duration::from_secs(2)),
        "inspect" => println!("{}", std::fs::read_to_string(&args[1]).unwrap()),
        "eof" => { let mut s=String::new(); io::stdin().read_to_string(&mut s).unwrap(); println!("eof:{}",s.len()); }
        "poll" => {
            print!("test res"); io::stdout().flush().unwrap();
            let mut s = String::new(); io::stdin().read_line(&mut s).unwrap();
            println!("ult: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.01s");
            io::stdout().flush().unwrap(); s.clear(); io::stdin().read_line(&mut s).unwrap();
            std::process::exit(101);
        }
        mode => {
            if mode == "noisy" { print!("{}\n", "\"\\\t".repeat(20000)); }
            let (p,f,i) = match mode { "green" => (1,0,0), "zero" => (0,0,0), "ignored" => (0,0,1), _ => (0,1,0) };
            println!("test result: {}. {p} passed; {f} failed; {i} ignored; 0 measured; 0 filtered out; finished in 0.01s", if f == 0 {"ok"} else {"FAILED"});
            if mode == "noisy" { print!("{}\n", "z".repeat(10000)); }
            if f > 0 { std::process::exit(101); }
        }
    }
}
"#;

struct Fixture {
    root: std::path::PathBuf,
    program: String,
    service: Arc<ProcessService>,
    generation: u64,
    cancel: CancelToken,
    mode: Arc<RwLock<PermissionMode>>,
}
impl Fixture {
    fn new() -> Self {
        let root = super::sbx4_tests::root();
        let source = root.join("probe.rs");
        std::fs::write(&source, PROBE).unwrap();
        let binary = root.join(format!("probe{}", std::env::consts::EXE_SUFFIX));
        crate::process::compile_rust_test_helper(&source, &binary).unwrap();
        let mode = Arc::new(RwLock::new(PermissionMode::FullAccess));
        let sandbox = Arc::new(
            SandboxService::new(root.clone(), SandboxModeSource::Shared(mode.clone())).unwrap(),
        );
        let service = Arc::new(ProcessService::new(Project::new(&root), sandbox));
        let cancel = CancelToken::new();
        let generation = service.bind_run("verification", cancel.clone()).unwrap();
        Self {
            root,
            program: binary.to_string_lossy().into_owned(),
            service,
            generation,
            cancel,
            mode,
        }
    }
    fn args(&self, mode: &str) -> Value {
        let mut args = json!({"argv":[self.program,mode],"yield_time_ms":250,
            "verification":{"format":"rust-test-summary","minimum_executed":1}});
        if matches!(mode, "poll" | "recover") {
            args["stdin"] = json!("pipe");
        }
        args
    }
    fn exec(&self, args: &Value) -> Result<Value, ToolError> {
        ExecCommandTool {
            service: self.service.clone(),
        }
        .invoke(args, &Project::new(&self.root), &CancelToken::new())
    }
    fn poll(&self, id: &Value, input: &str) -> Value {
        WriteStdinTool {
            service: self.service.clone(),
        }
        .invoke(
            &json!({"session_id":id,"chars":input,"yield_time_ms":250}),
            &Project::new(&self.root),
            &CancelToken::new(),
        )
        .unwrap()
    }
    fn finish(&self, mut output: Value) -> Value {
        let deadline = std::time::Instant::now() + Duration::from_secs(20);
        while output["running"] == true && std::time::Instant::now() < deadline {
            output = self.poll(&output["session_id"], "");
        }
        output
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.service.unbind_run(self.generation).unwrap();
        self.service.close().unwrap();
        crate::test_support::cleanup_tree(&self.root);
    }
}

#[test]
fn verification_argv_is_literal_and_uses_the_child_exit() {
    let f = Fixture::new();
    let literal = "|;$(echo unintended) 中文 \" quote";
    let output = f
        .exec(&json!({"argv":[f.program,"literal",literal,""],"yield_time_ms":1000}))
        .expect("argv must be a production execution input");
    let output = f.finish(output);
    assert_eq!(output["exit_code"], 7);
    assert_eq!(output["execution"], "argv");
    assert_eq!(
        std::fs::read_to_string(f.root.join("received-args")).unwrap(),
        format!("{:?}", [literal, ""])
    );
    #[cfg(unix)]
    {
        let output = f.finish(
            f.exec(&json!({"argv":[f.program,"green"],"tty":true}))
                .unwrap(),
        );
        assert_eq!(output["execution"], "argv");
        assert_eq!(output["command_outcome"]["reason"], "process_exit_zero");
        assert!(output.get("verification_receipt").is_none());
    }
}

#[test]
fn verification_cumulative_evidence_survives_polling_and_empty_poll() {
    let f = Fixture::new();
    let start = f.exec(&f.args("poll")).expect("verification must start");
    assert_eq!(start["verification_receipt"]["status"], "running");
    let id = &start["session_id"];
    let mut middle = f.poll(id, "go\n");
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while middle["verification_receipt"]["failed"] != 1 && std::time::Instant::now() < deadline {
        middle = f.poll(id, "");
    }
    assert_eq!(middle["running"], true);
    assert_eq!(middle["verification_receipt"]["failed"], 1, "{middle}");
    let done = f.poll(id, "end\n");
    assert_eq!(done["verification_receipt"]["status"], "failed");
    assert_eq!(done["verification_receipt"]["failed"], 1);
    let again = f.poll(id, "");
    assert_eq!(again["stdout"], "");
    assert_eq!(again["verification_receipt"], done["verification_receipt"]);
}

#[test]
fn verification_zero_ignored_and_normal_controls_have_honest_status() {
    let f = Fixture::new();
    for (mode, status) in [
        ("zero", "inconclusive"),
        ("ignored", "inconclusive"),
        ("green", "passed"),
        ("noisy", "failed"),
    ] {
        let mut args = f.args(mode);
        args["yield_time_ms"] = json!(1000);
        let output = f.finish(f.exec(&args).unwrap());
        let receipt = &output["verification_receipt"];
        assert_eq!(receipt["status"], status, "{output}");
        assert_eq!(receipt["authenticated"], false);
        assert_eq!(receipt["exit_scope"], "direct_child");
        assert!(
            serde_json::to_string(&output).unwrap().chars().count()
                <= crate::plugins::THRESHOLD_CHARS
        );
        let journal = process_journal_output(&output);
        assert_eq!(journal["verification_receipt"], *receipt);
        assert!(journal.get("stdout").is_none());
    }
    assert_eq!(
        std::fs::read_to_string(f.root.join("invocations"))
            .unwrap()
            .lines()
            .count(),
        4
    );
}

#[test]
fn verification_invalid_inputs_and_changed_prepared_args_never_execute() {
    let f = Fixture::new();
    let mut bad = vec![
        json!({}),
        json!({"argv":[]}),
        json!({"argv":[""]}),
        json!({"argv":["x\0y"]}),
        json!({"cmd":"echo x","argv":[f.program]}),
        json!({"argv":[1]}),
    ];
    for change in [
        json!({"tty":true}),
        json!({"verification":{"format":"other","minimum_executed":1}}),
        json!({"verification":{"format":"rust-test-summary","minimum_executed":0}}),
    ] {
        let mut args = f.args("green");
        for (k, v) in change.as_object().unwrap() {
            args[k] = v.clone();
        }
        bad.push(args);
    }
    for args in bad {
        assert!(f.exec(&args).is_err(), "{args}");
    }
    let tool = ExecCommandTool {
        service: f.service.clone(),
    };
    let args = f.args("green");
    let prepared = tool.prepare_execution(&args).unwrap();
    assert!(
        prepared
            .tool
            .invoke(
                &f.args("noisy"),
                &Project::new(&f.root),
                &CancelToken::new()
            )
            .is_err()
    );
    f.cancel.cancel();
    assert!(
        prepared
            .tool
            .invoke(&args, &Project::new(&f.root), &CancelToken::new())
            .is_err()
    );
    assert!(!f.root.join("invocations").exists());
}

#[test]
fn verification_running_cancel_is_not_a_passing_test_run() {
    let f = Fixture::new();
    let start = f.exec(&f.args("poll")).unwrap();
    f.cancel.cancel();
    let output = f
        .service
        .wait_and_consume(
            start["session_id"].as_u64().unwrap(),
            Duration::from_secs(5),
            4096,
        )
        .unwrap();
    assert!(!output.running);
    assert!(output.cancelled);
    assert_eq!(
        output.verification_receipt.unwrap()["status"],
        "inconclusive"
    );
}

#[test]
fn verification_real_cargo_build_failure_cannot_reuse_a_previous_green() {
    let f = Fixture::new();
    std::fs::write(f.root.join("Cargo.toml"), "[package]\nname='receipt_fixture'\nversion='0.1.0'\nedition='2021'\n[workspace]\n[lib]\npath='lib.rs'\n").unwrap();
    let source = f.root.join("lib.rs");
    std::fs::write(&source, "#[test] fn contract() { assert_eq!(2 + 2, 4); }\n").unwrap();
    let args = json!({"argv":["cargo","test","--color","always","--offline","--lib","contract","--","--exact"],
        "yield_time_ms":1000,"verification":{"format":"rust-test-summary","minimum_executed":1}});
    let good = f.finish(f.exec(&args).unwrap());
    assert_eq!(good["verification_receipt"]["status"], "passed", "{good}");
    std::fs::write(source, "compile_error!(\"intentional build failure\");\n").unwrap();
    let bad = f.finish(f.exec(&args).unwrap());
    assert_eq!(bad["verification_receipt"]["status"], "failed", "{bad}");
    assert_eq!(bad["verification_receipt"]["executed"], 0);
    assert_eq!(bad["verification_receipt"]["compile_failed"], true);
    assert_ne!(bad["session_id"], good["session_id"]);
}

#[test]
fn verification_argv_preserves_execute_policy_and_frozen_owner() {
    use crate::permission::{ModePolicy, PermissionPolicy};
    let mut f = Fixture::new();
    let tool = ExecCommandTool {
        service: f.service.clone(),
    };
    let args = f.args("green");
    *f.mode.write().unwrap() = PermissionMode::ReadOnly;
    let prepared = tool.prepare_execution(&args).unwrap();
    let call = crate::ToolCall {
        id: "policy".into(),
        name: "exec_command".into(),
        arguments: args.clone(),
    };
    assert!(matches!(
        ModePolicy::new(f.mode.clone()).check_prepared(
            &Project::new(&f.root),
            &tool.definition(),
            &call,
            Some(&prepared)
        ),
        crate::PermissionDecision::Ask { .. }
    ));
    *f.mode.write().unwrap() = PermissionMode::FullAccess;
    assert!(
        prepared
            .tool
            .invoke(&args, &Project::new(&f.root), &CancelToken::new())
            .is_err()
    );
    let prepared = tool.prepare_execution(&args).unwrap();
    f.service.unbind_run(f.generation).unwrap();
    f.generation = f
        .service
        .bind_run("replacement-run", f.cancel.clone())
        .unwrap();
    assert!(
        prepared
            .tool
            .invoke(&args, &Project::new(&f.root), &CancelToken::new())
            .is_err()
    );
    assert!(!f.root.join("invocations").exists());
    let prepared = tool.prepare_execution(&args).unwrap();
    assert_eq!(
        f.finish(
            prepared
                .tool
                .invoke(&args, &Project::new(&f.root), &CancelToken::new())
                .unwrap()
        )["exit_code"],
        0
    );
    assert!(
        prepared
            .tool
            .invoke(&args, &Project::new(&f.root), &CancelToken::new())
            .is_err()
    );
    assert_eq!(
        std::fs::read_to_string(f.root.join("invocations")).unwrap(),
        "green\n"
    );
}

#[test]
fn verification_argv_limits_are_bytes_and_preserve_empty_arguments() {
    let exact = "中".repeat(21_845);
    assert!(verification::Invocation::parse(&json!({"argv":["p",exact]})).is_ok());
    assert!(verification::Invocation::parse(&json!({"argv":["p",format!("{exact}中")]})).is_err());
    let mut args = vec!["p"; 128];
    assert!(verification::Invocation::parse(&json!({"argv":args})).is_ok());
    args.push("");
    assert!(verification::Invocation::parse(&json!({"argv":args})).is_err());
    let oversized = json!({"argv":["program","x".repeat(MAX_COMMAND_BYTES)]});
    let journal = verification::journal_arguments(&oversized);
    assert!(journal.get("argv").is_none());
    assert_eq!(journal["argv_omitted_from_journal"], true);
    let valid = json!({"argv":["p","", "literal|arg"]});
    assert_eq!(verification::journal_arguments(&valid), valid);
}

#[test]
fn verification_timeout_is_inconclusive_and_legacy_shell_stays_distinct() {
    let f = Fixture::new();
    let argv = vec![
        std::ffi::OsString::from(&f.program),
        std::ffi::OsString::from("wait"),
    ];
    let request = ProcessStart {
        command: "timeout fixture".into(),
        workdir: None,
        tty: false,
        network: false,
        sandbox: SandboxRequest::Auto,
    };
    let spec =
        serde_json::from_value(json!({"format":"rust-test-summary","minimum_executed":1})).unwrap();
    let prepared = f
        .service
        .prepare_direct(&request, argv, Some(spec))
        .unwrap();
    let output = f
        .service
        .run_compat_prepared(request, Duration::from_millis(50), prepared)
        .unwrap();
    assert!(output.timed_out);
    assert_eq!(
        output.verification_receipt.unwrap()["status"],
        "inconclusive"
    );
    #[cfg(unix)]
    {
        let shell = f
            .exec(&json!({"cmd":"false; printf shell-success","yield_time_ms":1000}))
            .unwrap();
        assert_eq!(shell["command_outcome"]["reason"], "shell_exit_zero");
        assert!(shell.get("verification_receipt").is_none());
    }
}

#[test]
fn ag4_batch_default_stdin_gives_batch_process_eof() {
    let f = Fixture::new();
    let output = f.finish(
        f.exec(&json!({"argv":[f.program,"eof"],"yield_time_ms":1000}))
            .unwrap(),
    );
    assert_eq!(
        output["running"], false,
        "a batch command must not wait forever for a pipe the caller never requested"
    );
    assert_eq!(output["exit_code"], 0);
    assert!(output["stdout"].as_str().unwrap().contains("eof:0"));
    let error = WriteStdinTool {
        service: f.service.clone(),
    }
    .invoke(
        &json!({"session_id":output["session_id"],"chars":"late input"}),
        &Project::new(&f.root),
        &CancelToken::new(),
    )
    .unwrap_err()
    .to_string();
    assert!(error.contains("stdin") && error.contains("pipe"), "{error}");
}

#[test]
fn ag4_batch_stdin_invalid_intent_is_rejected_before_spawn() {
    let f = Fixture::new();
    for flag in [json!("unknown"), json!(true), json!(null)] {
        assert!(
            f.exec(&json!({"argv":[f.program,"green"],"stdin":flag}))
                .is_err()
        );
    }
    assert!(
        f.exec(&json!({"argv":[f.program,"green"],"tty":true,"stdin":"closed"}))
            .is_err()
    );
    assert!(!f.root.join("invocations").exists());
}
