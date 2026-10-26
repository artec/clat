#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
#[cfg(any(target_os = "macos", windows))]
mod app;
#[cfg(any(target_os = "macos", windows))]
mod commands;
#[cfg(any(target_os = "macos", windows))]
mod localization;
#[cfg(any(target_os = "macos", windows))]
mod notifications;
mod policy;
#[cfg(any(target_os = "macos", windows))]
mod startup;
#[cfg(any(target_os = "macos", windows))]
mod update;
#[cfg(any(target_os = "macos", windows))]
mod window_frame;

#[cfg(any(target_os = "macos", windows))]
fn main() {
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
