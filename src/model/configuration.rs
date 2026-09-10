//! configuration implementation behind the stable model contract.
use super::*;

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelProtocol {
    OpenAiResponses,
    OpenAiCompatible,
}

impl ModelProtocol {
    pub const ALL: [Self; 2] = [Self::OpenAiCompatible, Self::OpenAiResponses];

    pub fn next(self) -> Self {
        match self {
            Self::OpenAiCompatible => Self::OpenAiResponses,
            Self::OpenAiResponses => Self::OpenAiCompatible,
        }
    }

    pub fn previous(self) -> Self {
        match self {
            Self::OpenAiCompatible => Self::OpenAiResponses,
            Self::OpenAiResponses => Self::OpenAiCompatible,
        }
    }

    pub fn default_request_path(self) -> &'static str {
        match self {
            Self::OpenAiResponses => "/responses",
            Self::OpenAiCompatible => "/chat/completions",
        }
    }
}

impl fmt::Display for ModelProtocol {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::OpenAiResponses => f.write_str("OpenAI Responses"),
            Self::OpenAiCompatible => f.write_str("OpenAI Compatible"),
        }
    }
}

/// 模型路由键：与 journal `assistant/message` 的 `source {provider,
/// model}` 同一口径（provider 由 agent 运行时传 `protocol.to_string()`）。
/// 状态栏 Cache 口径按它分桶（INV-C1：按路由累计、切换不混合不清零）；
/// journal 折叠、运行事件活账（RunEvent::ModelRequested）、当前配置
/// 显示三端共用，防键漂移。
pub(crate) fn model_route_key(protocol: &str, model: &str) -> String {
    format!("{protocol}/{model}")
}

/// INV-MM2-1：模型输入模态词表（能力快照的原子）。
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Modality {
    Text,
    Image,
}

/// INV-MM2-1/2：模型输入能力的冻结快照——attach admission、serve、
/// tool catalog（view_image 门控）、provider 投影消费同一份。内置
/// 预设在 `apply` 时 stamp；custom 配置持久化自己的值（编辑器显式
/// 选择归 MM-2 W2 切片，此前默认 fail-closed 纯文本）。禁止按模型
/// 名猜能力、禁止 paid 400 探测。
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ModelCapabilities {
    pub input_modalities: Vec<Modality>,
    pub tool_result_modalities: Vec<Modality>,
    /// 图片输入能力是否有**自有 live 探针证据**（INV-MM2-2：仅厂商
    /// 文档声明 → false，attach 拒绝并给可行动错误——不给全员为
    /// 未验证能力买单）。
    pub image_input_verified: bool,
}

impl Default for ModelCapabilities {
    fn default() -> Self {
        // fail-closed：custom/旧配置一律纯文本。
        Self {
            input_modalities: vec![Modality::Text],
            tool_result_modalities: vec![Modality::Text],
            image_input_verified: false,
        }
    }
}

impl ModelCapabilities {
    /// attach admission 的唯一判据（INV-MM2-2）。
    pub fn accepts_image_input(&self) -> bool {
        self.input_modalities.contains(&Modality::Image) && self.image_input_verified
    }

    /// Visual tool results are stricter than ordinary input: the route must
    /// be probe-verified and explicitly accept image results as well as image
    /// input. This single predicate drives the W5 catalog gate.
    pub fn accepts_image_tool_results(&self) -> bool {
        self.accepts_image_input() && self.tool_result_modalities.contains(&Modality::Image)
    }
}

/// 请求侧图片策略（F-2/F-3/F-5 的词表冻结；MM-2 W6 请求投影消费并
/// 强制）。custom 配置默认 = CLAT 全局 admission 口径。
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ImageRequestPolicy {
    /// 发给该通道的 media type 白名单。
    pub media_types: Vec<String>,
    pub max_images: usize,
    pub max_bytes: u64,
}

