//! Deliberately narrower than DSH wording heuristics: only explicit machine codes
//! recover — plus the one live-verified vendor template that carries no machine
//! code (Kimi's plan-tier context-limit 403, 2026-11-21; see
//! `plan_context_limit`).
use crate::model::ModelErrorKind;
use serde_json::Value;
pub(super) fn from_body(body: &str, fallback: ModelErrorKind) -> ModelErrorKind {
    serde_json::from_str(body).map_or(fallback, |value| from_value(&value, fallback))
}

/// 从错误正文提取档位上下文上限（token）。给 ModelError 的
/// `context_limit` 附着用——容量恢复靠它收紧压缩预算窗口
/// （2026-11-21 Kimi Plus 档 256K live 实测：预设 1M 窗的压缩目标
/// 对档位仍超限，“撞墙-压缩-又撞墙”）。
pub(super) fn plan_limit_from_body(body: &str) -> Option<u64> {
    serde_json::from_str(body).ok().and_then(|value| plan_context_limit(&value))
}
pub(super) fn from_value(value: &Value, fallback: ModelErrorKind) -> ModelErrorKind {
    // 档位限制例外（2026-11-21 Kimi K3.1 live 实测）：Moonshot 用
    // 403 permission_denied + 固定 message 模板（无机器码）表达
    // 上下文档位上限（Plus 档 256K，高级档 1M），误归 Authentication
    // 会永远不可恢复。它是可恢复的容量问题——归 ContextCapacity
    // 走既有压缩恢复；其余短路（RateLimited/Transport）与机器码
    // 纪律不放松。
    if fallback == ModelErrorKind::Authentication
        && plan_context_limit(value).is_some()
    {
        return ModelErrorKind::ContextCapacity;
    }
    if matches!(
        fallback,
        ModelErrorKind::Authentication | ModelErrorKind::RateLimited | ModelErrorKind::Transport
    ) {
        return fallback;
    }
    match value.pointer("/error/code").and_then(Value::as_str) {
        Some(
            "context_length_exceeded"
            | "context_window_exceeded"
            | "CONTEXT_WINDOW_EXCEEDED"
            | "max_context_length_exceeded",
        ) => ModelErrorKind::ContextCapacity,
        Some(
            "too_many_images"
            | "image_size_exceeded"
            | "image_token_limit_exceeded"
            | "image_limit_exceeded",
        ) => ModelErrorKind::ImageCapacity,
        _ => fallback,
    }
}

/// Kimi 档位上下文限制模板识别与上限提取：`/error/message` 同时含
/// "supports only" 与 "context"（固定模板 "Your current plan supports
/// only <model> up to <N> context…"，2026-11-21 live 抓取），并从首个
/// "up to <N>[KM] context" 提取 token 上限（256K → 262,144）。双锚点
/// 精确匹配固定模板，非宽泛措辞启发——其它 403 文本不同时命中这
/// 两个模板片段；模板后半句的 "1M context is available" 不会被误取
/// （锚定首个 "up to "）。
fn plan_context_limit(value: &Value) -> Option<u64> {
    let message = value.pointer("/error/message").and_then(Value::as_str)?;
    if !(message.contains("supports only") && message.contains("context")) {
        return None;
    }
    let start = message.find("up to ")? + "up to ".len();
    let rest = &message[start..];
    let digits = rest
        .chars()
        .take_while(char::is_ascii_digit)
        .count();
    if digits == 0 {
        return None;
    }
    let number: u64 = rest[..digits].parse().ok()?;
    let tail = &rest[digits..];
    let multiplier = if tail.starts_with("K ") {
        1024
    } else if tail.starts_with("M ") {
        1024 * 1024
    } else if tail.starts_with(' ') {
        1
    } else {
        return None;
    };
    Some(number.saturating_mul(multiplier))
}
#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn phase8_capacity_classification_requires_machine_code_and_preserves_auth() {
        let actual = json!({"error":{"code":"context_length_exceeded"}});
        assert_eq!(
            from_value(&actual, ModelErrorKind::Client),
            ModelErrorKind::ContextCapacity
        );
        assert_eq!(
            from_value(&actual, ModelErrorKind::Authentication),
            ModelErrorKind::Authentication
        );
        for value in [
            json!({"error":{"message":"maximum context length exceeded"}}),
            json!({"error":{"code":"invalid_request_error"}}),
            json!({"code":"context_length_exceeded"}),
        ] {
            assert_eq!(
                from_value(&value, ModelErrorKind::Client),
                ModelErrorKind::Client
            );
        }
    }

    /// 2026-11-21 Kimi K3.1 live 抓取形状：403 permission_denied +
    /// 固定 message 模板（无机器码）表达档位上下文上限（Plus 档
    /// 256K，高级档 1M）。例外穿透 Authentication 短路归
    /// ContextCapacity，接入既有压缩恢复（压缩到档位上限以下重试
    /// 一次）；其它 403 文本不同时命中双锚点，仍短路为
    /// Authentication。预修复上本测试红。
    #[test]
    fn plan_context_limit_403_recovers_as_context_capacity() {
        let body = json!({
            "error": {
                "message": "Your current plan supports only k3.1-pro up to 256K context. 1M context is available on higher-tier Kimi Code plans. Upgrade: https://www.kimi.com/code?from=server_k31-pro_error#pricing",
                "type": "permission_denied"
            }
        });
        assert_eq!(
            from_value(&body, ModelErrorKind::Authentication),
            ModelErrorKind::ContextCapacity
        );
        // 上限提取：256K → 262,144；后续 "1M context is available"
        // 不被误取（锚定首个 "up to "）。
        assert_eq!(plan_context_limit(&body), Some(262_144));
        let megabyte = json!({
            "error": {"message": "Your current plan supports only k3.1-pro up to 1M context. Upgrade later."}
        });
        assert_eq!(
            plan_limit_from_body(&serde_json::to_string(&megabyte).unwrap()),
            Some(1_048_576)
        );
        // 非模板形状的 403 文本仍短路：凭据吊销不含 "supports only"；
        // 计费文案只含 "supports" 不含 "supports only" 也不含
        // "context"。
        let revoked = json!({"error":{"message":"API key revoked","type":"permission_denied"}});
        assert_eq!(
            from_value(&revoked, ModelErrorKind::Authentication),
            ModelErrorKind::Authentication
        );
        let billing = json!({"error":{"message":"insufficient quota, supports monthly billing"}});
        assert_eq!(
            from_value(&billing, ModelErrorKind::Authentication),
            ModelErrorKind::Authentication
        );
    }
}
