use super::*;
use crate::plugin::{PluginContext, PluginError, ScopeKind};
struct Test(&'static PluginDescriptor);
impl Plugin for Test {
    fn descriptor(&self) -> &'static PluginDescriptor {
        self.0
    }
    fn mount(&self, _: &mut PluginContext<'_>) -> Result<(), PluginError> {
        panic!("selection must never mount plugins")
    }
}
const SERVICE: ServiceId = ServiceId::new("test.service");
const NEXT: ServiceId = ServiceId::new("test.next");
const SOURCE: PluginDescriptor = PluginDescriptor {
    id: PluginId::new("source"),
    scope: ScopeKind::TrustedProject,
    provides: &[SERVICE],
    requires: &[],
    optional: &[],
};
const CONSUMER: PluginDescriptor = PluginDescriptor {
    id: PluginId::new("consumer"),
    scope: ScopeKind::TrustedProject,
    provides: &[NEXT],
    requires: &[SERVICE],
    optional: &[],
};
const SOFT: PluginDescriptor = PluginDescriptor {
    id: PluginId::new("soft"),
    scope: ScopeKind::TrustedProject,
    provides: &[],
    requires: &[],
    optional: &[SERVICE],
};
fn catalog() -> Vec<Arc<dyn Plugin>> {
    vec![
        Arc::new(Test(&SOURCE)),
        Arc::new(Test(&CONSUMER)),
        Arc::new(Test(&SOFT)),
    ]
}
fn config(ids: &[&str]) -> Configuration {
    Configuration {
        disabled: ids.iter().map(|id| (*id).into()).collect(),
    }
}
#[test]
fn hard_dependency_absence_propagates_but_optional_absence_does_not() {
    let mut catalog = catalog();
    let plan = Selection::resolve(&catalog, &[], &config(&["source"])).unwrap();
    assert!(!plan.entries[0].enabled());
    assert_eq!(plan.entries[1].disabled_by, BTreeSet::from([SOURCE.id]));
    assert!(plan.entries[2].enabled());
    plan.apply(&mut catalog);
    assert_eq!(catalog.len(), 1);
    assert_eq!(catalog[0].descriptor().id, SOFT.id);
}
#[test]
fn required_contract_protects_only_its_hard_dependency_closure() {
    let catalog = catalog();
    let error = Selection::resolve(&catalog, &[NEXT], &config(&["source"]))
        .err()
        .unwrap();
    assert_eq!(error.key, "extension.required_builtin");
    let plan = Selection::resolve(&catalog, &[NEXT], &config(&["soft"])).unwrap();
    assert!(plan.entries[0].protected);
    assert!(plan.entries[1].protected);
    assert!(!plan.entries[2].protected);
}
#[test]
fn misspelled_configuration_and_invalid_catalog_cannot_silently_activate() {
    let catalog = catalog();
    let error = Selection::resolve(&catalog, &[], &config(&["soruce"]))
        .err()
        .unwrap();
    assert_eq!(error.key, "extension.unknown_builtin");
    let invalid = vec![Arc::new(Test(&CONSUMER)) as Arc<dyn Plugin>];
    let error = Selection::resolve(&invalid, &[], &config(&["consumer"]))
        .err()
        .unwrap();
    assert_eq!(error.key, "extension.invalid_composition");
    assert!(serde_json::from_str::<Configuration>(r#"{"diabled":[]}"#).is_err());
    assert!(
        Selection::resolve(
            &catalog,
            &[ServiceId::new("missing.root")],
            &Configuration::default()
        )
        .is_err(),
        "required roots must be present before mount"
    );
}
#[test]
fn default_configuration_preserves_catalog_order_and_all_candidates() {
    let mut catalog = catalog();
    let expected: Vec<_> = catalog.iter().map(|p| p.descriptor().id).collect();
    let plan = Selection::resolve(&catalog, &[], &Configuration::default()).unwrap();
    plan.apply(&mut catalog);
    assert_eq!(
        catalog
            .iter()
            .map(|p| p.descriptor().id)
            .collect::<Vec<_>>(),
        expected
    );
}
