//! Validation happens before acquiring the write transaction; errors contain no input.
use super::ApplicationError;
use serde_json::Value;
use ureq::http::{HeaderName, HeaderValue};

pub(super) fn validate(value: &Value) -> Result<(), ApplicationError> {
    let invalid = || ApplicationError::new("invalid extra headers object or header limits");
    let fields = value.as_object().ok_or_else(invalid)?;
    if fields.len() > 128 || value.to_string().len() > 65536 {
        return Err(invalid());
    }
    for (name, value) in fields {
        let value = value.as_str().ok_or_else(invalid)?;
        if name.len() > 256
            || value.len() > 8192
            || HeaderName::from_bytes(name.as_bytes()).is_err()
            || value.chars().any(char::is_control)
            || HeaderValue::from_str(value).is_err()
        {
            return Err(invalid());
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn model_headers_validate_shape_limits_and_hide_secrets() {
        for value in [
            json!([]),
            json!({"X":false}),
            json!({"bad name":"secret"}),
            json!({"X":"secret\n"}),
            json!({"X":"x".repeat(8193)}),
            json!({"x".repeat(257):"secret"}),
        ] {
            let error = validate(&value).unwrap_err().to_string();
            assert!(!error.contains("secret"));
        }
        let many: serde_json::Map<String, Value> =
            (0..129).map(|n| (format!("X-{n}"), json!(""))).collect();
        assert!(validate(&Value::Object(many)).is_err());
        let large: serde_json::Map<String, Value> = (0..9)
            .map(|n| (format!("X-{n}"), json!("x".repeat(8192))))
            .collect();
        assert!(validate(&Value::Object(large)).is_err());
        assert!(validate(&json!({})).is_ok());
        assert!(validate(&json!({"X-Test":"value"})).is_ok());
    }
}
