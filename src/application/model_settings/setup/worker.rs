use super::*;
use crate::model::{FinishReason, ModelErrorKind, ModelItem, ModelOptions, ModelRequest};
use crate::plugins::services::ProviderRegistry;

pub(super) fn finish(op: &Operation, state: &str, failure: Option<&str>) {
    let mut view = op.status.lock().expect("setup status");
    if op.cancel.is_cancelled() {
        view.state = "cancelled".into();
        view.failure = None;
    } else {
        view.state = state.into();
        view.failure = failure.map(str::to_owned);
    }
}
pub(super) fn start(
    op: Arc<Operation>,
    providers: Arc<ProviderRegistry>,
    config: ModelConfig,
    credentials: ProviderCredentials,
) {
    op.status.lock().expect("setup status").state = "checking".into();
    let worker_op = op.clone();
    if std::thread::Builder::new()
        .name("clat-model-check".into())
        .spawn(move || {
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let mut model = providers
                    .build(&config, &credentials)
                    .map_err(|_| "setup.invalid")?;
                check(model.as_mut(), &config, &worker_op.cancel)
            }))
            .unwrap_or(Err("setup.check_failed"));
            match result {
                Ok(()) => finish(&worker_op, "verified", None),
                Err(failure) => finish(&worker_op, "failed", Some(failure)),
            }
        })
        .is_err()
    {
        finish(&op, "failed", Some("setup.check_failed"));
    }
}
// A single direct adapter request: no retry wrapper, no history, no tools and
// no model response/error text returned to clients (it may contain secrets).
pub(super) fn check(
    model: &mut dyn crate::Model,
    config: &ModelConfig,
    cancel: &CancelToken,
) -> Result<(), &'static str> {
    if cancel.is_cancelled() {
        return Err("setup.cancelled");
    }
    let items = [ModelItem::user_text(
        "Reply with OK. This is a connection check.",
    )];
    let options = ModelOptions {
        output_limit: Some(config.output_limit.unwrap_or(4096).min(4096)),
        ..Default::default()
    };
    let response = model
        .stream(
            ModelRequest {
                instructions: None,
                items: &items,
                tools: &[],
                options: &options,
                cancel,
            },
            &mut IgnoreEvents,
        )
        .map_err(|error| match error.kind() {
            ModelErrorKind::Authentication => "setup.authentication",
            ModelErrorKind::RateLimited => "setup.rate_limited",
            ModelErrorKind::Transport => "setup.network",
            ModelErrorKind::Client => "setup.request_rejected",
            ModelErrorKind::Request => "setup.invalid",
            ModelErrorKind::Decode => "setup.protocol_error",
            ModelErrorKind::Cancelled => "setup.cancelled",
            _ => "setup.check_failed",
        })?;
    if cancel.is_cancelled() || response.finish_reason == FinishReason::Cancelled {
        return Err("setup.cancelled");
    }
    if response.finish_reason != FinishReason::Completed
        || response.text.trim().is_empty()
        || !response.tool_calls.is_empty()
    {
        return Err("setup.empty_response");
    }
    Ok(())
}
struct IgnoreEvents;
impl crate::model::ModelEventSink for IgnoreEvents {
    fn emit(&mut self, _: crate::model::ModelEvent) {}
}
