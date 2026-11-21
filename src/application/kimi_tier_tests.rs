//! Kimi Coding Plan subscription: vendor-level choice, model-effective capacity.
use super::*;

#[test]
fn kimi_membership_controls_context_without_impacting_other_vendors() {
    let (storage_root, project_root) = roots("kimi-membership-context");
    std::fs::create_dir_all(&project_root).unwrap();
    let project = Project::new(&project_root);
    let application = mount(&project, &storage_root, TestBehavior::Success);

    assert_eq!(application.model_settings_view().unwrap().kimi_tier, "plus");
    application
        .select_model_preset("k3.1-pro", Some("kimi-test-key".into()), None)
        .unwrap();
    let (config, credentials) = application.model_state().unwrap();
    assert_eq!(config.max_context_tokens, Some(262_144));
    assert_eq!(credentials.value(0), Some("kimi-test-key"));
    assert_eq!(
        application
            .model_settings_view()
            .unwrap()
            .current
            .limits
            .unwrap()
            .max_context_tokens,
        Some(262_144)
    );

    application.set_kimi_tier("pro").unwrap();
    let (config, credentials) = application.model_state().unwrap();
    assert_eq!(config.max_context_tokens, Some(1_048_576));
    assert_eq!(
        config.overrides.max_context_tokens,
        crate::Override::Set(1_048_576)
    );
    assert_eq!(credentials.value(0), Some("kimi-test-key"));
    assert_eq!(application.model_settings_view().unwrap().kimi_tier, "pro");

    // Old per-model context overrides never eclipse the explicit vendor tier.
    let mut stale = config.clone();
    stale.max_context_tokens = Some(262_144);
    stale.overrides.max_context_tokens = crate::Override::Set(262_144);
    application.save_model_state(&stale, &credentials).unwrap();
    assert_eq!(
        application.model_state().unwrap().0.max_context_tokens,
        Some(1_048_576)
    );

    application
        .select_model_preset("glm-5.3", Some("glm-test-key".into()), None)
        .unwrap();
    assert_eq!(
        application.model_state().unwrap().0.max_context_tokens,
        Some(1_000_000)
    );
    application.set_kimi_tier("plus").unwrap();
    assert_eq!(
        application.model_state().unwrap().0.max_context_tokens,
        Some(1_000_000)
    );
    assert_eq!(application.model_settings_view().unwrap().kimi_tier, "plus");
    assert!(application.set_kimi_tier("business").is_err());
    assert_eq!(application.model_settings_view().unwrap().kimi_tier, "plus");

    application
        .select_model_preset("kimi-for-coding", None, None)
        .unwrap();
    assert_eq!(
        application.model_state().unwrap().0.max_context_tokens,
        Some(262_144)
    );
    application.close().unwrap();
    crate::test_support::cleanup_tree(storage_root.parent().unwrap());
}

#[test]
fn kimi_membership_survives_switch_and_host_restart() {
    let (storage_root, project_root) = roots("kimi-membership-restart");
    std::fs::create_dir_all(&project_root).unwrap();
    let project = Project::new(&project_root);
    let application = mount(&project, &storage_root, TestBehavior::Success);
    application.set_kimi_tier("pro").unwrap();
    application
        .select_model_preset("k3.1-pro", Some("kimi-test-key".into()), None)
        .unwrap();
    assert_eq!(
        application.model_state().unwrap().0.max_context_tokens,
        Some(1_048_576)
    );
    application
        .select_model_preset("qwen4-max", None, None)
        .unwrap();
    application.close().unwrap();

    let reopened = crate::test_support::reopen_after_close(|| {
        mount_result(&project, &storage_root, TestBehavior::Success)
    });
    assert_eq!(reopened.model_settings_view().unwrap().kimi_tier, "pro");
    assert_eq!(
        reopened.model_state().unwrap().0.max_context_tokens,
        Some(1_000_000)
    );
    reopened
        .select_model_preset("k3.1-pro", None, None)
        .unwrap();
    let (config, credentials) = reopened.model_state().unwrap();
    assert_eq!(config.max_context_tokens, Some(1_048_576));
    assert_eq!(credentials.value(0), Some("kimi-test-key"));
    reopened.set_kimi_tier("plus").unwrap();
    assert_eq!(
        reopened.model_state().unwrap().0.max_context_tokens,
        Some(262_144)
    );
    reopened.close().unwrap();

    let again = crate::test_support::reopen_after_close(|| {
        mount_result(&project, &storage_root, TestBehavior::Success)
    });
    assert_eq!(again.model_settings_view().unwrap().kimi_tier, "plus");
    assert_eq!(
        again.model_state().unwrap().0.max_context_tokens,
        Some(262_144)
    );
    again.close().unwrap();
    crate::test_support::cleanup_tree(storage_root.parent().unwrap());
}
