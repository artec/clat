//! 工具结果预裁剪（能力批次 1 / C）。
//!
//! 经 `ToolResultTransformer` 接缝注册：Run 构造最终 `ToolResult`（成功、
//! 工具错误、权限拒绝三种路径）之后、持久化与 `ToolFinished` 之前，超长
//! 输出被替换为带 head/tail 的截断视图。参数取 DSH tool-result-pruner
//! 调优值：阈值 8192 chars，head 4096，tail 1024，错误摘要至多 2048。

use super::services::{TOOL_PIPELINE_SERVICE, TOOL_PIPELINE_SERVICE_ID};
use crate::plugin::{
    DisposeError, Plugin, PluginContext, PluginDescriptor, PluginError, PluginId, ScopeKind,
    ServiceId,
};
use crate::tool::{ToolResult, ToolResultTransformer};
use serde_json::{Value, json};
use std::sync::Arc;

const ID: PluginId = PluginId::new("builtin.tool_result_pruner");
const REQUIRES: &[ServiceId] = &[TOOL_PIPELINE_SERVICE_ID];
const DESCRIPTOR: PluginDescriptor = PluginDescriptor {
    id: ID,
    scope: ScopeKind::TrustedProject,
    provides: &[],
    requires: REQUIRES,
    optional: &[],
};

const THRESHOLD_CHARS: usize = 8192;
const HEAD_CHARS: usize = 4096;
const TAIL_CHARS: usize = 1024;
const ERROR_SUMMARY_CHARS: usize = 2048;

pub(crate) struct ToolResultPrunerPlugin;

impl Plugin for ToolResultPrunerPlugin {
    fn descriptor(&self) -> &'static PluginDescriptor {
        &DESCRIPTOR
    }

    fn mount(&self, context: &mut PluginContext<'_>) -> Result<(), PluginError> {
        let pipeline = context
            .require(TOOL_PIPELINE_SERVICE)
            .map_err(|error| PluginError::new(error.to_string()))?;
        let lease = pipeline
            .register_result_transformer(context.owner(), Arc::new(ResultPruner))
            .map_err(|error| PluginError::new(error.to_string()))?;
        context.defer(move || {
            lease
                .revoke()
                .map_err(|error| DisposeError::new(error.to_string()))
        });
        Ok(())
    }
}

/// 阈值裁剪器本体。pub(crate) 供 Run 级集成测试复用同一实现。
pub(crate) struct ResultPruner;

impl ToolResultTransformer for ResultPruner {
    fn transform_result(&self, result: &mut ToolResult) {
        // 序列化失败（非 JSON 不可达，output 本身是 Value）时按原样保留。
        let Ok(serialized) = serde_json::to_string(&result.output) else {
            return;
        };
        let total = serialized.chars().count();
        if total <= THRESHOLD_CHARS {
            return;
        }
        let chars: Vec<char> = serialized.chars().collect();
        let head: String = chars[..HEAD_CHARS.min(chars.len())].iter().collect();
        let tail_start = total.saturating_sub(TAIL_CHARS).max(HEAD_CHARS.min(total));
        let tail: String = chars[tail_start.min(total)..].iter().collect();
        let omitted = total - head.chars().count() - tail.chars().count();
        let mut replacement = json!({
            "clat_truncated": true,
            "head": head,
            "tail": tail,
            "omitted_chars": omitted,
        });
        // INV-P2：is_error 结果的错误信息不依赖它恰好落在 head——顶层
        // 保留可读摘要。
        if result.is_error
            && let Some(message) = error_message(&result.output)
        {
            replacement["error"] = Value::String(truncate_chars(&message, ERROR_SUMMARY_CHARS));
        }
        // CB1-13：固定次数、严格有进展地依次收缩可选文本字段。不能用
        // `head >= 64` 的开放 while：若 error/tail 自身已经超预算，head
        // 到达下限后 replacement 不再变化，会把整个 Run 卡成无限循环。
        for field in ["head", "tail", "error"] {
            shrink_string_field_to_fit(&mut replacement, field, THRESHOLD_CHARS);
            if serialized_chars(&replacement).is_some_and(|len| len <= THRESHOLD_CHARS) {
                break;
            }
        }
        let kept = replacement["head"]
            .as_str()
            .map(|text| text.chars().count())
            .unwrap_or(0)
            + replacement["tail"]
                .as_str()
                .map(|text| text.chars().count())
                .unwrap_or(0);
        replacement["omitted_chars"] = json!(total.saturating_sub(kept));
        // 元数据或未来字段即使发生意外膨胀，也必须有一个确定、很小的
        // fallback；转换器不得阻塞或输出超过契约上限。
        if serialized_chars(&replacement).is_none_or(|len| len > THRESHOLD_CHARS) {
            replacement = json!({
                "clat_truncated": true,
                "omitted_chars": total,
                "error": "tool result exceeded CLAT's persisted output limit",
            });
        }
        result.output = replacement;
    }
}

