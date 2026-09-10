//! thinking implementation behind the stable model contract.
use super::*;

/// 思考强度档位。DeepSeek V4 与 GLM 5.3 都接受
/// `reasoning_effort` + `thinking.type`，CLAT 据此提供统一抽象。
/// 快捷档位只负责开启状态下的强度：DeepSeek 另有 non-thinking 模式
/// （本项目不暴露），GLM 5.3 则不可关闭思考（`disabled` 请求失败）。
/// [`apply_thinking_level`] 因而始终写 enabled。
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ThinkingLevel {
    Low,
    High,
    Max,
}

impl ThinkingLevel {
    /// 展示名（标题栏 `Thinking · High`、flash 提示）。
    pub fn label(self) -> &'static str {
        match self {
            Self::Low => "Low",
            Self::High => "High",
            Self::Max => "Max",
        }
    }

    /// 线上 `reasoning_effort` 取值，厂商感知：DeepSeek/GLM/Kimi 拼写
    /// 一致（low/high/max）；Qwen3.8-Max 官方档位是 low/medium/xhigh
    /// （默认 xhigh），CLAT 的三档按阶梯映射 Low→low、High→medium、
    /// Max→xhigh，保证三档各自有效果（官方兼容表里 high/max 都归并
    /// 为 xhigh，直接透传会让 High/Max 两档无差别）。
    pub(super) fn wire_effort(self, vendor: ModelVendor) -> &'static str {
        match (self, vendor) {
            (Self::Low, _) => "low",
            (Self::High, ModelVendor::Qwen) => "medium",
            (Self::Max, ModelVendor::Qwen) => "xhigh",
            (Self::High, _) => "high",
            (Self::Max, _) => "max",
        }
    }

    /// 从线上取值解析（厂商感知的逆映射）。此处保留 CLAT 的三档规范
    /// 值；厂商对兼容档位的二次映射由其服务端执行。未声明时按预设
    /// 显式 pin 的档位处理。
    pub(super) fn from_wire_effort(vendor: ModelVendor, effort: &str) -> Self {
        if effort.eq_ignore_ascii_case("low") {
            Self::Low
        } else if effort.eq_ignore_ascii_case("max") {
            Self::Max
        } else if effort.eq_ignore_ascii_case("xhigh") && vendor == ModelVendor::Qwen {
            // Qwen 的 xhigh 是顶档＝CLAT 的 Max；其它厂商的 xhigh 按
            // 官方兼容表归并为 high（DeepSeek 映射表）。
            Self::Max
        } else {
            Self::High
        }
    }
}

/// 按端点识别的模型厂商。DeepSeek 与 GLM 提供思考档位与额度监控，
/// Kimi（月之暗面）与 Qwen（阿里云百炼）提供思考档位（额度监控暂无
/// 官方文档支撑，状态栏只显示 usage 派生的 Cache/Context），Tencent
/// （Hy Token Plan）只提供 key 记忆与 usage 派生状态——TC-0 探针实证
/// `reasoning_effort` 在 hy4-preview 上无可复现效果（无效值也不报
/// 错，网关接受但忽略），思考档位留空、不发送无效果参数，其它端点
/// 一律 `Other`（不提供该功能，显示层隐藏相关内容）。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ModelVendor {
    DeepSeek,
    Glm,
    Kimi,
    Qwen,
    Tencent,
    Other,
}

impl ModelVendor {
    /// 厂商 key 记忆库的存储键（INV-VK1：每个已知厂商一把持久 key，
    /// "同 vendor 共享一个 API key 槽位"的兑现）。`Other` 返回 None——
    /// 自定义端点互不相干，绝不互相串 key。
    pub fn storage_key(self) -> Option<&'static str> {
        match self {
            Self::DeepSeek => Some("DeepSeek"),
            Self::Glm => Some("Glm"),
            Self::Kimi => Some("Kimi"),
            Self::Qwen => Some("Qwen"),
            Self::Tencent => Some("Tencent"),
            Self::Other => None,
        }
    }
}

pub fn endpoint_vendor(endpoint: &str) -> ModelVendor {
    let endpoint = endpoint.to_lowercase();
    if endpoint.contains("deepseek.com") {
        ModelVendor::DeepSeek
    } else if endpoint.contains("bigmodel.cn") || endpoint.contains("z.ai") {
        ModelVendor::Glm
    } else if endpoint.contains("moonshot.") || endpoint.contains("kimi.com") {
        // Kimi Coding 会员端点（api.kimi.com/coding/v1）与开放平台
        // 端点（api.moonshot.cn / api.moonshot.ai）。
        ModelVendor::Kimi
    } else if endpoint.contains("aliyuncs.com") || endpoint.contains("dashscope") {
        // Qwen Token Plan 专用 MaaS 域名与百炼按量域名。
        ModelVendor::Qwen
    } else if endpoint.contains("lkeap.cloud.tencent.com") {
        // Tencent Hy Token Plan 的 OpenAI 兼容端点
        // （api.lkeap.cloud.tencent.com/plan/v3）。
        ModelVendor::Tencent
    } else {
        ModelVendor::Other
    }
}

