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
    ///
    /// 共享端点上的第三方家族例外（快照 id 含后缀同样命中）由
    /// [`Self::wire_effort_for`] 按模型分流；本函数保持纯厂商默认。
    pub(crate) fn wire_effort(self, vendor: ModelVendor) -> &'static str {
        match (self, vendor) {
            (Self::Low, _) => "low",
            (Self::High, ModelVendor::Qwen) => "medium",
            (Self::Max, ModelVendor::Qwen) => "xhigh",
            (Self::High, _) => "high",
            (Self::Max, _) => "max",
        }
    }

    /// 模型感知版线上取值：共享厂商端点上代管的第三方模型家族按
    /// 各自官方档位拼写分流，其余落厂商默认（[`Self::wire_effort`]）。
    /// 当前家族：Qwen Token Plan / 百炼端点上的 DeepSeek 与 GLM 系列
    /// （见 [`bailian_third_party_ladder`]）。
    pub(crate) fn wire_effort_for(self, vendor: ModelVendor, model: &str) -> &'static str {
        if bailian_third_party_ladder(vendor, model) {
            return match self {
                Self::Low => "low",
                Self::High => "high",
                Self::Max => "max",
            };
        }
        self.wire_effort(vendor)
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

/// 共享厂商端点上的第三方模型家族档位拼写分支（OC-17 逐模型阶梯的
/// 厂商侧推广）：Qwen Token Plan / 百炼代管的第三方系列官方
/// `reasoning_effort` 档位都是默认拼写 low/high/max，**不是** Qwen
/// 自家的 low/medium/xhigh——纯厂商映射会把 High/Max 档发成端点
/// 不认识的 medium/xhigh。已核验两族（help.aliyun.com/zh/model-studio，
/// 2026-11-28）：DeepSeek 系列（deepseek-api：low/high/max 默认
/// high，含 deepseek-v4.1-flash-1017 / deepseek-v4-pro-0813 快照）、
/// GLM 系列（glm-5-3：low/high/max 三档深度思考）。前缀枚举按
/// 证据走；后续新家族（kimi/minimax 上百炼）探针核验后再入表。
fn bailian_third_party_ladder(vendor: ModelVendor, model: &str) -> bool {
    vendor == ModelVendor::Qwen
        && model
            .to_ascii_lowercase()
            .split('-')
            .next()
            .is_some_and(|family| matches!(family, "deepseek" | "glm"))
}

