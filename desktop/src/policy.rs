//! Platform-independent checks; no native installer can bypass these.
#![cfg_attr(not(any(target_os = "macos", windows)), allow(dead_code))]
use clat_core::client_ports::presentation::UiMessage;
use clat_core::client_ports::upgrade::discovery::newer_formal_version;
use serde_json::Value;

pub use clat_core::client_ports::upgrade::discovery::{PUBLIC_KEY, REPO};

pub fn permitted_page(url: &str) -> bool {
    let Ok(url) = url::Url::parse(url) else {
        return false;
    };
    if !url.username().is_empty() || url.password().is_some() {
        return false;
    }
    matches!(
        (url.scheme(), url.host_str(), url.port()),
        ("tauri", Some("localhost"), None)
            | ("http", Some("tauri.localhost"), None)
            | ("http", Some("127.0.0.1"), Some(2691))
    )
}

pub fn validate_update(value: &Value, current: &str, target: &str) -> Result<String, UiMessage> {
    let version = newer_formal_version(value, current)?
        .ok_or(UiMessage::new("update.newer_formal_required"))?;
    let (os, arch, suffix) = match target {
        "darwin-aarch64" => ("macos", "arm64", ".app.tar.gz"),
        "darwin-x86_64" => ("macos", "x64", ".app.tar.gz"),
        "windows-aarch64" => ("windows", "arm64", "-setup.exe"),
        "windows-x86_64" => ("windows", "x64", "-setup.exe"),
        _ => return Err(UiMessage::new("update.target_unsupported")),
    };
    let expected =
        format!("{REPO}/releases/download/v{version}/clat-desktop-v{version}-{os}-{arch}{suffix}");
    if value["platforms"][target]["url"] != expected {
        return Err(UiMessage::new("update.asset_mismatch"));
    }
    Ok(version.to_owned())
}

#[cfg(test)]
pub use clat_core::client_ports::upgrade::discovery::verify_index_with_key;
#[cfg(all(test, any(target_os = "macos", windows)))]
pub use clat_core::client_ports::upgrade::discovery::{
    release_metadata, release_metadata_with_key,
};

#[cfg(test)]
pub fn verify_index(bytes: &[u8], signature: &str, expected: &Value) -> Result<(), UiMessage> {
    verify_index_with_key(bytes, signature, expected, PUBLIC_KEY)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn tray_navigation_commands_are_admitted_for_the_main_workbench() {
        let capability: Value =
            serde_json::from_str(include_str!("../capabilities/main.json")).unwrap();
        let permissions = capability["permissions"].as_array().unwrap();
        let manifest = include_str!("../build.rs").replace("\r\n", "\n");
        for command in [
            "desktop_remember_conversation",
            "desktop_take_tray_intent",
            "desktop_fonts",
        ] {
            assert!(
                manifest.contains(&format!("\"{command}\"")),
                "{command} must be declared in the application ACL manifest"
            );
            let permission = format!("allow-{}", command.replace('_', "-"));
            assert!(
                permissions.iter().any(|entry| entry == &permission),
                "the main workbench must be allowed to call {command}"
            );
        }
        assert_eq!(capability["windows"], serde_json::json!(["main"]));
        assert_eq!(
            capability["remote"]["urls"],
            serde_json::json!(["http://127.0.0.1:2691/*"])
        );
    }
    #[test]
    fn page_allowlist_has_exact_origin_and_no_userinfo() {
        for url in [
            "http://127.0.0.1:2691/",
            "tauri://localhost/index.html",
            "http://tauri.localhost/",
        ] {
            assert!(permitted_page(url), "{url}");
        }
        for url in [
            "https://example.com",
            "http://127.0.0.1:2692/",
            "http://127.0.0.1:2691.evil/",
            "http://user@127.0.0.1:2691/",
            "file:///etc/passwd",
        ] {
            assert!(!permitted_page(url), "{url}");
        }
    }
    #[test]
    fn updater_binds_repository_target_and_newer_version() {
        let mut value = serde_json::json!({"version":"2.2.0", "platforms":{"darwin-aarch64":{"url":format!("{REPO}/releases/download/v2.2.0/clat-desktop-v2.2.0-macos-arm64.app.tar.gz")}}});
        assert!(validate_update(&value, "2.1.0", "darwin-aarch64").is_ok());
        assert!(validate_update(&value, "2.2.0", "darwin-aarch64").is_err());
        assert!(validate_update(&value, "2.1.0", "windows-x86_64").is_err());
        value["version"] = serde_json::json!("99.0.0");
        assert!(validate_update(&value, "2.1.0", "darwin-aarch64").is_err());
        assert!(verify_index(b"{}", "invalid", &value).is_err());
    }
}
