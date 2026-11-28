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
    // K2.8 Preview 的 1M 窗无档位注记（官方 Kimi Code 模型表）：Plus 档
    // 同样给 1,048,576，且换档不动它。
    let (config, _) = application.model_state().unwrap();
    assert_eq!(config.max_context_tokens, Some(1_048_576));
    assert_eq!(
        config.overrides.max_context_tokens,
        crate::Override::Set(1_048_576)
    );
    application.set_kimi_tier("pro").unwrap();
    assert_eq!(
        application.model_state().unwrap().0.max_context_tokens,
        Some(1_048_576)
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

/// 档位可见性与降档守卫（2026-11-28）：k3.1-pro-highspeed 仅 Pro 档
/// 及以上——Plus 视图不含、select 拒选；Pro 放行；highspeed 活动时
/// 拒降 Plus（先换模型）。k3 全档可用，窗口随档位 stamp（同
/// k3.1-pro 的 256K 语义）。删视图过滤 / select 拒选 / 降档守卫任一
/// 对应断言红。
#[test]
fn kimi_pro_gate_hides_highspeed_on_plus_and_guards_downgrade() {
    let (storage_root, project_root) = roots("kimi-pro-gate-visibility");
    std::fs::create_dir_all(&project_root).unwrap();
    let project = Project::new(&project_root);
    let application = mount(&project, &storage_root, TestBehavior::Success);

    // Plus（默认档）：视图不含 Pro 门槛预设，k3 在列；直选被拒。
    let view = application.model_settings_view().unwrap();
    assert!(
        !view
            .presets
            .iter()
            .any(|preset| preset.id == "k3.1-pro-highspeed"),
        "Plus view must hide the Pro-gated preset"
    );
    assert!(view.presets.iter().any(|preset| preset.id == "k3"));
    assert!(
        application
            .select_model_preset("k3.1-pro-highspeed", Some("kimi-test-key".into()), None)
            .is_err()
    );

    // k3 全档可用：Plus 档窗口 262,144（档位 stamp，同 k3.1-pro）。
    application
        .select_model_preset("k3", Some("kimi-test-key".into()), None)
        .unwrap();
    assert_eq!(
        application.model_state().unwrap().0.max_context_tokens,
        Some(262_144)
    );

    // Pro：视图放行 highspeed，可选择，窗口 1,048,576。
    application.set_kimi_tier("pro").unwrap();
    let view = application.model_settings_view().unwrap();
    assert!(
        view.presets
            .iter()
            .any(|preset| preset.id == "k3.1-pro-highspeed")
    );
    application
        .select_model_preset("k3.1-pro-highspeed", None, None)
        .unwrap();
    assert_eq!(
        application.model_state().unwrap().0.max_context_tokens,
        Some(1_048_576)
    );

    // 降档守卫：highspeed 活动时拒降 Plus；换到全档模型后放行。
    assert!(application.set_kimi_tier("plus").is_err());
    application.select_model_preset("k3", None, None).unwrap();
    application.set_kimi_tier("plus").unwrap();
    assert_eq!(
        application.model_state().unwrap().0.max_context_tokens,
        Some(262_144)
    );
    application.close().unwrap();
    crate::test_support::cleanup_tree(storage_root.parent().unwrap());
}

/// Hy Token Plan 套餐（2026-11-28）：缺省 hy——通用八预设隐藏、混元
/// 两条在列；common 档放行全部十条；通用门槛预设活动时拒降 hy。
/// 删视图过滤 / select 拒选 / 降档守卫任一对应断言红。
#[test]
fn hy_tier_gates_the_common_plan_roster_and_guards_downgrade() {
    let (storage_root, project_root) = roots("hy-tier-common-roster");
    std::fs::create_dir_all(&project_root).unwrap();
    let project = Project::new(&project_root);
    let application = mount(&project, &storage_root, TestBehavior::Success);

    // 缺省 Hy 套餐：视图只有混元两条；通用预设隐藏且直选拒。
    let view = application.model_settings_view().unwrap();
    assert_eq!(view.hy_tier, "hy");
    let hy_presets: Vec<&str> = view
        .presets
        .iter()
        .filter(|preset| preset.vendor == "Hy Token Plan")
        .map(|preset| preset.id.as_str())
        .collect();
    assert_eq!(hy_presets, vec!["hy4", "hy4-flash"]);
    assert!(
        application
            .select_model_preset(
                "hy-deepseek-v4.1-flash-1017",
                Some("hy-test-key".into()),
                None
            )
            .is_err()
    );

    // hy4 可选（无门槛），切 common 放行通用八预设。
    application
        .select_model_preset("hy4", Some("hy-test-key".into()), None)
        .unwrap();
    application.set_hy_tier("common").unwrap();
    let view = application.model_settings_view().unwrap();
    assert_eq!(view.hy_tier, "common");
    let hy_presets: Vec<&str> = view
        .presets
        .iter()
        .filter(|preset| preset.vendor == "Hy Token Plan")
        .map(|preset| preset.id.as_str())
        .collect();
    assert_eq!(hy_presets.len(), 10);
    assert!(hy_presets.contains(&"hy-deepseek-v4.1-flash-1017"));
    application
        .select_model_preset("hy-deepseek-v4.1-flash-1017", None, None)
        .unwrap();
    assert_eq!(
        application.model_state().unwrap().0.max_context_tokens,
        Some(1_000_000)
    );

    // 降档守卫：通用门槛预设活动时拒降 hy；换回混元后放行。
    assert!(application.set_hy_tier("hy").is_err());
    application.select_model_preset("hy4", None, None).unwrap();
    application.set_hy_tier("hy").unwrap();
    assert_eq!(application.model_settings_view().unwrap().hy_tier, "hy");
    application.close().unwrap();
    crate::test_support::cleanup_tree(storage_root.parent().unwrap());
}
