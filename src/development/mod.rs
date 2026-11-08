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
mod git_read;
use crate::plugin::lifecycle;
pub(crate) mod plugin;
mod rename;
mod store;
mod terminal;
pub use git::GitModel;
pub use git_read::{GitReadClient, GitReadQuery};
pub use store::OperationView;

pub struct DevelopmentService {
    rust_support: std::sync::RwLock<Option<std::sync::Weak<crate::rust_support::RustSupport>>>,
    project: Project,
    store: store::Store,
    editor_lane: Mutex<()>,
    closed: std::sync::atomic::AtomicBool,
    lifetime: Arc<lifecycle::Lifecycle>,
    git: Arc<git::GitService>,
    terminals: terminal::Terminals,
}
impl DevelopmentService {
    pub(crate) fn new(project: Project, private: &Path) -> Self {
        #[cfg(test)]
        crate::test_support::execution_observation::record("development.create");
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
            rust_support: Default::default(),
            lifetime: store.lifetime.clone(),
            git: Arc::new(git::GitService::new(project.clone(), store.clone())),
            terminals: terminal::Terminals::new(project.clone(), store.clone()),
            project,
            store,
            editor_lane: Mutex::new(()),
            closed: std::sync::atomic::AtomicBool::new(false),
        }
    }
    /// Only trusted client ports expose this service. No Agent tool accepts a human flag.
    pub(crate) fn set_rust_support(
        &self,
        service: std::sync::Weak<crate::rust_support::RustSupport>,
    ) {
        *self.rust_support.write().expect("Rust support hook") = Some(service);
    }

    pub(super) fn rust_saved(&self, path: &str) -> Option<Value> {
        self.rust_support
            .read()
            .ok()?
            .as_ref()?
            .upgrade()?
            .saved(path, true, &CancelToken::new())
    }

    /// Only trusted client ports expose this service. No Agent tool accepts a human flag.
    pub fn request(
        self: &Arc<Self>,
        method: &str,
        value: Value,
        model: Option<GitModel>,
    ) -> Result<Value, String> {
        let _producer = self.lifetime.producer()?;
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
        let _producer = self.lifetime.producer()?;
        self.terminals.output(id, offset)
    }
    /// Bounded wait for fresh terminal output; see `Terminals::wait`.
    pub fn terminal_wait(
        &self,
        id: &str,
        offset: u64,
        timeout: std::time::Duration,
    ) -> Result<(), String> {
        let _producer = self.lifetime.producer()?;
        self.terminals.wait_for_output(id, offset, timeout)
    }
    pub fn has_live_work(&self) -> bool {
        self.lifetime.active() || self.git.has_live_work() || self.terminals.has_live_work()
    }
    pub(crate) fn with_lease(self, lease: Arc<dyn Send + Sync>) -> Self {
        self.lifetime.attach(lease);
        self
    }
    pub fn close(&self) -> Result<(), String> {
        self.closed.store(true, std::sync::atomic::Ordering::SeqCst);
        self.lifetime.close();
        let _editor = self.editor_lane.lock().map_err(|_| "state_busy")?;
        let git = self.git.cancel_all();
        let terminals = self.terminals.close_all();
        git.and(terminals)?;
        if self.lifetime.active() {
            return Err("cleanup_failed".into());
        }
        Ok(())
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

pub(crate) fn retained_terminals(project: &Project, private: &Path) -> Result<Value, String> {
    let root = project.root();
    let store = store::Store::new(
        private
            .join("development")
            .join(digest(root.to_string_lossy().as_bytes())),
    );
    terminal::retained(&store)
}
