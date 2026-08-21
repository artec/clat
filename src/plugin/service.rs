use super::{PluginId, ServiceId};
use std::any::Any;
use std::collections::{BTreeSet, HashMap};
use std::fmt;
use std::marker::PhantomData;
use std::sync::{Arc, Mutex};

pub struct ServiceKey<T: ?Sized + Send + Sync + 'static> {
    id: ServiceId,
    marker: PhantomData<Arc<T>>,
}

impl<T: ?Sized + Send + Sync + 'static> Copy for ServiceKey<T> {}

impl<T: ?Sized + Send + Sync + 'static> Clone for ServiceKey<T> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<T: ?Sized + Send + Sync + 'static> ServiceKey<T> {
    /// Service contracts define fixed keys inside the core. Frontends and
    /// plugins outside this crate cannot manufacture ad-hoc typed aliases.
    pub(crate) const fn new(id: ServiceId) -> Self {
        Self {
            id,
            marker: PhantomData,
        }
    }

    pub const fn id(self) -> ServiceId {
        self.id
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ServiceError {
    Missing(ServiceId),
    Duplicate {
        id: ServiceId,
        existing_owner: PluginId,
        attempted_owner: PluginId,
    },
    TypeMismatch(ServiceId),
    UndeclaredDependency {
        plugin: PluginId,
        service: ServiceId,
    },
    Poisoned,
}

impl fmt::Display for ServiceError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Missing(id) => write!(formatter, "missing service `{id}`"),
            Self::Duplicate {
                id,
                existing_owner,
                attempted_owner,
            } => write!(
                formatter,
                "service `{id}` is already provided by `{existing_owner}`; `{attempted_owner}` cannot override it"
            ),
            Self::TypeMismatch(id) => write!(formatter, "service `{id}` has a different Rust type"),
            Self::UndeclaredDependency { plugin, service } => write!(
                formatter,
                "plugin `{plugin}` requested undeclared dependency `{service}`"
            ),
            Self::Poisoned => formatter.write_str("service registry lock poisoned"),
        }
    }
}

impl std::error::Error for ServiceError {}

struct ServiceEntry {
    owner: PluginId,
    value: Box<dyn Any + Send + Sync>,
}

pub(crate) struct ServiceRegistry {
    parent: Option<Arc<ServiceRegistry>>,
    local: Mutex<HashMap<ServiceId, ServiceEntry>>,
}

impl ServiceRegistry {
    pub(crate) fn root() -> Arc<Self> {
        Arc::new(Self {
            parent: None,
            local: Mutex::new(HashMap::new()),
        })
    }

    pub(crate) fn child(parent: Arc<Self>) -> Arc<Self> {
        Arc::new(Self {
            parent: Some(parent),
            local: Mutex::new(HashMap::new()),
        })
    }

    pub(crate) fn provide<T: ?Sized + Send + Sync + 'static>(
        &self,
        owner: PluginId,
        key: ServiceKey<T>,
        service: Arc<T>,
    ) -> Result<(), ServiceError> {
        if let Some(existing_owner) = self.owner_of(key.id)? {
            return Err(ServiceError::Duplicate {
                id: key.id,
                existing_owner,
                attempted_owner: owner,
            });
        }
        self.local
            .lock()
            .map_err(|_| ServiceError::Poisoned)?
            .insert(
                key.id,
                ServiceEntry {
                    owner,
                    value: Box::new(service),
                },
            );
        Ok(())
    }

    pub(crate) fn require<T: ?Sized + Send + Sync + 'static>(
        &self,
        key: ServiceKey<T>,
    ) -> Result<Arc<T>, ServiceError> {
        {
            let local = self.local.lock().map_err(|_| ServiceError::Poisoned)?;
            if let Some(entry) = local.get(&key.id) {
                return entry
                    .value
                    .downcast_ref::<Arc<T>>()
                    .cloned()
                    .ok_or(ServiceError::TypeMismatch(key.id));
            }
        }
        self.parent
            .as_ref()
            .ok_or(ServiceError::Missing(key.id))?
            .require(key)
    }

    /// 可选获取：服务缺席返回 None（类型不匹配仍为错误——那是装配
    /// bug，不是缺席）。供 descriptor `optional` 声明的服务使用。
    pub(crate) fn try_require<T: ?Sized + Send + Sync + 'static>(
        &self,
        key: ServiceKey<T>,
    ) -> Result<Option<Arc<T>>, ServiceError> {
        match self.require(key) {
            Ok(service) => Ok(Some(service)),
            Err(ServiceError::Missing(_)) => Ok(None),
            Err(error) => Err(error),
        }
    }

    pub(crate) fn owner_of(&self, id: ServiceId) -> Result<Option<PluginId>, ServiceError> {
        if let Some(entry) = self
            .local
            .lock()
            .map_err(|_| ServiceError::Poisoned)?
            .get(&id)
        {
            return Ok(Some(entry.owner));
        }
        match &self.parent {
            Some(parent) => parent.owner_of(id),
            None => Ok(None),
        }
    }

    pub(crate) fn all_ids(&self) -> Result<BTreeSet<ServiceId>, ServiceError> {
        let mut ids = match &self.parent {
            Some(parent) => parent.all_ids()?,
            None => BTreeSet::new(),
        };
        ids.extend(
            self.local
                .lock()
                .map_err(|_| ServiceError::Poisoned)?
                .keys()
                .copied(),
        );
        Ok(ids)
    }

    pub(crate) fn remove_owner(&self, owner: PluginId) -> Result<(), ServiceError> {
        self.local
            .lock()
            .map_err(|_| ServiceError::Poisoned)?
            .retain(|_, entry| entry.owner != owner);
        Ok(())
    }
}
