//! Raw body replacement relinquishes typed thinking ownership, without rewriting JSON.
use super::*;
use serde_json::Value;

pub(super) fn validate(value: &Value) -> Result<(), ApplicationError> {
    if !value.is_object() || value.to_string().len() > 65536 || !bounded_depth(value, 0) {
        return Err(ApplicationError::new(
            "Extra Body must be a JSON object within 64 KiB and depth 32",
        ));
    }
    Ok(())
}

fn bounded_depth(value: &Value, depth: usize) -> bool {
    if depth > 32 {
        return false;
    }
    match value {
        Value::Object(fields) => fields.values().all(|value| bounded_depth(value, depth + 1)),
        Value::Array(items) => items.iter().all(|value| bounded_depth(value, depth + 1)),
        _ => true,
    }
}

pub(super) fn apply(value: Value, config: &mut ModelConfig) {
    config.migrate_legacy_overrides();
    config.thinking_level = None;
    config.overrides.thinking_level = crate::Override::Inherit;
    config.extra_body = value;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn model_body_bounds_and_legacy_thinking_reset() {
        let mut value = serde_json::json!("private");
        for _ in 0..33 {
            value = serde_json::json!({"nested":value});
        }
        let error = validate(&value).unwrap_err().to_string();
        assert!(!error.contains("private"));
        let raw = serde_json::json!({"reasoning_effort":"low"});
        let mut config = ModelConfig {
            endpoint: "https://api.deepseek.com/v1".into(),
            thinking_level: Some(crate::ThinkingLevel::Max),
            ..Default::default()
        };
        apply(raw.clone(), &mut config);
        config.migrate_legacy_overrides();
        config.apply_overrides();
        assert_eq!(config.extra_body, raw);
        assert!(config.thinking_level.is_none());
    }
}
