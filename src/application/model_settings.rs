//! Transport-neutral, secret-write-only model editing contract.
use super::*;
use crate::{ModelConfig, ModelProtocol, ProviderCredentials};
use serde::{Deserialize, Serialize};

mod tuning;
use tuning::ModelProfileTuning;
mod auth;
use auth::ModelProfileAuth;
mod body;
mod headers;

#[derive(Clone, Serialize, Deserialize)]
pub struct ModelSettingsView {
    pub current: ModelRouteView,
    pub active_profile: Option<String>,
    pub profiles: Vec<String>,
    pub presets: Vec<ModelPresetView>,
    pub utility: UtilitySettingsView,
}

#[derive(Clone, Debug, Serialize, Deserialize, Eq, PartialEq)]
pub struct UtilitySettingsView {
    pub naming_enabled: bool,
    pub suggestions_enabled: bool,
    pub profile: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UtilitySettingsEdit {
    pub naming_enabled: bool,
    pub suggestions_enabled: bool,
    pub profile: Option<String>,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct ModelPresetView {
    pub id: String,
    pub name: String,
    pub vendor: String,
}

/// Deliberately not ModelConfig: headers, bodies, auth prefixes and credentials
/// may contain secrets. Those fields never enter any transport read surface.
#[derive(Clone, Serialize, Deserialize)]
pub struct ModelRouteView {
    pub protocol: ModelProtocol,
    pub model: String,
    pub endpoint: String,
    pub request_path: String,
    pub preset: Option<String>,
    pub credential_set: bool,
    pub advanced_settings_present: bool,
    #[serde(default)]
    pub auth_edit_supported: bool,
    #[serde(default)]
    pub extra_headers_edit_supported: bool,
    #[serde(default)]
    pub extra_body_edit_supported: bool,
    #[serde(default)]
    pub image_input: bool,
    #[serde(default)]
    pub route_redacted: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limits: Option<ModelProfileLimits>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tuning: Option<ModelProfileTuning>,
}

#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelProfileLimits {
    pub output_limit: Option<u32>,
    pub max_context_tokens: Option<u32>,
    pub run_token_budget: Option<u64>,
}

impl ModelProfileLimits {
    fn validate(&self) -> Result<(), ApplicationError> {
        if self.output_limit == Some(0) || self.max_context_tokens.is_some_and(|value| value < 4096)
        {
            return Err(ApplicationError::new("invalid model limits"));
        }
        Ok(())
    }

