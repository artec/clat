//! Provider capacity errors are surfaced; independent image errors may recover.
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
    kind: ModelErrorKind,
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
        if self.calls == 1 {
            return Err(ModelError::with_kind(
                self.kind,
                "provider rejected the request",
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
fn journey(delivered: bool, progress: bool, kind: ModelErrorKind) -> (bool, usize, usize) {
    let tools = ToolRegistry::new();
    let project = Project::new(".");
    let mut model = CapacityModel {
        calls: 0,
        delivered,
        kind,
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
fn configured_window_provider_overflow_is_reported_without_probe_or_retry() {
    // A context error is not permission to change the user-selected window.
    assert_eq!(
        journey(false, true, ModelErrorKind::ContextCapacity),
        (false, 1, 0)
    );
    assert_eq!(
        journey(false, false, ModelErrorKind::ContextCapacity),
        (false, 1, 0)
    );
    assert_eq!(
        journey(false, true, ModelErrorKind::Authentication),
        (false, 1, 0)
    );
}
