//! 设置族文件（MP-1 §3/§4.1）：`settings.json`（模型状态 + 档案 +
//! 活动指针，结构原样平移——B9 刚审计闭环，本轮只换载体）、
//! `credentials.json`（厂商 key 记忆，0600，替代 `vendor:` 伪档案行）、
//! `trust.json`（项目信任门，CLAT 特色）。均为**事实类**：撕裂进抢救
//! 路径、版本错位 fail-closed（json_file 纪律）。

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::Path;

use super::json_file::{self, Loaded, UnitTag};

pub(crate) const SETTINGS_NAME: &str = "settings.json";
pub(crate) const SETTINGS_UNIT: (&str, u64) = ("settings", 1);
pub(crate) const CREDENTIALS_NAME: &str = "credentials.json";
pub(crate) const CREDENTIALS_UNIT: (&str, u64) = ("credentials", 1);
pub(crate) const TRUST_NAME: &str = "trust.json";
pub(crate) const TRUST_UNIT: (&str, u64) = ("trust", 1);

/// `settings.json`：单槽活动态 + 命名档案（原 model_state +
/// model_profiles 两表的 JSON 平移；config/runtime 内嵌为真 JSON 对象
/// 而非 SQLite 时代的转义字符串）。
#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct SettingsFile {
    pub unit: UnitTag,
    #[serde(
        default,
        rename = "modelState",
        skip_serializing_if = "Option::is_none"
    )]
    pub model_state: Option<ModelStateRow>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub profiles: BTreeMap<String, ProfileRow>,
}

