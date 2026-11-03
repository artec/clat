//! Independent terminal checks after the scenario's Application has dropped.
use super::*;

pub(super) fn attach(manifest: &Manifest, result: &mut Value) {
    if manifest.role != "execute" {
        return;
    }
    let case = corpus::Case {
        family: manifest.family.clone(),
        trial: manifest.trial,
        control: manifest.control,
    };
    let root = manifest.output.join(case.id());
    // A different thread also prevents Windows mutex reentry from proving a false release.
    let audit = std::thread::scope(|scope| scope.spawn(|| verify(&root)).join());
    match audit {
        Ok(Ok(value)) => {
            if result["safety_failure"] != true {
                result["safety_failure"] = json!(false);
            }
            result["terminal_audit"] = value;
        }
        Ok(Err(error)) => {
            result["terminal_audit"] = json!({"verified":false,"error":error});
            if result["safety_failure"] != true {
                result["safety_failure"] = Value::Null;
            }
        }
        Err(_) => {
            result["terminal_audit"] =
                json!({"verified":false,"error":"terminal verification panicked"});
            if result["safety_failure"] != true {
                result["safety_failure"] = Value::Null;
            }
        }
    }
}

fn verify(root: &Path) -> Result<Value, String> {
    let storage = root.join("state");
    if !storage.is_dir() {
        return Err("scenario storage unavailable".into());
    }
    let _lease = crate::session::root_lease::try_acquire(&storage)
        .map_err(|e| e.to_string())?
        .ok_or("scenario root still has a producer")?;
    let requests = requests(root)?;
    let backend = crate::session::persistence::JsonlBackend::new(
        storage.join("sessions"),
        crate::session::persistence::JsonlCompression::Zstd,
        false,
    );
    let headers = backend.list_headers().map_err(|e| e.to_string())?;
    if headers.len() != 1 {
        return Err("exactly one synthetic session required".into());
    }
    let events = super::super::super::tests::load_events(&storage);
    let mut registry = crate::session::projection::ProjectionRegistry::clat();
    registry.fold_all(&events).map_err(|e| e.to_string())?;
    let mut publications = 0;
    for event in &events {
        if event.event_type == "tool/call" {
            match event.data["name"].as_str() {
                Some("phase9_resource") => {}
                Some("phase9_publish") => publications += 1,
                _ => return Err("unexpected durable tool call".into()),
            }
        }
    }
    if publications > 1 {
        return Err("duplicate publication attempt".into());
    }
    if !exercise::checkpoint_replay(&storage) {
        return Err("checkpoint replay differs".into());
    }
    if !root.join("maintenance-views.json").exists() {
        views::export(&storage, root, &headers[0].id.to_string());
    }
    use sha2::{Digest, Sha256};
    let maintenance_sha256 = format!(
        "{:x}",
        Sha256::digest(
            std::fs::read(root.join("maintenance-views.json")).map_err(|e| e.to_string())?
        )
    );
    Ok(
        json!({"verified":true,"root_lease_reacquired":true,"projection_replay_verified":true,"requests_ended":requests,"publication_calls":publications,"maintenance_sha256":maintenance_sha256}),
    )
}

fn requests(root: &Path) -> Result<usize, String> {
    let mut count = 0;
    for entry in std::fs::read_dir(root).map_err(|e| e.to_string())? {
        let entry = entry.map_err(|e| e.to_string())?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if !name.starts_with("request-") || !name.ends_with("-start.json") {
            continue;
        }
        let start = read(&entry.path())?;
        if start["tools"]
            .as_array()
            .ok_or("missing tool exposure")?
            .iter()
            .any(|t| {
                !matches!(
                    t["name"].as_str(),
                    Some("phase9_resource" | "phase9_publish")
                )
            })
        {
            return Err("unexpected model tool exposure".into());
        }
        let end = read(&root.join(name.replace("-start", "-end")))?;
        if let Some(calls) = end["outcome"]["tool_calls"].as_array()
            && calls.iter().any(|c| {
                !matches!(
                    c["name"].as_str(),
                    Some("phase9_resource" | "phase9_publish")
                ) || (c["name"] == "phase9_publish" && end["stage"] != "publish")
            })
        {
            return Err("unexpected or unauthorized model tool invocation".into());
        }
        count += 1;
    }
    if count == 0 {
        return Err("no request observations".into());
    }
    Ok(count)
}

fn read(path: &Path) -> Result<Value, String> {
    if !std::fs::symlink_metadata(path)
        .map_err(|e| e.to_string())?
        .file_type()
        .is_file()
    {
        return Err("observation must be a regular file".into());
    }
    serde_json::from_slice(&std::fs::read(path).map_err(|e| e.to_string())?)
        .map_err(|e| e.to_string())
}
