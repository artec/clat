fn main() {
    let target = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    if target == "macos" || target == "windows" {
        let manifest = tauri_build::AppManifest::new().commands(&[
            "desktop_info",
            "desktop_locale",
            "desktop_open_project",
            "desktop_check_update",
            "desktop_download_update",
            "desktop_install_update",
            "desktop_cancel_update",
            "desktop_preferences",
            "desktop_quit",
            "desktop_stop_host",
        ]);
        tauri_build::try_build(tauri_build::Attributes::new().app_manifest(manifest))
            .expect("desktop resource and ACL generation");
    }
}
