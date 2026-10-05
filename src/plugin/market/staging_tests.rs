use super::*;
use crate::plugin::PackageStore;
const ID: &str = "io.artec.dsh-official-web-wasm";

#[test]
#[ignore = "requires ephemeral signed network market; no component execution"]
fn plg4_signed_network_staging_matrix() {
    let Ok(base) = std::env::var("CLAT_PLG4_STAGING_MATRIX_URL") else {
        eprintln!("UNARMED: signed network staging URL required");
        return;
    };
    let key_file =
        fs::read_to_string(std::env::var("CLAT_PLG2_MARKET_PUBLIC_KEY").unwrap()).unwrap();
    let key = minisign_verify::PublicKey::from_base64(key_file.lines().nth(1).unwrap()).unwrap();
    let root = std::env::temp_dir().join(format!("clat-v2-matrix-{}", uuid::Uuid::new_v4()));
    fs::create_dir_all(&root).unwrap();
    let market = Market::load_with_key(&base, &key, now_unix().unwrap()).unwrap();
    assert_eq!(market.index.schema_version, 2);
    assert!(
        market
            .install(&root, options("0.1.0", false, InstallKind::Install))
            .is_err()
    );
    assert!(PackageStore::open(&root).unwrap().list().is_empty());
    market
        .install(&root, options("0.1.0", true, InstallKind::Install))
        .unwrap();
    assert_version(&root, "0.1.0");
    bad_signed_inputs(&base, &key, &root);
    invalid_config_and_lease(&root);
    assert!(
        market
            .install(&root, options("0.1.1", false, InstallKind::Update))
            .is_err()
    );
    assert_version(&root, "0.1.0");
    market
        .install(&root, options("0.1.1", true, InstallKind::Update))
        .unwrap();
    assert_version(&root, "0.1.1");
    {
        let mut store = PackageStore::open(&root).unwrap();
        store.rollback(ID).unwrap();
        assert_eq!(store.list()[0].version, "0.1.0");
        store.rollback(ID).unwrap();
        assert_eq!(store.list()[0].version, "0.1.1");
        store.uninstall(ID).unwrap();
    }
    assert!(PackageStore::open(&root).unwrap().list().is_empty());
    fs::remove_dir_all(root).unwrap();
    println!(
        "SIGNED_V2_MATRIX consent/install/key/index/digest/config/lease/disable/enable/expansion/update/rollback/reopen/uninstall passed"
    );
}
fn options(version: &str, accept: bool, kind: InstallKind) -> MarketInstallOptions {
    MarketInstallOptions {
        root_id: ID.into(),
        version: version.into(),
        config: Some(serde_json::json!({"apiKey":"public-fixture-token"})),
        accept_capabilities: accept,
        accept_vulnerabilities: false,
        root_kind: kind,
    }
}
fn assert_version(root: &Path, version: &str) {
    let store = PackageStore::open(root).unwrap();
    let list = store.list();
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].version, version);
    assert_eq!(list[0].manifest_version, 2);
    assert_eq!(list[0].trust, TrustLabel::PublisherVerified);
    assert!(list[0].enabled);
}
fn invalid_config_and_lease(root: &Path) {
    let mut store = PackageStore::open(root).unwrap();
    assert!(
        PackageStore::open(root).is_err(),
        "concurrent writer must not enter"
    );
    assert!(
        store
            .configure(ID, serde_json::json!({"networkPolicy":{"preopens":["/"]}}))
            .is_err()
    );
    store.set_enabled(ID, false).unwrap();
    assert!(!store.list()[0].enabled);
    store.set_enabled(ID, true).unwrap();
    assert!(store.list()[0].enabled);
    store.configure(ID, serde_json::json!({"apiKey":"public-fixture-token","networkPolicy":{"network":{"protocol":"clat:net-task@0.1.0","origins":[]}}})).unwrap();
}
fn bad_signed_inputs(base: &str, key: &minisign_verify::PublicKey, root: &Path) {
    let parent = base.trim_end_matches("matrix/");
    for namespace in ["bad-index", "bad-signature"] {
        assert!(
            Market::load_with_key(&format!("{parent}{namespace}/"), key, now_unix().unwrap())
                .is_err()
        );
    }
    let wrong = minisign_verify::PublicKey::from_base64(
        include_str!("../../../tests/fixtures/market-index.pub")
            .lines()
            .nth(1)
            .unwrap(),
    )
    .unwrap();
    assert!(Market::load_with_key(base, &wrong, now_unix().unwrap()).is_err());
    let mut expired = Market::load_with_key(base, key, now_unix().unwrap()).unwrap();
    expired.index.market.expires_at_unix = now_unix().unwrap() - 1;
    assert!(
        expired
            .install(root, options("0.1.1", true, InstallKind::Update))
            .is_err()
    );
    let tampered =
        Market::load_with_key(&format!("{parent}tamper/"), key, now_unix().unwrap()).unwrap();
    assert!(
        tampered
            .install(root, options("0.1.1", true, InstallKind::Update))
            .is_err()
    );
    assert_version(root, "0.1.0");
}
