use crate::message::{AdmissionReceipt, AdmissionState};
use serde_json::Value;

/// Keep authoritative admission facts separate from diagnostic text.
#[derive(Clone, Debug)]
pub struct HostCallError {
    pub message: String,
    pub code: Option<String>,
    pub receipt: Option<Box<AdmissionReceipt>>,
}

impl HostCallError {
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
