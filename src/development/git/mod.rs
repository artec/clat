//! Human Git workflows. Operations survive client retries; unknown outcomes never replay.
use super::*;
use serde_json::json;
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
mod actions;
mod backups;
mod command;
mod index;
mod model;
mod operations;
mod query;
mod reconcile;
mod recovery;
pub use model::GitModel;
#[derive(Serialize, Deserialize, Clone)]
struct SavedOperation {
    signature: String,
    request: Value,
    view: OperationView,
}
#[derive(Default, Serialize, Deserialize)]
struct SavedOperations {
    rows: BTreeMap<String, SavedOperation>,
}
struct Operation {
    saved: Mutex<SavedOperation>,
    cancel: CancelToken,
}
pub(super) struct GitService {
    project: Project,
    store: store::Store,
    operations: Mutex<BTreeMap<String, Arc<Operation>>>,
    closed: AtomicBool,
    workers: Mutex<Vec<std::thread::JoinHandle<()>>>,
}
impl GitService {
    pub(super) fn new(project: Project, store: store::Store) -> Self {
        Self {
            project,
            store,
            operations: Mutex::new(BTreeMap::new()),
            closed: AtomicBool::new(false),
            workers: Mutex::new(Vec::new()),
        }
    }
    pub(super) fn request(
        self: &Arc<Self>,
        method: &str,
        value: &Value,
        model: Option<GitModel>,
    ) -> Result<Value, String> {
        match method {
            "git.snapshot" => self.snapshot(),
            "git.backups" => self.backups(),
            "git.diff" => self.diff(value),
            "git.branches" => self.branches(),
            "git.history" => self.history(value),
            "git.commit.detail" => self.commit_detail(value),
            "git.settings" | "git.settings.set" => self.settings(method, value),
            "git.operation" => self.operation(value, false),
            "git.cancel" => self.operation(value, true),
            "git.start" => self.start(value.clone(), model),
            _ => Err("unknown_method".into()),
        }
    }
    pub(super) fn has_live_work(&self) -> bool {
        self.operations
            .lock()
            .expect("git operations")
            .values()
            .any(|op| {
                matches!(
                    op.saved.lock().expect("git operation").view.phase.as_str(),
                    "pending" | "running" | "generating" | "committing"
                )
            })
    }
    pub(super) fn cancel_all(&self) {
        self.closed.store(true, Ordering::SeqCst);
        for op in self.operations.lock().expect("git operations").values() {
            op.cancel.cancel();
        }
        for worker in self.workers.lock().expect("git workers").drain(..) {
            let _ = crate::application::join_with_grace(
                worker,
                std::time::Duration::from_secs(2),
                "human git",
            );
        }
    }
    fn settings(&self, method: &str, value: &Value) -> Result<Value, String> {
        let mut settings: Value = self.store.load("git-settings.json")?;
        if method == "git.settings.set" {
            let locale = text(value, "language")?;
            if !["system", "en", "zh-Hans", "zh-Hant"].contains(&locale) {
                return Err("invalid_language".into());
            }
            settings = json!({"language":locale});
            self.store.save("git-settings.json", &settings)?;
        }
        if settings.is_null() {
            settings = json!({"language":"system"});
        }
        Ok(settings)
    }
}
