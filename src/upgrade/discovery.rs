//! Signed release discovery shared by the terminal and desktop clients.
use crate::i18n::UiMessage;
use base64::Engine;
use serde_json::Value;
use std::{io::Read, path::Path, time::Duration};

pub const REPO: &str = "https://github.com/artec/clat";
pub const ENDPOINT: &str = "https://github.com/artec/clat/releases/latest/download/latest.json";
pub const PUBLIC_KEY: &str = include_str!("../../desktop/updater.pub");
pub const INITIAL_DELAY: Duration = Duration::from_secs(5);
pub const CHECK_INTERVAL: Duration = Duration::from_secs(24 * 60 * 60);

pub fn automatic_checks_enabled(path: &Path) -> bool {
    crate::i18n::load_ui_preferences(path)
        .ok()
        .and_then(|value| value["autoCheck"].as_bool())
        .unwrap_or(true)
}

pub fn desktop_preferences_path() -> Option<std::path::PathBuf> {
    Some(
        dirs::config_dir()?
            .join("com.artec.clat.desktop")
            .join("preferences.json"),
    )
}

pub fn fetch_release_metadata() -> Result<Value, UiMessage> {
    let agent = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(30)))
        .build()
        .new_agent();
    let mut response = agent
        .get(ENDPOINT)
        .call()
        .map_err(|_| UiMessage::new("update.index_network"))?;
    let cap = super::manifest::MAX_INDEX_BYTES;
    let mut bytes = Vec::new();
    response
        .body_mut()
        .as_reader()
        .take(cap as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| UiMessage::new("update.index_read"))?;
    if bytes.len() > cap {
        return Err(UiMessage::new("update.index_large"));
    }
    release_metadata(&bytes)
}

pub fn newer_formal_version<'a>(
    value: &'a Value,
    current: &str,
) -> Result<Option<&'a str>, UiMessage> {
    let version = value["version"]
        .as_str()
        .ok_or(UiMessage::new("update.version_missing"))?;
    let parsed =
        semver::Version::parse(version).map_err(|_| UiMessage::new("update.version_invalid"))?;
    let current = semver::Version::parse(current)
        .map_err(|_| UiMessage::new("update.current_version_invalid"))?;
    Ok((parsed > current && parsed.pre.is_empty() && parsed.build.is_empty()).then_some(version))
}

pub fn release_metadata(bytes: &[u8]) -> Result<Value, UiMessage> {
    release_metadata_with_key(bytes, PUBLIC_KEY)
}

pub fn release_metadata_with_key(bytes: &[u8], key: &str) -> Result<Value, UiMessage> {
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

pub fn verify_index_with_key(
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
    fn tui_update_discovery_uses_verified_formal_versions() {
        let fixture = include_bytes!("../../tests/fixtures/release-index.json");
        let key = include_str!("../../tests/fixtures/release-index-desktop.pub");
        let mut metadata = release_metadata_with_key(fixture, key).unwrap();
        assert_eq!(
            newer_formal_version(&metadata, "1.0.0").unwrap(),
            Some("2.0.0")
        );
        for current in ["2.0.0", "3.0.0"] {
            assert_eq!(newer_formal_version(&metadata, current).unwrap(), None);
        }
        for version in ["3.0.0-rc.1", "3.0.0+local"] {
            metadata["version"] = serde_json::json!(version);
            assert_eq!(newer_formal_version(&metadata, "2.0.0").unwrap(), None);
        }
        assert!(release_metadata(fixture).is_err());
    }

    #[test]
    fn tui_update_discovery_follows_desktop_automatic_check_preference() {
        let (root, _) = crate::test_support::roots("automatic-update-preference");
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("preferences.json");
        assert!(automatic_checks_enabled(&path));
        std::fs::write(&path, r#"{"autoCheck":false}"#).unwrap();
        assert!(!automatic_checks_enabled(&path));
        std::fs::write(&path, r#"{"autoCheck":true}"#).unwrap();
        assert!(automatic_checks_enabled(&path));
        crate::test_support::cleanup_tree(&root);
    }
}
