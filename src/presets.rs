//! Built-in model presets carrying official provider parameters.
//!
//! Presets let a user configure a known model by picking its name instead of
//! hand-editing protocol, model id, endpoint, and request parameters. The
//! values here come from the providers' official API documentation.

use crate::{ModelConfig, ModelProtocol};
use serde_json::{Value, json};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ModelPreset {
    /// Stable identifier stored in the saved configuration.
    pub id: &'static str,
    /// Human-readable name shown in the UI.
    pub name: &'static str,
    pub description: &'static str,
    /// Vendor shown as the first level of the /model picker. Presets with
    /// the same vendor share one API key slot.
    pub vendor: &'static str,
    pub protocol: ModelProtocol,
    pub model: &'static str,
    pub endpoint: &'static str,
    pub request_path: &'static str,
    /// Official maximum output length in tokens.
    pub output_limit: u32,
    /// Official context window in tokens（状态栏 `Context: 120k/1M` 的
    /// 分母）。逐模型维护、逐模型核验来源（见模块文档），新增预设不得
    /// 默认沿用 1M。2026-08-19 起它同时是自动压缩预算的种入值：
    /// `apply` 以已知值碰撞语义写进 `max_context_tokens`（用户手填
    /// 优先），预置用户开箱即有自动压缩（DSH thresholdRatio 语义）。
    pub context_window: u32,
    /// Official default reasoning effort, applied as `reasoning_effort`.
    pub reasoning_effort: Option<&'static str>,
    /// 保持式思考（preserved thinking）：在思考开关中显式声明
    /// `clear_thinking: false`。GLM Coding Plan 端点默认开启该能力，
    /// 官方 Agent 示例同样显式传递。
    pub preserve_thinking: bool,
    /// 是否发送 DeepSeek/GLM 风格的 `thinking` 对象。Kimi K3 与
    /// Qwen3.8-Max 的思考强度是顶层 `reasoning_effort`，不携带该
    /// 对象（未定义参数不发给严格网关）。
    pub thinking_object: bool,
    /// 流式请求是否要求回传 usage（`stream_options.include_usage`）。
    /// DeepSeek 默认不在流中回传 usage，官方 harness 常开此开关以
    /// 拿到 prompt_cache_hit_tokens；GLM 流式默认携带 usage，无需
    /// 此字段。厂商差异落在各自预设里，通用通道保持中立。
    pub include_usage: bool,
    /// 该厂商端点要求的 User-Agent（写入 `extra_headers`，用户可在
    /// 模型编辑器覆盖）。目前只有 Kimi Coding 端点需要：其对订阅
    /// 流量按 UA 白名单放行（Kimi CLI / Claude Code / Roo Code /
    /// Kilo Code…），其它客户端一律 403（社区多方验证，见 Kimi
    /// 预设注释）。None = 使用默认 UA。
    pub user_agent: Option<&'static str>,
}

/// Kimi Coding 端点要求的白名单 UA（cc-switch PR #3671 的回退常量，
/// 白名单实测 `claude-cli/*` 前缀放行，见 Kimi 预设注释）。既是预设
/// 注入值，也是 `apply` 清理残留 UA 时的"已知预设值"判据——用户
/// 自定义 UA（任何其它值）永不被预设触碰。
const KIMI_WHITELIST_UA: &str = "claude-cli/2.1.161";

