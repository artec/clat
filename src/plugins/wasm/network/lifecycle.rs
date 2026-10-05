use super::*;
use crate::plugin::{InstallKind, PackageStore, PluginManager};
use crate::plugins::ToolRegistryPlugin;
use sha2::{Digest, Sha256};
#[test]
#[ignore = "requires PLG4_TOOLS_COMPONENT; does not use system DNS"]
fn plg4_original_component_installs_mounts_rejects_undeclared_and_uninstalls() {
    let Ok(path) = std::env::var("PLG4_TOOLS_COMPONENT") else {
        eprintln!(
            "UNARMED: PLG4_TOOLS_COMPONENT required for actual original component acceptance"
        );
        return;
    };
    let root = std::env::temp_dir().join(format!("clat-v2-actual-{}", uuid::Uuid::new_v4()));
    let storage = root.join("storage");
    let source = root.join("source");
    std::fs::create_dir_all(&source).unwrap();
    let bytes = std::fs::read(path).unwrap();
    std::fs::write(source.join("official.wasm"), &bytes).unwrap();
    let id = "io.artec.dsh-official-web-wasm";
    let manifest = serde_json::json!({"manifestVersion":2,"id":id,"name":"Official Web WASM","version":"0.1.0",
        "runtime":{"kind":"wasm-component","entry":"official.wasm","sha256":format!("{:x}",Sha256::digest(&bytes))},
        "capabilities":{"tools":true,"network":{"protocol":"clat:net-task@0.1.0","origins":[{"scheme":"https","host":"api.deepseek.com","port":443,"methods":["POST"]}]},"clock":{"protocol":"wasi:clocks@0.2.10"}}});
    std::fs::write(
        source.join("clat-plugin.json"),
        serde_json::to_vec(&manifest).unwrap(),
    )
    .unwrap();
    let config =
        serde_json::json!({"origins":["https://undeclared.invalid"],"apiKey":"DO-NOT-LOG-SECRET"});
    {
        let mut store = PackageStore::open(&storage).unwrap();
        store
            .install(&source, Some(config), true, InstallKind::Install)
            .unwrap();
    }
    let calls = Arc::new(AtomicUsize::new(0));
    let seen = calls.clone();
    let bridge = crate::plugin_host::network::tests::fixture(
        PermissionMode::ReadOnly,
        false,
        Arc::new(move |_: PermissionRequest, _: &CancelToken| {
            seen.fetch_add(1, AtomicOrdering::SeqCst);
            PermissionDecision::Allow
        }),
    );
    let mut manager = PluginManager::root(ScopeKind::TrustedProject);
    let adapter = WasmAdapterPlugin::new(storage.clone(), bridge.clone(), storage.clone(), None);
    manager
        .mount_all(vec![Arc::new(ToolRegistryPlugin), Arc::new(adapter)])
        .unwrap();
    let registry = manager.require(TOOL_SERVICE).unwrap();
    let tool = registry
        .get("wasm_io_artec_dsh_official_web_wasm_web_fetch")
        .expect("official tool must mount through production pipeline");
    let args = serde_json::json!({"url":"https://undeclared.invalid/path"});
    let error = tool
        .invoke(&args, &Project::new(&root), &CancelToken::new())
        .unwrap_err();
    assert!(!error.to_string().contains("DO-NOT-LOG-SECRET"));
    assert_eq!(calls.load(AtomicOrdering::SeqCst), 0);
    manager.close().unwrap();
    assert!(
        registry
            .get("wasm_io_artec_dsh_official_web_wasm_web_fetch")
            .is_none()
    );
    assert!(
        tool.invoke(&args, &Project::new(&root), &CancelToken::new())
            .is_err()
    );
    {
        let mut store = PackageStore::open(&storage).unwrap();
        store.uninstall(id).unwrap();
        assert!(store.list().is_empty());
    }
    assert!(PackageStore::open(&storage).unwrap().list().is_empty());
    std::fs::remove_dir_all(root).unwrap();
}