/// 按端点识别的模型厂商。DeepSeek 与 GLM 提供思考档位与额度监控，
/// Kimi（月之暗面）与 Qwen（阿里云百炼）提供思考档位（额度监控暂无
/// 官方文档支撑，状态栏只显示 usage 派生的 Cache/Context），Tencent
/// （Hy Token Plan）只提供 key 记忆与 usage 派生状态——TC-0 探针实证
/// `reasoning_effort` 在 Hy Token Plan 订阅端点上无可复现效果（无效
/// 值也不报错，网关接受但忽略；2026-11-07 GA 后官方调用指南在
/// TokenHub 平台 `/v1` 端点文档化了该参数，但 plan/v3 订阅端点未经
/// 重新探针，结论维持），思考档位留空、不发送无效果参数，其它端点
/// 一律 `Other`（不提供该功能，显示层隐藏相关内容）。OpenCode
/// （OC-Go，2026-11-18 接入）：opencode.ai 合规网关提供思考档位
/// （网关统一 low/medium/high/max）与额度查询（`/usage` 三窗）。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ModelVendor {
    DeepSeek,
    Glm,
    Kimi,
    Qwen,
    Tencent,
    OpenCode,
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
            Self::OpenCode => Some("OpenCode"),
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
    } else if endpoint.contains("aliyuncs.com")
        || endpoint.contains("dashscope")
        || endpoint.contains("qianwenaiapi")
    {
        // Qwen Token Plan 专用 MaaS 域名（百炼/千问双入口，VP-2C——
        // 同厂商同 Key 槽同阶梯）与百炼按量域名。
        ModelVendor::Qwen
    } else if endpoint.contains("tokenhub.tencentmaas.com")
        || endpoint.contains("lkeap.cloud.tencent.com")
    {
        // Tencent Hy Token Plan 的 OpenAI 兼容端点。官方套餐文档
        // （2026-11-04 更新）主推 tokenhub.tencentmaas.com/plan/v3，
        // 预设 2026-11-07 随新；旧 lkeap 域名并存，为存量自定义配置
        // 保留推导（key 记忆槽与思考显示不因域名拼写漂移）。
        ModelVendor::Tencent
    } else if endpoint.contains("opencode.ai") {
        // OC-Go（2026-11-18）：OpenCode Go/Go Plus 合规推理网关
        // （opencode.ai/inference/go）。端点判据与适配器侧
        // `x-opencode-session` 头的门控（openai_compatible.rs）一致。
        ModelVendor::OpenCode
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
        // Tencent hy4（GA；原 hy4-preview）：思考服务端常开
        //（reasoning_content 恒在），但 TC-0 探针实证 reasoning_effort
        // 无可复现效果且无效值被静默接受（docs/research/tc0-probe/）
        // ——档位循环会是"切换无效果"的谎话，留空 = Shift+Tab 对该
        // 厂商无效（同 Other）。GA 调用指南在 TokenHub 平台 API
        // 文档化了 effort（默认 high），plan/v3 订阅端点待重新探针
        // 后再评估开档。
        ModelVendor::Tencent => &[],
        // OC-Go：厂商级默认三档——但该网关的 effort 数组是**逐模型**
        // 的（OC-17），实际可用档位以 [`model_thinking_levels`] 的
        // 逐模型表为准；此函数保留作非网关路径的厂商默认。
        ModelVendor::OpenCode => &[ThinkingLevel::Low, ThinkingLevel::High, ThinkingLevel::Max],
        ModelVendor::Other => &[],
    }
}

/// OC-17：**逐模型**思考档位。OpenCode 网关的 effort 数组按模型各异
/// （deepseek-v4-pro 无 low、kimi-k3 仅 max、qwen4-max/gemini/grok 无
/// max、minimax 仅 toggle），厂商级阶梯会让 Shift+Tab 选到网关必拒的
/// 档位——真机红值与钉值同源于 2026-11-19 `/v1/models` 快照。判据：
/// CLAT 档位 L 可用 ⇔ `wire_effort(L)` ∈ 该模型网关数组。未知网关
/// 模型 fail-closed 返回空阶梯（同 Other/Tencent：不提供无证据的
/// 档位；预设 pin 的 effort 照常发送）。
pub fn model_thinking_levels(vendor: ModelVendor, model: &str) -> &'static [ThinkingLevel] {
    if vendor != ModelVendor::OpenCode {
        return thinking_levels(vendor);
    }
    match model {
        "deepseek-v4.1-flash"
        | "kimi-k3.1-pro"
        | "glm-5.3"
        | "glm-5.3-flash"
        | "claude-haiku-5-5"
        | "gpt-6-luna" => &[ThinkingLevel::Low, ThinkingLevel::High, ThinkingLevel::Max],
        // 数组 high,max：无 low。
        "deepseek-v4-pro" => &[ThinkingLevel::High, ThinkingLevel::Max],
        // 数组仅 max。
        "kimi-k3" => &[ThinkingLevel::Max],
        // 数组止于 xhigh（qwen4-max）/ high（gemini-4-argon、grok-4.7）：无 max。
        "qwen4-max" | "gemini-4-argon" | "grok-4.7" => &[ThinkingLevel::Low, ThinkingLevel::High],
        // minimax-m3：reasoning 仅 toggle、无 effort 档位（OC-14）。
        _ => &[],
    }
}

