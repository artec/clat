//! Real adapter registration, discovery and execution against a local fixture.
use super::*;
use crate::plugin::{PluginManager, ScopeKind};
use crate::plugins::ToolRegistryPlugin;
use serde_json::{Value, json};
#[test]
#[ignore = "local python3 fixture; run explicitly with --ignored"]
fn ga3_mcp_fixture_discovery_does_not_call_or_probe_and_close_expires_identity() {
    let root = std::env::temp_dir().join(format!("clat-ga3-mcp-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&root).unwrap();
    let requests = root.join("requests");
    std::fs::write(
        root.join("mcp.json"),
        crate::test_support::mcp_discovery::config(&requests, false).to_string(),
    )
    .unwrap();
    let mut manager = PluginManager::root(ScopeKind::TrustedProject);
    manager
        .mount_all(vec![
            Arc::new(ToolRegistryPlugin),
            Arc::new(McpAdapterPlugin::new(
                root.clone(),
                vec![],
                crate::plugin_host::PluginHostBridge::shared(),
            )),
        ])
        .unwrap();
    let status = manager.require(MCP_STATUS_SERVICE).unwrap();
    assert!(status.wait_until_settled(std::time::Duration::from_secs(15)));
    assert_eq!(status.snapshot().connected, 1, "{:?}", status.snapshot());
    let registry = manager.require(TOOL_SERVICE).unwrap();
    registry.install_capabilities(
        registry.capability_view(Some(&crate::tool::ToolAccessPolicy::all())),
    );
    assert!(registry.get("mcp_fixture_unsafe").is_none());
    assert!(registry.get("mcp_fixture_unsafe_env").is_none());
    assert!(!format!("{:?}", status.snapshot()).contains("raw-private-config-value"));
    assert_eq!(registry.definitions().len(), 1);
    let before = std::fs::read(&requests).unwrap();
    let cancel = crate::CancelToken::new();
    let mut id = Value::Null;
    for _ in 0..10 {
        let page = registry
            .query_capabilities(
                &json!({"source":"fixture","name":"mcp_fixture_echo"}),
                &cancel,
            )
            .unwrap();
        let row = &page["candidates"][0];
        assert_eq!(row["owner"], "builtin.mcp_adapter");
        assert_eq!(row["source"], "mcp");
        assert_eq!(row["scope"], "external");
        assert_eq!(row["outputs"], json!([]));
        assert!(row["readiness"].as_str().unwrap().contains("unknown"));
        assert!(!page.to_string().contains("fixturesecret123"));
        assert!(!page.to_string().contains("raw-private-config-value"));
        id = row["id"].clone();
        let detail = registry
            .query_capabilities(&json!({"id":id}), &cancel)
            .unwrap();
        assert_eq!(detail["schema"]["complete"], true);
        assert!(
            !detail.to_string().contains("opaquecredential123"),
            "known credential env value leaked in schema annotation"
        );
        assert!(!page.to_string().contains("opaquecredential123"));
        assert!(
            !registry
                .definitions()
                .into_iter()
                .find(|d| d.name == "mcp_fixture_echo")
                .unwrap()
                .description
                .contains("opaquecredential123")
        );
        let actual: Value =
            serde_json::from_str(detail["schema"]["text"].as_str().unwrap()).unwrap();
        assert_eq!(
            actual["type"], "object",
            "GA3-R1: ordinary private config cannot rewrite schema type"
        );
        assert!(!detail.to_string().contains("raw-private-config-value"));
        let original = registry
            .get("mcp_fixture_echo")
            .unwrap()
            .definition()
            .input_schema;
        let published = &registry
            .definitions()
            .into_iter()
            .find(|d| d.name == "mcp_fixture_echo")
            .unwrap()
            .input_schema;
        assert_eq!(&actual, published);
        for key in ["required", "additionalProperties"] {
            assert_eq!(actual[key], original[key]);
        }
        for key in ["object", "[REDACTED]"] {
            assert_eq!(actual["properties"][key], original["properties"][key]);
        }
        assert_eq!(actual["properties"].as_object().unwrap().len(), 3);
    }
    assert_eq!(
        std::fs::read(&requests).unwrap(),
        before,
        "query must send zero MCP messages"
    );
    let output = registry
        .get("mcp_fixture_echo")
        .unwrap()
        .invoke(
            &json!({"value":7,"object":"object"}),
            &crate::Project::new(&root),
            &cancel,
        )
        .unwrap();
    assert!(output.to_string().contains('7'));
    assert_eq!(
        std::fs::read_to_string(&requests)
            .unwrap()
            .matches("tools/call")
            .count(),
        1
    );
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        let current = registry
            .query_capabilities(&json!({"name":"mcp_fixture_echo"}), &cancel)
            .unwrap();
        if current["candidates"][0]["readiness"]
            .as_str()
            .unwrap()
            .contains("blocked")
        {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "fixture channel did not close"
        );
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    assert!(
        registry
            .query_capabilities(&json!({"id":id}), &cancel)
            .is_err(),
        "old observation id must expire on observed EOF"
    );
    assert_eq!(
        std::fs::read_to_string(&requests)
            .unwrap()
            .matches("tools/call")
            .count(),
        1,
        "disconnect diagnostics must not retry"
    );
    manager.close().unwrap();
    assert!(
        registry
            .query_capabilities(&json!({"id":id}), &cancel)
            .is_err()
    );
    crate::test_support::cleanup_tree(&root);
}
