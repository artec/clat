use super::*;

#[test]
fn infrastructure_capture_enforces_output_and_time_limits_without_shells() {
    let (_, root) = crate::test_support::roots("capture-limits");
    std::fs::create_dir_all(&root).unwrap();
    let source = root.join("child.rs");
    let binary = root.join(if cfg!(windows) { "child.exe" } else { "child" });
    std::fs::write(
        &source,
        r#"
use std::io::Write;
fn main() {
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
    let mut output = Command::new(&binary);
    output.arg("limit");
    let start = Instant::now();
    let error = capture_command(output, 1024, Duration::from_secs(2)).unwrap_err();
    assert!(error.contains("byte limit"), "{error}");
    assert!(start.elapsed() < Duration::from_secs(5));
    let start = Instant::now();
    let error =
        capture_command(Command::new(&binary), 1024, Duration::from_millis(100)).unwrap_err();
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