fn serialized_chars(value: &Value) -> Option<usize> {
    serde_json::to_string(value)
        .ok()
        .map(|text| text.chars().count())
}

/// 保留字段的最长前缀，使整个 replacement 满足硬上限。二分最多
/// `log2(chars)` 次且每次边界严格收缩；其他字段单独超限时会把本字段
/// 降到空串，再由下一个字段继续承担收缩。
fn shrink_string_field_to_fit(replacement: &mut Value, field: &str, limit: usize) {
    if serialized_chars(replacement).is_some_and(|len| len <= limit) {
        return;
    }
    let Some(original) = replacement
        .get(field)
        .and_then(Value::as_str)
        .map(str::to_owned)
    else {
        return;
    };
    let chars: Vec<char> = original.chars().collect();
    let mut low = 0usize;
    let mut high = chars.len();
    let mut best = None;
    while low <= high {
        let mid = low + (high - low) / 2;
        replacement[field] = Value::String(chars[..mid].iter().collect());
        if serialized_chars(replacement).is_some_and(|len| len <= limit) {
            best = Some(mid);
            low = mid.saturating_add(1);
        } else if mid == 0 {
            break;
        } else {
            high = mid - 1;
        }
    }
    let keep = best.unwrap_or(0);
    replacement[field] = Value::String(chars[..keep].iter().collect());
}

fn error_message(output: &Value) -> Option<String> {
    output
        .get("error")
        .and_then(Value::as_str)
        .map(str::to_owned)
}

