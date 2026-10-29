use super::*;
impl Terminals {
    pub(super) fn create(&self, value: &Value) -> Result<Value, String> {
        if self.closed.load(Ordering::SeqCst) {
            return Err("host_closed".into());
        }
        let id = identity(value, "operationId")?;
        let signature = digest(value.to_string().as_bytes());
        let client = identity(value, "clientId")?;
        let size = initial_size(value)?;
        let mut entries = self.entries.lock().map_err(|_| "terminal_busy")?;
        if let Some(entry) = entries.get(&id) {
            let row = entry.metadata.lock().map_err(|_| "terminal_busy")?;
            if row.signature != signature {
                return Err("operation_conflict".into());
            }
            return Ok(json!(*row));
        }
        let mut registry: Registry = self.store.load("terminals.json")?;
        if let Some(old) = registry.removed.get(&id) {
            return Err(if *old == signature {
                "terminal_ended"
            } else {
                "operation_conflict"
            }
            .into());
        }
        if let Some(row) = registry.terminals.iter().find(|r| r.id == id) {
            if row.signature != signature {
                return Err("operation_conflict".into());
            }
            let mut row = row.clone();
            row.phase = "ended".into();
            return Ok(json!(row));
        }
        if entries
            .values()
            .filter(|e| !e.closed.load(Ordering::SeqCst))
            .count()
            >= 8
        {
            return Err("terminal_limit".into());
        }
        let (mut metadata, cwd) = self.prepare_create(value, id.clone(), signature)?;
        registry.terminals.push(metadata.clone());
        self.store.save("terminals.json", &registry)?;
        let session = platform::start(&metadata.shell, &shells::args(&metadata.shell), &cwd, size)?;
        metadata.phase = "running".into();
        let entry = super::service::start_entry(metadata.clone(), client, session, size)?;
        entries.insert(id, entry);
        drop(entries);
        self.persist()?;
        Ok(json!(metadata))
    }
    fn prepare_create(
        &self,
        value: &Value,
        id: String,
        signature: String,
    ) -> Result<(Metadata, PathBuf), String> {
        let choices = shells::choices();
        let shell = value["shell"]
            .as_str()
            .or_else(|| choices.first().map(String::as_str))
            .ok_or("shell_missing")?;
        if !choices.iter().any(|s| s == shell) {
            return Err("shell_missing".into());
        }
        let directory = value["directory"].as_str().unwrap_or("");
        if !directory.is_empty() {
            relative(directory)?;
        }
        let cwd = self
            .project
            .root()
            .join(directory)
            .canonicalize()
            .map_err(io)?;
        if !cwd.starts_with(self.project.root().canonicalize().map_err(io)?) {
            return Err("path_denied".into());
        }
        let metadata = Metadata {
            id: id.clone(),
            name: Path::new(shell)
                .file_stem()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned(),
            shell: shell.into(),
            directory: directory.into(),
            phase: "starting".into(),
            signature,
        };
        Ok((metadata, cwd))
    }
}
fn initial_size(value: &Value) -> Result<(u16, u16), String> {
    let rows = value
        .get("rows")
        .map(|v| {
            v.as_u64()
                .filter(|v| *v > 0 && *v <= 500)
                .ok_or("invalid_size")
        })
        .transpose()?
        .unwrap_or(24);
    let cols = value
        .get("cols")
        .map(|v| {
            v.as_u64()
                .filter(|v| *v > 0 && *v <= 1000)
                .ok_or("invalid_size")
        })
        .transpose()?
        .unwrap_or(80);
    Ok((rows as u16, cols as u16))
}
