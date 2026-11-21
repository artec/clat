//! One pressure policy; the measured input never includes the future output twice.
use super::{ModelConfig, ModelItem};
use crate::tool::ToolDefinition;
use std::collections::HashMap;
use std::sync::{LazyLock, RwLock};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct ContextBudget {
    pub threshold: u64,
    pub capacity: u64,
    pub retain: u64,
    pub output: u64,
    pub headroom: u64,
}
impl ContextBudget {
    pub fn resolve(config: &ModelConfig) -> Result<Option<Self>, String> {
        Self::resolve_with_limit(config, None)
    }

    /// 档位上限发现后的预算：窗口取配置与发现上限的较小者。
    /// 背景（2026-11-21 Kimi Plus 档 live 实测）：档位 403 报告实际
    /// 上限（256K），预设 1M 窗下 retain≈139K 的压缩目标对 256K 档
    /// 仍超限——"撞墙-压缩-又撞墙"。上限只收紧不放宽（更大的
    /// 报告值不放大配置窗口）。
    pub fn resolve_with_limit(
        config: &ModelConfig,
        discovered_limit: Option<u64>,
    ) -> Result<Option<Self>, String> {
        let mut clamped = config.clone();
        if let Some(limit) = discovered_limit
            && let Ok(small) = u32::try_from(limit)
            && let Some(window) = clamped.max_context_tokens
            && small < window
        {
            clamped.max_context_tokens = Some(small);
        }
        let config = &clamped;
        let Some(window) = config.max_context_tokens.map(u64::from) else {
            return Ok(None);
        };
        let output = u64::from(config.output_limit.unwrap_or(4096));
        let messages = window.checked_sub(output).ok_or(format!(
            "invalid context capacity: output reserve ({output}) exceeds the model window ({window})"
        ))?;
        let headroom = (messages / 10).clamp(512, 65536);
        let threshold = (window * 4 / 5).min(messages.saturating_sub(headroom));
        let retain = messages * 16 / 100;
        if threshold == 0 || retain >= threshold {
            // 带上实际数值：陈旧持久化配置（如旧预设种下的 output_limit）
            // 与当前预设表的分歧一眼可见（OC-16 真机诊断教训）。
            return Err(format!(
                "invalid context capacity (window {window}, output reserve {output}): reduce \
                 output reserve or increase model window"
            ));
        }
        Ok(Some(Self {
            threshold,
            capacity: messages,
            retain,
            output,
            headroom,
        }))
    }
}

// ─── 全局档位上限缓存 ──────────────────────────────────────────────
// 背景（2026-11-21 Kimi Plus 档 live）：ActiveContext 的 discovered_limit
// 是 per-run 的——每条用户消息新建实例，发现即丢，导致主动压缩阈值
// 永远按 1M 预设算（800K），每条消息都撞墙-恢复。全局缓存修复：
// 一次发现终身生效（同 endpoint+model），磁盘持久化（
// ~/.clat/tier_limits.json），升级档位后删文件即可重置。
static TIER_LIMITS: LazyLock<RwLock<HashMap<String, u64>>> =
    LazyLock::new(|| RwLock::new(load_tier_limits()));

fn tier_limit_key(endpoint: &str, model: &str) -> String {
    format!("{endpoint}|{model}")
}

fn tier_limits_path() -> std::path::PathBuf {
    std::path::PathBuf::from(
        std::env::var("HOME").unwrap_or_default(),
    )
    .join(".clat")
    .join("tier_limits.json")
}

fn load_tier_limits() -> HashMap<String, u64> {
    std::fs::read_to_string(tier_limits_path())
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default()
}

/// 记住档位上限：写内存 + 持久化磁盘。只收紧（更小的值覆盖更大的
/// 值）；磁盘写失败静默降级为仅内存。
pub(crate) fn remember_tier_limit(endpoint: &str, model: &str, limit: u64) {
    let key = tier_limit_key(endpoint, model);
    let mut map = TIER_LIMITS.write().unwrap();
    if map.get(&key).is_some_and(|&prev| limit >= prev) {
        return;
    }
    map.insert(key, limit);
    if let Ok(json) = serde_json::to_string_pretty(&*map) {
        let _ = std::fs::write(tier_limits_path(), json);
    }
}

/// 查档位上限：内存缓存（首次访问时已从磁盘惰性加载）。
pub(crate) fn lookup_tier_limit(endpoint: &str, model: &str) -> Option<u64> {
    TIER_LIMITS
        .read()
        .unwrap()
        .get(&tier_limit_key(endpoint, model))
        .copied()
}

