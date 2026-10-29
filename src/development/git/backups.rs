use super::*;
impl GitService {
    pub(super) fn backups(&self) -> Result<Value, String> {
        let dir = self.store.dir()?;
        let mut rows = Vec::new();
        for entry in dir.entries().map_err(io)?.take(10000) {
            let entry = entry.map_err(io)?;
            let name = entry.file_name().to_string_lossy().into_owned();
            if !name.starts_with("git-backup-") || !name.ends_with(".json") {
                continue;
            }
            let files: Vec<Value> = self.store.load(&name)?;
            if files.is_empty() || files.iter().any(|f| f["ready"] != true) {
                continue;
            }
            let modified = entry
                .metadata()
                .map_err(io)?
                .modified()
                .map_err(io)?
                .into_std()
                .duration_since(std::time::UNIX_EPOCH)
                .map_err(|_| "invalid_time")?
                .as_millis()
                .to_string();
            rows.push(json!({"id":name.trim_start_matches("git-backup-").trim_end_matches(".json"),"paths":files.iter().map(|f|&f["path"]).collect::<Vec<_>>(),"modified":modified}));
        }
        rows.sort_by_key(|row| {
            std::cmp::Reverse(row["modified"].as_str().unwrap_or("").to_owned())
        });
        rows.truncate(50);
        Ok(json!({"backups":rows}))
    }
}
