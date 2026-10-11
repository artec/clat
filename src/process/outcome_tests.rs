use super::*;

fn entry(provider: &str, code: Option<i32>, signal: Option<&str>) -> Arc<ProcessEntry> {
    let owner = RunOwner {
        generation: 1,
        session_id: "outcome-unit".into(),
        cancel: CancelToken::new(),
    };
    let entry = ProcessEntry::new(
        &owner,
        ProcessEntryInit {
            id: 1,
            command: "unit".into(),
            tty: false,
            sandbox: SandboxFacts {
                provider: provider.into(),
                mode: crate::sandbox::SandboxLevel::WorkspaceWrite,
                enforcement: if provider == "none" {
                    "none"
                } else {
                    "partial"
                }
                .into(),
                policy_digest: None,
                fallback_reason: None,
            },
            limits: ProcessLimits::default(),
            stdin: Box::new(std::io::sink()),
            stdout_capacity: 1024,
            stderr_capacity: 1024,
            pty_capacity: 1024,
            notice_sink: None,
        },
    );
    entry.state.lock().unwrap().terminal = Some(TerminalStatus {
        exit_code: code,
        signal: signal.map(str::to_owned),
        timed_out: false,
        cancelled: false,
        terminated: false,
    });
    entry
}

#[test]
fn command_outcome_diagnostics_ignore_consumption_budget_and_preserve_raw_fields() {
    let entry = entry("none", Some(126), None);
    entry.append(StreamKind::Stdout, b"a full stdout budget");
    entry.append(StreamKind::Stderr, b"sh: ./probe: Permission denied\n");
    let hidden = entry.consume_limits(8, 0, 0);
    assert_eq!(hidden.stderr, "");
    assert!(hidden.stderr_truncated);
    assert_eq!(hidden.command_outcome.json()["reason"], "execution_denied");
    let visible = entry.consume_limits(1024, 1024, 0);
    assert_eq!(visible.stderr, "sh: ./probe: Permission denied\n");
    assert_eq!(visible.exit_code, Some(126));
    assert!(!visible.timed_out);
    assert_eq!(hidden.command_outcome, visible.command_outcome);
    assert_eq!(
        entry.consume_limits(1024, 1024, 0).command_outcome,
        visible.command_outcome
    );
}

#[test]
fn command_outcome_resource_signatures_are_inferences_and_bare_kill_is_unknown() {
    for (code, text, reason) in [
        (126, "sh: ./probe: Exec format error", "executable_format"),
        (
            127,
            "sh: missing: command not found",
            "executable_not_found",
        ),
        (
            1,
            "rustc: No space left on device (os error 28)",
            "resource_unavailable",
        ),
        (1, "loader: Cannot allocate memory", "resource_unavailable"),
    ] {
        let entry = entry("none", Some(code), None);
        entry.append(StreamKind::Stderr, text.as_bytes());
        let output = entry.consume_limits(0, 0, 0);
        assert_eq!(output.command_outcome.json()["reason"], reason);
        assert_eq!(
            output.command_outcome.json()["evidence"],
            "output_signature"
        );
        assert!(!output.sandbox_denied);
    }
    let killed = entry("none", None, Some("9"));
    killed.append(StreamKind::Stderr, b"Killed");
    assert_eq!(
        killed.consume(1024).command_outcome.json()["kind"],
        "unknown_failure"
    );
}

#[test]
fn command_outcome_lifecycle_and_sandbox_diagnostics_remain_distinct() {
    let entry = entry("windows-acl", Some(127), None);
    entry.append(
        StreamKind::Stderr,
        b"windows-acl-run: CreateRestrictedToken Win32 5",
    );
    let unavailable = entry.consume(0);
    assert!(unavailable.sandbox_unavailable);
    assert_eq!(
        unavailable.command_outcome.json()["reason"],
        "sandbox_unavailable"
    );
    assert_eq!(
        unavailable.command_outcome.json()["evidence"],
        "sandbox_diagnostic"
    );
    entry
        .state
        .lock()
        .unwrap()
        .terminal
        .as_mut()
        .unwrap()
        .cancelled = true;
    let cancelled = entry.consume(0);
    assert!(cancelled.cancelled);
    assert_eq!(cancelled.command_outcome.json()["reason"], "cancelled");
    assert_eq!(cancelled.command_outcome.json()["kind"], "interrupted");
    let mut state = entry.state.lock().unwrap();
    state.terminal.as_mut().unwrap().cancelled = false;
    state.terminal.as_mut().unwrap().terminated = true;
    drop(state);
    assert_eq!(
        entry.consume(0).command_outcome.json()["reason"],
        "terminated"
    );
}

#[test]
#[cfg(not(windows))]
fn command_outcome_pty_uses_merged_diagnostics() {
    let entry = entry("none", Some(126), None);
    entry.append(StreamKind::Pty, b"sh: ./probe: Permission denied\r\n");
    let output = entry.consume_limits(0, 0, 1024);
    assert_eq!(output.command_outcome.json()["reason"], "execution_denied");
    assert!(output.pty.contains("Permission denied"));
}