/// 按模型名查档位上限（任意端点下的匹配键，取最小；UI 显示用——
/// 快照 JSON 不携带端点字段）。同一模型名跨端点的场景极少，
/// 取最小值是安全侧（显示收紧的窗口）。
pub fn lookup_tier_limit_by_model(model: &str) -> Option<u64> {
    let suffix = format!("|{model}");
    TIER_LIMITS
        .read()
        .unwrap()
        .iter()
        .filter(|(key, _)| key.ends_with(&suffix))
        .map(|(_, &limit)| limit)
        .min()
}

#[derive(Clone, Debug)]
pub(crate) struct RequestMeasurement {
    pub input: u64,
    pub output: u64,
}
impl RequestMeasurement {
    pub fn new(
        config: &ModelConfig,
        instructions: Option<&str>,
        items: &[ModelItem],
        tools: &[ToolDefinition],
    ) -> Self {
        Self {
            input: super::estimate_request_tokens(instructions, items, tools),
            output: u64::from(config.output_limit.unwrap_or(4096)),
        }
    }
    pub fn total(&self) -> u64 {
        self.input.saturating_add(self.output)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn phase8_budget_scales_and_rejects_invalid_capacity() {
        let mut config = ModelConfig {
            max_context_tokens: Some(4096),
            output_limit: Some(1024),
            ..Default::default()
        };
        let budget = ContextBudget::resolve(&config).unwrap().unwrap();
        assert_eq!(
            (budget.threshold, budget.retain, budget.headroom),
            (2560, 491, 512)
        );
        config.output_limit = Some(4096);
        assert!(ContextBudget::resolve(&config).is_err());
        config.max_context_tokens = None;
        assert!(ContextBudget::resolve(&config).unwrap().is_none());
    }

    /// 2026-11-21 Kimi Plus 档（256K）live 实测：档位 403 报告实际上限，
    /// 预算必须按它收紧——1M 预设窗的 retain≈139K 对 256K 档仍超限
    /// （"撞墙-压缩-又撞墙"）。上限只收紧不放大。预修复（忽略上限）
    /// 上本测试红。
    #[test]
    fn discovered_plan_limit_tightens_the_budget_window() {
        let config = ModelConfig {
            max_context_tokens: Some(1_000_000),
            output_limit: Some(131_072),
            ..Default::default()
        };
        let full = ContextBudget::resolve(&config).unwrap().unwrap();
        assert_eq!((full.threshold, full.retain), (800_000, 139_028));
        let tier = ContextBudget::resolve_with_limit(&config, Some(262_144))
            .unwrap()
            .unwrap();
        assert_eq!((tier.threshold, tier.retain), (117_965, 20_971));
        // 上限只收紧：更大的报告值（高级档 1M）不放大已收紧的窗口。
        let small = ModelConfig {
            max_context_tokens: Some(262_144),
            output_limit: Some(131_072),
            ..Default::default()
        };
        let same = ContextBudget::resolve_with_limit(&small, Some(1_048_576))
            .unwrap()
            .unwrap();
        assert_eq!((same.threshold, same.retain), (117_965, 20_971));
    }

    /// 全局档位缓存：per-run 的 discovered_limit 每条消息重置，没有缓存时
    /// 每条消息都撞墙-恢复。缓存按 endpoint|model 键控（切模型不串）；
    /// 只收紧不放宽。预修复（无缓存）上本测试红。
    #[test]
    fn tier_limit_cache_is_per_model_and_only_tightens() {
        let (ep, model) = ("test://tier-cache", "test-model");
        // 幂等：上次运行可能把该键收紧并持久化到磁盘，惰性加载回来
        // 后“只收紧不放宽”会拒绝首欢写入——先清键两来干净状态。
        TIER_LIMITS.write().unwrap().remove(&tier_limit_key(ep, model));
        // 基本存取
        remember_tier_limit(ep, model, 262_144);
        assert_eq!(lookup_tier_limit(ep, model), Some(262_144));
        // 不同端点/模型不串
        assert_eq!(lookup_tier_limit("test://other", model), None);
        assert_eq!(lookup_tier_limit(ep, "other-model"), None);
        // 更大的报告值不放宽（只收紧）
        remember_tier_limit(ep, model, 1_048_576);
        assert_eq!(lookup_tier_limit(ep, model), Some(262_144));
        // 更小的值收紧
        remember_tier_limit(ep, model, 131_072);
        assert_eq!(lookup_tier_limit(ep, model), Some(131_072));
    }
}
