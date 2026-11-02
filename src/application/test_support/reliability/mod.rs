//! Explicitly armed synthetic long-task campaign. Never reads owner state.
use super::*;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};
mod contracts;
mod corpus;
mod diagnosis;
mod exercise;
mod fidelity;
mod harness2;
mod model;
mod recompute;
mod semantic_regression;
mod title_race;
mod tools;
mod wire;

fn local_evidence_armed() -> bool {
    local_evidence_root().is_some()
}

fn evidence_root(value: Option<std::ffi::OsString>) -> Option<PathBuf> {
    value.map(PathBuf::from).filter(|p| p.is_absolute())
}

fn local_evidence_root() -> Option<PathBuf> {
    let root = evidence_root(std::env::var_os("CLAT_PHASE9_RECOMPUTE"));
    if root.is_none() {
        eprintln!("phase9 local evidence NOT_ARMED (not an executed acceptance)");
    }
    root
}

#[test]
fn reliability_empty_or_relative_evidence_values_do_not_arm_local_readers() {
    for value in [None, Some("".into()), Some("relative/campaign".into())] {
        assert!(evidence_root(value).is_none());
    }
    let absolute = std::env::temp_dir().join("synthetic-evidence-not-opened");
    assert_eq!(
        evidence_root(Some(absolute.clone().into_os_string())),
        Some(absolute)
    );
}

#[test]
#[ignore = "paid synthetic GLM campaign: explicitly set CLAT_PHASE9_LIVE and CLAT_GLM_CODING_PLAN_KEY"]
fn live_glm_long_task_reliability_campaign() {
    if std::env::var("CLAT_PHASE9_LIVE").as_deref() != Ok("1") {
        eprintln!("phase9 campaign NOT_ARMED (not an executed acceptance)");
        return;
    }
    assert!(
        !std::env::var("CLAT_GLM_CODING_PLAN_KEY")
            .unwrap()
            .is_empty()
    );
    let out = PathBuf::from(std::env::var_os("CLAT_PHASE9_OUTPUT").expect("new output directory"));
    std::fs::create_dir(&out).expect("never overwrite an earlier campaign");
    let roster = corpus::roster();
    write_new(
        &out.join("preregistration.json"),
        &json!({
            "version":1,"model":"glm-5.3-flash","route":"GLM Coding Plan",
            "candidates":9,"controls":3,"roster":roster,"concurrent_families":3,"resource_chars":3000,
            "window":24000,"output_limit":4096,"control_window":1000000,"image_batches":exercise::IMAGE_BATCHES,
            "policy":"all started samples retained; no retry-to-green; no private inputs",
            "source_sha256":corpus::digest(),"cost":null
        }),
    );
    let results = Mutex::new(Vec::new());
    let stop = AtomicBool::new(false);
    std::thread::scope(|scope| {
        for family in roster.chunks(4) {
            let results = &results;
            let stop = &stop;
            let out = &out;
            scope.spawn(move || {
                for case in family {
                    if stop.load(Ordering::SeqCst) {
                        break;
                    }
                    let row = run_one(case, out);
                    if row["safety_failure"] == true {
                        stop.store(true, Ordering::SeqCst);
                    }
                    results.lock().unwrap().push(row);
                }
            });
        }
    });
    let mut results = results.into_inner().unwrap();
    results.sort_by_key(|row| row["id"].as_str().unwrap().to_owned());
    write_new(
        &out.join("results.json"),
        &json!({"results":results,"expected":12}),
    );
    assert_eq!(results.len(), 12, "incomplete roster is not a pass");
    assert!(
        results.iter().all(|r| r["status"] == "passed"),
        "retain and review failed cases"
    );
}

fn run_one(case: &corpus::Case, out: &Path) -> Value {
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        exercise::run_case(case, out)
    }));
    let row = match result {
        Ok(Ok(row)) => row,
        Ok(Err(error)) => json!({"id":case.id(),"status":"failed","error":error}),
        Err(error) => json!({"id":case.id(),"status":"failed","error":panic_text(error)}),
    };
    write_new(&out.join(format!("{}.json", case.id())), &row);
    eprintln!("phase9 {} {}", case.id(), row["status"]);
    row
}

fn write_new(path: &Path, value: &Value) {
    use std::io::Write;
    let mut options = std::fs::OpenOptions::new();
    options.create_new(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path).unwrap();
    let mut text = serde_json::to_string_pretty(value).unwrap();
    for name in ["CLAT_GLM_CODING_PLAN_KEY", "CLAT_HARNESS2_DEEPSEEK_KEY"] {
        if let Ok(secret) = std::env::var(name)
            && !secret.is_empty()
        {
            text = text.replace(&secret, "[REDACTED]");
        }
    }
    file.write_all(text.as_bytes()).unwrap();
}

fn panic_text(error: Box<dyn std::any::Any + Send>) -> String {
    error
        .downcast_ref::<String>()
        .cloned()
        .or_else(|| error.downcast_ref::<&str>().map(|s| s.to_string()))
        .unwrap_or_else(|| "non-string panic".into())
}
