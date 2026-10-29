//! Per-client drafts are private snapshots, never writes to project files.
use super::*;
#[derive(Default, Serialize, Deserialize)]
struct Drafts {
    version: u64,
    rows: std::collections::BTreeMap<String, Value>,
}
impl DevelopmentService {
    pub(super) fn draft_request(&self, method: &str, value: &Value) -> Result<Value, String> {
        let client = identity(value, "clientId")?;
        let name = format!("drafts-{}.json", digest(client.as_bytes()));
        if method == "editor.drafts.recover" {
            return self.recover_drafts(&name);
        }
        let mut drafts: Drafts = self.store.load(&name)?;
        if method == "editor.drafts" {
            return Ok(
                json!({"version":drafts.version,"documents":drafts.rows.values().collect::<Vec<_>>()}),
            );
        }
        if value.get("expectedVersion").and_then(Value::as_u64) != Some(drafts.version) {
            return Err("draft_conflict".into());
        }
        let path = text(value, "path")?;
        relative(path)?;
        self.check_private(path)?;
        match method {
            "editor.draft.save" => {
                if text(value, "text")?.len() > FILE_LIMIT {
                    return Err("file_large".into());
                }
                if drafts.rows.len() >= 32 && !drafts.rows.contains_key(path) {
                    return Err("draft_limit".into());
                }
                drafts.rows.insert(path.into(),json!({"path":path,"text":text(value,"text")?,"version":text(value,"version")?,"bom":value["bom"],"newline":value["newline"]}));
            }
            "editor.draft.delete" => {
                drafts.rows.remove(path);
            }
            _ => return Err("unknown_method".into()),
        }
        drafts.version += 1;
        self.store.save(&name, &drafts)?;
        Ok(json!({"version":drafts.version}))
    }
    fn recover_drafts(&self, current: &str) -> Result<Value, String> {
        let dir = self.store.dir()?;
        let mut recovered = Vec::new();
        for entry in dir.entries().map_err(io)?.take(512) {
            let entry = entry.map_err(io)?;
            let name = entry.file_name().to_string_lossy().into_owned();
            if name == current || !name.starts_with("drafts-") || !name.ends_with(".json") {
                continue;
            }
            let drafts: Drafts = self.store.load(&name)?;
            for row in drafts.rows.into_values() {
                recovered.push(row);
            }
        }
        Ok(json!({"documents":recovered}))
    }
}
