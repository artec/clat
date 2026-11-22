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
pub(super) mod setup;
pub use setup::{ModelSetupOperation, ModelSetupView};

#[derive(Clone, Serialize, Deserialize)]
pub struct ModelSettingsView {
    pub current: ModelRouteView,
    pub active_profile: Option<String>,
    pub profiles: Vec<String>,
    #[serde(default)]
    pub profile_image_input: std::collections::BTreeMap<String, bool>,
    pub presets: Vec<ModelPresetView>,
    #[serde(default)]
    pub qwen_entry: String,
    #[serde(default)]
    pub kimi_tier: String,
    pub utility: UtilitySettingsView,
}

#[derive(Clone, Debug, Serialize, Deserialize, Eq, PartialEq)]
pub struct UtilitySettingsView {
    pub naming_enabled: bool,
    #[serde(default = "automatic_followups_default")]
    pub summary_enabled: bool,
    pub suggestions_enabled: bool,
    #[serde(default = "automatic_followups_default")]
    pub automatic_followups: bool,
    pub profile: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UtilitySettingsEdit {
    pub naming_enabled: bool,
    pub summary_enabled: Option<bool>,
    pub suggestions_enabled: bool,
    #[serde(default = "automatic_followups_default")]
    pub automatic_followups: bool,
    pub profile: Option<String>,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct ModelPresetView {
    pub id: String,
    pub name: String,
    pub vendor: String,
    #[serde(default)]
    pub image_input: bool,
}

/// Deliberately not ModelConfig: headers, bodies, auth prefixes and credentials
/// may contain secrets. Those fields never enter any transport read surface.
#[derive(Clone, Serialize, Deserialize)]
pub struct ModelRouteView {
    #[serde(default)]
    pub thinking_levels: Vec<crate::ThinkingLevel>,
    #[serde(default)]
    pub thinking_level: Option<crate::ThinkingLevel>,
    pub protocol: ModelProtocol,
    pub model: String,
    /// 与运行快照 `RunModelSnapshot::vendor` 同源的厂商显示 key：模型选
    /// 择器"当前"行与每轮模型请求行显示同一厂商名；无匹配预设时为
    /// None。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vendor: Option<String>,
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
            // OC-17：按该模型的阶梯循环（网关 effort 数组逐模型收窄）。
            let next = crate::next_thinking_level_for(vendor, &config.model, current)
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
        let profiles = self.list_model_profiles()?;
        let profile_image_input = profiles
            .iter()
            .filter_map(|profile| {
                self.load_model_profile(&profile.name)
                    .ok()
                    .flatten()
                    .map(|(config, _)| {
                        (
                            profile.name.clone(),
                            config.capabilities.accepts_image_input(),
                        )
                    })
            })
            .collect();
        Ok(ModelSettingsView {
            current: route_view(&config, &credentials),
            active_profile: self.active_model_profile()?,
            profiles: profiles.into_iter().map(|item| item.name).collect(),
            profile_image_input,
            presets: crate::presets::MODEL_PRESETS
                .iter()
                .map(|preset| ModelPresetView {
                    id: preset.id.into(),
                    name: preset.name.into(),
                    vendor: preset.vendor.into(),
                    image_input: preset.owned_capabilities().accepts_image_input(),
                })
                .collect(),
            // VP-2C 单选回显的单一事实源（2026-11-21 负责人报告"设置页
            // 单选显示百炼但实际记忆千问"）：当前模型是 Qwen Token Plan
            // 预设时按配置端点；否则按 "Qwen" 记忆槽 key 平台前缀
            // （INV-VC8 同源）；无 key → 百炼默认。
            qwen_entry: crate::presets::qwen_token_plan_entry(&config.endpoint)
                .map(str::to_owned)
                .unwrap_or_else(|| {
                    let remembered = self
                        .vendor_key(
                            config.protocol,
                            crate::presets::QWEN_TOKEN_PLAN_ALIYUN_ENDPOINT,
                        )
                        .and_then(|credentials| credentials.value(0).map(str::to_owned))
                        .unwrap_or_default();
                    crate::presets::qwen_entry_for_key(&remembered).to_owned()
                }),
            kimi_tier: self.control.kimi_tier().as_str().to_owned(),
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
            summary_enabled: settings.summary_enabled,
            suggestions_enabled: settings.suggestions_enabled,
            automatic_followups: settings.automatic_followups,
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
        let summary_enabled = match edit.summary_enabled {
            Some(enabled) => enabled,
            None => {
                self.config
                    .load_utility_settings()
                    .map_err(store_error)?
                    .summary_enabled
            }
        };
        self.config
            .save_utility_settings(crate::plugins::services::UtilitySettings {
                naming_enabled: edit.naming_enabled,
                summary_enabled,
                suggestions_enabled: edit.suggestions_enabled,
                automatic_followups: edit.automatic_followups,
                profile,
            })
            .map_err(store_error)?;
        if !edit.automatic_followups {
            self.cancel_followups();
        }
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
        entry: Option<&str>,
    ) -> Result<(), ApplicationError> {
        let preset = crate::presets::preset_by_id(id)
            .ok_or_else(|| ApplicationError::new("unknown model preset"))?;
        validate_key(api_key.as_deref())?;
        // VP-2C：Qwen Token Plan 双入口单选——entry 仅对该厂商预设合法，
        // 决定预设应用后的配置端点（百炼默认 / 千问平台）。其余厂商
        // 预设不接受该参数（显式报错，不静默忽略）。
        let entry_endpoint = match entry {
            None => None,
            Some(entry @ ("aliyun" | "qianwen")) => {
                let endpoint = crate::presets::qwen_token_plan_entry(preset.endpoint)
                    .map(|_| qwen_entry_endpoint(entry))
                    .ok_or_else(|| {
                        ApplicationError::new("entry selection requires a Qwen Token Plan preset")
                    })?;
                Some(endpoint)
            }
            Some(_) => {
                return Err(ApplicationError::new("entry must be aliyun or qianwen"));
            }
        };
        let _update = self
            .host_storage
            .model_updates
            .lock()
            .expect("model updates");
        // VP-2C 事故修复（2026-11-20 真机 401）：不带 entry 的应用不得
        // 回弹已选入口。当前端点仍是该预设的合法入口（preset_accepts_
        // endpoint：相等或同属 Qwen Token Plan 双入口）时原样携带——
        // 单选是入口的唯一事实源；显式 entry 参数才覆盖。
        let carried_endpoint = self
            .model_state()
            .ok()
            .map(|(config, _)| config.endpoint)
            .filter(|endpoint| crate::presets::preset_accepts_endpoint(preset, endpoint));
        let mut config = ModelConfig::default();
        preset.apply(&mut config);
        apply_kimi_tier(&mut config, self.control.kimi_tier());
        // INV-VC8（2026-11-21 负责人复现）：无显式 entry、当前端点也不
        // 可携带时（典型：换厂商后重启再切回），入口随 "Qwen" 记忆槽
        // key 的平台前缀恢复——key 即入口的持久载体（sk-sp- → 千问，
        // 否则百炼）。按预设默认端点取槽即可：双入口同 vendor 同槽。
        let restored_entry = if entry_endpoint.is_none()
            && carried_endpoint.is_none()
            && crate::presets::qwen_token_plan_entry(preset.endpoint).is_some()
        {
            self.vendor_key(config.protocol, &config.endpoint)
                .and_then(|credentials| credentials.value(0).map(str::to_owned))
                .filter(|key| !key.trim().is_empty())
                .map(|key| qwen_entry_endpoint(crate::presets::qwen_entry_for_key(&key)))
        } else {
            None
        };
        if let Some(endpoint) = entry_endpoint
            .or(carried_endpoint.as_deref())
            .or(restored_entry)
        {
            config.endpoint = endpoint.into();
        }
        // 双入口共用 "Qwen" Key 记忆槽（一套 Key 裁定）：换端点不换槽。
        let mut credentials = self
            .vendor_key(config.protocol, &config.endpoint)
            .unwrap_or_else(|| ProviderCredentials::for_protocol(config.protocol));
        if let Some(key) = api_key {
            credentials.set_value(0, key);
        }
        self.save_model_state_locked(&config, &credentials)
    }

    /// VP-2C：单选切换**当前** Qwen Token Plan 模型的入口——只改写端点，
    /// 预设其余钉值与用户 override（思考档位等）原样保留（非破坏切换）。
    /// 当前模型不是 Qwen Token Plan 预设路由时显式拒绝。Key 复用现有
    /// 凭据（双入口同槽）。
    pub fn set_model_preset_entry(
        &self,
        entry: &str,
        api_key: Option<String>,
    ) -> Result<(), ApplicationError> {
        let endpoint = match entry {
            "aliyun" | "qianwen" => qwen_entry_endpoint(entry),
            _ => return Err(ApplicationError::new("entry must be aliyun or qianwen")),
        };
        validate_key(api_key.as_deref())?;
        let _update = self
            .host_storage
            .model_updates
            .lock()
            .expect("model updates");
        let (mut config, mut credentials) = self.model_state()?;
        if !super::dto::matching_preset(&config)
            .is_some_and(|preset| crate::presets::qwen_token_plan_entry(preset.endpoint).is_some())
        {
            return Err(ApplicationError::new(
                "the active model is not a Qwen Token Plan preset",
            ));
        }
        config.endpoint = endpoint.into();
        // VP-2C 事故修复（2026-11-20 真机 401）：一套 Key 二选一——key
        // 框已输入新平台 key 时单选一步完成"换端点+换 key"，否则旧平台
        // key 打新端点必然 401。缺省沿用现值（既有非破坏语义不变）。
        if let Some(key) = api_key {
            credentials.set_value(0, key);
        }
        self.save_model_state_locked(&config, &credentials)
    }

    /// Kimi subscription is a persisted vendor preference, not an API guess.
    pub fn set_kimi_tier(&self, value: &str) -> Result<(), ApplicationError> {
        let tier = crate::control_storage::settings::KimiTier::parse(value)
            .ok_or_else(|| ApplicationError::new("tier must be plus or pro"))?;
        let _update = self
            .host_storage
            .model_updates
            .lock()
            .expect("model updates");
        let (mut config, credentials) = self.model_state()?;
        self.control
            .set_kimi_tier(tier)
            .map_err(|error| ApplicationError::new(error.to_string()))?;
        let active = super::dto::matching_preset(&config)
            .is_some_and(|preset| preset.vendor == "Kimi Coding Plan");
        if active {
            apply_kimi_tier(&mut config, tier);
            self.host_storage.publish_models(&config, &credentials);
        } else {
            self.host_storage.publish_model_settings();
        }
        Ok(())
    }
    pub fn edit_model_profile(&self, edit: ModelProfileEdit) -> Result<(), ApplicationError> {
        let _update = self
            .host_storage
            .model_updates
            .lock()
            .expect("model updates");
        let (name, config, credentials) = self.prepare_model_profile(edit)?;
        self.save_model_profile_locked(&name, &config, &credentials)
    }

    fn prepare_model_profile(
        &self,
        edit: ModelProfileEdit,
    ) -> Result<(String, ModelConfig, ProviderCredentials), ApplicationError> {
        validate_edit(&edit)?;
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
        Ok((edit.name, config, credentials))
    }
}

fn route_view(config: &ModelConfig, credentials: &ProviderCredentials) -> ModelRouteView {
    ModelRouteView {
        // OC-17：逐模型阶梯（网关 effort 数组按模型收窄）。
        thinking_levels: crate::model_thinking_levels(config.vendor(), &config.model).to_vec(),
        thinking_level: crate::effective_thinking_level(config),
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
        vendor: super::dto::route_vendor_key(config).map(str::to_owned),
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

/// The explicit account choice wins over old Kimi model overrides and
/// applies to both built-in Kimi Coding Plan presets, never custom routes.
/// `kimi-for-coding`（K2.8 Preview）豁免档位门槛：官方 Kimi Code 模型表
/// 的最高 1,048,576 无档位注记（Plus 及以上可用），k3.1-pro 的 1M
/// 才是 Pro/Allegretto 解锁项。负责人裁定 2026-11-21 放开实测，
/// Plus 档真机验证通过（端点受理 1M 长上下文）——豁免转正，非实验态。
pub(super) fn apply_kimi_tier(
    config: &mut ModelConfig,
    tier: crate::control_storage::settings::KimiTier,
) {
    let Some(preset) = super::dto::matching_preset(config) else {
        return;
    };
    if preset.vendor != "Kimi Coding Plan" {
        return;
    }
    let window = if preset.id == "kimi-for-coding" {
        1_048_576
    } else {
        tier.context_window()
    };
    config.max_context_tokens = Some(window);
    config.overrides_version = Some(1);
    config.overrides.max_context_tokens = crate::Override::Set(window);
}
/// VP-2C：入口名 → Qwen Token Plan 端点（调用方已验证 entry 合法）。
fn qwen_entry_endpoint(entry: &str) -> &'static str {
    match entry {
        "qianwen" => crate::presets::QWEN_TOKEN_PLAN_QIANWEN_ENDPOINT,
        _ => crate::presets::QWEN_TOKEN_PLAN_ALIYUN_ENDPOINT,
    }
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

fn automatic_followups_default() -> bool {
    true
}
