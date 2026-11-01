//! Immutable startup composition. No plugin code runs while resolving absence.
use super::{Plugin, PluginDescriptor, PluginId, ServiceId};
use crate::i18n::UiMessage;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct Configuration {
    pub disabled: BTreeSet<String>,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct Requirement {
    pub service_id: &'static str,
    pub reason_presentation: UiMessage,
}
impl Requirement {
    pub(crate) fn new(service: ServiceId, reason_presentation: UiMessage) -> Self {
        Self {
            service_id: service.as_str(),
            reason_presentation,
        }
    }
}

pub(crate) struct Entry {
    pub descriptor: &'static PluginDescriptor,
    pub required_by: Vec<Requirement>,
    pub disabled_by: BTreeSet<PluginId>,
}
impl Entry {
    pub(crate) fn protected(&self) -> bool {
        !self.required_by.is_empty()
    }
    pub(crate) fn enabled(&self) -> bool {
        self.disabled_by.is_empty()
    }
}

pub(crate) struct Selection {
    pub entries: Vec<Entry>,
}
impl Selection {
    pub(crate) fn resolve(
        catalog: &[Arc<dyn Plugin>],
        required: &[Requirement],
        config: &Configuration,
    ) -> Result<Self, UiMessage> {
        // Reuse the kernel's complete catalog validation before filtering can hide defects.
        super::PluginManager::validate_catalog(super::ScopeKind::TrustedProject, catalog).map_err(
            |error| UiMessage::new("extension.invalid_composition").arg("error", error.to_string()),
        )?;
        let descriptors: Vec<_> = catalog.iter().map(|plugin| plugin.descriptor()).collect();
        Self::from_descriptors(&descriptors, required, config)
    }

    /// Replan the complete graph captured after successful startup validation.
    pub(crate) fn from_descriptors(
        catalog: &[&'static PluginDescriptor],
        required: &[Requirement],
        config: &Configuration,
    ) -> Result<Self, UiMessage> {
        let providers = providers(catalog);
        let missing: Vec<_> = required
            .iter()
            .filter(|id| !providers.contains_key(&ServiceId::new(id.service_id)))
            .map(|id| id.service_id)
            .collect();
        if !missing.is_empty() {
            return Err(
                UiMessage::new("extension.invalid_composition").arg("error", missing.join(", "))
            );
        }
        let protected = protection(catalog, &providers, required);
        let known: BTreeSet<_> = catalog.iter().map(|p| p.id.as_str()).collect();
        for id in &config.disabled {
            if !known.contains(id.as_str()) {
                return Err(UiMessage::new("extension.unknown_builtin").arg("id", id.clone()));
            }
            if let Some(roots) = protected
                .iter()
                .find_map(|(owner, roots)| (owner.as_str() == id).then_some(roots))
            {
                let reason = required
                    .iter()
                    .find(|r| roots.contains(&ServiceId::new(r.service_id)))
                    .expect("protection originates from a required contract");
                return Err(UiMessage::new("extension.required_builtin")
                    .arg("id", id.clone())
                    .arg_message("reason", reason.reason_presentation.clone()));
            }
        }
        let mut entries: Vec<_> = catalog
            .iter()
            .map(|plugin| {
                let descriptor = *plugin;
                Entry {
                    descriptor,
                    required_by: {
                        let mut roots: Vec<_> = required
                            .iter()
                            .filter(|root| {
                                protected.get(&descriptor.id).is_some_and(|ids| {
                                    ids.contains(&ServiceId::new(root.service_id))
                                })
                            })
                            .cloned()
                            .collect();
                        roots.sort_by_key(|root| root.service_id);
                        roots
                    },
                    disabled_by: if config.disabled.contains(descriptor.id.as_str()) {
                        BTreeSet::from([descriptor.id])
                    } else {
                        BTreeSet::new()
                    },
                }
            })
            .collect();
        propagate(&mut entries, &providers);
        Ok(Self { entries })
    }

    pub(crate) fn apply(&self, catalog: &mut Vec<Arc<dyn Plugin>>) {
        catalog.retain(|plugin| {
            self.entries
                .iter()
                .any(|entry| entry.descriptor.id == plugin.descriptor().id && entry.enabled())
        });
    }
}

fn providers(catalog: &[&'static PluginDescriptor]) -> BTreeMap<ServiceId, PluginId> {
    catalog
        .iter()
        .flat_map(|plugin| {
            let descriptor = *plugin;
            descriptor
                .provides
                .iter()
                .map(move |service| (*service, descriptor.id))
        })
        .collect()
}

fn protection(
    catalog: &[&'static PluginDescriptor],
    providers: &BTreeMap<ServiceId, PluginId>,
    required: &[Requirement],
) -> BTreeMap<PluginId, BTreeSet<ServiceId>> {
    let mut protected: BTreeMap<PluginId, BTreeSet<ServiceId>> = BTreeMap::new();
    for requirement in required {
        let service = ServiceId::new(requirement.service_id);
        if let Some(owner) = providers.get(&service) {
            protected.entry(*owner).or_default().insert(service);
        }
    }
    loop {
        let previous = protected.clone();
        for plugin in catalog {
            let descriptor = *plugin;
            if let Some(roots) = previous.get(&descriptor.id) {
                for owner in descriptor
                    .requires
                    .iter()
                    .filter_map(|service| providers.get(service))
                {
                    protected
                        .entry(*owner)
                        .or_default()
                        .extend(roots.iter().copied());
                }
            }
        }
        if protected == previous {
            return protected;
        }
    }
}

fn propagate(entries: &mut [Entry], providers: &BTreeMap<ServiceId, PluginId>) {
    loop {
        let disabled: BTreeMap<_, _> = entries
            .iter()
            .map(|entry| (entry.descriptor.id, entry.disabled_by.clone()))
            .collect();
        let mut changed = false;
        for entry in &mut *entries {
            let previous = entry.disabled_by.len();
            for provider in entry
                .descriptor
                .requires
                .iter()
                .filter_map(|id| providers.get(id))
            {
                entry.disabled_by.extend(disabled[provider].iter().copied());
            }
            changed |= previous != entry.disabled_by.len();
        }
        if !changed {
            return;
        }
    }
}

#[cfg(test)]
mod tests;
