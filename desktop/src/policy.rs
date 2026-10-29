//! Platform-independent checks; no native installer can bypass these.
#![cfg_attr(not(any(target_os = "macos", windows)), allow(dead_code))]
use base64::Engine;
use clat_core::client_ports::presentation::UiMessage;
use serde_json::Value;

pub const REPO: &str = "https://github.com/artec/clat";
pub const ENDPOINT: &str = "https://github.com/artec/clat/releases/latest/download/latest.json";
pub const PUBLIC_KEY: &str = include_str!("../updater.pub");

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
    let version = value["version"]
        .as_str()
        .ok_or(UiMessage::new("update.version_missing"))?;
    let parsed =
        semver::Version::parse(version).map_err(|_| UiMessage::new("update.version_invalid"))?;
    let current = semver::Version::parse(current)
        .map_err(|_| UiMessage::new("update.current_version_invalid"))?;
    if parsed <= current || !parsed.pre.is_empty() || !parsed.build.is_empty() {
        return Err(UiMessage::new("update.newer_formal_required"));
    }
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

pub fn release_metadata(bytes: &[u8]) -> Result<Value, UiMessage> {
    release_metadata_with_key(bytes, PUBLIC_KEY)
}

pub(crate) fn release_metadata_with_key(bytes: &[u8], key: &str) -> Result<Value, UiMessage> {
    let envelope: Value =
        serde_json::from_slice(bytes).map_err(|_| UiMessage::new("update.index_invalid"))?;
    if envelope["schema_version"] != 1 {
        return Err(UiMessage::new("update.schema_unsupported"));
    }
    let payload = envelope["payload"]
        .as_str()
        .ok_or(UiMessage::new("update.payload_missing"))?;
    let signature = envelope["signatures"]["desktop"]
        .as_str()
        .ok_or(UiMessage::new("update.signature_missing"))?;
    let value =
        serde_json::from_str(payload).map_err(|_| UiMessage::new("update.signed_index_invalid"))?;
    verify_index_with_key(payload.as_bytes(), signature, &value, key)?;
    Ok(value)
}

#[cfg(test)]
pub fn verify_index(bytes: &[u8], signature: &str, expected: &Value) -> Result<(), UiMessage> {
    verify_index_with_key(bytes, signature, expected, PUBLIC_KEY)
}

pub(crate) fn verify_index_with_key(
    bytes: &[u8],
    signature: &str,
    expected: &Value,
    public_key: &str,
) -> Result<(), UiMessage> {
    let public = base64::engine::general_purpose::STANDARD
        .decode(public_key.trim())
        .map_err(|_| UiMessage::new("update.public_key_invalid"))?;
    let public =
        std::str::from_utf8(&public).map_err(|_| UiMessage::new("update.public_key_invalid"))?;
    let key = minisign_verify::PublicKey::decode(public)
        .map_err(|_| UiMessage::new("update.public_key_invalid"))?;
    let signature = base64::engine::general_purpose::STANDARD
        .decode(signature.trim())
        .map_err(|_| UiMessage::new("update.signature_invalid"))?;
    let signature =
        std::str::from_utf8(&signature).map_err(|_| UiMessage::new("update.signature_invalid"))?;
    let signature = minisign_verify::Signature::decode(signature)
        .map_err(|_| UiMessage::new("update.signature_invalid"))?;
    key.verify(bytes, &signature, false)
        .map_err(|_| UiMessage::new("update.signature_failed"))?;
    let value: Value =
        serde_json::from_slice(bytes).map_err(|_| UiMessage::new("update.signed_index_invalid"))?;
    if &value != expected {
        return Err(UiMessage::new("update.metadata_mismatch"));
    }
    Ok(())
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
