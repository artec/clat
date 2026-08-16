use super::context::PluginContext;
use super::effect::{DisposeError, DisposeErrors, EffectScope};
use super::service::{ServiceError, ServiceKey, ServiceRegistry};
use super::{Plugin, PluginError, PluginId, ScopeKind, ServiceId};
use std::collections::{BTreeSet, HashMap};
use std::fmt;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CatalogError {
    EmptyPluginId,
    EmptyServiceId {
        plugin: PluginId,
    },
    DuplicatePlugin(PluginId),
    ScopeMismatch {
        plugin: PluginId,
        declared: ScopeKind,
        catalog: ScopeKind,
    },
    DuplicateDeclaredService {
        plugin: PluginId,
        service: ServiceId,
    },
    DuplicateService {
        service: ServiceId,
        first: PluginId,
        second: PluginId,
    },
    ParentServiceOverride {
        plugin: PluginId,
        service: ServiceId,
    },
    MissingDependency {
        plugin: PluginId,
        service: ServiceId,
    },
    DependencyCycle(Vec<PluginId>),
    Registry(String),
}

impl fmt::Display for CatalogError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyPluginId => formatter.write_str("plugin id must not be empty"),
            Self::EmptyServiceId { plugin } => {
                write!(formatter, "plugin `{plugin}` declares an empty service id")
            }
            Self::DuplicatePlugin(id) => write!(formatter, "duplicate plugin id `{id}`"),
            Self::ScopeMismatch {
                plugin,
                declared,
                catalog,
            } => write!(
                formatter,
                "plugin `{plugin}` declares {declared:?} but was placed in {catalog:?}"
            ),
            Self::DuplicateDeclaredService { plugin, service } => write!(
                formatter,
                "plugin `{plugin}` declares service `{service}` more than once"
            ),
            Self::DuplicateService {
                service,
                first,
                second,
            } => write!(
                formatter,
                "service `{service}` is provided by both `{first}` and `{second}`"
            ),
            Self::ParentServiceOverride { plugin, service } => write!(
                formatter,
                "plugin `{plugin}` cannot override parent service `{service}`"
            ),
            Self::MissingDependency { plugin, service } => {
                write!(
                    formatter,
                    "plugin `{plugin}` requires missing service `{service}`"
                )
            }
            Self::DependencyCycle(plugins) => write!(
                formatter,
                "plugin dependency cycle: {}",
                plugins
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join(" -> ")
            ),
            Self::Registry(message) => write!(formatter, "service registry error: {message}"),
        }
    }
}

impl std::error::Error for CatalogError {}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PluginStartError {
    pub plugin: PluginId,
    pub primary: PluginError,
    pub rollback_failures: Vec<DisposeError>,
}

impl fmt::Display for PluginStartError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "plugin `{}` failed: {}",
            self.plugin, self.primary
        )?;
        for error in &self.rollback_failures {
            write!(formatter, "; rollback: {error}")?;
        }
        Ok(())
    }
}

impl std::error::Error for PluginStartError {}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PluginManagerError {
    Catalog(CatalogError),
    Start(PluginStartError),
    AlreadyMounted,
    Closed,
    InvalidChildScope { parent: ScopeKind, child: ScopeKind },
}

impl fmt::Display for PluginManagerError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Catalog(error) => error.fmt(formatter),
            Self::Start(error) => error.fmt(formatter),
            Self::AlreadyMounted => formatter.write_str("plugin catalog already mounted"),
            Self::Closed => formatter.write_str("plugin scope is closed"),
            Self::InvalidChildScope { parent, child } => {
                write!(formatter, "{child:?} cannot be a child of {parent:?}")
            }
        }
    }
}

impl std::error::Error for PluginManagerError {}

impl From<CatalogError> for PluginManagerError {
    fn from(error: CatalogError) -> Self {
        Self::Catalog(error)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ScopeCloseError {
    ActiveChildren(usize),
    Cleanup(DisposeErrors),
}

impl fmt::Display for ScopeCloseError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ActiveChildren(count) => {
                write!(formatter, "scope still has {count} active child scope(s)")
            }
            Self::Cleanup(errors) => errors.fmt(formatter),
        }
    }
}

impl std::error::Error for ScopeCloseError {}

struct MountedPlugin {
    id: PluginId,
    effects: EffectScope,
}

struct ParentGuard {
    children: Arc<AtomicUsize>,
    released: bool,
}

