use crate::message::{AdmissionReceipt, AdmissionState};
use serde_json::Value;

/// Keep authoritative admission facts separate from diagnostic text.
#[derive(Clone, Debug)]
pub struct HostCallError {
    pub message: String,
    pub code: Option<String>,
    pub presentation: Option<Box<crate::i18n::UiMessage>>,
    pub receipt: Option<Box<AdmissionReceipt>>,
}

impl HostCallError {
    pub fn presentation_or_fallback(&self) -> crate::i18n::UiMessage {
        self.presentation.as_deref().cloned().unwrap_or_else(|| {
            crate::i18n::UiMessage::new("common.diagnostic").arg("detail", self.message.clone())
        })
    }
    pub fn committed(&self) -> bool {
        self.receipt
            .as_ref()
            .is_some_and(|receipt| receipt.state == AdmissionState::Committed)
    }
}

impl From<String> for HostCallError {
    fn from(message: String) -> Self {
        Self {
            message,
            code: None,
            presentation: None,
            receipt: None,
        }
    }
}

impl From<&str> for HostCallError {
    fn from(message: &str) -> Self {
        message.to_owned().into()
    }
}

impl std::fmt::Display for HostCallError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for HostCallError {}

pub(super) fn decode_result(result: Value) -> Result<Value, HostCallError> {
    match result["ok"].as_bool() {
        Some(true) => result
            .get("value")
            .cloned()
            .ok_or_else(|| "host omitted result".into()),
        Some(false) => {
            let error = &result["error"];
            let receipt = match error.get("receipt") {
                None | Some(Value::Null) => None,
                Some(value) => Some(serde_json::from_value(value.clone()).map_err(|_| {
                    HostCallError::from("invalid host admission receipt; outcome uncertain")
                })?),
            };
            Err(HostCallError {
                message: error["message"]
                    .as_str()
                    .unwrap_or("host rejected operation")
                    .into(),
                code: error["code"].as_str().map(str::to_owned),
                presentation: error
                    .get("presentation")
                    .and_then(|v| serde_json::from_value(v.clone()).ok()),
                receipt,
            })
        }
        None => Err("invalid host response envelope".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn host_errors_preserve_authoritative_receipts_and_codes() {
        for state in ["committed", "rolled-back", "reserved"] {
            let receipt = json!({"client_message_id":"request-1", "state":state,
                "committed_message_id":"message-1", "attachment_ids":["image-1"],
                "retryable":false,"failure_phase":"run-start"});
            let error = decode_result(json!({"ok":false,"error":{
                "code":"internal", "message":"failed", "receipt":receipt
            }}))
            .unwrap_err();
            assert_eq!(error.code.as_deref(), Some("internal"));
            assert_eq!(
                serde_json::to_value(error.receipt.as_ref().unwrap()).unwrap(),
                receipt
            );
            assert_eq!(error.committed(), state == "committed");
        }
    }

    #[test]
    fn invalid_or_missing_receipts_never_claim_a_safe_rollback() {
        for receipt in [
            Value::Null,
            json!({"state":"future-state"}),
            json!("invalid"),
        ] {
            let error = decode_result(json!({"ok":false,"error":{
                "message":"failed", "receipt":receipt
            }}))
            .unwrap_err();
            assert!(error.receipt.is_none());
            assert!(!error.committed());
        }
        assert!(decode_result(json!({"ok":true})).is_err());
        assert_eq!(
            decode_result(json!({"ok":true,"value":{"answer":42}})).unwrap(),
            json!({"answer":42})
        );
    }
}

#[cfg(test)]
mod presentation_tests {
    use super::*;
    #[test]
    fn presentation_never_replaces_execution_receipt_or_original_diagnostic() {
        let msg = crate::i18n::UiMessage::new("common.error").arg("detail", "fixture");
        let error = decode_result(serde_json::json!({"ok":false,"error":{
            "code":"busy", "message":"raw diagnostic", "presentation":msg,
        }}))
        .unwrap_err();
        assert_eq!(error.code.as_deref(), Some("busy"));
        assert_eq!(error.to_string(), "raw diagnostic");
        assert_eq!(error.presentation.as_deref(), Some(&msg));
        assert!(!error.committed());
    }
}

impl From<crate::i18n::UiMessage> for HostCallError {
    fn from(message: crate::i18n::UiMessage) -> Self {
        Self {
            message: message.english(),
            code: None,
            presentation: Some(Box::new(message)),
            receipt: None,
        }
    }
}
