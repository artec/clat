//! Write-only authentication edits. Never serialize these into model views.
use super::*;

#[derive(Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ModelProfileAuth {
    pub header: Option<String>,
    pub prefix: Option<String>,
}

impl ModelProfileAuth {
    pub(super) fn validate(&self) -> Result<(), ApplicationError> {
        if self.header.as_ref().is_some_and(|header| {
            header.len() > 256
                || (!header.is_empty()
                    && ureq::http::HeaderName::from_bytes(header.as_bytes()).is_err())
        }) {
            return Err(ApplicationError::new("invalid authentication header name"));
        }
        if self.prefix.as_ref().is_some_and(|prefix| {
            prefix.len() > 8192
                || prefix.chars().any(char::is_control)
                || ureq::http::HeaderValue::from_str(prefix).is_err()
        }) {
            return Err(ApplicationError::new("invalid authentication prefix"));
        }
        Ok(())
    }

    pub(super) fn apply(self, config: &mut ModelConfig) {
        if let Some(header) = self.header {
            config.auth_header = header;
        }
        if let Some(prefix) = self.prefix {
            config.auth_prefix = prefix;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn model_auth_validation_is_bounded_and_does_not_echo_inputs() {
        for value in [
            serde_json::json!({"header":"bad header"}),
            serde_json::json!({"header":"a".repeat(257)}),
            serde_json::json!({"prefix":"secret\n"}),
            serde_json::json!({"prefix":"a".repeat(8193)}),
        ] {
            let edit: ModelProfileAuth = serde_json::from_value(value).unwrap();
            let error = edit.validate().unwrap_err().to_string();
            assert!(!error.contains("secret"));
        }
        for value in [
            serde_json::json!({"header":false}),
            serde_json::json!({"extra_body":{}}),
        ] {
            assert!(serde_json::from_value::<ModelProfileAuth>(value).is_err());
        }
        let edit: ModelProfileAuth =
            serde_json::from_value(serde_json::json!({"header":"", "prefix":""})).unwrap();
        edit.validate().unwrap();
    }
}