impl ParentGuard {
    fn release(&mut self) {
        if !self.released {
            self.children.fetch_sub(1, Ordering::AcqRel);
            self.released = true;
        }
    }
}

impl Drop for ParentGuard {
    fn drop(&mut self) {
        self.release();
    }
}

pub(crate) struct PluginManager {
    scope: ScopeKind,
    registry: Arc<ServiceRegistry>,
    mounted: Vec<MountedPlugin>,
    children: Arc<AtomicUsize>,
    parent_guard: Option<ParentGuard>,
    mount_called: bool,
    closed: bool,
}

impl PluginManager {
    pub(crate) fn root(scope: ScopeKind) -> Self {
        Self {
            scope,
            registry: ServiceRegistry::root(),
            mounted: Vec::new(),
            children: Arc::new(AtomicUsize::new(0)),
            parent_guard: None,
            mount_called: false,
            closed: false,
        }
    }

    pub(crate) fn child(&mut self, scope: ScopeKind) -> Result<Self, PluginManagerError> {
        if self.closed {
            return Err(PluginManagerError::Closed);
        }
        if !matches!(
            (self.scope, scope),
            (ScopeKind::Bootstrap, ScopeKind::TrustedProject)
                | (ScopeKind::TrustedProject, ScopeKind::Run)
        ) {
            return Err(PluginManagerError::InvalidChildScope {
                parent: self.scope,
                child: scope,
            });
        }
        self.children.fetch_add(1, Ordering::AcqRel);
        Ok(Self {
            scope,
            registry: ServiceRegistry::child(Arc::clone(&self.registry)),
            mounted: Vec::new(),
            children: Arc::new(AtomicUsize::new(0)),
            parent_guard: Some(ParentGuard {
                children: Arc::clone(&self.children),
                released: false,
            }),
            mount_called: false,
            closed: false,
        })
    }

    pub(crate) fn mount_all(
        &mut self,
        catalog: Vec<Arc<dyn Plugin>>,
    ) -> Result<(), PluginManagerError> {
        if self.closed {
            return Err(PluginManagerError::Closed);
        }
        if self.mount_called {
            return Err(PluginManagerError::AlreadyMounted);
        }
        self.mount_called = true;
        let order = plan(&catalog, self.scope, &self.registry)?;

        for index in order {
            let plugin = &catalog[index];
            let descriptor = plugin.descriptor();
            let mut context = PluginContext::new(
                descriptor.id,
                self.scope,
                Arc::clone(&self.registry),
                descriptor
                    .requires
                    .iter()
                    .chain(descriptor.optional)
                    .copied(),
            );
            let mount_result = catch_unwind(AssertUnwindSafe(|| plugin.mount(&mut context)))
                .map_err(|payload| {
                    PluginError::new(format!("mount panicked: {}", panic_message(payload)))
                })
                .and_then(|result| result);
            let expected = descriptor.provides.iter().copied().collect::<BTreeSet<_>>();
            let mount_result = mount_result.and_then(|()| {
                if context.provided() == &expected {
                    Ok(())
                } else {
                    Err(PluginError::new(format!(
                        "declared services {:?} but provided {:?}",
                        expected,
                        context.provided()
                    )))
                }
            });

            match mount_result {
                Ok(()) => self.mounted.push(MountedPlugin {
                    id: descriptor.id,
                    effects: context.into_effects(),
                }),
                Err(primary) => {
                    let mut rollback_failures = context.rollback();
                    rollback_failures.extend(self.rollback_mounted());
                    return Err(PluginManagerError::Start(PluginStartError {
                        plugin: descriptor.id,
                        primary,
                        rollback_failures,
                    }));
                }
            }
        }
        Ok(())
    }

    pub(crate) fn require<T: ?Sized + Send + Sync + 'static>(
        &self,
        key: ServiceKey<T>,
    ) -> Result<Arc<T>, ServiceError> {
        self.registry.require(key)
    }

    pub(crate) fn close(&mut self) -> Result<(), ScopeCloseError> {
        if self.closed {
            return Ok(());
        }
        let active = self.children.load(Ordering::Acquire);
        if active != 0 {
            return Err(ScopeCloseError::ActiveChildren(active));
        }
        self.closed = true;
        let errors = self.rollback_mounted();
        if let Some(parent) = &mut self.parent_guard {
            parent.release();
        }
        if errors.is_empty() {
            Ok(())
        } else {
            Err(ScopeCloseError::Cleanup(DisposeErrors::new(errors)))
        }
    }

    fn rollback_mounted(&mut self) -> Vec<DisposeError> {
        let mut errors = Vec::new();
        while let Some(mut mounted) = self.mounted.pop() {
            if let Err(scope_errors) = mounted.effects.close() {
                errors.extend(scope_errors.into_errors());
            }
            if let Err(error) = self.registry.remove_owner(mounted.id) {
                errors.push(DisposeError::new(error.to_string()));
            }
        }
        errors
    }
}

