//! Resolve the same remembered preset routes used by the model picker, plus named profiles.
use super::*;
use crate::control_storage::ControlStorage;
use crate::goal::HandoffSettings;
use crate::plugins::services::ConfigStore;
use crate::presets::{GlmCodingEntry, MODEL_PRESETS, ModelPreset};

pub(super) fn load_route(
    store: &dyn ConfigStore,
    control: &ControlStorage,
    source: &str,
) -> Option<super::goal_handoff::Route> {
    let Some(preset_ref) = source.strip_prefix("preset:") else {
        return store
            .load_profile(source.strip_prefix("profile:").unwrap_or(source))
            .ok()
            .flatten();
    };
    let (id, entry) = preset_ref
        .split_once(':')
        .map_or((preset_ref, None), |(id, entry)| (id, Some(entry)));
    let preset = crate::presets::preset_by_id(id)?;
    let endpoint = if let Some(entry) = entry {
        super::super::model_settings::resolve_preset_entry(preset, entry).ok()?
    } else {
        preset.endpoint
    };
    let vendor = crate::presets::vendor_credential_slot(endpoint)?;
    let credentials = store
        .load_vendor_key(vendor, preset.protocol)
        .ok()
        .flatten()?;
    if crate::presets::qwen_token_plan_entry(endpoint).is_some_and(|entry| {
        entry != crate::presets::qwen_entry_for_key(credentials.value(0).unwrap_or(""))
    }) {
        return None;
    }
    let mut config = ModelConfig::default();
    preset.apply(&mut config);
    super::super::model_settings::apply_kimi_tier(&mut config, control.kimi_tier());
    config.endpoint = endpoint.to_owned();
    Some((config, credentials))
}

fn preset_sources(preset: &ModelPreset) -> Vec<String> {
    let base = format!("preset:{}", preset.id);
    let entries: &[&str] = if GlmCodingEntry::from_endpoint(preset.endpoint).is_some() {
        &["bigmodel", "zai"]
    } else if crate::presets::qwen_token_plan_entry(preset.endpoint).is_some() {
        &["aliyun", "qianwen"]
    } else {
        return vec![base];
    };
    entries
        .iter()
        .map(|entry| format!("{base}:{entry}"))
        .collect()
}

pub(super) fn allowance_provider(
    config: &ModelConfig,
    keys: &ProviderCredentials,
    control: &ControlStorage,
) -> Option<&'static str> {
    keys.value(0).filter(|value| !value.trim().is_empty())?;
    if let Some(provider) = super::super::console_quota::console_provider(&config.endpoint) {
        let url = url::Url::parse(&config.endpoint).ok()?;
        let host = url.host_str()?;
        if url.scheme() != "https"
            || !url.username().is_empty()
            || url.password().is_some()
            || !console_host(provider, host)
        {
            return None;
        }
        control.read_console_credential(provider).ok().flatten()?;
        Some(provider)
    } else {
        crate::providers::goal_direct_allowance_provider(config)
    }
}

fn console_host(provider: &str, host: &str) -> bool {
    match provider {
        "qianwen" => host == "token-plan.maas.qianwenaiapi.com",
        "bailian" => host.starts_with("token-plan.") && host.ends_with(".maas.aliyuncs.com"),
        "tencent" => matches!(
            host,
            "tokenhub.tencentmaas.com" | "api.lkeap.cloud.tencent.com" | "lkeap.cloud.tencent.com"
        ),
        _ => false,
    }
}

impl TrustedProjectApplication {
    pub fn handoff_settings_view(&self) -> Result<Value, ApplicationError> {
        let profiles = self.list_model_profiles()?;
        let sources = profiles
            .into_iter()
            .map(|profile| format!("profile:{}", profile.name))
            .chain(MODEL_PRESETS.iter().flat_map(preset_sources));
        let models = sources
            .filter_map(|id| {
                let (config, credentials) = load_route(self.config.as_ref(), &self.control, &id)?;
                let provider = allowance_provider(&config, &credentials, &self.control)?;
                let name = id.strip_prefix("profile:").unwrap_or(&config.model);
                let entry = GlmCodingEntry::from_gateway(&config.endpoint).map(|entry| entry.as_str());
            Some(json!({"id":id, "name":name, "provider":provider, "entry":entry, "model":config.model}))
            })
            .collect::<Vec<_>>();
        Ok(json!({"settings":self.control.goal_handoff(), "models":models}))
    }

    pub fn save_handoff_settings(
        &self,
        settings: Option<HandoffSettings>,
    ) -> Result<(), ApplicationError> {
        if let Some(value) = &settings {
            self.validate_handoff_settings(value).map_err(|_| {
                ApplicationError::presented(crate::i18n::UiMessage::new("workflow.handoff_invalid"))
            })?;
        }
        self.control
            .set_goal_handoff(settings)
            .map_err(|error| ApplicationError::new(error.to_string()))
    }

    pub(in crate::application) fn validate_handoff_settings(
        &self,
        value: &HandoffSettings,
    ) -> Result<(), String> {
        let catalog = self
            .handoff_settings_view()
            .map_err(|err| err.to_string())?;
        let mut seen = std::collections::HashSet::new();
        if !(1..=99).contains(&value.threshold)
            || value.profiles.is_empty()
            || value.note.len() > 16 * 1024
            || value.profiles.iter().any(|id| {
                !seen.insert(id)
                    || !catalog["models"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .any(|row| row["id"] == *id)
            })
        {
            return Err("Invalid or unavailable Goal handoff settings".into());
        }
        Ok(())
    }
}
