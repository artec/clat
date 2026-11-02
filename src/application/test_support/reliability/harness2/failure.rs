//! Preserve incomplete experiment observations without claiming lifecycle acceptance.
use super::*;

pub(super) fn record(manifest: &Manifest, error: String) -> Value {
    let mut result =
        json!({"status":"failed","error":error,"safety_failure":null,"lifecycle_verified":false});
    if manifest.role != "execute" {
        return result;
    }
    let case = corpus::Case {
        family: manifest.family.clone(),
        trial: manifest.trial,
        control: manifest.control,
    };
    let root = manifest.output.join(case.id());
    result["evidence_root"] = json!(root);
    match observations(&root) {
        Ok((stages, last)) => {
            result["completed_stages"] = json!(stages);
            result["failure"] =
                json!({"last_completed_request":last,"classification":"not_adjudicated"});
        }
        Err(error) => result["diagnostic_error"] = json!(error),
    }
    result
}

fn observations(root: &Path) -> Result<(Vec<Value>, Option<Value>), String> {
    let mut stages = Vec::new();
    let mut last = None;
    for entry in std::fs::read_dir(root).map_err(|e| e.to_string())? {
        let entry = entry.map_err(|e| e.to_string())?;
        let name = entry.file_name().to_string_lossy().into_owned();
        let number = name
            .strip_prefix("request-")
            .and_then(|s| s.strip_suffix("-end.json"))
            .and_then(|s| s.parse::<u64>().ok());
        let stage = name.starts_with("stage-") && name.ends_with(".json");
        if !stage && number.is_none() {
            continue;
        }
        if !entry.file_type().map_err(|e| e.to_string())?.is_file() {
            return Err("diagnostic observation must be a regular file".into());
        }
        let data: Value =
            serde_json::from_slice(&std::fs::read(entry.path()).map_err(|e| e.to_string())?)
                .map_err(|e| e.to_string())?;
        let row = json!({"file":name,"observation":data});
        if stage {
            stages.push(row);
        } else if let Some(number) = number
            && last.as_ref().is_none_or(|(old, _)| number > *old)
        {
            last = Some((number, row));
        }
    }
    stages.sort_by_key(|row| row["file"].as_str().unwrap_or_default().to_owned());
    Ok((stages, last.map(|(_, value)| value)))
}

#[test]
fn harness2_failure_keeps_observed_usage_and_marks_unverified_safety_unknown() {
    let root = std::env::temp_dir().join(format!("harness2-failure-{}", uuid::Uuid::new_v4()));
    let evidence = root.join("tools-candidate-10");
    std::fs::create_dir_all(&evidence).unwrap();
    let source = evidence.join("request-0004-end.json");
    let bytes = serde_json::to_vec(&json!({"stage":"recall-pre-tools","usage":{"input":17,"output":4096},"outcome":{"finish":"MaxTokens"}})).unwrap();
    std::fs::write(&source, &bytes).unwrap();
    std::fs::write(
        evidence.join("stage-initial.json"),
        br#"{"stage":"initial","text":"ACK"}"#,
    )
    .unwrap();
    let manifest: Manifest =
        serde_json::from_value(json!({"role":"execute","output":root,"family":"tools","trial":10}))
            .unwrap();
    let report = record(&manifest, "caught failure".into());
    assert_eq!(report["status"], "failed");
    assert_eq!(report["error"], "caught failure");
    assert_eq!(
        report["failure"]["last_completed_request"]["observation"]["outcome"]["finish"],
        "MaxTokens"
    );
    assert_eq!(
        report["failure"]["last_completed_request"]["observation"]["usage"]["output"],
        4096
    );
    assert_eq!(
        report["completed_stages"][0]["observation"]["stage"],
        "initial"
    );
    assert!(report["safety_failure"].is_null());
    assert_eq!(report["lifecycle_verified"], false);
    assert_eq!(std::fs::read(&source).unwrap(), bytes);
    crate::test_support::cleanup_tree(&root);
}
