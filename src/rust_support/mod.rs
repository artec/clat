//! Optional Rust maintenance. Cargo owns resolution; this module never rewrites a lockfile.
use crate::{CancelToken, Project};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, RwLock};

mod cargo;
mod changes;
pub(crate) mod plugin;
mod reactions;

type Runner = dyn Fn(&Path, &[&str], bool, &CancelToken) -> Result<String, String> + Send + Sync;

pub(crate) fn observes(path: &str) -> bool {
    reactions::find(path).is_some()
}

pub(crate) struct RustSupport {
    project: Project,
    mode: Option<Arc<RwLock<crate::permission::PermissionMode>>>,
    runner: Arc<Runner>,
    lane: Mutex<()>,
    closed: CancelToken,
    active_cancel: Mutex<CancelToken>,
}

impl RustSupport {
    pub(crate) fn new(
        project: Project,
        mode: Option<Arc<RwLock<crate::permission::PermissionMode>>>,
    ) -> Self {
        Self {
            project,
            mode,
            runner: Arc::new(cargo::run),
            lane: Mutex::new(()),
            closed: CancelToken::new(),
            active_cancel: Mutex::new(CancelToken::new()),
        }
    }

    pub(crate) fn close(&self) -> Result<(), String> {
        self.closed.cancel();
        self.active_cancel
            .lock()
            .map_err(|_| "Rust maintenance cancellation lock poisoned")?
            .cancel();
        let _guard = self
            .lane
            .lock()
            .map_err(|_| "Rust maintenance lock poisoned")?;
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn saved(&self, path: &str, human: bool, cancel: &CancelToken) -> Option<Value> {
        self.react(path, human, cancel, None)
    }

    pub(crate) fn react(
        &self,
        path: &str,
        human: bool,
        cancel: &CancelToken,
        previous: Option<&[u8]>,
    ) -> Option<Value> {
        let reaction = reactions::find(path)?;
        let result = (reaction.run)(self, path, human, cancel, previous);
        Some(match result {
            Ok(value) => value,
            Err(error) => json!({
                "status":"failed",
                "artifact": reaction.artifact,
                "error":crate::redact::redact_secrets(&error),
                "presentation":crate::i18n::UiMessage::new(reaction.failure),
            }),
        })
    }

    fn synchronize(&self, path: &str, human: bool, cancel: &CancelToken) -> Result<String, String> {
        let cancel = cancel
            .child_with_deadline(std::time::Instant::now() + std::time::Duration::from_secs(30));
        let _guard = self
            .lane
            .lock()
            .map_err(|_| "Rust maintenance lock poisoned")?;
        let mode = self
            .mode
            .as_ref()
            .map(|mode| mode.read().map_err(|_| "permission mode lock poisoned"))
            .transpose()?;
        if !human
            && !matches!(
                mode.as_deref(),
                Some(
                    crate::permission::PermissionMode::ProjectWrite
                        | crate::permission::PermissionMode::FullAccess
                )
            )
        {
            return Err(
                "Automatic Rust commands require interactive Project Write or Full Access mode"
                    .into(),
            );
        }
        if self.closed.is_cancelled() || cancel.is_cancelled() {
            return Err("Rust maintenance cancelled".into());
        }
        *self
            .active_cancel
            .lock()
            .map_err(|_| "Rust maintenance cancellation lock poisoned")? = cancel.clone();
        if self.closed.is_cancelled() {
            return Err("Rust maintenance cancelled".into());
        }
        let root = self
            .project
            .root()
            .canonicalize()
            .map_err(|e| e.to_string())?;
        let workspace = self.workspace(&root, path, &cancel)?;
        let workspace = workspace
            .to_str()
            .ok_or("Cargo workspace path is not UTF-8")?;
        if cancel.is_cancelled() {
            return Err("Rust maintenance cancelled".into());
        }
        (self.runner)(
            &root,
            &[
                "update",
                "--workspace",
                "--offline",
                "--manifest-path",
                workspace,
            ],
            true,
            &cancel,
        )?;
        Ok(Path::new(workspace)
            .with_file_name("Cargo.lock")
            .strip_prefix(&root)
            .map_err(|e| e.to_string())?
            .to_string_lossy()
            .into_owned())
    }

    fn workspace(&self, root: &Path, path: &str, cancel: &CancelToken) -> Result<PathBuf, String> {
        let manifest = self
            .project
            .resolve_existing(path)
            .map_err(|e| e.to_string())?;
        if !manifest.starts_with(root) {
            return Err("Rust manifest is outside the project".into());
        }
        let manifest = manifest.to_str().ok_or("Rust manifest path is not UTF-8")?;
        let workspace = (self.runner)(
            root,
            &[
                "locate-project",
                "--workspace",
                "--message-format",
                "plain",
                "--manifest-path",
                manifest,
                "--offline",
            ],
            false,
            cancel,
        )?;
        let workspace = PathBuf::from(workspace.trim())
            .canonicalize()
            .map_err(|e| e.to_string())?;
        if !workspace.starts_with(root) {
            return Err("Cargo workspace is outside the project; lockfile was not modified".into());
        }
        Ok(workspace)
    }
}

#[cfg(test)]
mod tests;
