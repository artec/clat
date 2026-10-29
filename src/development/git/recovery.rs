use super::*;
#[derive(Serialize, Deserialize)]
struct Backup {
    path: String,
    bytes: Option<Vec<u8>>,
    #[serde(default)]
    after: Option<Vec<u8>>,
    #[serde(default)]
    ready: bool,
}
impl GitService {
    pub(super) fn finish_backup(&self, value: &Value) -> Result<(), String> {
        let name = format!("git-backup-{}.json", identity(value, "operationId")?);
        let mut files: Vec<Backup> = self.store.load(&name)?;
        for file in &mut files {
            file.after = self
                .project
                .read_file_limited(&file.path, 16 * 1024 * 1024 + 1)
                .map_err(io)?;
            file.ready = true;
        }
        self.store.save(&name, &files)
    }
    pub(super) fn restore_backup(&self, value: &Value) -> Result<Value, String> {
        if value["confirmed"] != true {
            return Err("confirmation_required".into());
        }
        let name = format!("git-backup-{}.json", identity(value, "backupId")?);
        let files: Vec<Backup> = self.store.load(&name)?;
        if files.is_empty() || files.iter().any(|f| !f.ready) {
            return Err("backup_unavailable".into());
        }
        for file in &files {
            relative(&file.path)?;
            let current = self
                .project
                .read_file_limited(&file.path, 16 * 1024 * 1024 + 1)
                .map_err(io)?;
            if current != file.after && current != file.bytes {
                return Err("file_conflict".into());
            }
        }
        for file in files {
            if self
                .project
                .read_file_limited(&file.path, 16 * 1024 * 1024 + 1)
                .map_err(io)?
                == file.bytes
            {
                continue;
            }
            self.project
                .replace_human_file(&file.path, file.after.as_deref(), file.bytes.as_deref())
                .map_err(io)?;
        }
        Ok(json!({"restored":true,"snapshot":self.snapshot()?}))
    }
}
