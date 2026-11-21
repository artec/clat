use super::*;
use crate::Project;
use crate::test_support::{TestBehavior, TestProviderPlugin, roots};
use std::sync::Arc;

fn mount(
    project: &Project,
    storage_root: &std::path::Path,
    behavior: TestBehavior,
) -> TrustedProjectApplication {
    BootstrapApplication::open(project.clone(), storage_root.to_path_buf())
        .unwrap()
        .authorize_and_mount_with_provider(Arc::new(TestProviderPlugin { behavior }))
        .unwrap()
}

#[test]
fn context_capacity_surfaces_follow_active_model_and_membership_immediately() {
    let (storage_root, project_root) = roots("context-capacity-surfaces");
    std::fs::create_dir_all(&project_root).unwrap();
    let project = Project::new(&project_root);
    let application = mount(&project, &storage_root, TestBehavior::Success);

    let assert_window = |expected: Option<u32>| {
        let host = application.workbench_snapshot().unwrap();
        assert_eq!(host.model.max_context_tokens, expected);
        let stats = application.conversation_stats().unwrap();
        assert_eq!(
            stats["context"]["capacity"],
            serde_json::to_value(expected).unwrap(),
            "composer popover must read the current Host model window"
        );
    };

    application
        .select_model_preset("k3.1-pro", Some("kimi-test-key".into()), None)
        .unwrap();
    assert_window(Some(262_144));
    application.set_kimi_tier("pro").unwrap();
    assert_window(Some(1_048_576));
    application.set_kimi_tier("plus").unwrap();
    assert_window(Some(262_144));

    application
        .select_model_preset("opencode-grok-4.7", Some("grok-test-key".into()), None)
        .unwrap();
    assert_window(Some(500_000));
    let (mut config, credentials) = application.model_state().unwrap();
    config.max_context_tokens = Some(131_072);
    config.overrides.max_context_tokens = crate::Override::Set(131_072);
    config.overrides_version = Some(1);
    application.save_model_state(&config, &credentials).unwrap();
    assert_window(Some(131_072));

    // Explicitly clearing the window does not resurrect preset defaults.
    config.max_context_tokens = None;
    config.overrides.max_context_tokens = crate::Override::Clear;
    application.save_model_state(&config, &credentials).unwrap();
    assert_window(None);

    application
        .select_model_preset("k3.1-pro", None, None)
        .unwrap();
    assert_window(Some(262_144));

    application.close().unwrap();
    crate::test_support::cleanup_tree(storage_root.parent().unwrap());
}