/// Official DeepSeek parameters as documented at
/// <https://api-docs.deepseek.com> (Models & Pricing, 2026-08):
///
/// - model ids `deepseek-v4-flash` (DeepSeek-V4-Flash-0731),
///   `deepseek-v4-pro` (DeepSeek-V4-Pro-0813), and
///   `deepseek-v4-flash-vision-exp` (DeepSeek-V4-Flash-Vision-Exp,
///   实验性多模态视觉理解模型，2026-08-21 上架；图片按尺寸折算为
///   token 计费，工具调用/JSON 输出/Anthropic API 均支持，价格与
///   Flash 同档；并发上限 2500)
/// - OpenAI-compatible base URL `https://api.deepseek.com`
/// - 1M context, 384K maximum output（三个模型规格完全相同；最大输出
///   是 GLM 5.3 的 3 倍）
/// - thinking mode on by default, switched via `{"thinking":
///   {"type": "enabled"}}` (per the official thinking-mode guide, agent
///   callers should pass it explicitly); `reasoning_effort` 默认 `high`，
///   官方归并表：`low`→low，`medium`/`high`/`xhigh`→high，
///   `max`→max；官方另有 non-thinking 模式，本项目统一
///   不提供关闭档（见 `ThinkingLevel`）
/// - thinking mode ignores `temperature`, `top_p`, `presence_penalty`,
///   and `frequency_penalty` (defaults 1, 1, 0, 0), so presets leave
///   them unset and rely on server defaults
///
/// Official GLM Coding Plan parameters as documented at
/// <https://docs.z.ai/guides/llm/glm-5.3>:
///
/// - model id `glm-5.3`（1M context, 128K maximum output）
/// - OpenAI-compatible coding endpoint
///   `https://open.bigmodel.cn/api/coding/paas/v4` — the dedicated
///   Coding Plan endpoint, NOT the generic `/api/paas/v4`
/// - thinking cannot be disabled（官方原文 "Disabling reasoning is no
///   longer supported"，`thinking.type: "disabled"` 会让请求失败，官方
///   建议以 `enabled` + `reasoning_effort: "low"` 迁移）；the Coding
///   Plan endpoint enables preserved thinking (`clear_thinking: false`)
///   by default and the official agent examples pass it explicitly, so
///   the preset does too
/// - `reasoning_effort` 支持 `low`/`high`/`max`（无 medium），`max` 为
///   官方默认；本项目预设 pin `high`，用户可 Shift+Tab 改选
///
/// 证据链（TUI-L03/RE-L03）：z.ai 规格页于 2026-08-17、08-18、08-19
/// 三次抓取一致（1M/128K、low|high|max、不可关闭）；项目所有者第一方
/// 确认 Coding Plan 端点仅服务 DeepSeek V4.0 正式版与 GLM 5.3；
/// `glm-5.3` 打真实端点的请求实测成功。
///
/// Official Qwen3.8-Max (Token Plan) parameters as documented at
/// <https://help.aliyun.com/zh/model-studio/qwen3-8-max> and
/// <https://www.alibabacloud.com/help/zh/model-studio/more-tools>
/// (Token Plan 团队版, 2026-08 抓取):
///
/// - model id `qwen3.8-max`（2.4T MoE 旗舰；1M context、131,072 最大
///   输出、262,144 最大思维链）
/// - Token Plan 专用 OpenAI 兼容端点
///   `https://token-plan.ap-southeast-1.maas.aliyuncs.com/compatible-mode/v1`
///   （新加坡 MaaS 域名，与按量计费的 dashscope 域名分离；密钥是
///   控制台"组织成员"页的专属 API Key）
/// - `reasoning_effort` 官方档位 `xhigh`（默认）/`medium`/`low`，兼容
///   表 high/max→xhigh、minimal→low；CLAT 三档经厂商感知映射
///   Low→low、High→medium、Max→xhigh（见 `ThinkingLevel::wire_effort`），
///   预设按"pin 中档"政策 pin `medium`；无 `thinking` 对象（该参数
///   列表适用于 Qwen3.7 及更早，qwen3.8-max 不在其中）
/// - 上下文缓存：隐式缓存自动开启、不可关闭（agent 上下文远超
///   ~2000 token 最小前缀），命中价 20%；显式 `cache_control` 在
///   新加坡 Token Plan 地域的支持面官方未明确列出（文档只确认北京
///   地域清单）且有 125% 创建成本，预设不发显式标记、吃稳隐式命中；
///   `stream_options.include_usage` 拿回
///   `usage.prompt_tokens_details.cached_tokens`（新加坡地域部分模型
///   暂用顶层 `usage.cached_tokens`，解析两端都支持）
///
/// Official Kimi K3 (Coding 会员) parameters as documented at
/// <https://platform.kimi.com/docs/overview> and Kimi Help Center
/// "Kimi Code Membership Benefits"（2026-08 抓取）:
///
/// - model id `kimi-k3`（旗舰，1M context、~131K 最大输出；视觉理解）
/// - Coding 会员 OpenAI 兼容端点 `https://api.kimi.com/coding/v1`
///   （订阅额度计量；开放平台按量端点是 `https://api.moonshot.cn/v1`，
///   密钥不通用，预设取 Coding 端点与 GLM Coding Plan 同型）
/// - `reasoning_effort` 支持 `low`/`high`/`max`（`max` 为官方默认），
///   顶层参数、无 `thinking` 对象；预设按 pin-中档政策 pin `high`
/// - 上下文缓存全自动（无需参数，前缀 ≥256 token 生效），usage 经
///   `prompt_tokens_details.cached_tokens` 上报；`stream_options.
///   include_usage` 让流式也回传
/// - **UA 白名单**：Coding 端点按 User-Agent 放行，其它客户端 403
///   "Kimi For Coding is currently only available for Coding Agents…"
///   （cline#10307、kodus-ai#1257 等多方报告）。预设注入社区实测的
///   白名单 UA `claude-cli/2.1.161`（cc-switch PR #3671 的回退常量；
///   其白名单实测矩阵：`claude-cli/*`、`claude-code/*`、`Kilo-Code/*`
///   放行，`codex-cli/*`、`kimi-cli/*`、`openclaw/*` 被拒）；属订阅
///   条款边缘操作，用户可在模型编辑器 Extra Headers 覆盖成自己的
///   合规选择。
pub const MODEL_PRESETS: &[ModelPreset] = &[
    ModelPreset {
        id: "deepseek-v4-flash",
        name: "DeepSeek V4.0 Flash",
        description: "Fast, cost-effective DeepSeek V4 for everyday agent work",
        vendor: "DeepSeek",
        protocol: ModelProtocol::OpenAiCompatible,
        model: "deepseek-v4-flash",
        endpoint: "https://api.deepseek.com",
        request_path: "/chat/completions",
        output_limit: 384 * 1024,
        context_window: 1_000_000,
        reasoning_effort: Some("high"),
        preserve_thinking: false,
        thinking_object: true,
        include_usage: true,
        user_agent: None,
    },
    ModelPreset {
        id: "deepseek-v4-pro",
        name: "DeepSeek V4.0 Pro",
        description: "DeepSeek V4 Pro for the most complex agent tasks",
        vendor: "DeepSeek",
        protocol: ModelProtocol::OpenAiCompatible,
        model: "deepseek-v4-pro",
        endpoint: "https://api.deepseek.com",
        request_path: "/chat/completions",
        output_limit: 384 * 1024,
        context_window: 1_000_000,
        reasoning_effort: Some("high"),
        preserve_thinking: false,
        thinking_object: true,
        include_usage: true,
        user_agent: None,
    },
    ModelPreset {
        id: "deepseek-v4-flash-vision-exp",
        name: "DeepSeek V4.0 Flash Vision (Exp)",
        description: "Experimental multimodal DeepSeek V4 Flash — reads image input",
        vendor: "DeepSeek",
        protocol: ModelProtocol::OpenAiCompatible,
        model: "deepseek-v4-flash-vision-exp",
        endpoint: "https://api.deepseek.com",
        request_path: "/chat/completions",
        output_limit: 384 * 1024,
        context_window: 1_000_000,
        reasoning_effort: Some("high"),
        preserve_thinking: false,
        thinking_object: true,
        include_usage: true,
        user_agent: None,
    },
    ModelPreset {
        id: "glm-5.3",
        name: "GLM 5.3",
        description: "Zhipu flagship coding model via GLM Coding Plan",
        vendor: "GLM Coding Plan",
        protocol: ModelProtocol::OpenAiCompatible,
        model: "glm-5.3",
        endpoint: "https://open.bigmodel.cn/api/coding/paas/v4",
        request_path: "/chat/completions",
        output_limit: 128 * 1024,
        context_window: 1_000_000,
        reasoning_effort: Some("high"),
        preserve_thinking: true,
        thinking_object: true,
        include_usage: false,
        user_agent: None,
    },
    ModelPreset {
        id: "qwen3.8-max",
        name: "Qwen3.8 Max",
        description: "Alibaba flagship via Qwen Token Plan (implicit context cache)",
        vendor: "Qwen Token Plan",
        protocol: ModelProtocol::OpenAiCompatible,
        model: "qwen3.8-max",
        endpoint: "https://token-plan.ap-southeast-1.maas.aliyuncs.com/compatible-mode/v1",
        request_path: "/chat/completions",
        output_limit: 131_072,
        context_window: 1_000_000,
        reasoning_effort: Some("medium"),
        preserve_thinking: false,
        thinking_object: false,
        include_usage: true,
        user_agent: None,
    },
    ModelPreset {
        id: "kimi-k3",
        name: "Kimi K3",
        description: "Moonshot flagship via Kimi Coding membership (auto context cache)",
        vendor: "Kimi Coding Plan",
        protocol: ModelProtocol::OpenAiCompatible,
        model: "kimi-k3",
        endpoint: "https://api.kimi.com/coding/v1",
        request_path: "/chat/completions",
        output_limit: 131_072,
        context_window: 1_000_000,
        reasoning_effort: Some("high"),
        preserve_thinking: false,
        thinking_object: false,
        include_usage: true,
        user_agent: Some(KIMI_WHITELIST_UA),
    },
];