impl SettingsFile {
    pub(crate) fn empty() -> Self {
        Self {
            unit: UnitTag::new(SETTINGS_UNIT.0, SETTINGS_UNIT.1),
            model_state: None,
            profiles: BTreeMap::new(),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct ModelStateRow {
    pub config: serde_json::Value,
    pub runtime: serde_json::Value,
    #[serde(
        default,
        rename = "activeProfile",
        skip_serializing_if = "Option::is_none"
    )]
    pub active_profile: Option<String>,
    #[serde(rename = "updatedAt")]
    pub updated_at: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct ProfileRow {
    pub config: serde_json::Value,
    pub runtime: serde_json::Value,
    #[serde(rename = "createdAt")]
    pub created_at: String,
    #[serde(rename = "updatedAt")]
    pub updated_at: String,
}

/// `credentials.json`：厂商 key 记忆库（INV-VK1..3 的载体从
/// `model_profiles` 的 `vendor:` 保留行迁出为物理独立文件；用户档案
/// 命名仍拒绝 `vendor:` 前缀——纵深防御，语义不变）。
#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct CredentialsFile {
    pub unit: UnitTag,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub vendors: BTreeMap<String, VendorRow>,
}

impl CredentialsFile {
    pub(crate) fn empty() -> Self {
        Self {
            unit: UnitTag::new(CREDENTIALS_UNIT.0, CREDENTIALS_UNIT.1),
            vendors: BTreeMap::new(),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct VendorRow {
    pub runtime: serde_json::Value,
    #[serde(rename = "updatedAt")]
    pub updated_at: String,
}

/// `trust.json`：canonical 路径 → 信任时间。
#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct TrustFile {
    pub unit: UnitTag,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub projects: BTreeMap<String, TrustRow>,
}

impl TrustFile {
    pub(crate) fn empty() -> Self {
        Self {
            unit: UnitTag::new(TRUST_UNIT.0, TRUST_UNIT.1),
            projects: BTreeMap::new(),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct TrustRow {
    #[serde(rename = "trustedAt")]
    pub trusted_at: String,
}

/// 一次设置族加载的结果：文件态 + 抢救诊断（撕裂残件已改名保留）。
pub(crate) struct LoadedSettings {
    pub settings: SettingsFile,
    pub credentials: CredentialsFile,
    pub trust: TrustFile,
    pub diagnostics: Vec<String>,
}

pub(crate) fn load(
    root_dir: &cap_std::fs::Dir,
    root: &Path,
) -> Result<LoadedSettings, super::ControlError> {
    let mut diagnostics = Vec::new();
    let settings = load_one(
        json_file::load::<SettingsFile>(root_dir, root, SETTINGS_NAME, SETTINGS_UNIT),
        SettingsFile::empty,
        SETTINGS_NAME,
        &mut diagnostics,
    )?;
    let credentials = load_one(
        json_file::load::<CredentialsFile>(root_dir, root, CREDENTIALS_NAME, CREDENTIALS_UNIT),
        CredentialsFile::empty,
        CREDENTIALS_NAME,
        &mut diagnostics,
    )?;
    let trust = load_one(
        json_file::load::<TrustFile>(root_dir, root, TRUST_NAME, TRUST_UNIT),
        TrustFile::empty,
        TRUST_NAME,
        &mut diagnostics,
    )?;
    Ok(LoadedSettings {
        settings,
        credentials,
        trust,
        diagnostics,
    })
}

fn load_one<T>(
    loaded: Result<Loaded<T>, json_file::LoadError>,
    empty: fn() -> T,
    name: &str,
    diagnostics: &mut Vec<String>,
) -> Result<T, super::ControlError> {
    match loaded {
        Ok(Loaded::Missing) => Ok(empty()),
        Ok(Loaded::Intact(file)) => Ok(file),
        Ok(Loaded::Salvaged { remnant }) => {
            diagnostics.push(format!(
                "{name} was torn (crash artifact); the remnant is preserved as {remnant} \
                 and a fresh empty state was started — re-enter the affected settings"
            ));
            Ok(empty())
        }
        Err(error) => Err(super::control_error(error.message())),
    }
}

pub(crate) fn save_settings(
    dir: &cap_std::fs::Dir,
    root: &Path,
    settings: &SettingsFile,
) -> Result<(), super::ControlError> {
    json_file::write(dir, root, SETTINGS_NAME, settings)
        .map_err(|error| super::control_error(format!("cannot save {SETTINGS_NAME}: {error}")))
}

pub(crate) fn save_credentials(
    dir: &cap_std::fs::Dir,
    root: &Path,
    credentials: &CredentialsFile,
) -> Result<(), super::ControlError> {
    json_file::write(dir, root, CREDENTIALS_NAME, credentials)
        .map_err(|error| super::control_error(format!("cannot save {CREDENTIALS_NAME}: {error}")))
}

pub(crate) fn save_trust(
    dir: &cap_std::fs::Dir,
    root: &Path,
    trust: &TrustFile,
) -> Result<(), super::ControlError> {
    json_file::write(dir, root, TRUST_NAME, trust)
        .map_err(|error| super::control_error(format!("cannot save {TRUST_NAME}: {error}")))
}

/// 零写信任查询（bootstrap `is_trusted` 的数据面）：撕裂视为未信任
/// （挂载的抢救路径会自愈——重批一次信任），异构/版本错位 fail-closed。
pub(crate) fn is_trusted_read_only(root: &Path, project_key: &str) -> Result<bool, String> {
    let path = root.join(TRUST_NAME);
    match std::fs::symlink_metadata(&path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => {
            return Err(format!("cannot inspect {}: {error}", path.display()));
        }
        Ok(metadata) if metadata.file_type().is_symlink() => {
            return Err(format!("{} must not be a symbolic link", path.display()));
        }
        Ok(_) => {}
    }
    let text = std::fs::read_to_string(&path)
        .map_err(|error| format!("cannot read {}: {error}", path.display()))?;
    let value: serde_json::Value = match serde_json::from_str(&text) {
        Ok(value) => value,
        // 撕裂 = 崩溃残迹：视为未信任，authorize_and_mount 的抢救路径
        // 会改名残件并重建（不阻塞用户）。
        Err(_) => return Ok(false),
    };
    let file: TrustFile = serde_json::from_value(value)
        .map_err(|error| format!("{} is not a CLAT trust file: {error}", path.display()))?;
    if !file.unit.matches(TRUST_UNIT) {
        return Err(format!(
            "{} carries unit {} v{}; this build expects {} v{} and will not guess-read it",
            path.display(),
            file.unit.name,
            file.unit.version,
            TRUST_UNIT.0,
            TRUST_UNIT.1
        ));
    }
    Ok(file.projects.contains_key(project_key))
}
