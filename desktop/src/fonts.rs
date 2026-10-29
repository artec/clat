//! Device-local font discovery behind the curated native client bridge.
mod catalog;
use catalog::{Face, inventory};
use clat_core::client_ports::presentation::UiMessage;

#[tauri::command]
pub async fn desktop_fonts(window: tauri::WebviewWindow) -> Result<Vec<Face>, UiMessage> {
    crate::commands::guard(&window)?;
    tauri::async_runtime::spawn_blocking(inventory)
        .await
        .map_err(|e| UiMessage::new("dev.font_list_failed").arg("reason", e.to_string()))?
        .map_err(|e| UiMessage::new("dev.font_list_failed").arg("reason", e))
}