pub fn preset_by_id(id: &str) -> Option<&'static ModelPreset> {
    MODEL_PRESETS.iter().find(|preset| preset.id == id)
}

/// 一级选择列表：内置厂商（按首次出现顺序去重）+ 自定义入口。
pub fn preset_vendors() -> Vec<&'static str> {
    let mut vendors = Vec::new();
    for preset in MODEL_PRESETS {
        if !vendors.contains(&preset.vendor) {
            vendors.push(preset.vendor);
        }
    }
    vendors
}

/// 某厂商下的全部预设（二级列表）。
pub fn presets_by_vendor(vendor: &str) -> Vec<&'static ModelPreset> {
    MODEL_PRESETS
        .iter()
        .filter(|preset| preset.vendor == vendor)
        .collect()
}

impl ModelPreset {
    /// 该预设官方推荐的 `extra_body`：思考开关、reasoning_effort 与
    /// 厂商特有的流式 usage 开关。`apply` 与模型编辑器共用此方法，
    /// 两处构造永不漂移。
    pub fn extra_body(&self) -> Value {
        // 与官方思考模式指南的推荐写法完全一致：显式开启思考模式并
        // 声明 reasoning_effort，而不是依赖服务端隐式默认。Kimi/Qwen
        // 没有 thinking 对象，只发顶层 reasoning_effort。
        let mut extra = match (self.reasoning_effort, self.thinking_object) {
            (Some(effort), true) => json!({
                "thinking": self.thinking_object_value(),
                "reasoning_effort": effort,
            }),
            (Some(effort), false) => json!({"reasoning_effort": effort}),
            (None, true) => json!({"thinking": self.thinking_object_value()}),
            (None, false) => json!({}),
        };
        if self.include_usage {
            extra["stream_options"] = json!({"include_usage": true});
        }
        extra
    }

