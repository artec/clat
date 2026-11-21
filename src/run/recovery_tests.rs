//! Recovery invariants: a request can repeat, a tool cannot.
use super::*;
use crate::model::{ModelError, ModelErrorKind, ModelResponse};
use crate::permission::AllowAll;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
struct CapacityModel {
    calls: usize,
    delivered: bool,
    always: bool,
    limit: Option<u64>,
}
impl Model for CapacityModel {
    fn provider(&self) -> &str {
        "offline"
    }
    fn model_id(&self) -> &str {
        "capacity"
    }
    fn stream(
        &mut self,
        _: ModelRequest<'_>,
        sink: &mut dyn ModelEventSink,
    ) -> Result<ModelResponse, ModelError> {
        self.calls += 1;
        if self.delivered {
            sink.emit(ModelEvent::ToolArgumentsDelta {
                call_id: "never-dispatched".into(),
                delta: "{".into(),
            });
        }
        if self.calls == 1 || self.always {
            let mut error = ModelError::with_kind(
                ModelErrorKind::ContextCapacity,
                "explicit capacity code",
            );
            if let Some(limit) = self.limit {
                error = error.with_context_limit(limit);
            }
            return Err(error);
        }
        Ok(crate::test_support::response(
            "done",
            FinishReason::Completed,
        ))
    }
}
struct Maintenance {
    calls: Arc<AtomicUsize>,
    progress: bool,
    discovered: Arc<std::sync::Mutex<Option<u64>>>,
}
impl RequestContextManager for Maintenance {
    fn maintain(
        &mut self,
        _: &mut Vec<ModelItem>,
        _: &mut Usage,
        _: Option<&str>,
        _: &[crate::ToolDefinition],
        _: &ModelOptions,
    ) -> Result<(), String> {
        Ok(())
    }
    fn discover_context_limit(&mut self, limit: u64) {
        *self.discovered.lock().unwrap() = Some(limit);
    }
    fn recover(
        &mut self,
        _: ModelErrorKind,
        items: &mut Vec<ModelItem>,
        _: &mut Usage,
        _: Option<&str>,
        _: &[crate::ToolDefinition],
        _: &ModelOptions,
    ) -> Result<bool, String> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if self.progress {
            items.remove(0);
        }
        Ok(self.progress)
    }
}
fn journey(delivered: bool, progress: bool, always: bool) -> (bool, usize, usize) {
    let (ok, calls, recoveries, _) = journey_with_limit(delivered, progress, always, None);
    (ok, calls, recoveries)
}
fn journey_with_limit(
    delivered: bool,
    progress: bool,
    always: bool,
    limit: Option<u64>,
) -> (bool, usize, usize, Option<u64>) {
    let tools = ToolRegistry::new();
    let project = Project::new(".");
    let mut model = CapacityModel {
        calls: 0,
        delivered,
        always,
        limit,
    };
    let calls = Arc::new(AtomicUsize::new(0));
    let discovered = Arc::new(std::sync::Mutex::new(None));
    let result = Run::new(&mut model, &tools, &AllowAll, &project)
        .with_context_manager(Some(Box::new(Maintenance {
            calls: calls.clone(),
            progress,
            discovered: discovered.clone(),
        })))
        .execute_with_items(
            vec![
                ModelItem::user_text("history ".repeat(2000)),
                ModelItem::user_text("keep"),
            ],
            crate::message::MessageContent::text("keep"),
            None,
            &mut Vec::new(),
        );
    (
        result.is_ok(),
        model.calls,
        calls.load(Ordering::SeqCst),
        *discovered.lock().unwrap(),
    )
}
#[test]
fn phase8_overflow_retries_once_after_strict_progress() {
    assert_eq!(journey(false, true, false), (true, 2, 1));
    assert_eq!(journey(false, true, true), (false, 2, 1));
}
#[test]
fn phase8_overflow_cannot_retry_after_tool_increment_or_without_progress() {
    assert_eq!(journey(true, true, false), (false, 1, 0));
    assert_eq!(journey(false, false, false), (false, 1, 1));
}

/// 档位上限发现（2026-11-21 Kimi Plus 256K live 实测）：错误携带的
/// context limit 必须在恢复前到达 context manager——恢复压缩与后续
/// 主动压缩的预算窗口都按它收紧，不再“撞墙-压缩-又撞墙”。预修复
///（wiring 缺失）上本测试红。
#[test]
fn phase8_discovered_context_limit_reaches_the_manager() {
    let (ok, calls, recoveries, discovered) =
        journey_with_limit(false, true, false, Some(262_144));
    assert_eq!((ok, calls, recoveries), (true, 2, 1));
    assert_eq!(discovered, Some(262_144));
}
