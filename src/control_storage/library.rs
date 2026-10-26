//! Organization metadata only. Message and attachment bodies remain in journals.
use super::*;
use serde::{Deserialize, Serialize};
#[derive(Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Library {
    entries: Vec<serde_json::Value>,
}
impl ControlStorage {
    pub(crate) fn library(
        &self,
        project: &str,
        change: Option<(serde_json::Value, bool)>,
    ) -> Result<Vec<serde_json::Value>, String> {
        let _guard = self.lock();
        let raw =
            crate::private_fs::read_private_text(&self.dir, "library-v1.json", 2 * 1024 * 1024)?;
        let mut data: Library = raw
            .map(|s| serde_json::from_str(&s))
            .transpose()
            .map_err(|e| e.to_string())?
            .unwrap_or_default();
        if let Some((mut entry, remove)) = change {
            let id = entry["id"]
                .as_str()
                .ok_or("invalid library identity")?
                .to_owned();
            data.entries
                .retain(|e| e["project_id"] != project || e["id"] != id);
            if !remove {
                if data.entries.len() >= 2000 {
                    return Err("Library capacity reached".into());
                }
                entry["project_id"] = serde_json::json!(project);
                data.entries.push(entry);
            }
            let text = serde_json::to_string_pretty(&data).map_err(|e| e.to_string())?;
            if text.len() > 2 * 1024 * 1024 {
                return Err("Library capacity reached".into());
            }
            crate::private_fs::write_text_atomic_in_dir(&self.dir, "library-v1.json", &text)?;
        }
        Ok(data
            .entries
            .into_iter()
            .filter(|e| e["project_id"] == project)
            .collect())
    }
}
