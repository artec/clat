//! Provider-neutral capacity classification: only explicit machine codes
//! authorize recovery. HTTP 401/403 must never be reclassified by wording.
use crate::model::ModelErrorKind;
use serde_json::Value;

pub(super) fn from_body(body: &str, fallback: ModelErrorKind) -> ModelErrorKind {
    serde_json::from_str(body).map_or(fallback, |value| from_value(&value, fallback))
}
pub(super) fn from_value(value: &Value, fallback: ModelErrorKind) -> ModelErrorKind {
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

    #[test]
    fn text_claiming_smaller_plan_does_not_probe_override_or_retry() {
        let message = json!({"error":{
            "type":"permission_denied",
            "message":"Your current plan supports only k3.1-pro up to 256K context. 1M context is available on higher plans."
        }});
        assert_eq!(
            from_value(&message, ModelErrorKind::Authentication),
            ModelErrorKind::Authentication
        );
        assert_eq!(
            from_value(&message, ModelErrorKind::Client),
            ModelErrorKind::Client
        );
    }
}
