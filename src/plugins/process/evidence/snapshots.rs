use super::*;
const FILE_LIMIT: usize = 256 * 1024;

pub(super) fn bindings(project: &Project, files: &[String], cancel: &CancelToken) -> Value {
    let mut budget = 512 * 1024 * 1024u64;
    let mut rows = serde_json::Map::new();
    for path in files {
        let limit = budget.min(256 * 1024 * 1024);
        let row = match project.fingerprint_file(path, limit, cancel) {
            Ok(identity) => {
                budget = budget.saturating_sub(identity.bytes);
                json!({"status":"observed","bytes":identity.bytes,"sha256":identity.sha256})
            }
            Err(error) => {
                budget = budget.saturating_sub(limit);
                json!({"status":"unavailable","reason":format!("{:?}", error.kind())})
            }
        };
        rows.insert(path.clone(), row);
    }
    Value::Object(rows)
}

pub(super) fn executable(
    project: &Project,
    args: &Value,
    cwd: &str,
    cancel: &CancelToken,
) -> Value {
    let Some(program) = args
        .get("argv")
        .and_then(Value::as_array)
        .and_then(|a| a.first())
        .and_then(Value::as_str)
    else {
        return json!({"status":"unavailable","reason":"shell_execution"});
    };
    if !Path::new(program).is_absolute() {
        return json!({"status":"unavailable","reason":"relative_or_path_lookup_not_bound"});
    }
    let full = Path::new(cwd).join(program);
    let Ok(relative) = full.strip_prefix(project.root()) else {
        return json!({"status":"unavailable","reason":"outside_project"});
    };
    let Some(relative) = relative.to_str() else {
        return json!({"status":"unavailable","reason":"non_unicode_path"});
    };
    let Some(identity) = bindings(project, &[relative.into()], cancel)
        .get(relative)
        .cloned()
    else {
        return json!({"status":"unavailable"});
    };
    json!({"scope":"literal_project_file_observed_before_execution","path":relative,"identity":identity,
        "execution_authenticated":false})
}

pub(super) fn file(project: &Project, path: &str) -> Value {
    match project.read_file_limited(path, FILE_LIMIT + 1) {
        Ok(Some(mut bytes)) => {
            let truncated = bytes.len() > FILE_LIMIT;
            bytes.truncate(FILE_LIMIT);
            let text = String::from_utf8_lossy(&bytes);
            json!({"path":path,"status":"captured","bytes":bytes.len(),"truncated":truncated,
                "encoding":if matches!(text,std::borrow::Cow::Owned(_)) {"utf8-lossy"} else {"utf8"},
                "captured_bytes_sha256":format!("{:x}",Sha256::digest(&bytes)),"content":text})
        }
        Ok(None) => json!({"path":path,"status":"unavailable","reason":"missing"}),
        Err(error) => {
            json!({"path":path,"status":"unavailable","reason":format!("{:?}",error.kind())})
        }
    }
}
