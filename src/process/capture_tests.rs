use super::*;

#[test]
fn infrastructure_capture_enforces_output_and_time_limits_without_shells() {
    capture_limits(false);
}

#[test]
fn infrastructure_capture_cold_start_does_not_masquerade_as_output_limit_failure() {
    capture_limits(true);
}

fn capture_limits(slow_cold_start: bool) {
    let (_, root) = crate::test_support::roots("capture-limits");
    std::fs::create_dir_all(&root).unwrap();
    let source = root.join("child.rs");
    let binary = root.join(if cfg!(windows) { "child.exe" } else { "child" });
    std::fs::write(
        &source,
        r#"
use std::io::Write;
fn main() {
    if let Some(marker) = std::env::args().nth(2) {
        if std::fs::remove_file(marker).is_ok() {
            std::thread::sleep(std::time::Duration::from_secs(4));
        }
    }
    if std::env::args().nth(1).as_deref() == Some("ready") {
        print!("ready");
        return;
    }
    if std::env::args().nth(1).as_deref() == Some("limit") {
        std::io::stdout().write_all(&vec![b'x'; 32768]).unwrap();
        std::io::stdout().flush().unwrap();
    }
    std::thread::sleep(std::time::Duration::from_secs(30));
}
"#,
    )
    .unwrap();
    super::super::compile_rust_test_helper(&source, &binary).unwrap();
    let marker = root.join("cold-start-once");
    if slow_cold_start {
        std::fs::write(&marker, "").unwrap();
    }
    let command = |mode: &str| {
        let mut child = Command::new(&binary);
        child.arg(mode).arg(&marker);
        child
    };
    // Establish executable readiness separately from the byte/timeout contract.
    // First launch can spend time in OS validation or scheduler queues.
    let ready = capture_command(command("ready"), 16, Duration::from_secs(30)).unwrap();
    assert!(ready.status.success());
    assert_eq!(ready.stdout, b"ready");
    let start = Instant::now();
    let error = capture_command(command("limit"), 1024, Duration::from_secs(2)).unwrap_err();
    assert!(error.contains("byte limit"), "{error}");
    assert!(start.elapsed() < Duration::from_secs(5));
    let start = Instant::now();
    let error = capture_command(command("timeout"), 1024, Duration::from_millis(100)).unwrap_err();
    assert!(error.contains("timed out"), "{error}");
    assert!(start.elapsed() < Duration::from_secs(5));
    let missing = capture_command(
        Command::new(root.join("absent-binary")),
        1024,
        Duration::from_secs(1),
    )
    .unwrap_err();
    assert!(missing.starts_with("spawn-not-found:"));
    std::fs::remove_dir_all(root).unwrap();
}
