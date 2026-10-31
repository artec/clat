//! Desktop-only resume intent. Trust and backend execution remain core-owned.
use clat_core::client_ports::presentation::{
    PresentedText, UiMessage, load_ui_preferences, save_ui_preferences,
};
use serde_json::json;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use tauri::Manager;

pub struct Startup {
    pending: AtomicBool,
    blocked: AtomicBool,
    loading: AtomicBool,
}
impl Default for Startup {
    fn default() -> Self {
        Self {
            pending: AtomicBool::new(true),
            blocked: AtomicBool::new(false),
            loading: AtomicBool::new(true),
        }
    }
}
impl Startup {
    pub fn cancel(&self) {
        self.pending.store(false, Ordering::Release);
        self.blocked.store(false, Ordering::Release);
        self.finish();
    }
    pub fn blocked(&self) -> bool {
        self.blocked.load(Ordering::Acquire)
    }
    pub fn loading(&self) -> bool {
        self.loading.load(Ordering::Acquire)
    }
    pub fn finish(&self) {
        self.loading.store(false, Ordering::Release);
    }
    fn resolve<T>(
        &self,
        load: impl FnOnce() -> Result<Option<PathBuf>, PresentedText>,
        discover: impl FnOnce() -> Result<Option<T>, PresentedText>,
        resume: impl FnOnce(&Path) -> Result<T, PresentedText>,
    ) -> Result<Option<T>, PresentedText> {
        if !self.pending.swap(false, Ordering::AcqRel) {
            return Ok(None);
        }
        let result = (|| {
            let saved = load()?;
            let existing = discover()?;
            match saved {
                Some(path) => resume(&path).map(Some),
                None => Ok(existing),
            }
        })();
        if result.is_err() {
            self.blocked.store(true, Ordering::Release);
        }
        result
    }
}

fn load(path: &Path) -> Result<Option<PathBuf>, PresentedText> {
    let value = load_ui_preferences(path)?;
    match value.get("lastProject") {
        None | Some(serde_json::Value::Null) => Ok(None),
        Some(serde_json::Value::String(s)) if !s.is_empty() && Path::new(s).is_absolute() => {
            Ok(Some(s.into()))
        }
        _ => Err(UiMessage::new("desktop.restore_invalid").into()),
    }
}

/// Explicit retry uses only the saved project and never grants new trust.
pub fn retry(app: &tauri::AppHandle) -> Result<(), PresentedText> {
    let state = app.state::<crate::app::Desktop>();
    let _guard = state
        .connection
        .lock()
        .map_err(|_| UiMessage::new("desktop.state_unavailable"))?;
    state.startup.cancel();
    let path = load(&crate::localization::path(app)?)?
        .ok_or_else(|| UiMessage::new("setup.select_project"))?;
    let backend = state
        .backend
        .as_ref()
        .ok_or_else(|| UiMessage::new("desktop.backend_reinstall"))?;
    let client = backend.resume_project_presented(&path)?;
    save_project(app, &client)?;
    crate::app::connected(app, client).map_err(Into::into)
}

pub fn restore(app: &tauri::AppHandle) {
    let state = app.state::<crate::app::Desktop>();
    let _guard = state.connection.lock().expect("connection state");
    let Some(backend) = state.backend.as_ref() else {
        state.startup.finish();
        return;
    };
    let result = state.startup.resolve(
        || load(&crate::localization::path(app)?),
        || match backend.reconcile_presented(None)? {
            clat_core::client_ports::host::BackendConnection::Missing => Ok(None),
            clat_core::client_ports::host::BackendConnection::Connected(client) => Ok(Some(client)),
        },
        |path| backend.resume_project_presented(path),
    );
    match result {
        Ok(Some(client)) => {
            if let Err(error) = crate::app::connected(app, client) {
                *state.connection_error.lock().expect("connection state") = Some(error.into());
            }
        }
        Ok(None) => {}
        Err(error) => {
            *state.connection_error.lock().expect("connection state") = Some(
                UiMessage::new("desktop.restore_failed")
                    .arg_message("reason", error.message())
                    .into(),
            );
        }
    }
    state.startup.finish();
}

pub fn remember(app: &tauri::AppHandle, address: &str) -> Result<(), PresentedText> {
    let state = app.state::<crate::app::Desktop>();
    let _guard = state
        .connection
        .lock()
        .map_err(|_| "connection state unavailable")?;
    if app
        .get_webview_window("main")
        .and_then(|w| w.url().ok())
        .as_ref()
        .map(|u| u.as_str())
        != Some(address)
    {
        return Ok(());
    }
    let host = state
        .host
        .lock()
        .map_err(|_| "host state unavailable")?
        .clone();
    let Some(host) = host else {
        return Ok(());
    };
    let scoped = host.for_browser_url(address)?;
    save_project(app, &scoped)
}

pub fn save_project(
    app: &tauri::AppHandle,
    client: &clat_core::client_ports::host::HostClient,
) -> Result<(), PresentedText> {
    let info = client.call("workbench.info", &json!({}))?;
    let root = info["project"]["root"]
        .as_str()
        .ok_or("host omitted project root")?;
    let path = crate::localization::path(app)?;
    if load_ui_preferences(&path)?["lastProject"].as_str() != Some(root) {
        save_ui_preferences(&path, &json!({"lastProject":root}))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests;
