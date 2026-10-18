use super::*;
#[test]
fn ag9_terminal_snapshot_folds_cursor_erase_and_alternate_screen_not_history() {
    let mut screen = TerminalScreen::new();
    screen.append(b"\x1b[2J\x1b[Hdraft-kept\x1b[2;1Hstatus", None);
    screen.append(b"\x1b[2;1Hupdated\x1b[K", None);
    let before = screen.snapshot(0, 2);
    assert_eq!(before["lines"], json!(["draft-kept", "updated"]));
    screen.append(b"\x1b[Hgone\x1b[K", None);
    assert_eq!(screen.snapshot(0, 1)["lines"], json!(["gone"]));
    screen.append(b"\x1b[?1049halt", None);
    assert_eq!(screen.snapshot(0, 1)["alternate_screen"], true);
    screen.append(b"\x1b[?1049l", None);
    assert_eq!(screen.snapshot(0, 1)["lines"], json!(["gone"]));
    assert_eq!(screen.snapshot(0, 1)["application_state_verified"], false);
}
#[test]
fn ag9_terminal_snapshot_limits_fail_closed_and_utf8_spans_chunks() {
    let mut screen = TerminalScreen::new();
    for b in "中e\u{301}".as_bytes() {
        screen.append(&[*b], None);
    }
    assert_eq!(screen.snapshot(0, 1)["lines"][0], "中e\u{301}");
    screen.append(&vec![b'x'; INPUT_LIMIT + 1], None);
    assert_eq!(screen.snapshot(0, 1)["projection_complete"], false);
    assert_eq!(screen.snapshot(0, 1)["lines"], json!([]));
    let mut screen = TerminalScreen::new();
    screen.append(b"x", Some((101, 80)));
    assert_eq!(screen.snapshot(0, 1)["projection_complete"], false);
}
#[test]
fn ag9_terminal_snapshot_escaped_text_fits_pruning_and_rows_advance() {
    let mut screen = TerminalScreen::new();
    screen.append(&vec![b'"'; 5760], Some((100, 240)));
    let out = screen.snapshot(0, 24);
    assert_eq!(out["text_truncated"], true);
    assert_eq!(out["next_row"], 12);
    assert!(out.to_string().chars().count() < crate::plugins::THRESHOLD_CHARS);
    assert_eq!(screen.snapshot(12, 12)["text_truncated"], false);
}
#[cfg(all(unix, feature = "runtime-tests"))]
#[test]
fn ag9_terminal_snapshot_real_pty_is_owner_scoped_and_does_not_consume() {
    let (root, service) = crate::process::tests::fixture("ag9-screen", ProcessLimits::default());
    let cancel = CancelToken::new();
    let generation = service.bind_run("one", cancel.clone()).unwrap();
    let id = service
        .start(ProcessStart {
            command: r"printf '\033[2J\033[Hdraft-kept\033[2;1Hstatus'".into(),
            workdir: None,
            tty: true,
            network: false,
            sandbox: SandboxRequest::Auto,
        })
        .unwrap();
    let entry = service.entry_for_current_owner(id).unwrap();
    entry.wait_until(Duration::from_secs(2));
    let before = entry.state.lock().unwrap().last_activity;
    let first = service.terminal_snapshot(id, 0, 2).unwrap();
    assert_eq!(first["lines"], json!(["draft-kept", "status"]));
    assert_eq!(first, service.terminal_snapshot(id, 0, 2).unwrap());
    assert_eq!(before, entry.state.lock().unwrap().last_activity);
    let raw = service.wait_and_consume(id, Duration::ZERO, 1024).unwrap();
    assert!(raw.pty.contains("draft-kept"));
    cancel.cancel();
    assert!(service.terminal_snapshot(id, 0, 2).is_err());
    service.unbind_run(generation).unwrap();
    assert!(service.terminal_snapshot(id, 0, 2).is_err());
    let generation = service.bind_run("two", CancelToken::new()).unwrap();
    assert!(service.terminal_snapshot(id, 0, 2).is_err());
    let pipe = service
        .start(ProcessStart {
            command: "echo pipe".into(),
            workdir: None,
            tty: false,
            network: false,
            sandbox: SandboxRequest::Auto,
        })
        .unwrap();
    assert!(service.terminal_snapshot(pipe, 0, 2).is_err());
    service.unbind_run(generation).unwrap();
    crate::test_support::cleanup_tree(&root);
}
