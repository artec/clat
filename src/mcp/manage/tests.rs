use super::*;
use std::path::PathBuf;
fn root() -> PathBuf {
    let path = std::env::temp_dir().join(format!("clat-mcp-managed-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&path).unwrap();
    path
}
#[test]
fn managed_mcp_user_project_override_revision_and_redaction() {
    let storage = root();
    let a = storage.join("project-a");
    let b = storage.join("project-b");
    std::fs::create_dir_all(&a).unwrap();
    std::fs::create_dir_all(&b).unwrap();
    let marker = "canary-value-ms-h1";
    let value = McpManaged::save(
        &storage,
        &a,
        "user",
        "custom",
        "missing",
        true,
        Some(json!({"command":"custom-server","env":{"PRIVATE_OPTION":marker}})),
    )
    .unwrap();
    let revision = value["revision"].as_str().unwrap();
    let got = McpManaged::get(&storage, &a, "user", "custom").unwrap();
    assert!(
        !got.to_string().contains(marker),
        "Host read must not return private values"
    );
    assert_eq!(got["entry"]["env_keys"][0], "PRIVATE_OPTION");
    assert!(McpManaged::save(&storage, &a, "user", "custom", "missing", false, None).is_err());
    let manual = McpConfig::new();
    let mut in_b = McpConfig::new();
    McpManaged::merge(&storage, &b, &mut in_b, &manual).unwrap();
    assert_eq!(in_b["custom"].command, "custom-server");
    let disabled = McpManaged::save(
        &storage,
        &a,
        "project",
        "custom",
        revision,
        false,
        Some(json!({"command":"custom-server"})),
    )
    .unwrap();
    let next = disabled["revision"].as_str().unwrap();
    let mut in_a = McpConfig::new();
    McpManaged::merge(&storage, &a, &mut in_a, &manual).unwrap();
    assert!(
        !in_a.contains_key("custom"),
        "project disable shadows global"
    );
    let mut in_b = McpConfig::new();
    McpManaged::merge(&storage, &b, &mut in_b, &manual).unwrap();
    assert!(
        in_b.contains_key("custom"),
        "other project keeps global config"
    );
    let mut manual = McpConfig::new();
    manual.insert(
        "custom".into(),
        McpServerConfig {
            command: "legacy-server".into(),
            ..Default::default()
        },
    );
    let mut in_b = McpConfig::new();
    McpManaged::merge(&storage, &b, &mut in_b, &manual).unwrap();
    assert_eq!(in_b["custom"].command, "legacy-server");
    let mut in_a = McpConfig::new();
    McpManaged::merge(&storage, &a, &mut in_a, &manual).unwrap();
    assert_eq!(
        in_a["custom"].command, "legacy-server",
        "manual mcp.json must beat a project-level managed disable"
    );
    assert!(McpManaged::remove(&storage, &a, "project", "custom", revision).is_err());
    McpManaged::remove(&storage, &a, "project", "custom", next).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        assert_eq!(
            std::fs::metadata(storage.join("mcp-managed.json"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }
    crate::test_support::cleanup_tree(&storage);
}
#[test]
fn managed_mcp_rejects_invalid_mixed_config_and_does_not_persist() {
    let storage = root();
    let project = storage.join("project");
    std::fs::create_dir_all(&project).unwrap();
    for config in [
        json!({"command":"node","url":"https://example.com/mcp"}),
        json!({"url":"http://user:pass@example.com/mcp"}),
        json!({"url":"https://example.com/mcp","headers":{"Auth":"bad\r\nInjected: yes"}}),
        json!({"command":""}),
        json!({"command":"node","headers":{"Auth":"unexpected"}}),
    ] {
        McpManaged::save(
            &storage,
            &project,
            "user",
            "invalid",
            "missing",
            true,
            Some(config),
        )
        .unwrap_err();
        assert_eq!(McpManaged::load(&storage).unwrap().1, "missing");
    }
    crate::test_support::cleanup_tree(&storage);
}

#[test]
fn managed_mcp_query_bearing_url_is_write_only_in_management_reads() {
    let storage = root();
    let project = storage.join("project");
    std::fs::create_dir_all(&project).unwrap();
    let marker = "ms1-private-query-canary";
    let url = format!("https://mcp.example.test/connect?session={marker}");
    McpManaged::save(
        &storage,
        &project,
        "user",
        "remote-query",
        "missing",
        true,
        Some(json!({"url": url})),
    )
    .unwrap();

    let details = McpManaged::get(&storage, &project, "user", "remote-query").unwrap();
    assert!(details["entry"]["url"].is_null(), "URL query is write-only");
    assert_eq!(details["entry"]["url_has_query"], true);
    assert!(!details.to_string().contains(marker));
    let listing = McpManaged::list(&storage, &project).unwrap();
    assert!(!listing.to_string().contains(marker));

    // Redaction affects presentation only; the configured connection retains
    // the original URL and still has its query on the execution side.
    let configured = McpManaged::configured(&storage, &project, "user", "remote-query").unwrap();
    assert_eq!(configured.url.as_deref(), Some(url.as_str()));
    crate::test_support::cleanup_tree(&storage);
}