    fn apply(self, config: &mut ModelConfig) {
        config.migrate_legacy_overrides();
        config.output_limit = self.output_limit;
        config.max_context_tokens = self.max_context_tokens;
        config.run_token_budget = self.run_token_budget;
        config.overrides.output_limit = self
            .output_limit
            .map_or(crate::Override::Clear, crate::Override::Set);
        config.overrides.max_context_tokens = self
            .max_context_tokens
            .map_or(crate::Override::Clear, crate::Override::Set);
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelProfileEdit {
    pub name: String,
    pub protocol: ModelProtocol,
    pub model: String,
    pub endpoint: String,
    pub request_path: String,
    /// Omitted preserves an existing key only for the exact same endpoint.
    /// Empty explicitly clears; a changed route never inherits another key.
    pub api_key: Option<String>,
    /// Omitted by older clients: retain the existing route's limits.
    pub limits: Option<ModelProfileLimits>,
    /// Only explicit Set/Clear entries update the matching field.
    pub tuning: Option<ModelProfileTuning>,
    /// Write-only replacements; omitted entries preserve same-route fields.
    pub auth: Option<ModelProfileAuth>,
    /// Write-only whole-map replacement; omission preserves the same route.
    pub extra_headers: Option<serde_json::Value>,
    /// Whole-object replacement also relinquishes typed thinking ownership.
    pub extra_body: Option<serde_json::Value>,
}

impl TrustedProjectApplication {
    pub fn cycle_model_thinking(&self) -> Result<crate::ThinkingLevel, ApplicationError> {
        self.update_model_config(|config| {
            let vendor = config.vendor();
            let current =
                crate::effective_thinking_level(config).unwrap_or(crate::ThinkingLevel::High);
            let next = crate::next_thinking_level(vendor, current)
                .ok_or_else(|| ApplicationError::new("model has no adjustable thinking levels"))?;
            config.thinking_level = Some(next);
            config.overrides_version = Some(1);
            config.overrides.thinking_level = crate::Override::Set(next);
            crate::apply_thinking_level(&mut config.extra_body, vendor, next);
            Ok(next)
        })
    }

    pub(crate) fn update_model_config<T, E: From<ApplicationError>>(
        &self,
        update: impl FnOnce(&mut ModelConfig) -> Result<T, E>,
    ) -> Result<T, E> {
        let _update = self
            .host_storage
            .model_updates
            .lock()
            .expect("model updates");
        let (mut config, credentials) = self.model_state()?;
        let result = update(&mut config)?;
        self.save_model_state_locked(&config, &credentials)?;
        Ok(result)
    }
    pub(super) fn probe_model_snapshot(
        &self,
    ) -> Result<(ModelConfig, ProviderCredentials, u64), ApplicationError> {
        let _update = self
            .host_storage
            .model_updates
            .lock()
            .expect("model updates");
        let (config, credentials) = self.model_state()?;
        Ok((config, credentials, self.host_storage.model_revision()))
    }
    pub fn model_settings_view(&self) -> Result<ModelSettingsView, ApplicationError> {
        let _update = self
            .host_storage
            .model_updates
            .lock()
            .expect("model updates");
        let (config, credentials) = self.model_state()?;
        Ok(ModelSettingsView {
            current: route_view(&config, &credentials),
            active_profile: self.active_model_profile()?,
            profiles: self
                .list_model_profiles()?
                .into_iter()
                .map(|item| item.name)
                .collect(),
            presets: crate::presets::MODEL_PRESETS
                .iter()
                .map(|preset| ModelPresetView {
                    id: preset.id.into(),
                    name: preset.name.into(),
                    vendor: preset.vendor.into(),
                })
                .collect(),
            utility: self.utility_settings_view_locked()?,
        })
    }

    pub fn utility_settings_view(&self) -> Result<UtilitySettingsView, ApplicationError> {
        let _update = self
            .host_storage
            .model_updates
            .lock()
            .expect("model updates");
        self.utility_settings_view_locked()
    }

    fn utility_settings_view_locked(&self) -> Result<UtilitySettingsView, ApplicationError> {
        let settings = self.config.load_utility_settings().map_err(store_error)?;
        Ok(UtilitySettingsView {
            naming_enabled: settings.naming_enabled,
            suggestions_enabled: settings.suggestions_enabled,
            profile: settings.profile,
        })
    }

    pub fn edit_utility_settings(&self, edit: UtilitySettingsEdit) -> Result<(), ApplicationError> {
        let _update = self
            .host_storage
            .model_updates
            .lock()
            .expect("model updates");
        let profile = edit
            .profile
            .map(|name| name.trim().to_owned())
            .filter(|name| !name.is_empty());
        self.config
            .save_utility_settings(crate::plugins::services::UtilitySettings {
                naming_enabled: edit.naming_enabled,
                suggestions_enabled: edit.suggestions_enabled,
                profile,
            })
            .map_err(store_error)?;
        self.host_storage.publish_model_settings();
        Ok(())
    }

    pub fn model_profile_view(
        &self,
        name: &str,
    ) -> Result<Option<ModelRouteView>, ApplicationError> {
        Ok(self
            .load_model_profile(name)?
            .map(|(config, credentials)| route_view(&config, &credentials)))
    }

    pub fn select_model_preset(
        &self,
        id: &str,
        api_key: Option<String>,
    ) -> Result<(), ApplicationError> {
        let preset = crate::presets::preset_by_id(id)
            .ok_or_else(|| ApplicationError::new("unknown model preset"))?;
        validate_key(api_key.as_deref())?;
        let _update = self
            .host_storage
            .model_updates
            .lock()
            .expect("model updates");
        let mut config = ModelConfig::default();
        preset.apply(&mut config);
        let mut credentials = self
            .vendor_key(config.protocol, &config.endpoint)
            .unwrap_or_else(|| ProviderCredentials::for_protocol(config.protocol));
        if let Some(key) = api_key {
            credentials.set_value(0, key);
        }
        self.save_model_state_locked(&config, &credentials)
    }

    pub fn edit_model_profile(&self, edit: ModelProfileEdit) -> Result<(), ApplicationError> {
        validate_edit(&edit)?;
        let _update = self
            .host_storage
            .model_updates
            .lock()
            .expect("model updates");
        let old = self.load_model_profile(&edit.name)?;
        let unchanged = old.as_ref().is_some_and(|(config, _)| {
            config.protocol == edit.protocol
                && config.endpoint == edit.endpoint
                && config.request_path == edit.request_path
                && config.model == edit.model
        });
        let (mut config, mut credentials) = if unchanged {
            old.expect("existing unchanged route")
        } else {
            (
                ModelConfig::default(),
                ProviderCredentials::for_protocol(edit.protocol),
            )
        };
        config.preset = None;
        config.protocol = edit.protocol;
        config.model = edit.model;
        config.endpoint = edit.endpoint;
        config.request_path = edit.request_path;
        if let Some(headers) = edit.extra_headers {
            config.extra_headers = headers;
        }
        if let Some(auth) = edit.auth {
            auth.apply(&mut config);
        }
        if let Some(limits) = edit.limits {
            limits.apply(&mut config);
        }
        if let Some(tuning) = edit.tuning {
            tuning.apply(&mut config);
        }
        if let Some(body) = edit.extra_body {
            body::apply(body, &mut config);
        }
        if let Some(key) = edit.api_key {
            credentials.set_value(0, key);
        }
        self.save_model_profile_locked(&edit.name, &config, &credentials)
    }
}

fn route_view(config: &ModelConfig, credentials: &ProviderCredentials) -> ModelRouteView {
    ModelRouteView {
        auth_edit_supported: true,
        extra_headers_edit_supported: true,
        extra_body_edit_supported: true,
        tuning: Some(ModelProfileTuning::from_config(config)),
        limits: Some(ModelProfileLimits {
            output_limit: config.output_limit,
            max_context_tokens: config.max_context_tokens,
            run_token_budget: config.run_token_budget,
        }),
        route_redacted: public_url(&config.endpoint) != config.endpoint
            || config.request_path.contains(['?', '#']),
        image_input: config.capabilities.accepts_image_input(),
        protocol: config.protocol,
        model: config.model.clone(),
        endpoint: public_url(&config.endpoint),
        request_path: config
            .request_path
            .split(['?', '#'])
            .next()
            .unwrap_or_default()
            .into(),
        preset: config.preset.clone(),
        credential_set: credentials.value(0).is_some_and(|key| !key.is_empty()),
        advanced_settings_present: config
            .extra_headers
            .as_object()
            .is_some_and(|map| !map.is_empty())
            || config
                .extra_body
                .as_object()
                .is_some_and(|map| !map.is_empty()),
    }
}

fn public_url(endpoint: &str) -> String {
    let Ok(mut url) = url::Url::parse(endpoint) else {
        return String::new();
    };
    if url.username().is_empty()
        && url.password().is_none()
        && url.query().is_none()
        && url.fragment().is_none()
    {
        return endpoint.to_owned();
    }
    let _ = url.set_username("");
    let _ = url.set_password(None);
    url.set_query(None);
    url.set_fragment(None);
    url.to_string().trim_end_matches('/').to_owned()
}

fn validate_key(key: Option<&str>) -> Result<(), ApplicationError> {
    if key.is_some_and(|key| key.len() > 8192 || key.chars().any(char::is_control)) {
        return Err(ApplicationError::new(
            "API key has invalid length or control characters",
        ));
    }
    Ok(())
}

fn validate_edit(edit: &ModelProfileEdit) -> Result<(), ApplicationError> {
    if let Some(value) = &edit.extra_body {
        body::validate(value)?;
        if edit
            .tuning
            .is_some_and(|tuning| !tuning.thinking_level.is_inherit())
        {
            return Err(ApplicationError::new(
                "save Extra Body and thinking changes separately",
            ));
        }
    }
    if let Some(value) = &edit.extra_headers {
        headers::validate(value)?;
    }
    validate_key(edit.api_key.as_deref())?;
    if let Some(auth) = &edit.auth {
        auth.validate()?;
    }
    if let Some(limits) = &edit.limits {
        limits.validate()?;
    }
    if let Some(tuning) = &edit.tuning {
        tuning.validate()?;
    }
    if edit.name.trim().is_empty()
        || edit.name.len() > 128
        || edit.model.trim().is_empty()
        || edit.model.len() > 256
        || edit.name.chars().any(char::is_control)
        || edit.model.chars().any(char::is_control)
    {
        return Err(ApplicationError::new("profile name or model is invalid"));
    }
    let url = url::Url::parse(&edit.endpoint)
        .map_err(|_| ApplicationError::new("endpoint must be an HTTP(S) URL"))?;
    if !matches!(url.scheme(), "http" | "https")
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || !edit.request_path.starts_with('/')
        || edit.request_path.starts_with("//")
        || edit.request_path.contains(['?', '#'])
        || edit.request_path.chars().any(char::is_control)
    {
        return Err(ApplicationError::new(
            "endpoint/path must not contain credentials, query, or fragment",
        ));
    }
    Ok(())
}
