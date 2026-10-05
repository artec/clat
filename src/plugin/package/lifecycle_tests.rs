use super::network_tests::{candidate, parsed};
use crate::plugin::{InstallKind, PackageStore};
use serde_json::json;
use sha2::{Digest, Sha256};
#[test]
fn plg4_v2_store_reopens_rolls_back_and_uninstalls_without_losing_ceiling() {
    let root = std::env::temp_dir().join(format!("clat-v2-lifecycle-{}", uuid::Uuid::new_v4()));
    let source = root.join("source");
    let storage = root.join("storage");
    std::fs::create_dir_all(&source).unwrap();
    let bytes = b"not executed during installation";
    std::fs::write(source.join("official.wasm"), bytes).unwrap();
    let mut value = candidate();
    value["runtime"]["sha256"] = json!(format!("{:x}", Sha256::digest(bytes)));
    let id = value["id"].as_str().unwrap().to_owned();
    let manifest_path = source.join("clat-plugin.json");
    std::fs::write(&manifest_path, serde_json::to_vec(&value).unwrap()).unwrap();
    let narrow =
        json!({"networkPolicy":{"network":{"protocol":"clat:net-task@0.1.0","origins":[]}}});
    {
        let mut store = PackageStore::open(&storage).unwrap();
        assert!(
            store
                .install(&source, None, false, InstallKind::Install)
                .is_err()
        );
        store
            .install(&source, Some(narrow.clone()), true, InstallKind::Install)
            .unwrap();
        assert_eq!(store.configuration(&id), Some(narrow.clone()));
        assert!(
            store
                .configure(&id, json!({"networkPolicy":{"hostTools":["shell"]}}))
                .is_err()
        );
        assert_eq!(store.configuration(&id), Some(narrow.clone()));
    }
    {
        let mut store = PackageStore::open(&storage).unwrap();
        assert_eq!(store.configuration(&id), Some(narrow.clone()));
        value["version"] = json!("0.2.0");
        value["capabilities"]["network"]["origins"][0]["methods"] = json!(["GET", "POST"]);
        std::fs::write(&manifest_path, serde_json::to_vec(&value).unwrap()).unwrap();
        assert!(
            store
                .install(&source, Some(narrow.clone()), false, InstallKind::Update)
                .is_err()
        );
        store
            .install(&source, Some(narrow.clone()), true, InstallKind::Update)
            .unwrap();
        store.rollback(&id).unwrap();
        let restored = store.review_manifest(&id, false).unwrap();
        assert_eq!(restored.version, "0.1.0");
        assert_eq!(
            restored.capabilities.network,
            parsed(candidate()).unwrap().capabilities.network
        );
        store.uninstall(&id).unwrap();
        assert!(store.list().is_empty());
    }
    assert!(PackageStore::open(&storage).unwrap().list().is_empty());
    std::fs::remove_dir_all(root).unwrap();
}
