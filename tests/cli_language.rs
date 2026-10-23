//! Isolated process tests: UI preference commands never start or mount a Host.
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::Duration;
#[allow(dead_code)]
#[path = "support/bounded_process.rs"]
mod bounded_process;

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "clat-language-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(root.join("home")).unwrap();
        Self(root)
    }
    fn run(&self, args: &[&str], environment: Option<&str>) -> Output {
        let mut command = Command::new(env!("CARGO_BIN_EXE_clat"));
        command
            .current_dir(&self.0)
            .args(args)
            .env("HOME", self.0.join("home"))
            .env("USERPROFILE", self.0.join("home"))
            .env_remove("CLAT_LANG")
            .stdin(Stdio::null());
        if let Some(value) = environment {
            command.env("CLAT_LANG", value);
        }
        bounded_process::BoundedProcess::spawn(
            &mut command,
            &self.0,
            "language",
            Duration::from_secs(15),
        )
        .unwrap()
        .finish()
        .unwrap()
    }
    fn preferences(&self) -> PathBuf {
        self.0.join("home/.clat/ui/terminal.json")
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
fn stdout(output: &Output) -> String {
    String::from_utf8(output.stdout.clone()).unwrap()
}
fn assert_no_business_state(root: &Path) {
    for name in [
        "config.json",
        "host-endpoint.json",
        "sessions",
        "projects.json",
        "storages",
    ] {
        assert!(
            !root.join("home/.clat").join(name).exists(),
            "language command created {name}"
        );
    }
}
#[test]
fn language_help_and_usage_use_the_explicit_client_language() {
    let fixture = Fixture::new();
    for (locale, needle) in [("en", "Usage:"), ("zh-Hans", "用法"), ("zh-Hant", "用法")] {
        let output = fixture.run(&["--lang", locale, "--help"], Some("invalid-environment"));
        assert!(output.status.success(), "{output:?}");
        assert!(output.stderr.is_empty());
        assert!(stdout(&output).contains(needle));
        assert!(stdout(&output).contains("--lang"));
    }
    assert_eq!(
        fixture
            .run(&["--lang", "invalid", "--help"], None)
            .status
            .code(),
        Some(2)
    );
    assert!(!fixture.preferences().exists());
    assert_no_business_state(&fixture.0);
}

#[test]
fn explicit_language_translates_argument_errors_without_opening_storage() {
    let fixture = Fixture::new();
    for (locale, needle) in [("zh-Hans", "未知选项"), ("zh-Hant", "未知選項")] {
        let output = fixture.run(&["--lang", locale, "exec", "--unknown"], Some("en"));
        assert_eq!(output.status.code(), Some(2));
        assert!(output.stdout.is_empty());
        let diagnostic = String::from_utf8_lossy(&output.stderr);
        assert!(diagnostic.contains(needle), "{diagnostic}");
        assert!(diagnostic.contains("--unknown"));
    }
    assert_no_business_state(&fixture.0);
}
#[test]
fn saving_language_is_independent_and_corrupt_preferences_are_preserved() {
    let fixture = Fixture::new();
    assert!(
        fixture
            .run(&["language", "zh-Hant"], Some("en"))
            .status
            .success()
    );
    let output = fixture.run(&["language"], None);
    assert!(output.status.success());
    assert!(stdout(&output).contains("zh-Hant"));
    let preferences = std::fs::read_to_string(fixture.preferences()).unwrap();
    assert!(preferences.contains("zh-Hant"));
    assert!(
        stdout(&fixture.run(&["--lang", "en", "language"], Some("zh-Hans")))
            .starts_with("Language:")
    );
    assert_eq!(
        std::fs::read_to_string(fixture.preferences()).unwrap(),
        preferences
    );
    std::fs::write(fixture.preferences(), "damaged json").unwrap();
    assert!(!fixture.run(&["language", "en"], None).status.success());
    assert_eq!(
        std::fs::read_to_string(fixture.preferences()).unwrap(),
        "damaged json"
    );
    assert_no_business_state(&fixture.0);
}
#[test]
fn machine_version_output_and_subcommand_argument_parsing_do_not_change_with_locale() {
    let fixture = Fixture::new();
    let expected = stdout(&fixture.run(&["--lang", "en", "--version"], None));
    for locale in ["zh-Hans", "zh-Hant"] {
        assert_eq!(
            stdout(&fixture.run(&["--lang", locale, "--version"], None)),
            expected
        );
    }
    // A language-looking argument after the command remains an exec argument.
    let output = fixture.run(&["exec", "--lang=zh-Hant"], Some("en"));
    assert_eq!(output.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&output.stderr).contains("unknown option: --lang=zh-Hant"));
    assert_no_business_state(&fixture.0);
}

#[test]
fn plugin_and_host_argument_failures_translate_without_mounting_business_state() {
    let fixture = Fixture::new();
    for (locale, unknown, port) in [
        ("en", "unknown command", "nonzero port"),
        ("zh-Hans", "未知命令", "非零端口"),
        ("zh-Hant", "未知命令", "非零連接埠"),
    ] {
        let output = fixture.run(&["--lang", locale, "plugin", "bogus"], None);
        assert_eq!(output.status.code(), Some(2));
        assert!(String::from_utf8_lossy(&output.stderr).contains(unknown));
        let output = fixture.run(&["--lang", locale, "host", "status", "--port", "0"], None);
        assert_eq!(output.status.code(), Some(2));
        assert!(String::from_utf8_lossy(&output.stderr).contains(port));
    }
    assert_no_business_state(&fixture.0);
}