/// 该厂商支持的思考档位，循环切换按此列表 wrap。
pub fn thinking_levels(vendor: ModelVendor) -> &'static [ThinkingLevel] {
    match vendor {
        ModelVendor::DeepSeek => &[ThinkingLevel::Low, ThinkingLevel::High, ThinkingLevel::Max],
        // GLM 5.3 官方支持 low/high/max 三档（无 medium），且不可关闭
        // 思考（disabled 请求失败，见 presets.rs 证据链）。
        ModelVendor::Glm => &[ThinkingLevel::Low, ThinkingLevel::High, ThinkingLevel::Max],
        // Kimi K3 官方支持 low/high/max（默认 max；见
        // platform.kimi.com/docs/overview，2026-08 核验）。
        ModelVendor::Kimi => &[ThinkingLevel::Low, ThinkingLevel::High, ThinkingLevel::Max],
        // Qwen3.8-Max 官方档位 low/medium/xhigh（默认 xhigh），CLAT 三档
        // 经 wire_effort 映射后各有效果（见 wire_effort 注释）。
        ModelVendor::Qwen => &[ThinkingLevel::Low, ThinkingLevel::High, ThinkingLevel::Max],
        // Tencent hy4-preview：思考服务端常开（reasoning_content 恒在），
        // 但 TC-0 探针实证 reasoning_effort 无可复现效果且无效值被
        // 静默接受（docs/research/tc0-probe/）——档位循环会是"切换无
        // 效果"的谎话，留空 = Shift+Tab 对该厂商无效（同 Other）。
        ModelVendor::Tencent => &[],
        ModelVendor::Other => &[],
    }
}

/// 循环切换的下一档；`Other` 厂商返回 `None`（按键无效）。当前档位
/// 不在厂商列表（未来厂商列表收窄时的历史遗留状态）时从首档起步——
/// Shift+Tab 永不静默失效。
pub fn next_thinking_level(vendor: ModelVendor, current: ThinkingLevel) -> Option<ThinkingLevel> {
    let levels = thinking_levels(vendor);
    levels.first().map(
        |first| match levels.iter().position(|&level| level == current) {
            Some(index) => levels[(index + 1) % levels.len()],
            None => *first,
        },
    )
}

/// 把档位写进 `extra_body`（线上格式的唯一写入口）：`reasoning_effort`
/// 按厂商映射（见内部 `ThinkingLevel::wire_effort`）；`thinking` 对象只
/// 写给使用 DeepSeek/GLM 风格开关的厂商——Kimi K3 与 Qwen3.8-Max 的
/// 思考强度是顶层 `reasoning_effort`，不携带 `thinking` 对象（避免
/// 未定义参数），对象内的其它键（GLM 的 `clear_thinking`）原样保留。
pub fn apply_thinking_level(extra_body: &mut Value, vendor: ModelVendor, level: ThinkingLevel) {
    if !extra_body.is_object() {
        *extra_body = Value::Object(Default::default());
    }
    let Some(map) = extra_body.as_object_mut() else {
        return;
    };
    if matches!(vendor, ModelVendor::DeepSeek | ModelVendor::Glm) {
        let thinking = map
            .entry("thinking")
            .or_insert_with(|| Value::Object(Default::default()));
        if !thinking.is_object() {
            *thinking = Value::Object(Default::default());
        }
        if let Some(thinking) = thinking.as_object_mut() {
            thinking.insert("type".into(), Value::String("enabled".into()));
        }
    }
    map.insert(
        "reasoning_effort".into(),
        Value::String(level.wire_effort(vendor).into()),
    );
}

/// 当前生效的思考档位：一等字段优先，其次解析 `extra_body`（预设
/// 默认写法）。手工把 `extra_body` 编辑成 `thinking.type: "disabled"`
/// 视为用户明确关闭思考——返回 `None`，标题栏不显示，下一次
/// Shift+Tab 会恢复成 `enabled` + 三档之一。无档位厂商（`Other`；
/// Tencent Hy——TC-0 探针实证 `reasoning_effort` 无可复现效果）
/// 一律 `None`。
pub fn effective_thinking_level(config: &ModelConfig) -> Option<ThinkingLevel> {
    let vendor = config.vendor();
    if thinking_levels(vendor).is_empty() {
        return None;
    }
    let level = if let Some(level) = config.thinking_level {
        level
    } else {
        let disabled = config
            .extra_body
            .get("thinking")
            .and_then(|thinking| thinking.get("type"))
            .and_then(Value::as_str)
            .is_some_and(|kind| kind.eq_ignore_ascii_case("disabled"));
        if disabled {
            return None;
        }
        let effort = config
            .extra_body
            .get("reasoning_effort")
            .and_then(Value::as_str)
            .unwrap_or("high");
        ThinkingLevel::from_wire_effort(vendor, effort)
    };
    Some(level)
}

/// 标题栏思考段的三态显示（TC-3，2026-09-02）：可循环档位 → 档位名
/// （`Thinking · High`）；**服务端常开但无档位**（Tencent Hy——TC-0
/// 实证 effort 无效果、disabled 被忽略，思考关不掉）→ `"Server"`
/// （`Thinking · Server`：状态可见，Shift+Tab 不可循环，wire 零参数
/// ——不违背探针结论）；无思考面 → `None`（整段省略）。
pub fn thinking_display(config: &ModelConfig) -> Option<&'static str> {
    if let Some(level) = effective_thinking_level(config) {
        return Some(level.label());
    }
    matches!(config.vendor(), ModelVendor::Tencent).then_some("Server")
}
