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
            return Err(ModelError::with_kind(
                ModelErrorKind::ContextCapacity,
                "explicit capacity code",
            ));
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
    let tools = ToolRegistry::new();
    let project = Project::new(".");
    let mut model = CapacityModel {
        calls: 0,
        delivered,
        always,
    };
    let calls = Arc::new(AtomicUsize::new(0));
    let result = Run::new(&mut model, &tools, &AllowAll, &project)
        .with_context_manager(Some(Box::new(Maintenance {
            calls: calls.clone(),
            progress,
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
    (result.is_ok(), model.calls, calls.load(Ordering::SeqCst))
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
