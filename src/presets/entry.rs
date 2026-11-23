use super::{ModelConfig, ModelPreset, preset_by_id};
use crate::model::{ModelVendor, endpoint_vendor};
use serde::{Deserialize, Serialize};

pub const QWEN_TOKEN_PLAN_ALIYUN_ENDPOINT: &str =
    "https://token-plan.ap-southeast-1.maas.aliyuncs.com/compatible-mode/v1";
pub const QWEN_TOKEN_PLAN_QIANWEN_ENDPOINT: &str =
    "https://token-plan.maas.qianwenaiapi.com/compatible-mode/v1";

pub fn qwen_token_plan_entry(endpoint: &str) -> Option<&'static str> {
    match endpoint {
        QWEN_TOKEN_PLAN_ALIYUN_ENDPOINT => Some("aliyun"),
        QWEN_TOKEN_PLAN_QIANWEN_ENDPOINT => Some("qianwen"),
        _ => None,
    }
}

pub(crate) fn qwen_entry_endpoint(entry: &str) -> Option<&'static str> {
    match entry {
        "aliyun" => Some(QWEN_TOKEN_PLAN_ALIYUN_ENDPOINT),
        "qianwen" => Some(QWEN_TOKEN_PLAN_QIANWEN_ENDPOINT),
        _ => None,
    }
}

/// Qwen shares a key slot. Its platform-specific prefix restores the entry
/// after switching away, with Aliyun as the fallback for unfamiliar keys.
pub fn qwen_entry_for_key(key: &str) -> &'static str {
    if key.trim_start().starts_with("sk-sp-") {
        "qianwen"
    } else {
        "aliyun"
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum GlmCodingEntry {
    #[default]
    BigModel,
    Zai,
}

impl GlmCodingEntry {
    const ALL: [Self; 2] = [Self::BigModel, Self::Zai];

    pub(crate) const fn endpoint(self) -> &'static str {
        match self {
            Self::BigModel => "https://open.bigmodel.cn/api/coding/paas/v4",
            Self::Zai => "https://api.z.ai/api/coding/paas/v4",
        }
    }

    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::BigModel => "bigmodel",
            Self::Zai => "zai",
        }
    }

    pub(crate) const fn origin(self) -> &'static str {
        match self {
            Self::BigModel => "https://open.bigmodel.cn",
            Self::Zai => "https://api.z.ai",
        }
    }

    pub(crate) fn from_endpoint(endpoint: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|entry| entry.endpoint() == endpoint)
    }

    pub(crate) fn from_preset_config(config: &ModelConfig) -> Option<Self> {
        let preset = preset_by_id(config.preset.as_deref()?)?;
        Self::from_endpoint(preset.endpoint)?;
        if preset.model != config.model
            || preset.protocol != config.protocol
            || preset.request_path != config.request_path
        {
            return None;
        }
        Self::from_endpoint(&config.endpoint)
    }

    pub(crate) fn from_gateway(endpoint: &str) -> Option<Self> {
        let url = url::Url::parse(endpoint).ok()?;
        if !url.username().is_empty() || url.password().is_some() {
            return None;
        }
        let origin = url.origin().ascii_serialization();
        Self::ALL.into_iter().find(|entry| entry.origin() == origin)
    }
}

pub(crate) fn vendor_credential_slot(endpoint: &str) -> Option<&'static str> {
    let vendor = endpoint_vendor(endpoint);
    if vendor != ModelVendor::Glm {
        return vendor.storage_key();
    }
    match GlmCodingEntry::from_gateway(endpoint)? {
        // Keep the domestic built-ins' saved slot from earlier versions.
        GlmCodingEntry::BigModel => vendor.storage_key(),
        GlmCodingEntry::Zai => Some("GlmZai"),
    }
}

pub(crate) fn preset_entry_endpoint(preset: &ModelPreset, entry: &str) -> Option<&'static str> {
    if qwen_token_plan_entry(preset.endpoint).is_some() {
        return qwen_entry_endpoint(entry);
    }
    GlmCodingEntry::from_endpoint(preset.endpoint)?;
    GlmCodingEntry::ALL
        .into_iter()
        .find(|candidate| candidate.as_str() == entry)
        .map(GlmCodingEntry::endpoint)
}

pub fn preset_accepts_endpoint(preset: &ModelPreset, endpoint: &str) -> bool {
    preset.endpoint == endpoint
        || (qwen_token_plan_entry(preset.endpoint).is_some()
            && qwen_token_plan_entry(endpoint).is_some())
        || (GlmCodingEntry::from_endpoint(preset.endpoint).is_some()
            && GlmCodingEntry::from_endpoint(endpoint).is_some())
}

#[cfg(test)]
#[test]
fn glm_entry_credential_slots_require_the_official_gateway() {
    assert_eq!(
        vendor_credential_slot("https://open.bigmodel.cn/api/anthropic"),
        Some("Glm")
    );
    assert_eq!(
        vendor_credential_slot("https://api.z.ai/api/anthropic"),
        Some("GlmZai")
    );
    assert_eq!(
        vendor_credential_slot("https://api.z.ai.example.com/api/anthropic"),
        None
    );
    assert_eq!(
        vendor_credential_slot("https://login@api.z.ai/api/anthropic"),
        None
    );
    assert_eq!(
        vendor_credential_slot("http://api.z.ai/api/anthropic"),
        None
    );
}