impl Default for ImageRequestPolicy {
    fn default() -> Self {
        Self {
            media_types: vec!["image/png".into(), "image/jpeg".into()],
            max_images: 8,
            max_bytes: 4 * 1024 * 1024,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ModelConfig {
    /// Identifier of the built-in preset this configuration came from, if any.
    pub preset: Option<String>,
    pub protocol: ModelProtocol,
    pub model: String,
    pub endpoint: String,
    pub request_path: String,
    pub auth_header: String,
    pub auth_prefix: String,
    pub extra_headers: Value,
    pub extra_body: Value,
    pub output_limit: Option<u32>,
    pub temperature: Option<f64>,
    pub parallel_tool_calls: bool,
    /// 上下文窗口预算（tokens）。`None`（旧配置默认）时自动压缩关闭；
    /// `/compact` 手动路径不受此限制。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_context_tokens: Option<u32>,
    /// 思考强度档位（DeepSeek/GLM）。`None`（旧配置默认）跟随预设与
    /// 服务端默认；`Some` 是用户显式选择，加载时经
    /// [`apply_thinking_level`] 映射进 `extra_body` 后随请求发送。
    /// 存成一等字段而不是直接放 `extra_body`：`model_state()` 每次加载
    /// 都会 `preset.apply` 整体重置 `extra_body`，只有独立字段能在
    /// 回填后存活。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thinking_level: Option<ThinkingLevel>,
    /// per-run token 花费护栏（B1，2026-08-22 定案）：口径
    /// `input_tokens + output_tokens`（缓存命中计入 input、不重复计）。
    /// `None` = 缺省 [`RUN_TOKEN_BUDGET_DEFAULT`]；`Some(0)` = 显式关闭
    /// （文档标注不建议）；`Some(n)` = 上限 n。独立一等字段（同
    /// `thinking_level` 的存活理由）；过顶 → run 以三要素错误终止，
    /// 50%/90% 各一次持久化预警（`clat/budget`，ignorable 事件）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_token_budget: Option<u64>,
    /// INV-MM2-1：模型输入能力快照。内置预设 `apply` 时 stamp；旧
    /// 配置/未选择能力的 custom 配置反序列化为 fail-closed 纯文本。
    #[serde(default)]
    pub capabilities: ModelCapabilities,
    /// INV-MM2-6 词表（F-2/F-3/F-5）：请求侧图片策略。预设 stamp；
    /// custom 默认 = CLAT 全局 admission 口径（W6 起强制）。
    #[serde(default)]
    pub image_policy: ImageRequestPolicy,
    /// INV-MM2-3（MM-2 W2）：typed 显式 overrides——preset 切换不
    /// 碰它（用户真正的 override 存活），merge 在
    /// [`ModelConfig::apply_overrides`]。
    #[serde(default)]
    pub overrides: ModelOverrides,
    /// INV-MM2-3 迁移版本：`None` = 旧配置未迁移（load 时按字段
    /// 精确相等语义生成 overrides 并写 1，见
    /// [`ModelConfig::migrate_legacy_overrides`]）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub overrides_version: Option<u32>,
}

/// INV-MM2-3：一等字段的三态 override。`Inherit` 跟随 preset-managed
/// 默认；`Set` 显式用户值（预设切换存活）；`Clear` 是 suppress/
/// tombstone——字段完全不发（如 output_limit Clear → 请求不带
/// `max_tokens`）。
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Override<T> {
    #[default]
    Inherit,
    Set(T),
    Clear,
}

impl<T> Override<T> {
    pub fn is_inherit(&self) -> bool {
        matches!(self, Self::Inherit)
    }
}

/// INV-MM2-3：typed overrides 词表（冻结面）。`run_token_budget` 是
/// 纯用户 run policy，**不在** preset/override 词表内——预设与
/// overrides 都不重置它。受控 extra body/header 的 allowlist 层走
/// `extra_body`/`extra_headers`（值 `null` = tombstone，W2 起
/// provider 侧抑制该键）。
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize, Default)]
pub struct ModelOverrides {
    #[serde(default, skip_serializing_if = "Override::is_inherit")]
    pub output_limit: Override<u32>,
    #[serde(default, skip_serializing_if = "Override::is_inherit")]
    pub temperature: Override<f64>,
    #[serde(default, skip_serializing_if = "Override::is_inherit")]
    pub parallel_tool_calls: Override<bool>,
    #[serde(default, skip_serializing_if = "Override::is_inherit")]
    pub thinking_level: Override<ThinkingLevel>,
    #[serde(default, skip_serializing_if = "Override::is_inherit")]
    pub max_context_tokens: Override<u32>,
}

impl ModelConfig {
    /// Provider request projection for the tri-state parallel-tools field.
    /// The legacy/effective bool remains available to UI code, while Clear
    /// must omit the wire key entirely instead of silently sending `true`.
    pub fn request_parallel_tool_calls(&self) -> Option<bool> {
        match self.overrides.parallel_tool_calls {
            Override::Clear => None,
            Override::Inherit | Override::Set(_) => Some(self.parallel_tool_calls),
        }
    }