/// 循环切换的下一档；`Other` 厂商返回 `None`（按键无效）。当前档位
/// 不在厂商列表（未来厂商列表收窄时的历史遗留状态）时从首档起步——
/// Shift+Tab 永不静默失效。
pub fn next_thinking_level(vendor: ModelVendor, current: ThinkingLevel) -> Option<ThinkingLevel> {
    let levels = thinking_levels(vendor);
    wrap_level(levels, current)
}

/// OC-17：逐模型版循环（网关模型用 [`model_thinking_levels`] 的阶梯；
/// 其余厂商等同 [`next_thinking_level`]）。
pub fn next_thinking_level_for(
    vendor: ModelVendor,
    model: &str,
    current: ThinkingLevel,
) -> Option<ThinkingLevel> {
    let levels = model_thinking_levels(vendor, model);
    wrap_level(levels, current)
}

fn wrap_level(levels: &[ThinkingLevel], current: ThinkingLevel) -> Option<ThinkingLevel> {
    levels.first().map(
        |first| match levels.iter().position(|&level| level == current) {
            Some(index) => levels[(index + 1) % levels.len()],
            None => *first,
        },
    )
}

/// 把档位写进 `extra_body`（线上格式的唯一写入口）：`reasoning_effort`
/// 按厂商+模型映射（见 `ThinkingLevel::wire_effort_for`——Qwen 端点上
/// 的第三方 DeepSeek 系列按 DeepSeek 拼写）；`thinking` 对象只写给
/// 使用 DeepSeek/GLM 风格开关的厂商——Kimi K3、Qwen 系与百炼代管的
/// DeepSeek 系列思考强度都是顶层 `reasoning_effort`，不携带 `thinking`
/// 对象（避免未定义参数），对象内的其它键（GLM 的 `clear_thinking`）
/// 原样保留。
pub fn apply_thinking_level(
    extra_body: &mut Value,
    vendor: ModelVendor,
    model: &str,
    level: ThinkingLevel,
) {
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
        Value::String(level.wire_effort_for(vendor, model).into()),
    );
}

/// 当前生效的思考档位：一等字段优先（OC-17：仅当它在**该模型**的
/// 阶梯内——跨模型/跨预设携带的陈旧值不显示也不发送，回落到
/// `extra_body` 的预设 pin 解析，显示与实际发送一致），其次解析
/// `extra_body`（预设默认写法）。手工把 `extra_body` 编辑成
/// `thinking.type: "disabled"` 视为用户明确关闭思考——返回 `None`，
/// 标题栏不显示，下一次 Shift+Tab 会恢复成 `enabled` + 阶梯内档位。
/// 无档位模型（`Other`、Tencent Hy、无 effort 数组的网关模型）
/// 一律 `None`。
pub fn effective_thinking_level(config: &ModelConfig) -> Option<ThinkingLevel> {
    let vendor = config.vendor();
    let levels = model_thinking_levels(vendor, &config.model);
    if levels.is_empty() {
        return None;
    }
    let level = if let Some(level) = config.thinking_level
        && levels.contains(&level)
    {
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

/// Derive a one-shot text profile without changing the persisted agent configuration.
/// Preserve unknown endpoint fields and only reduce an already-advertised effort.
pub(crate) fn internal_text_config(config: &ModelConfig) -> ModelConfig {
    let mut shaped = config.clone();
    if let Some(body) = shaped.extra_body.as_object_mut() {
        body.remove("thinking");
        if body.contains_key("reasoning_effort") {
            body.insert("reasoning_effort".into(), serde_json::json!("low"));
        }
    }
    if shaped.thinking_level.is_some() {
        shaped.thinking_level = Some(ThinkingLevel::Low);
    }
    if matches!(shaped.overrides.thinking_level, Override::Set(_)) {
        shaped.overrides.thinking_level = Override::Set(ThinkingLevel::Low);
    }
    shaped
}
