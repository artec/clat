//! Project-owned human development services, independent of Agent run/session state.
use crate::{CancelToken, Project};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex};
mod editor;
mod errors;
mod git;
pub(crate) mod plugin;
mod rename;
mod store;
mod terminal;
pub use git::GitModel;
pub use store::OperationView;

pub struct DevelopmentService {
    project: Project,
    store: store::Store,
    editor_lane: Mutex<()>,
    closed: std::sync::atomic::AtomicBool,
    git: Arc<git::GitService>,
    terminals: terminal::Terminals,
}
impl DevelopmentService {
    pub(crate) fn new(project: Project, private: &Path) -> Self {
        let root = project
            .root()
            .canonicalize()
            .unwrap_or_else(|_| project.root().to_owned());
        let store = store::Store::new(
            private
                .join("development")
                .join(digest(root.to_string_lossy().as_bytes())),
        );
        Self {
            git: Arc::new(git::GitService::new(project.clone(), store.clone())),
            terminals: terminal::Terminals::new(project.clone(), store.clone()),
            project,
            store,
            editor_lane: Mutex::new(()),
            closed: std::sync::atomic::AtomicBool::new(false),
        }
    }
    /// Only trusted client ports expose this service. No Agent tool accepts a human flag.
    pub fn request(
        self: &Arc<Self>,
        method: &str,
        value: Value,
        model: Option<GitModel>,
    ) -> Result<Value, String> {
        if self.closed.load(std::sync::atomic::Ordering::SeqCst) {
            return Err("host_closed".into());
        }
        if method.starts_with("editor.") {
            return self.editor_request(method, &value);
        }
        if method.starts_with("git.") {
            return self.git.request(method, &value, model);
        }
        self.terminals.request(method, &value)
    }
    pub fn error_presentation(reason: &str) -> crate::i18n::UiMessage {
        errors::presentation(reason)
    }
    pub fn terminal_output(&self, id: &str, offset: u64) -> Result<Value, String> {
        self.terminals.output(id, offset)
    }
    pub fn has_live_work(&self) -> bool {
        self.git.has_live_work() || self.terminals.has_live_work()
    }
    pub fn close(&self) {
        if self.closed.swap(true, std::sync::atomic::Ordering::SeqCst) {
            return;
        }
        let _editor = self.editor_lane.lock().expect("human editor close");
        self.git.cancel_all();
        self.terminals.close_all();
    }
}
pub(super) fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
pub(super) fn text<'a>(value: &'a Value, key: &str) -> Result<&'a str, String> {
    value
        .get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| format!("invalid:{key}"))
}
pub(super) fn identity(value: &Value, key: &str) -> Result<String, String> {
    let id = text(value, key)?;
    if id.is_empty()
        || id.len() > 200
        || !id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b))
    {
        return Err(format!("invalid:{key}"));
    }
    Ok(id.into())
}
pub(super) fn relative(path: &str) -> Result<&Path, String> {
    let path = Path::new(path);
    if path.as_os_str().is_empty()
        || path
            .components()
            .any(|c| !matches!(c, Component::Normal(_)))
        || path.to_string_lossy().contains(['\\', '\0', ':'])
    {
        return Err("path_denied".into());
    }
    if path
        .components()
        .any(|c| c.as_os_str().eq_ignore_ascii_case(".git"))
    {
        return Err("path_denied".into());
    }
    Ok(path)
}
pub(super) fn io(error: std::io::Error) -> String {
    format!("io:{error}")
}

#[cfg(test)]
mod tests;
