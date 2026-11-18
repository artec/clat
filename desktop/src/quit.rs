use crate::app::Desktop;
use clat_core::client_ports::{host::HostClient, presentation::UiMessage};
use serde_json::Value;
use std::{sync::atomic::Ordering, time::Duration};
use tauri::{ExitRequestApi, Manager};
use tauri_plugin_dialog::{DialogExt, MessageDialogButtons, MessageDialogKind};

pub fn request(app: &tauri::AppHandle) -> Result<(), UiMessage> {
    let state = app.state::<Desktop>();
    if state
        .updates
        .lock()
        .map_err(|_| UiMessage::new("desktop.update_state_unavailable"))?
        .phase
        == crate::update::Phase::Installing
    {
        return Err(UiMessage::new("update.cannot_cancel"));
    }
    if state.quitting.load(Ordering::Acquire) || state.quit_pending.swap(true, Ordering::AcqRel) {
        return Ok(());
    }
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        let status = host_status(&app).await;
        let confirmed = confirm_if_needed(status, |key| confirm(&app, key)).await;
        finish(&app, confirmed);
    });
    Ok(())
}

async fn host_status(app: &tauri::AppHandle) -> Result<Option<Value>, String> {
    if crate::app::smoke_mode() {
        return Ok(None);
    }
    let host = app
        .state::<Desktop>()
        .host
        .lock()
        .map_err(|e| e.to_string())?
        .clone();
    let worker = tauri::async_runtime::spawn_blocking(move || {
        if let Some(host) = host
            && let Ok(status) = host.runtime_status()
        {
            return Ok(Some(status));
        }
        HostClient::local_runtime_status()
    });
    tokio::time::timeout(Duration::from_secs(3), worker)
        .await
        .map_err(|e| e.to_string())?
        .map_err(|e| e.to_string())?
}

async fn confirm_if_needed<F: Future<Output = bool>>(
    status: Result<Option<Value>, String>,
    ask: impl FnOnce(&'static str) -> F,
) -> bool {
    let key = match status {
        Ok(None) => return true,
        Ok(Some(value)) if value["ready"] == true => return true,
        Ok(Some(value)) if value["ready"] == false => "desktop.quit_busy",
        _ => "desktop.quit_status_unavailable",
    };
    ask(key).await
}

async fn confirm(app: &tauri::AppHandle, key: &str) -> bool {
    crate::app::show(app);
    let l = crate::localization::context(app);
    let mut dialog = app
        .dialog()
        .message(l.text(key))
        .title(l.text("desktop.menu_quit"))
        .kind(MessageDialogKind::Warning)
        .buttons(MessageDialogButtons::OkCancelCustom(
            l.text("desktop.menu_quit").into(),
            l.text("common.cancel").into(),
        ));
    if let Some(window) = app.get_webview_window("main") {
        dialog = dialog.parent(&window);
    }
    let (decision, response) = tokio::sync::oneshot::channel();
    dialog.show(move |confirmed| {
        let _ = decision.send(confirmed);
    });
    response.await.unwrap_or(false)
}

fn finish(app: &tauri::AppHandle, confirmed: bool) {
    if confirmed && prepare_exit(app).is_ok() {
        app.exit(0);
    }
    app.state::<Desktop>()
        .quit_pending
        .store(false, Ordering::Release);
}

/// Commit desktop exit without repeating the Host check after a menu quit.
/// Native exit still runs the same installer guard and cleanup path.
pub(super) fn prepare_exit(app: &tauri::AppHandle) -> Result<(), UiMessage> {
    let state = app.state::<Desktop>();
    let updates = state
        .updates
        .lock()
        .map_err(|_| UiMessage::new("desktop.update_state_unavailable"))?;
    if !updates.allow_exit(false) {
        return Err(UiMessage::new("update.cannot_cancel"));
    }
    state.quitting.store(true, Ordering::Release);
    Ok(())
}

pub fn on_exit_requested(app: &tauri::AppHandle, code: Option<i32>, api: ExitRequestApi) {
    let state = app.state::<Desktop>();
    let restart = code == Some(tauri::RESTART_EXIT_CODE);
    if !restart && !state.quitting.load(Ordering::Acquire) {
        api.prevent_exit();
        let _ = request(app);
    } else if !state.updates.lock().expect("updates").allow_exit(restart) {
        api.prevent_exit();
        state.quitting.store(false, Ordering::Release);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[tokio::test]
    async fn desktop_quit_requires_acceptance_when_another_project_is_busy() {
        let busy = Some(json!({"ready":false,"busy_workspaces":["other-project"]}));
        for accepted in [false, true] {
            let quit = confirm_if_needed(Ok(busy.clone()), |key| async move {
                assert_eq!(key, "desktop.quit_busy");
                accepted
            })
            .await;
            assert_eq!(quit, accepted);
        }
    }

    #[tokio::test]
    async fn desktop_quit_skips_the_dialog_for_idle_and_confirms_when_status_is_unknown() {
        for status in [Ok(None), Ok(Some(json!({"ready":true})))] {
            assert!(confirm_if_needed(status, |_| async { panic!("unexpected dialog") }).await);
        }
        for status in [Ok(Some(json!({}))), Err("timeout".into())] {
            assert!(
                !confirm_if_needed(status, |key| async move {
                    assert_eq!(key, "desktop.quit_status_unavailable");
                    false
                })
                .await
            );
        }
    }
}
