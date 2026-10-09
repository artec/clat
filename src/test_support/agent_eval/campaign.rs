//! No-paid report round trip through the original Application scenario seam.
use super::*;

#[test]
fn offline_campaign_exports_real_application_control() {
    let fixture = Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../.."))
        .join("tests/fixtures/agent-scenarios/write-file-permission-control");
    let report = run_fixture(&fixture).expect("real Application control");
    assert_eq!(report.gate, GateStatus::Matched);
    assert!(report.files_match);
    assert_eq!(report.application_close, CloseStatus::Clean);
    assert!(
        report
            .durable_event_kinds
            .iter()
            .any(|kind| kind == "tool/result")
    );
    let serialized = report_json(&report).unwrap();
    let restored: serde_json::Value = serde_json::from_str(&serialized).unwrap();
    assert_eq!(restored["gate"], "matched");
    // Explicit developer-only output. Default tests never publish campaign data.
    if let Some(directory) = std::env::var_os("CLAT_AGENT_EVAL_EXPORT_DIR") {
        let directory = PathBuf::from(directory);
        assert!(directory.is_absolute(), "export directory must be absolute");
        let metadata = std::fs::symlink_metadata(&directory).expect("existing private output");
        assert!(metadata.is_dir() && !metadata.file_type().is_symlink());
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        use std::io::Write;
        let mut file = options
            .open(directory.join("application-control.json"))
            .unwrap();
        file.write_all(serialized.as_bytes()).unwrap();
        file.sync_all().unwrap();
    }
}
