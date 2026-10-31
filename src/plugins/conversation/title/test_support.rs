//! Keep real mounted owners alive for every title-provider test.
use crate::model::ModelFactory;
use crate::plugin::{PluginId, PluginManager, PluginOwner, ScopeKind};
use crate::plugins::services::{
    PROVIDER_SERVICE, ProviderLease, SESSION_TITLE_SERVICE, SessionTitler,
};
use crate::plugins::{CompanionUtilityPlugin, ProviderRegistryPlugin, SessionTitlePlugin};
use std::sync::Arc;
pub(crate) struct TestTitler {
    titler: Arc<dyn SessionTitler>,
    _manager: PluginManager,
    _provider: ProviderLease,
}
impl std::ops::Deref for TestTitler {
    type Target = Arc<dyn SessionTitler>;
    fn deref(&self) -> &Self::Target {
        &self.titler
    }
}
pub(crate) fn test_titler(factory: impl ModelFactory + 'static) -> TestTitler {
    let mut manager = PluginManager::root(ScopeKind::TrustedProject);
    manager
        .mount_all(vec![
            Arc::new(ProviderRegistryPlugin),
            Arc::new(CompanionUtilityPlugin::default()),
            Arc::new(SessionTitlePlugin),
        ])
        .unwrap();
    let providers = manager.require(PROVIDER_SERVICE).unwrap();
    let provider = providers
        .register(
            PluginOwner::for_test(PluginId::new("test.title")),
            Arc::new(factory),
        )
        .unwrap();
    TestTitler {
        titler: manager.require(SESSION_TITLE_SERVICE).unwrap(),
        _manager: manager,
        _provider: provider,
    }
}
