//! Uses a throwaway key; only the macOS installer test replaces a temporary
//! dummy bundle. No test contacts GitHub or opens the user's Host/installation.
use base64::Engine;
use serde_json::json;
use tauri_plugin_updater::UpdaterExt;

const KEY: &str = include_str!("../fixtures/updater.pub");
const SIGNATURE: &str = include_str!("../fixtures/package.bin.sig");
const PACKAGE: &[u8] = include_bytes!("../fixtures/package.bin");

fn app() -> tauri::App<tauri::test::MockRuntime> {
    let mut context = tauri::test::mock_context(tauri::test::noop_assets());
    context.config_mut().plugins.0.insert("updater".into(), json!({"pubkey":KEY.trim(),"requireSignedVersion":true,"endpoints":["https://example.invalid/update"]}));
    tauri::test::mock_builder()
        .plugin(
            tauri_plugin_updater::Builder::new()
                .pubkey(KEY.trim())
                .build(),
        )
        .build(context)
        .unwrap()
}

fn metadata(version: &str) -> serde_json::Value {
    json!({"version":version,"platforms":{"test-target":{
        "url":"https://example.invalid/package", "signature":SIGNATURE.trim()}}})
}

#[test]
fn signed_upgrade_accepts_exact_package_and_rejects_tampering() {
    let app = app();
    let updater = app
        .updater_builder()
        .target("test-target")
        .executable_path("fixture/app")
        .build()
        .unwrap();
    let update = updater.restore_update(metadata("2.0.0")).unwrap().unwrap();
    assert_eq!(update.current_version, "0.1.0");
    update.verify(PACKAGE).unwrap();
    assert!(update.verify(b"tampered").is_err());
    let forged = updater.restore_update(metadata("99.0.0")).unwrap().unwrap();
    assert!(
        forged.verify(PACKAGE).is_err(),
        "a signed old package cannot impersonate a newer version"
    );
    assert!(
        updater.restore_update(metadata("0.0.1")).unwrap().is_none(),
        "downgrade must be ignored"
    );
    assert!(
        app.updater_builder()
            .target("missing-platform")
            .build()
            .unwrap()
            .restore_update(metadata("2.0.0"))
            .is_err()
    );
}

#[test]
fn index_authentication_binds_exact_metadata_and_separate_trust_root() {
    let bytes = include_bytes!("../fixtures/index.json");
    let expected = serde_json::from_slice(bytes).unwrap();
    let signature = include_str!("../fixtures/index.json.sig");
    crate::policy::verify_index_with_key(bytes, signature, &expected, KEY).unwrap();
    let mut forged = expected.clone();
    forged["version"] = json!("99.0.0");
    assert!(crate::policy::verify_index_with_key(bytes, signature, &forged, KEY).is_err());
    assert!(crate::policy::verify_index_with_key(b"{}", signature, &json!({}), KEY).is_err());
    assert!(
        crate::policy::verify_index(bytes, signature, &expected).is_err(),
        "fixture key is never trusted by production"
    );
    let decoded = base64::engine::general_purpose::STANDARD
        .decode(SIGNATURE.trim())
        .unwrap();
    assert!(
        std::str::from_utf8(&decoded)
            .unwrap()
            .contains("version:2.0.0")
    );
}

#[cfg(target_os = "macos")]
#[test]
fn macos_signed_install_replaces_only_the_isolated_gui_bundle() {
    let root = std::env::temp_dir().join(format!(
        "clat-desktop-install-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    struct Cleanup(std::path::PathBuf);
    impl Drop for Cleanup {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    let _cleanup = Cleanup(root.clone());
    let binary = root.join("CLAT Desktop.app/Contents/MacOS/fixture");
    std::fs::create_dir_all(binary.parent().unwrap()).unwrap();
    std::fs::write(&binary, b"old GUI version 0.1.0").unwrap();
    let cli = root.join("clat-cli");
    let runtime = root.join("runtimes/running-backend");
    std::fs::create_dir_all(runtime.parent().unwrap()).unwrap();
    std::fs::write(&cli, b"independent CLI").unwrap();
    std::fs::write(&runtime, b"running backend").unwrap();
    let lease = std::fs::File::open(&runtime).unwrap();
    lease.lock_shared().unwrap();
    let app = app();
    let updater = app
        .updater_builder()
        .target("test-target")
        .executable_path(&binary)
        .build()
        .unwrap();
    let mut value = metadata("2.0.0");
    value["platforms"]["test-target"]["signature"] =
        json!(include_str!("../fixtures/install.app.tar.gz.sig").trim());
    let update = updater.restore_update(value).unwrap().unwrap();
    assert!(update.install(b"bad package").is_err());
    assert_eq!(std::fs::read(&binary).unwrap(), b"old GUI version 0.1.0");
    update
        .install(include_bytes!("../fixtures/install.app.tar.gz"))
        .unwrap();
    assert_eq!(std::fs::read(&binary).unwrap(), b"new GUI version 2.0.0\n");
    assert_eq!(std::fs::read(&cli).unwrap(), b"independent CLI");
    assert_eq!(std::fs::read(&runtime).unwrap(), b"running backend");
    drop(lease);
}

#[test]
fn compact_index_drives_the_real_updater_without_signature_sidecars() {
    let key = include_str!("../../tests/fixtures/release-index-desktop.pub");
    let bytes = include_bytes!("../../tests/fixtures/release-index.json");
    let metadata = crate::policy::release_metadata_with_key(bytes, key).unwrap();
    crate::policy::validate_update(&metadata, "1.0.0", "darwin-aarch64").unwrap();
    let app = app();
    let updater = app
        .updater_builder()
        .pubkey(key.trim())
        .target("darwin-aarch64")
        .executable_path("fixture/app")
        .build()
        .unwrap();
    let update = updater.restore_update(metadata).unwrap().unwrap();
    update.verify(PACKAGE).unwrap();
    assert!(update.verify(b"tampered").is_err());
    assert!(
        crate::policy::release_metadata(bytes).is_err(),
        "test key is never a production trust root"
    );
    for field in ["payload", "signatures", "schema_version"] {
        let mut value: serde_json::Value = serde_json::from_slice(bytes).unwrap();
        value.as_object_mut().unwrap().remove(field);
        assert!(
            crate::policy::release_metadata_with_key(&serde_json::to_vec(&value).unwrap(), key)
                .is_err()
        );
    }
    let mut value: serde_json::Value = serde_json::from_slice(bytes).unwrap();
    value["payload"] = json!(
        value["payload"]
            .as_str()
            .unwrap()
            .replace("2.0.0", "99.0.0")
    );
    assert!(
        crate::policy::release_metadata_with_key(&serde_json::to_vec(&value).unwrap(), key)
            .is_err()
    );
}