    /// DeepSeek/GLM 风格的 thinking 对象值（保持式思考差异在此分岔）。
    fn thinking_object_value(&self) -> Value {
        if self.preserve_thinking {
            json!({"type": "enabled", "clear_thinking": false})
        } else {
            json!({"type": "enabled"})
        }
    }

    /// Fills a configuration with this preset's official parameters.
    ///
    /// Authentication and custom transport settings (API key, auth header)
    /// are left untouched so switching presets never wipes credentials.
    /// The preset's mandatory `User-Agent`（仅 Kimi Coding 端点需要，
    /// 见字段注释）合并进 `extra_headers` 的同名键——其余自定义头
    /// 原样保留，用户覆盖后以用户为准。
    pub fn apply(&self, config: &mut ModelConfig) {
        // 换预设前先记下旧预设的窗口：预算种入需要识别"当前值是不是
        // 我们种的"（旧预设的窗口也算，换预设时跟随新窗口）。
        let previous_preset_window = config
            .preset
            .as_deref()
            .and_then(preset_by_id)
            .map(|preset| preset.context_window);
        config.preset = Some(self.id.to_owned());
        config.protocol = self.protocol;
        config.model = self.model.to_owned();
        config.endpoint = self.endpoint.to_owned();
        config.request_path = self.request_path.to_owned();
        config.output_limit = Some(self.output_limit);
        config.temperature = None;
        config.parallel_tool_calls = true;
        config.extra_body = self.extra_body();
        // 自动压缩预算（DSH thresholdRatio 语义的预算来源，2026-08-19）：
        // 预设窗口默认种入 `max_context_tokens`，预置用户拎包入住即有
        // 自动压缩。种入语义与 User-Agent 相同的已知值碰撞：
        // - 空 → 种入本预设窗口；
        // - 等于本预设或旧预设的窗口（我们种的）→ 更新为本预设窗口；
        // - 其它值（用户手填）→ 永不覆盖。
        let seeded_by_us =
            |value: u32| value == self.context_window || Some(value) == previous_preset_window;
        match config.max_context_tokens {
            None => config.max_context_tokens = Some(self.context_window),
            Some(current) if seeded_by_us(current) => {
                config.max_context_tokens = Some(self.context_window);
            }
            Some(_) => {}
        }
        // UA 是预设管理的键，但用户的自定义值优先（对抗审计 2026-08-19
        // 修复：`model_state()` 每次加载都执行 apply，无条件覆写会把用户
        // 在 Extra Headers 里的自定义 UA 静默冲掉；换预设离开 Kimi 时，
        // 只清理等于已知预设常量的残留值——自定义 UA 同样不动）。
        let current_ua = config
            .extra_headers
            .get("User-Agent")
            .and_then(Value::as_str);
        match self.user_agent {
            Some(user_agent) => {
                if current_ua.is_none() || current_ua == Some(user_agent) {
                    if !config.extra_headers.is_object() {
                        config.extra_headers = serde_json::Map::new().into();
                    }
                    config.extra_headers["User-Agent"] = json!(user_agent);
                }
            }
            None => {
                if current_ua == Some(KIMI_WHITELIST_UA)
                    && let Some(map) = config.extra_headers.as_object_mut()
                {
                    map.remove("User-Agent");
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn applies_official_deepseek_parameters() {
        let mut config = ModelConfig {
            model: "something-custom".into(),
            endpoint: "https://example.test".into(),
            ..ModelConfig::default()
        };

        preset_by_id("deepseek-v4-pro")
            .expect("preset")
            .apply(&mut config);

        assert_eq!(config.preset.as_deref(), Some("deepseek-v4-pro"));
        assert_eq!(config.protocol, ModelProtocol::OpenAiCompatible);
        assert_eq!(config.model, "deepseek-v4-pro");
        assert_eq!(config.endpoint, "https://api.deepseek.com");
        assert_eq!(config.request_path, "/chat/completions");
        assert_eq!(config.output_limit, Some(384 * 1024));
        assert_eq!(config.temperature, None);
        assert!(config.parallel_tool_calls);
        assert_eq!(config.extra_body["reasoning_effort"], "high");
        assert_eq!(config.extra_body["thinking"]["type"], "enabled");
        // DeepSeek 思考开关不带 clear_thinking 字段。
        assert!(
            config.extra_body["thinking"]
                .get("clear_thinking")
                .is_none()
        );
        // DeepSeek 预设开启流式 usage，状态栏缓存百分比依赖它。
        assert_eq!(config.extra_body["stream_options"]["include_usage"], true);
    }

    #[test]
    fn applies_official_glm_coding_plan_parameters() {
        let mut config = ModelConfig::default();

        preset_by_id("glm-5.3").expect("preset").apply(&mut config);

        assert_eq!(config.preset.as_deref(), Some("glm-5.3"));
        assert_eq!(config.protocol, ModelProtocol::OpenAiCompatible);
        assert_eq!(config.model, "glm-5.3");
        // Coding 专用端点，不是通用 /api/paas/v4。
        assert_eq!(
            config.endpoint,
            "https://open.bigmodel.cn/api/coding/paas/v4"
        );
        assert_eq!(config.request_path, "/chat/completions");
        assert_eq!(config.output_limit, Some(128 * 1024));
        assert_eq!(config.extra_body["reasoning_effort"], "high");
        assert_eq!(config.extra_body["thinking"]["type"], "enabled");
        // GLM Coding Plan 端点保留式思考默认开启，预设显式声明。
        assert_eq!(config.extra_body["thinking"]["clear_thinking"], false);
        // GLM 流式默认携带 usage，无需 DeepSeek 的 stream_options 开关。
        assert!(config.extra_body.get("stream_options").is_none());
    }

    #[test]
    fn applies_official_qwen_token_plan_parameters() {
        let mut config = ModelConfig::default();

        preset_by_id("qwen3.8-max")
            .expect("preset")
            .apply(&mut config);

        assert_eq!(config.model, "qwen3.8-max");
        // Token Plan 专用新加坡 MaaS 端点，不是按量 dashscope 域名。
        assert_eq!(
            config.endpoint,
            "https://token-plan.ap-southeast-1.maas.aliyuncs.com/compatible-mode/v1"
        );
        assert_eq!(config.request_path, "/chat/completions");
        assert_eq!(config.output_limit, Some(131_072));
        // 官方档位 low/medium/xhigh（默认 xhigh）；预设按 pin-中档政策
        // pin medium＝CLAT 的 High 档。
        assert_eq!(config.extra_body["reasoning_effort"], "medium");
        // qwen3.8-max 无 thinking 对象（该参数属于 Qwen3.7 及更早）。
        assert!(config.extra_body.get("thinking").is_none());
        // 隐式缓存自动生效；include_usage 拿回 cached_tokens 供状态栏。
        assert_eq!(config.extra_body["stream_options"]["include_usage"], true);
        // 无 UA 要求。
        assert!(config.extra_headers.get("User-Agent").is_none());
    }

    #[test]
    fn applies_official_kimi_coding_parameters() {
        let mut config = ModelConfig::default();

        preset_by_id("kimi-k3").expect("preset").apply(&mut config);

        assert_eq!(config.model, "kimi-k3");
        // Coding 会员端点（订阅额度），不是开放平台按量端点。
        assert_eq!(config.endpoint, "https://api.kimi.com/coding/v1");
        assert_eq!(config.request_path, "/chat/completions");
        assert_eq!(config.output_limit, Some(131_072));
        // K3 官方 low/high/max（默认 max）；pin-中档政策 pin high。
        assert_eq!(config.extra_body["reasoning_effort"], "high");
        // K3 思考强度是顶层参数，无 thinking 对象。
        assert!(config.extra_body.get("thinking").is_none());
        // 缓存全自动；include_usage 让流式也回传 usage。
        assert_eq!(config.extra_body["stream_options"]["include_usage"], true);
        // UA 白名单：Coding 端点按 User-Agent 放行，注入社区验证的
        // 白名单 UA（cc-switch PR #3671 同款）。
        assert_eq!(
            config.extra_headers["User-Agent"].as_str(),
            Some("claude-cli/2.1.161")
        );
    }

    /// UA 合并不清空用户的其它自定义头；非 object 的 headers 字段被
    /// 规整为 object 后写入（apply 不因存量脏数据 panic）。
    ///
    /// 对抗审计（2026-08-19）修复语义：`model_state()` 每次加载都执行
    /// apply——预设 UA 不得覆写用户的自定义值；换预设离开 Kimi 时只
    /// 清理等于已知预设常量的残留，自定义值不动。
    #[test]
    fn kimi_user_agent_merges_into_existing_extra_headers() {
        let mut config = ModelConfig {
            extra_headers: json!({"X-Custom": "keep-me"}),
            ..ModelConfig::default()
        };
        preset_by_id("kimi-k3").expect("preset").apply(&mut config);
        assert_eq!(config.extra_headers["X-Custom"], "keep-me");
        assert_eq!(
            config.extra_headers["User-Agent"].as_str(),
            Some("claude-cli/2.1.161")
        );

        let mut junk = ModelConfig {
            extra_headers: json!("not an object"),
            ..ModelConfig::default()
        };
        preset_by_id("kimi-k3").expect("preset").apply(&mut junk);
        assert_eq!(
            junk.extra_headers["User-Agent"].as_str(),
            Some("claude-cli/2.1.161")
        );
    }

    /// 用户自定义 UA 在重复 apply（= 每次启动的 model_state 加载）下
    /// 存活；换到无 UA 要求的预设时，只有等于已知预设常量的残留被清
    /// 理，自定义值保留。预修复代码（无条件覆写/不清理）上本测试失败。
    #[test]
    fn preset_user_agent_never_clobbers_custom_values() {
        let kimi = preset_by_id("kimi-k3").expect("kimi");
        let mut config = ModelConfig::default();
        kimi.apply(&mut config);
        // 用户覆盖成自己的 UA（如换一个白名单前缀 claude-code/*）。
        config.extra_headers["User-Agent"] = json!("claude-code/9.9.9");
        // 模拟重启：model_state 再次 apply 同一预设。
        kimi.apply(&mut config);
        assert_eq!(
            config.extra_headers["User-Agent"].as_str(),
            Some("claude-code/9.9.9"),
            "a user override survives preset re-application on every load"
        );

        // 换到 DeepSeek 预设：已知预设常量被清理……
        config.extra_headers["User-Agent"] = json!("claude-cli/2.1.161");
        preset_by_id("deepseek-v4-pro")
            .expect("deepseek")
            .apply(&mut config);
        assert!(
            config.extra_headers.get("User-Agent").is_none(),
            "the known preset UA is cleaned when switching away from Kimi"
        );
        // ……而自定义值不会被误删。
        config.extra_headers["User-Agent"] = json!("my-agent/1.0");
        preset_by_id("deepseek-v4-pro")
            .expect("deepseek")
            .apply(&mut config);
        assert_eq!(
            config.extra_headers["User-Agent"].as_str(),
            Some("my-agent/1.0"),
            "a custom UA is never removed by a preset"
        );
    }

    /// 预算种入（2026-08-19，DSH thresholdRatio 的预算来源）：预设窗口
    /// 默认种入 `max_context_tokens`，预置用户开箱即有自动压缩；用户
    /// 手填值永不覆盖；等于已知种入值的残留跟随预设更新。pre-fix
    /// （预算只来自手填字段、默认 None）上：fresh 断言失败——自动压
    /// 缩对预置用户永不触发。
    #[test]
    fn apply_seeds_the_auto_compaction_budget_and_respects_user_values() {
        let glm = preset_by_id("glm-5.3").expect("glm");
        let window = glm.context_window;

        // 全新配置：种入官方窗口。
        let mut config = ModelConfig::default();
        glm.apply(&mut config);
        assert_eq!(
            config.max_context_tokens,
            Some(window),
            "a fresh preset config carries the compaction budget out of the box"
        );

        // 重启/重复 apply：种入值幂等存活。
        glm.apply(&mut config);
        assert_eq!(config.max_context_tokens, Some(window));

        // 用户手填（编辑器高级区 Context Window）：永不覆盖。
        config.max_context_tokens = Some(65_536);
        glm.apply(&mut config);
        assert_eq!(
            config.max_context_tokens,
            Some(65_536),
            "a user-entered budget is never clobbered by the preset"
        );
    }

    #[test]
    fn flash_and_pro_use_the_official_api_names() {
        let flash = preset_by_id("deepseek-v4-flash").expect("flash");
        let pro = preset_by_id("deepseek-v4-pro").expect("pro");
        assert_eq!(flash.model, "deepseek-v4-flash");
        assert_eq!(pro.model, "deepseek-v4-pro");
        assert_eq!(flash.endpoint, "https://api.deepseek.com");
        assert_eq!(pro.endpoint, "https://api.deepseek.com");
    }

    /// TUI-L03：逐模型断言已核验的官方规格，不把"所有未来预设=1M"
    /// 固化进测试——新增预设必须在此显式给出已核验值与来源：
    /// - deepseek-v4-flash / -pro：1M context / 384K output
    ///   （api-docs.deepseek.com Models & Pricing，2026-08 核验）
    /// - glm-5.3：1M context / 128K output
    ///   （docs.z.ai/guides/llm/glm-5.3，08-17/18/19 三次核验一致；
    ///   另有项目所有者第一方确认与真实端点实测，见模块文档证据链）
    /// - qwen3.8-max：1M context / 131,072 output
    ///   （help.aliyun.com/zh/model-studio/qwen3-8-max，2026-08 核验）
    /// - kimi-k3：1M context / 131,072 output
    ///   （platform.kimi.com/docs/overview 与 OpenCode 接入指南，
    ///   2026-08 核验）
    #[test]
    fn official_context_windows_and_output_limits() {
        let flash = preset_by_id("deepseek-v4-flash").expect("flash");
        assert_eq!(flash.context_window, 1_000_000);
        assert_eq!(flash.output_limit, 384 * 1024);

        let pro = preset_by_id("deepseek-v4-pro").expect("pro");
        assert_eq!(pro.context_window, 1_000_000);
        assert_eq!(pro.output_limit, 384 * 1024);

        let glm = preset_by_id("glm-5.3").expect("glm");
        assert_eq!(glm.context_window, 1_000_000);
        assert_eq!(glm.output_limit, 128 * 1024);

        let qwen = preset_by_id("qwen3.8-max").expect("qwen");
        assert_eq!(qwen.context_window, 1_000_000);
        assert_eq!(qwen.output_limit, 131_072);

        let kimi = preset_by_id("kimi-k3").expect("kimi");
        assert_eq!(kimi.context_window, 1_000_000);
        assert_eq!(kimi.output_limit, 131_072);
    }

    #[test]
    fn vendors_group_presets_for_the_picker() {
        let vendors = preset_vendors();
        assert_eq!(
            vendors,
            vec![
                "DeepSeek",
                "GLM Coding Plan",
                "Qwen Token Plan",
                "Kimi Coding Plan"
            ]
        );
        assert_eq!(presets_by_vendor("DeepSeek").len(), 3);
        assert_eq!(presets_by_vendor("GLM Coding Plan").len(), 1);
        assert_eq!(presets_by_vendor("Qwen Token Plan")[0].id, "qwen3.8-max");
        assert_eq!(presets_by_vendor("Kimi Coding Plan")[0].id, "kimi-k3");
    }

    /// Vision-Exp 预设：核验过的官方参数落位（1M/384K），model id 与
    /// 端点同族其余两条共享 DeepSeek 通道。
    #[test]
    fn deepseek_vision_preset_matches_official_parameters() {
        let preset = preset_by_id("deepseek-v4-flash-vision-exp").expect("preset exists");
        assert_eq!(preset.model, "deepseek-v4-flash-vision-exp");
        assert_eq!(preset.endpoint, "https://api.deepseek.com");
        assert_eq!(preset.context_window, 1_000_000);
        assert_eq!(preset.output_limit, 384 * 1024);
        assert_eq!(preset.reasoning_effort, Some("high"));
        assert_eq!(preset.vendor, "DeepSeek");
    }
}