impl Drop for PluginManager {
    fn drop(&mut self) {
        let _ = self.close();
    }
}

fn plan(
    catalog: &[Arc<dyn Plugin>],
    scope: ScopeKind,
    registry: &ServiceRegistry,
) -> Result<Vec<usize>, CatalogError> {
    let parent_services = registry
        .all_ids()
        .map_err(|error| CatalogError::Registry(error.to_string()))?;
    let mut plugin_ids = BTreeSet::new();
    let mut providers = HashMap::<ServiceId, (usize, PluginId)>::new();

    for (index, plugin) in catalog.iter().enumerate() {
        let descriptor = plugin.descriptor();
        if descriptor.id.as_str().is_empty() {
            return Err(CatalogError::EmptyPluginId);
        }
        if !plugin_ids.insert(descriptor.id) {
            return Err(CatalogError::DuplicatePlugin(descriptor.id));
        }
        if descriptor.scope != scope {
            return Err(CatalogError::ScopeMismatch {
                plugin: descriptor.id,
                declared: descriptor.scope,
                catalog: scope,
            });
        }
        if descriptor
            .provides
            .iter()
            .chain(descriptor.requires)
            .chain(descriptor.optional)
            .any(|service| service.as_str().is_empty())
        {
            return Err(CatalogError::EmptyServiceId {
                plugin: descriptor.id,
            });
        }
        let mut declared = BTreeSet::new();
        for service in descriptor.provides {
            if !declared.insert(*service) {
                return Err(CatalogError::DuplicateDeclaredService {
                    plugin: descriptor.id,
                    service: *service,
                });
            }
            if parent_services.contains(service) {
                return Err(CatalogError::ParentServiceOverride {
                    plugin: descriptor.id,
                    service: *service,
                });
            }
            if let Some((_, first)) = providers.insert(*service, (index, descriptor.id)) {
                return Err(CatalogError::DuplicateService {
                    service: *service,
                    first,
                    second: descriptor.id,
                });
            }
        }
    }

    let mut outgoing = vec![BTreeSet::<usize>::new(); catalog.len()];
    let mut indegree = vec![0usize; catalog.len()];
    for (dependent, plugin) in catalog.iter().enumerate() {
        let descriptor = plugin.descriptor();
        for required in descriptor.requires {
            if parent_services.contains(required) {
                continue;
            }
            let Some((provider, _)) = providers.get(required) else {
                return Err(CatalogError::MissingDependency {
                    plugin: descriptor.id,
                    service: *required,
                });
            };
            if outgoing[*provider].insert(dependent) {
                indegree[dependent] += 1;
            }
        }
        for optional in descriptor.optional {
            if parent_services.contains(optional) {
                continue;
            }
            if let Some((provider, _)) = providers.get(optional)
                && outgoing[*provider].insert(dependent)
            {
                indegree[dependent] += 1;
            }
        }
    }

    let mut ready = indegree
        .iter()
        .enumerate()
        .filter_map(|(index, degree)| (*degree == 0).then_some(index))
        .collect::<BTreeSet<_>>();
    let mut order = Vec::with_capacity(catalog.len());
    while let Some(index) = ready.pop_first() {
        order.push(index);
        for dependent in outgoing[index].iter().copied() {
            indegree[dependent] -= 1;
            if indegree[dependent] == 0 {
                ready.insert(dependent);
            }
        }
    }
    if order.len() != catalog.len() {
        let cycle = indegree
            .iter()
            .enumerate()
            .filter_map(|(index, degree)| (*degree > 0).then_some(catalog[index].descriptor().id))
            .collect();
        return Err(CatalogError::DependencyCycle(cycle));
    }
    Ok(order)
}

fn panic_message(payload: Box<dyn std::any::Any + Send>) -> String {
    if let Some(message) = payload.downcast_ref::<&str>() {
        (*message).to_owned()
    } else if let Some(message) = payload.downcast_ref::<String>() {
        message.clone()
    } else {
        "non-string panic payload".into()
    }
}
