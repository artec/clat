//! Immutable startup composition. No plugin code runs while resolving absence.
use super::{Plugin, PluginDescriptor, PluginId, ServiceId};
use crate::i18n::UiMessage;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct Configuration {
    pub disabled: BTreeSet<String>,
}

pub(crate) struct Entry {
    pub descriptor: &'static PluginDescriptor,
    pub protected: bool,
    pub disabled_by: BTreeSet<PluginId>,
}
impl Entry {
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
        required: &[ServiceId],
        config: &Configuration,
    ) -> Result<Self, UiMessage> {
        // Reuse the kernel's complete catalog validation before filtering can hide defects.
        super::PluginManager::validate_catalog(super::ScopeKind::TrustedProject, catalog).map_err(
            |error| UiMessage::new("extension.invalid_composition").arg("error", error.to_string()),
        )?;
        let providers = providers(catalog);
        let missing: Vec<_> = required
            .iter()
            .filter(|id| !providers.contains_key(id))
            .map(|id| id.as_str())
            .collect();
        if !missing.is_empty() {
            return Err(
                UiMessage::new("extension.invalid_composition").arg("error", missing.join(", "))
            );
        }
        let protected = protection(catalog, &providers, required);
        let known: BTreeSet<_> = catalog.iter().map(|p| p.descriptor().id.as_str()).collect();
        for id in &config.disabled {
            let key = if !known.contains(id.as_str()) {
                Some("extension.unknown_builtin")
            } else if protected.iter().any(|key| key.as_str() == id) {
                Some("extension.required_builtin")
            } else {
                None
            };
            if let Some(key) = key {
                return Err(UiMessage::new(key).arg("id", id.clone()));
            }
        }
        let mut entries: Vec<_> = catalog
            .iter()
            .map(|plugin| {
                let descriptor = plugin.descriptor();
                Entry {
                    descriptor,
                    protected: protected.contains(&descriptor.id),
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

fn providers(catalog: &[Arc<dyn Plugin>]) -> BTreeMap<ServiceId, PluginId> {
    catalog
        .iter()
        .flat_map(|plugin| {
            let descriptor = plugin.descriptor();
            descriptor
                .provides
                .iter()
                .map(move |service| (*service, descriptor.id))
        })
        .collect()
}

fn protection(
    catalog: &[Arc<dyn Plugin>],
    providers: &BTreeMap<ServiceId, PluginId>,
    required: &[ServiceId],
) -> BTreeSet<PluginId> {
    let mut protected: BTreeSet<_> = required
        .iter()
        .filter_map(|id| providers.get(id).copied())
        .collect();
    loop {
        let previous = protected.len();
        for plugin in catalog {
            let descriptor = plugin.descriptor();
            if protected.contains(&descriptor.id) {
                protected.extend(
                    descriptor
                        .requires
                        .iter()
                        .filter_map(|id| providers.get(id)),
                );
            }
        }
        if protected.len() == previous {
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
