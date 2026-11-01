use super::*;
struct DescriptorOnly(&'static crate::plugin::PluginDescriptor);
impl Plugin for DescriptorOnly {
    fn descriptor(&self) -> &'static crate::plugin::PluginDescriptor {
        self.0
    }
    fn mount(
        &self,
        _: &mut crate::plugin::PluginContext<'_>,
    ) -> Result<(), crate::plugin::PluginError> {
        panic!("combination planning must never mount services")
    }
}
#[test]
fn all_32768_optional_capability_combinations_plan_without_side_effects() {
    let (storage, root) = crate::test_support::roots("all-plugin-plans");
    std::fs::create_dir_all(&root).unwrap();
    let app = crate::BootstrapApplication::open(Project::new(&root), storage.clone())
        .unwrap()
        .authorize_and_mount_with_provider(Arc::new(crate::test_support::TestProviderPlugin {
            behavior: crate::test_support::TestBehavior::Success,
        }))
        .unwrap();
    let catalog: Vec<Arc<dyn Plugin>> = app
        .composition
        .selection
        .entries
        .iter()
        .map(|e| Arc::new(DescriptorOnly(e.descriptor)) as Arc<dyn Plugin>)
        .collect();
    let ids = [
        "builtin.goal",
        "builtin.task",
        "builtin.jobs",
        "builtin.scheduler",
        "builtin.skills",
        "builtin.memory",
        "builtin.subagent",
        "builtin.mcp_adapter",
        "builtin.plan_mode",
        "builtin.development",
        "builtin.draft_images",
        "builtin.project_instructions",
        "builtin.utility_model",
        "builtin.view_image",
        "builtin.compaction_image_offload",
    ];
    for mask in 0..32768 {
        let config = crate::plugin::selection::Configuration {
            disabled: ids
                .iter()
                .enumerate()
                .filter(|(bit, _)| mask & (1 << bit) != 0)
                .map(|(_, id)| id.to_string())
                .collect(),
        };
        let selection = crate::plugin::selection::Selection::resolve(
            &catalog,
            &project_service_contract(),
            &config,
        )
        .unwrap();
        assert_preview_matches(&catalog, &config, &selection);
        for (bit, id) in ids.iter().enumerate() {
            let entry = selection
                .entries
                .iter()
                .find(|e| e.descriptor.id.as_str() == *id)
                .unwrap();
            assert!(!entry.protected());
            assert_eq!(
                entry.enabled(),
                mask & (1 << bit) == 0,
                "mask={mask} id={id}"
            );
        }
    }
    app.close().unwrap();
    crate::test_support::cleanup_tree(&storage);
    crate::test_support::cleanup_tree(&root);
}

fn assert_preview_matches(
    catalog: &[Arc<dyn Plugin>],
    config: &crate::plugin::selection::Configuration,
    selection: &crate::plugin::selection::Selection,
) {
    let descriptors: Vec<_> = catalog.iter().map(|plugin| plugin.descriptor()).collect();
    let preview = crate::plugin::selection::Selection::from_descriptors(
        &descriptors,
        &project_service_contract(),
        config,
    )
    .unwrap();
    for (active, planned) in selection.entries.iter().zip(&preview.entries) {
        assert_eq!(active.descriptor.id, planned.descriptor.id);
        assert_eq!(active.disabled_by, planned.disabled_by);
        assert_eq!(
            serde_json::to_value(&active.required_by).unwrap(),
            serde_json::to_value(&planned.required_by).unwrap()
        );
    }
}

#[test]
fn sandbox_is_an_explicit_core_root_without_process_dependency() {
    assert!(
        project_service_contract()
            .iter()
            .any(|r| r.service_id == crate::plugins::services::SANDBOX_SERVICE_ID.as_str()),
        "sandbox protection must survive removal of the Process dependency edge"
    );
}

#[test]
fn core_contract_names_thirteen_roots_without_expanding_protected_plugins() {
    let (storage, root) = crate::test_support::roots("core-contract-roots");
    std::fs::create_dir_all(&root).unwrap();
    let app = crate::BootstrapApplication::open(Project::new(&root), storage.clone())
        .unwrap()
        .authorize_and_mount_with_provider(Arc::new(crate::test_support::TestProviderPlugin {
            behavior: crate::test_support::TestBehavior::Success,
        }))
        .unwrap();
    let catalog: Vec<Arc<dyn Plugin>> = app
        .composition
        .selection
        .entries
        .iter()
        .map(|e| Arc::new(DescriptorOnly(e.descriptor)) as Arc<dyn Plugin>)
        .collect();
    let roots = project_service_contract();
    assert_eq!(roots.len(), 13);
    assert_eq!(
        roots
            .iter()
            .map(|r| r.service_id)
            .collect::<std::collections::BTreeSet<_>>()
            .len(),
        13
    );
    let previous: Vec<_> = roots
        .iter()
        .filter(|r| r.service_id != "core.sandbox")
        .cloned()
        .collect();
    let select = |roots: &[_]| {
        crate::plugin::selection::Selection::resolve(&catalog, roots, &Default::default()).unwrap()
    };
    let protected = |plan: crate::plugin::selection::Selection| {
        plan.entries
            .into_iter()
            .filter(|e| e.protected())
            .map(|e| e.descriptor.id)
            .collect::<std::collections::BTreeSet<_>>()
    };
    assert_eq!(protected(select(&roots)), protected(select(&previous)));
    assert_eq!(protected(select(&roots)).len(), 12);
    for entry in select(&roots).entries.iter().filter(|e| e.protected()) {
        let config = crate::plugin::selection::Configuration {
            disabled: [entry.descriptor.id.as_str().to_string()].into(),
        };
        let error = crate::plugin::selection::Selection::resolve(&catalog, &roots, &config)
            .err()
            .unwrap();
        assert_eq!(error.key, "extension.required_builtin");
        assert!(error.arg_presentations.contains_key("reason"));
    }
    for root in &roots {
        let without: Vec<_> = catalog
            .iter()
            .filter(|p| {
                !p.descriptor()
                    .provides
                    .iter()
                    .any(|s| s.as_str() == root.service_id)
            })
            .cloned()
            .collect();
        assert!(
            crate::plugin::selection::Selection::resolve(&without, &roots, &Default::default())
                .is_err()
        );
    }
    let sandbox = roots
        .iter()
        .find(|r| r.service_id == "core.sandbox")
        .unwrap()
        .clone();
    assert_eq!(
        protected(select(&[sandbox])),
        [crate::plugin::PluginId::new("builtin.sandbox.policy")].into()
    );
    app.close().unwrap();
    crate::test_support::cleanup_tree(storage.parent().unwrap());
}
