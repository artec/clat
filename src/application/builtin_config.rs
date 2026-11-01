//! Host-owned next-start configuration; the active composition is immutable.
use super::{ApplicationError, HostApplication, TrustedProjectApplication};
use crate::i18n::UiMessage;
use crate::plugin::selection::{Configuration, Requirement, Selection};
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;

pub(super) struct BuiltinConfig {
    instance: String,
    graph: Vec<&'static crate::plugin::PluginDescriptor>,
    required: Vec<Requirement>,
    startup: Configuration,
    model_adapters: BTreeSet<crate::plugin::PluginId>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Edit {
    instance_id: String,
    revision: String,
    disabled: BTreeSet<String>,
    #[serde(default)]
    preview_id: Option<String>,
}
impl BuiltinConfig {
    pub(super) fn new(app: &TrustedProjectApplication) -> Self {
        Self {
            instance: uuid::Uuid::new_v4().to_string(),
            graph: app.composition.builtin_graph(),
            model_adapters: app
                .composition
                .builtin_graph()
                .iter()
                .filter(|d| crate::plugins::providers::is_model_adapter(d.id))
                .map(|d| d.id)
                .chain(app.providers.owners())
                .collect(),
            required: super::composition::project_service_contract(),
            startup: app.control.builtin_plugins(),
        }
    }
    fn select(&self, config: &Configuration) -> Result<Selection, ApplicationError> {
        Selection::from_descriptors(&self.graph, &self.required, config)
            .map_err(ApplicationError::presented)
    }
    fn rows(&self, config: &Configuration) -> Result<Value, ApplicationError> {
        Ok(json!(self.select(config)?.entries.iter().map(|entry| {
            let descriptor = entry.descriptor;
            let (name, summary) = crate::plugins::inventory::presentation::messages(descriptor.id);
            json!({"id":descriptor.id.as_str(),"name_presentation":name,"summary_presentation":summary,
                "configured_enabled":!config.disabled.contains(descriptor.id.as_str()),"enabled":entry.enabled(),
                "configurable":!entry.protected(),"required_by":entry.required_by,
                "provides":descriptor.provides.iter().map(|id|id.as_str()).collect::<Vec<_>>(),
                "requires":descriptor.requires.iter().map(|id|id.as_str()).collect::<Vec<_>>(),
                "optional":descriptor.optional.iter().map(|id|id.as_str()).collect::<Vec<_>>(),
                "disabled_by":entry.disabled_by.iter().map(|id|id.as_str()).collect::<Vec<_>>()})
        }).collect::<Vec<_>>()))
    }
    fn preview_id(&self, revision: &str, config: &Configuration) -> String {
        let bytes = serde_json::to_vec(&json!([self.instance, revision, config]))
            .expect("serializable config");
        format!("{:x}", Sha256::digest(bytes))
    }
    fn view(
        &self,
        saved: &crate::control_storage::BuiltinConfigSnapshot,
    ) -> Result<Value, ApplicationError> {
        Ok(
            json!({"instance_id":self.instance,"revision":saved.revision,"disabled":saved.config.disabled,
            "startup_disabled":self.startup.disabled,"current":self.rows(&self.startup)?,"next":self.rows(&saved.config)?,
            "pending_restart":self.startup!=saved.config}),
        )
    }
    fn validate(
        &self,
        value: Value,
        saved: &crate::control_storage::BuiltinConfigSnapshot,
    ) -> Result<(Edit, Configuration), ApplicationError> {
        if serde_json::to_vec(&value).map_or(true, |bytes| bytes.len() > 65_536) {
            return Err(error("extension.config_limit"));
        }
        let edit: Edit =
            serde_json::from_value(value).map_err(|_| error("extension.config_invalid"))?;
        if edit.instance_id != self.instance {
            return Err(error("extension.config_host_changed"));
        }
        if edit.revision != saved.revision {
            return Err(error("extension.config_conflict"));
        }
        let config = Configuration {
            disabled: edit.disabled.clone(),
        };
        self.select(&config)?;
        Ok((edit, config))
    }
}
impl HostApplication {
    pub(crate) fn instance_id(&self) -> &str {
        &self.builtin_config.instance
    }
    pub fn builtin_plugins_get(&self) -> Result<Value, ApplicationError> {
        let saved = self
            .storage
            .control
            .read_builtin_plugins()
            .map_err(storage_error)?;
        let mut result = self.builtin_config.view(&saved)?;
        result["storage_root"] = json!(self.storage.root());
        Ok(result)
    }
    pub fn builtin_plugins_preview(&self, value: Value) -> Result<Value, ApplicationError> {
        let saved = self
            .storage
            .control
            .read_builtin_plugins()
            .map_err(storage_error)?;
        let (_, config) = self.builtin_config.validate(value, &saved)?;
        let mut result = self.builtin_config.view(&saved)?;
        result["next"] = self.builtin_config.rows(&config)?;
        result["disabled"] = json!(config.disabled);
        result["preview_id"] = json!(self.builtin_config.preview_id(&saved.revision, &config));
        result["model_execution_available"] = json!(
            self.builtin_config
                .select(&config)?
                .entries
                .iter()
                .any(|entry| entry.enabled()
                    && self
                        .builtin_config
                        .model_adapters
                        .contains(&entry.descriptor.id))
        );
        result["changes"] = changes(&saved.config, &config, &self.builtin_config)?;
        Ok(result)
    }
    pub fn builtin_plugins_set(&mut self, value: Value) -> Result<Value, ApplicationError> {
        let saved = self
            .storage
            .control
            .read_builtin_plugins()
            .map_err(storage_error)?;
        let (edit, config) = self.builtin_config.validate(value, &saved)?;
        if edit.preview_id.as_deref()
            != Some(&self.builtin_config.preview_id(&saved.revision, &config))
        {
            return Err(error("extension.config_preview_expired"));
        }
        self.storage
            .control
            .save_builtin_plugins(&saved.revision, &config)
            .map_err(storage_error)?;
        self.builtin_plugins_get()
            .map_err(|_| error("extension.config_write_uncertain"))
    }
}
fn changes(
    before: &Configuration,
    after: &Configuration,
    owner: &BuiltinConfig,
) -> Result<Value, ApplicationError> {
    let before = owner.select(before)?;
    let after = owner.select(after)?;
    Ok(json!(before.entries.iter().zip(&after.entries).filter_map(|(a,b)| {
        let id=a.descriptor.id;
        let explicit_a=a.disabled_by.contains(&id);
        let explicit_b=b.disabled_by.contains(&id);
        (a.enabled()!=b.enabled()||explicit_a!=explicit_b||a.disabled_by!=b.disabled_by).then(||json!({
            "id":id.as_str(),"name_presentation":crate::plugins::inventory::presentation::messages(id).0,
            "direct":explicit_a!=explicit_b,"configured_enabled":!explicit_b,"before_enabled":a.enabled(),"enabled":b.enabled(),"disabled_by":b.disabled_by.iter().map(|id|id.as_str()).collect::<Vec<_>>()
        }))
    }).collect::<Vec<_>>()))
}
fn error(key: &str) -> ApplicationError {
    ApplicationError::presented(UiMessage::new(key))
}
fn storage_error(error: crate::control_storage::ControlError) -> ApplicationError {
    let text = error.to_string();
    if text.starts_with("extension.") {
        return self::error(&text);
    }
    ApplicationError::presented(UiMessage::new("extension.config_read_failed").arg("error", text))
}
#[cfg(test)]
mod tests;
