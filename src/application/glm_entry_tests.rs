use super::*;

const CHINA: &str = "https://open.bigmodel.cn/api/coding/paas/v4";
const GLOBAL: &str = "https://api.z.ai/api/coding/paas/v4";

#[test]
fn glm_entry_switch_keeps_overrides_and_uses_only_the_destination_key() {
    let (storage_root, project_root) = roots("glm-entry-switch");
    std::fs::create_dir_all(&project_root).unwrap();
    let project = Project::new(&project_root);
    let app = mount(&project, &storage_root, TestBehavior::Success);
    assert_eq!(app.model_settings_view().unwrap().glm_entry, "bigmodel");

    let mut legacy =
        crate::ProviderCredentials::for_protocol(crate::ModelProtocol::OpenAiCompatible);
    legacy.set_value(0, "domestic-key".into());
    app.control.upsert_vendor_key("Glm", &legacy).unwrap();
    app.select_model_preset("glm-5.3", None, None).unwrap();
    let (mut config, credentials) = app.model_state().unwrap();
    assert_eq!(config.endpoint, CHINA);
    assert_eq!(credentials.value(0), Some("domestic-key"));

    config.overrides.thinking_level = crate::Override::Set(crate::ThinkingLevel::Max);
    app.save_model_state(&config, &credentials).unwrap();
    app.set_model_preset_entry("zai", None).unwrap();
    let (config, credentials) = app.model_state().unwrap();
    assert_eq!(config.endpoint, GLOBAL);
    assert_eq!(config.thinking_level, Some(crate::ThinkingLevel::Max));
    assert!(credentials.value(0).is_none_or(|key| key.is_empty()));
    assert!(glm_mcp_pack_from_control(&app.control).is_empty());
    let view = app.model_settings_view().unwrap();
    assert_eq!(view.glm_entry, "zai");
    assert_eq!(view.current.vendor.as_deref(), Some("GLM"));
    assert_eq!(view.current.preset.as_deref(), Some("glm-5.3"));

    app.set_model_preset_entry("zai", Some("international-key".into()))
        .unwrap();
    let pack = glm_mcp_pack_from_control(&app.control);
    assert_eq!(pack.len(), 4);
    for (_, server) in &pack {
        if let Some(url) = &server.url {
            assert!(url.starts_with("https://api.z.ai/api/mcp/"));
            assert_eq!(server.headers["Authorization"], "Bearer international-key");
        } else {
            assert_eq!(server.env["Z_AI_MODE"], "ZAI");
            assert_eq!(server.env["Z_AI_API_KEY"], "international-key");
        }
    }
    assert!(app.set_model_preset_entry("qianwen", None).is_err());
    app.set_model_preset_entry("bigmodel", None).unwrap();
    let (config, credentials) = app.model_state().unwrap();
    assert_eq!(config.endpoint, CHINA);
    assert_eq!(config.thinking_level, Some(crate::ThinkingLevel::Max));
    assert_eq!(credentials.value(0), Some("domestic-key"));
    app.close().unwrap();
    crate::test_support::cleanup_tree(storage_root.parent().unwrap());
}

#[test]
fn glm_entry_survives_model_changes_and_restart() {
    let (storage_root, project_root) = roots("glm-entry-restart");
    std::fs::create_dir_all(&project_root).unwrap();
    let project = Project::new(&project_root);
    let app = mount(&project, &storage_root, TestBehavior::Success);
    app.select_model_preset("glm-5.3", Some("international-key".into()), Some("zai"))
        .unwrap();
    app.select_model_preset("glm-5.4", None, None).unwrap();
    assert_eq!(app.model_state().unwrap().0.endpoint, GLOBAL);
    assert!(
        app.select_model_preset("opencode-glm-5.3", None, Some("zai"))
            .is_err()
    );
    app.select_model_preset("qwen4-max", None, None).unwrap();
    assert!(app.set_model_preset_entry("zai", None).is_err());
    app.close().unwrap();

    let reopened = crate::test_support::reopen_after_close(|| {
        mount_result(&project, &storage_root, TestBehavior::Success)
    });
    let view = reopened.model_settings_view().unwrap();
    assert_eq!(view.glm_entry, "zai");
    assert_eq!(view.current.preset.as_deref(), Some("qwen4-max"));
    assert!(
        !serde_json::to_string(&view)
            .unwrap()
            .contains("international-key")
    );
    let preset = preset_by_id("glm-5.3-flash").unwrap();
    assert_eq!(reopened.glm_preset_endpoint(preset), Some(GLOBAL));
    reopened.select_model_preset(preset.id, None, None).unwrap();
    let (config, credentials) = reopened.model_state().unwrap();
    assert_eq!(config.endpoint, GLOBAL);
    assert_eq!(config.request_path, "/chat/completions");
    assert_eq!(credentials.value(0), Some("international-key"));
    reopened.close().unwrap();
    crate::test_support::cleanup_tree(storage_root.parent().unwrap());
}
