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
fn all_512_context_and_execution_combinations_plan_without_side_effects() {
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
    ];
    for mask in 0..512 {
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
        for (bit, id) in ids.iter().enumerate() {
            let entry = selection
                .entries
                .iter()
                .find(|e| e.descriptor.id.as_str() == *id)
                .unwrap();
            assert!(!entry.protected);
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
