//! Human document snapshots and CAS publication. No Agent recovery attribution.
use super::*;
use serde_json::json;
mod drafts;
mod mutations;
const FILE_LIMIT: usize = 2 * 1024 * 1024;
#[derive(Clone, Serialize, Deserialize, Default)]
struct Receipts {
    rows: std::collections::BTreeMap<String, (String, Value)>,
}
impl DevelopmentService {
    pub(super) fn editor_request(&self, method: &str, value: &Value) -> Result<Value, String> {
        let _guard = self.editor_lane.lock().map_err(|_| "state_busy")?;
        if self.closed.load(std::sync::atomic::Ordering::SeqCst) {
            return Err("host_closed".into());
        }
        if method == "editor.inspect" {
            return self.inspect_document(text(value, "path")?);
        }
        if method == "editor.open" {
            return self.open_document(text(value, "path")?);
        }
        if method.starts_with("editor.draft") {
            return self.draft_request(method, value);
        }
        let id = identity(value, "operationId")?;
        let signature = digest(format!("{method}:{value}").as_bytes());
        let mut receipts: Receipts = self.store.load("editor-receipts.json")?;
        if let Some((old, result)) = receipts.rows.get(&id) {
            return if *old != signature {
                Err("operation_conflict".into())
            } else if let Some(reason) = result.get("_error").and_then(Value::as_str) {
                Err(reason.into())
            } else {
                let mut result = result.clone();
                if method == "editor.save" {
                    result["text"] = json!(text(value, "text")?.replace("\r\n", "\n"));
                }
                Ok(result)
            };
        }
        receipts.rows.insert(
            id.clone(),
            (signature.clone(), json!({"_error":"operation_unknown"})),
        );
        self.store.save("editor-receipts.json", &receipts)?;
        let outcome = match method {
            "editor.save" => self.save_document(value),
            "editor.create" | "editor.rename" | "editor.delete" | "editor.restore" => {
                self.mutate_document(method, value, &id)
            }
            _ => Err("unknown_method".into()),
        };
        let result = match outcome {
            Ok(value) => value,
            Err(error) => {
                receipts
                    .rows
                    .insert(id, (signature, json!({"_error":error})));
                self.store.save("editor-receipts.json", &receipts)?;
                return Err(error);
            }
        };
        let mut compact = result.clone();
        if method == "editor.save" {
            compact
                .as_object_mut()
                .ok_or("invalid_state")?
                .remove("text");
        }
        receipts.rows.insert(id, (signature, compact));
        self.store.save("editor-receipts.json", &receipts)?;
        Ok(result)
    }
    pub(super) fn file_bytes(&self, path: &str) -> Result<Option<Vec<u8>>, String> {
        relative(path)?;
        self.check_private(path)?;
        self.project
            .read_file_limited(path, FILE_LIMIT + 1)
            .map_err(io)
    }
    fn check_private(&self, path: &str) -> Result<(), String> {
        let candidate = self.project.root().join(path);
        let private = self.store.root_ancestor();
        let mut ancestor = candidate.as_path();
        loop {
            if let Ok(resolved) = ancestor.canonicalize() {
                if resolved.starts_with(&private) {
                    return Err("path_denied".into());
                }
                break;
            }
            let Some(parent) = ancestor.parent() else {
                break;
            };
            ancestor = parent;
        }
        Ok(())
    }
    pub(super) fn open_document(&self, path: &str) -> Result<Value, String> {
        let bytes = self.file_bytes(path)?.ok_or("file_missing")?;
        let version = digest(&bytes);
        if bytes.len() > FILE_LIMIT {
            return Ok(
                json!({"path":path,"version":version,"editable":false,"reason":"file_large","text":""}),
            );
        }
        let Ok(raw) = std::str::from_utf8(&bytes) else {
            return Ok(
                json!({"path":path,"version":version,"editable":false,"reason":"file_encoding","text":""}),
            );
        };
        let bom = raw.starts_with('\u{feff}');
        let content = raw.strip_prefix('\u{feff}').unwrap_or(raw);
        let crlf = content.contains("\r\n");
        let mixed = content.replace("\r\n", "").contains('\r')
            || crlf && content.replace("\r\n", "").contains('\n');
        let reason = if content.contains('\0') {
            "file_binary"
        } else if mixed {
            "file_newlines"
        } else {
            ""
        };
        Ok(
            json!({"path":path,"version":version,"editable":reason.is_empty(),"reason":reason,"text":content.replace("\r\n","\n"),"bom":bom,"newline":if crlf{"crlf"}else{"lf"}}),
        )
    }
    fn save_document(&self, value: &Value) -> Result<Value, String> {
        let path = text(value, "path")?;
        let Some(before) = self.file_bytes(path)? else {
            return self.recreate_document(value);
        };
        if digest(&before) != text(value, "version")? {
            return Err("file_conflict".into());
        }
        let doc = self.open_document(path)?;
        if doc["editable"] != true {
            return Err("file_readonly".into());
        }
        let mut content = text(value, "text")?.replace("\r\n", "\n");
        if doc["newline"] == "crlf" {
            content = content.replace('\n', "\r\n");
        }
        if doc["bom"] == true {
            content.insert(0, '\u{feff}');
        }
        if content.len() > FILE_LIMIT || content.contains('\0') {
            return Err("file_large".into());
        }
        self.store.save(
            &format!("backup-{}.json", identity(value, "operationId")?),
            &json!({"path":path,"bytes":before}),
        )?;
        // Human writes must not be folded into the currently running Agent's recovery ledger.
        Project::new(self.project.root())
            .writable_target(path, false, crate::WriteScope::ProjectRoot)
            .map_err(io)?
            .atomic_write(
                &content,
                Some(std::str::from_utf8(&before).map_err(|_| "file_encoding")?),
            )
            .map_err(|e| {
                if e.kind() == std::io::ErrorKind::ResourceBusy {
                    "file_conflict".into()
                } else {
                    io(e)
                }
            })?;
        self.open_document(path)
    }
}

impl DevelopmentService {
    fn recreate_document(&self, value: &Value) -> Result<Value, String> {
        if value["version"] != "missing" {
            return Err("file_missing".into());
        }
        let path = text(value, "path")?;
        let mut content = text(value, "text")?.replace("\r\n", "\n");
        if value["newline"] == "crlf" {
            content = content.replace('\n', "\r\n");
        }
        if value["bom"] == true {
            content.insert(0, '\u{feff}');
        }
        if content.len() > FILE_LIMIT || content.contains('\0') {
            return Err("file_large".into());
        }
        Project::new(self.project.root())
            .writable_target(path, false, crate::WriteScope::ProjectRoot)
            .map_err(io)?
            .atomic_create(&content)
            .map_err(io)?;
        self.open_document(path)
    }
}