fn truncate_chars(text: &str, limit: usize) -> String {
    if text.chars().count() <= limit {
        return text.to_owned();
    }
    text.chars().take(limit).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugin::PluginManager;
    use crate::plugins::ToolPipelinePlugin;
    use crate::tool::ToolExecutionPipeline;

    fn pipeline_with_pruner() -> (Arc<ToolExecutionPipeline>, PluginManager) {
        let mut manager = PluginManager::root(ScopeKind::TrustedProject);
        manager
            .mount_all(vec![
                Arc::new(ToolPipelinePlugin),
                Arc::new(ToolResultPrunerPlugin),
            ])
            .expect("mount");
        let pipeline = manager.require(TOOL_PIPELINE_SERVICE).expect("pipeline");
        (pipeline, manager)
    }

    fn result_with_output_len(chars: usize, is_error: bool) -> ToolResult {
        let text = "x".repeat(chars);
        ToolResult {
            call_id: "call-1".into(),
            tool_name: "run_command".into(),
            output: json!({ "stdout": text }),
            is_error,
        }
    }

    fn serialized_len(result: &ToolResult) -> usize {
        serde_json::to_string(&result.output)
            .expect("serialize")
            .chars()
            .count()
    }

    #[test]
    fn outputs_at_or_below_the_threshold_pass_through_untouched() {
        let (pipeline, mut manager) = pipeline_with_pruner();
        let baseline = result_with_output_len(1, false);
        let baseline_len = serialized_len(&baseline);
        assert!(baseline_len <= THRESHOLD_CHARS);
        let mut result = baseline.clone();
        pipeline.transform_result(&mut result);
        assert_eq!(
            result.output, baseline.output,
            "INV-P3: threshold untouched"
        );
        manager.close().expect("close");
    }

    /// CB1-13：转义密集内容（大量引号/反斜杠）在二次编码后也不得
    /// 超过阈值；边界 8192/8193 精确成立。
    #[test]
    fn escape_heavy_results_stay_under_the_hard_cap_after_reencoding() {
        let (pipeline, mut manager) = pipeline_with_pruner();
        let mut result = ToolResult {
            call_id: "call-1".into(),
            tool_name: "run_command".into(),
            output: json!({ "stdout": "\"\\".repeat(12_000) }),
            is_error: false,
        };
        pipeline.transform_result(&mut result);
        assert!(serialized_len(&result) <= THRESHOLD_CHARS);
        // 精确边界：恰好 8192 通过、8193 被裁——先量出包装开销再定内容长度。
        let probe = ToolResult {
            call_id: "call-1".into(),
            tool_name: "run_command".into(),
            output: json!({ "stdout": "" }),
            is_error: false,
        };
        let overhead = serialized_len(&probe);
        let mut at_threshold = ToolResult {
            call_id: "call-1".into(),
            tool_name: "run_command".into(),
            output: json!({ "stdout": "a".repeat(THRESHOLD_CHARS - overhead) }),
            is_error: false,
        };
        assert_eq!(serialized_len(&at_threshold), THRESHOLD_CHARS);
        pipeline.transform_result(&mut at_threshold);
        assert_eq!(
            serialized_len(&at_threshold),
            THRESHOLD_CHARS,
            "exactly at the threshold must pass through"
        );
        let mut over = ToolResult {
            call_id: "call-1".into(),
            tool_name: "run_command".into(),
            output: json!({ "stdout": "a".repeat(THRESHOLD_CHARS - overhead + 1) }),
            is_error: false,
        };
        assert_eq!(serialized_len(&over), THRESHOLD_CHARS + 1);
        pipeline.transform_result(&mut over);
        assert!(serialized_len(&over) <= THRESHOLD_CHARS);
        manager.close().expect("close");
    }

    #[test]
    fn control_character_error_cannot_stall_the_pruner() {
        let (pipeline, mut manager) = pipeline_with_pruner();
        let mut result = ToolResult {
            call_id: "call-control".into(),
            tool_name: "run_command".into(),
            output: json!({ "error": "\0".repeat(9_000) }),
            is_error: true,
        };
        pipeline.transform_result(&mut result);
        assert!(serialized_len(&result) <= THRESHOLD_CHARS);
        assert_eq!(result.output["clat_truncated"], json!(true));
        manager.close().expect("close");
    }

    #[test]
    fn oversized_output_is_replaced_with_head_tail_and_omitted_count() {
        let (pipeline, mut manager) = pipeline_with_pruner();
        // 序列化总长 = JSON 包装 + 字符串本身；用足够长的内容确保超过阈值。
        let mut result = result_with_output_len(THRESHOLD_CHARS, false);
        let total = serialized_len(&result);
        assert!(total > THRESHOLD_CHARS);
        pipeline.transform_result(&mut result);
        let output = &result.output;
        assert_eq!(output["clat_truncated"], json!(true));
        assert_eq!(
            output["head"].as_str().expect("head").chars().count(),
            HEAD_CHARS
        );
        assert_eq!(
            output["tail"].as_str().expect("tail").chars().count(),
            TAIL_CHARS
        );
        assert_eq!(
            output["omitted_chars"].as_u64().expect("omitted") as usize,
            total - HEAD_CHARS - TAIL_CHARS
        );
        assert!(!result.is_error, "pruning never flips is_error");
        manager.close().expect("close");
    }

    #[test]
    fn oversized_error_result_keeps_a_readable_top_level_summary() {
        let (pipeline, mut manager) = pipeline_with_pruner();
        let message = "E".repeat(9_000);
        let mut result = ToolResult {
            call_id: "call-2".into(),
            tool_name: "run_command".into(),
            output: json!({ "error": format!("command failed: {message}") }),
            is_error: true,
        };
        pipeline.transform_result(&mut result);
        let summary = result.output["error"].as_str().expect("error summary");
        assert!(summary.starts_with("command failed: E"));
        assert!(summary.chars().count() <= ERROR_SUMMARY_CHARS);
        assert_eq!(result.output["clat_truncated"], json!(true));
        manager.close().expect("close");
    }

    #[test]
    fn pruning_is_revoked_with_the_plugin_scope() {
        // INV-P5：scope close 后贡献消失，行为回到未注册。
        let (pipeline, mut manager) = pipeline_with_pruner();
        manager.close().expect("close");
        let mut result = result_with_output_len(THRESHOLD_CHARS, false);
        let before = result.output.clone();
        pipeline.transform_result(&mut result);
        assert_eq!(result.output, before, "transformer revoked with scope");
    }

    #[test]
    fn transformers_run_in_registration_order_and_freeze_rejects_late_ones() {
        use crate::plugin::PluginOwner;
        use crate::tool::ToolRegistryError;

        let pipeline = ToolExecutionPipeline::new();
        let owner = PluginOwner::for_test(PluginId::new("test.transformer"));
        // 追加标记：证明在 pruner 之后运行（注册顺序稳定）。
        struct AppendMarker;
        impl ToolResultTransformer for AppendMarker {
            fn transform_result(&self, result: &mut ToolResult) {
                if let Some(object) = result.output.as_object_mut() {
                    object.insert("marker".into(), json!(true));
                }
            }
        }
        let _pruner_lease = pipeline
            .register_result_transformer(owner, Arc::new(ResultPruner))
            .map_err(|error| error.to_string())
            .expect("register pruner");
        let _marker_lease = pipeline
            .register_result_transformer(owner, Arc::new(AppendMarker))
            .map_err(|error| error.to_string())
            .expect("register marker");

        let mut small = result_with_output_len(1, false);
        pipeline.transform_result(&mut small);
        assert_eq!(
            small.output["marker"],
            json!(true),
            "marker ran after pruner"
        );

        pipeline.freeze().expect("freeze");
        match pipeline.register_result_transformer(owner, Arc::new(AppendMarker)) {
            Err(ToolRegistryError::Frozen) => {}
            Err(other) => panic!("expected frozen rejection, got {other}"),
            Ok(_) => panic!("registration after freeze must fail"),
        }
    }
}