    /// INV-MM2-3 冻结合并序的第三步：preset-managed 默认（apply 已
    /// stamp）之上应用 typed 显式 overrides。**thinking_level 的厂商
    /// 映射在本函数内一次完成**——`model_state()` 之后不得再二次改
    /// `extra_body`。allowlisted extra 层（第四步）在 provider 请求
    /// 构造时合并（`merge_extra_body`，null = tombstone）。
    pub fn apply_overrides(&mut self) {
        match self.overrides.output_limit {
            Override::Inherit => {}
            Override::Set(value) => self.output_limit = Some(value),
            Override::Clear => self.output_limit = None,
        }
        match self.overrides.temperature {
            Override::Inherit => {}
            Override::Set(value) => self.temperature = Some(value),
            Override::Clear => self.temperature = None,
        }
        match self.overrides.parallel_tool_calls {
            Override::Inherit => {}
            Override::Set(value) => self.parallel_tool_calls = value,
            // Clear：请求不携带 parallel_tool_calls（provider 侧按
            // Option<bool>=None 处理——由 build_request_options 消费
            // config 的 None 语义；这里保持 true/false 一等字段在
            // Clear 时回落端点默认）。
            Override::Clear => self.parallel_tool_calls = true,
        }
        match self.overrides.max_context_tokens {
            Override::Inherit => {}
            Override::Set(value) => self.max_context_tokens = Some(value),
            Override::Clear => self.max_context_tokens = None,
        }
        match self.overrides.thinking_level {
            Override::Inherit => {}
            Override::Set(level) => {
                let vendor = endpoint_vendor(&self.endpoint);
                if !thinking_levels(vendor).is_empty() {
                    // 一次成型：apply stamp 的预设 effort 被用户档位
                    // 覆盖；unknown/无档位 vendor 不注入（严格网关拒
                    // 未定义参数；Tencent Hy 的 effort 无效果——TC-3
                    // wire 零参数，与 effective_thinking_level 口径
                    // 一致）。
                    apply_thinking_level(&mut self.extra_body, vendor, level);
                }
                // 一等字段回填（UI/持久层继续读它；merge 的唯一事实
                // 源是 overrides）。
                self.thinking_level = Some(level);
            }
            Override::Clear => {
                if let Some(map) = self.extra_body.as_object_mut() {
                    map.remove("reasoning_effort");
                }
                self.thinking_level = None;
            }
        }
    }

    /// INV-MM2-3 旧配置迁移（版本门 + 幂等）：与**当时 preset-managed
    /// key/value 精确相等**的值归 Inherit；不相等归显式 Set。旧 schema
    /// 无 Clear 表达（None 即跟随预设 = Inherit），如实记档。无预设的
    /// custom 配置按 ModelConfig 缺省为 managed 基线同律比较。
    pub fn migrate_legacy_overrides(&mut self) {
        if self.overrides_version.is_some() {
            return;
        }
        let preset = self
            .preset
            .as_deref()
            .and_then(crate::presets::preset_by_id);
        let managed_output = preset.map(|preset| preset.output_limit);
        let managed_window = preset.map(|preset| preset.context_window);
        let managed_parallel = preset.is_none_or(|preset| preset.parallel_managed_default());

        self.overrides.output_limit = match self.output_limit {
            Some(value) if Some(value) != managed_output => Override::Set(value),
            _ => Override::Inherit,
        };
        self.overrides.temperature = match self.temperature {
            Some(value) => Override::Set(value),
            None => Override::Inherit,
        };
        self.overrides.parallel_tool_calls = if self.parallel_tool_calls == managed_parallel {
            Override::Inherit
        } else {
            Override::Set(self.parallel_tool_calls)
        };
        self.overrides.thinking_level = match self.thinking_level {
            Some(level) => Override::Set(level),
            None => Override::Inherit,
        };
        self.overrides.max_context_tokens = match self.max_context_tokens {
            Some(value) if Some(value) != managed_window => Override::Set(value),
            _ => Override::Inherit,
        };
        self.overrides_version = Some(1);
    }
}

impl Default for ModelConfig {
    fn default() -> Self {
        Self {
            preset: None,
            protocol: ModelProtocol::OpenAiCompatible,
            model: String::new(),
            endpoint: String::new(),
            request_path: "/chat/completions".into(),
            auth_header: "Authorization".into(),
            auth_prefix: "Bearer ".into(),
            extra_headers: Value::Object(Default::default()),
            extra_body: Value::Object(Default::default()),
            output_limit: Some(4096),
            temperature: None,
            parallel_tool_calls: true,
            max_context_tokens: None,
            run_token_budget: None,
            thinking_level: None,
            capabilities: ModelCapabilities::default(),
            image_policy: ImageRequestPolicy::default(),
            overrides: ModelOverrides::default(),
            overrides_version: None,
        }
    }
}
