#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
#[cfg(any(target_os = "macos", windows))]
mod app;
#[cfg(any(target_os = "macos", windows))]
mod background;
#[cfg(any(target_os = "macos", windows))]
mod commands;
#[cfg(any(target_os = "macos", windows))]
mod computer_use;
#[cfg(any(target_os = "macos", windows))]
mod fonts;
#[cfg(target_os = "macos")]
mod host_menu;
#[cfg(any(target_os = "macos", windows))]
mod localization;
#[cfg(any(target_os = "macos", windows))]
mod notification_sounds;
#[cfg(any(target_os = "macos", windows))]
mod notifications;
mod policy;
#[cfg(any(target_os = "macos", windows))]
mod quit;
#[cfg(target_os = "macos")]
mod quit_ipc;
#[cfg(any(target_os = "macos", windows))]
mod startup;
#[cfg(any(target_os = "macos", windows))]
mod tray;
#[cfg(any(target_os = "macos", windows))]
mod update;
#[cfg(any(target_os = "macos", windows))]
mod window_frame;

#[cfg(any(target_os = "macos", windows))]
fn main() {
    #[cfg(target_os = "macos")]
    if std::env::args_os().nth(1).as_deref() == Some(host_menu::ARG.as_ref()) {
        host_menu::run();
        return;
    }
    app::run();
}
#[cfg(not(any(target_os = "macos", windows)))]
fn main() {
    use clat_core::client_ports::presentation::{LanguagePreference, LocaleContext};
    eprintln!(
        "{}",
        LocaleContext::system(LanguagePreference::System).text("desktop.unsupported_platform")
    );
}

#[cfg(all(test, any(target_os = "macos", windows)))]
mod updater_tests;
