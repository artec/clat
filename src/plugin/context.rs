use super::effect::{DisposeError, DisposeErrors, EffectScope};
use super::service::{ServiceError, ServiceKey, ServiceRegistry};
use super::{PluginId, PluginOwner, ScopeKind, ServiceId};
use std::collections::BTreeSet;
use std::sync::Arc;

pub struct PluginContext<'a> {
    owner: PluginId,
    scope: ScopeKind,
    registry: Arc<ServiceRegistry>,
    effects: EffectScope,
    provided: BTreeSet<ServiceId>,
    dependencies: BTreeSet<ServiceId>,
    marker: std::marker::PhantomData<&'a mut ()>,
}

impl<'a> PluginContext<'a> {
    pub(crate) fn new(
        owner: PluginId,
        scope: ScopeKind,
        registry: Arc<ServiceRegistry>,
        dependencies: impl IntoIterator<Item = ServiceId>,
    ) -> Self {
        Self {
            owner,
            scope,
            registry,
            effects: EffectScope::new(),
            provided: BTreeSet::new(),
            dependencies: dependencies.into_iter().collect(),
            marker: std::marker::PhantomData,
        }
    }

    pub(crate) fn owner(&self) -> PluginOwner {
        PluginOwner::new(self.owner)
    }

    pub fn scope(&self) -> ScopeKind {
        self.scope
    }

    pub fn require<T: ?Sized + Send + Sync + 'static>(
        &self,
        key: ServiceKey<T>,
    ) -> Result<Arc<T>, ServiceError> {
        if !self.dependencies.contains(&key.id()) {
            return Err(ServiceError::UndeclaredDependency {
                plugin: self.owner,
                service: key.id(),
            });
        }
        self.registry.require(key)
    }

    pub fn provide<T: ?Sized + Send + Sync + 'static>(
        &mut self,
        key: ServiceKey<T>,
        service: Arc<T>,
    ) -> Result<(), ServiceError> {
        self.registry.provide(self.owner, key, service)?;
        self.provided.insert(key.id());
        Ok(())
    }

    pub fn defer(&mut self, disposer: impl FnOnce() -> Result<(), DisposeError> + Send + 'static) {
        self.effects.defer(disposer);
    }

    pub fn acquire<T>(
        &mut self,
        resource: T,
        dispose: impl FnOnce(Arc<T>) -> Result<(), DisposeError> + Send + 'static,
    ) -> Arc<T>
    where
        T: Send + Sync + 'static,
    {
        self.effects.acquire(resource, dispose)
    }

    pub(crate) fn provided(&self) -> &BTreeSet<ServiceId> {
        &self.provided
    }

    pub(crate) fn into_effects(self) -> EffectScope {
        self.effects
    }

    pub(crate) fn rollback(mut self) -> Vec<DisposeError> {
        let errors = self
            .effects
            .close()
            .err()
            .map(DisposeErrors::into_errors)
            .unwrap_or_default();
        let mut errors = errors;
        if let Err(error) = self.registry.remove_owner(self.owner) {
            errors.push(DisposeError::new(error.to_string()));
        }
        errors
    }
}
