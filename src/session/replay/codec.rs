//! Read-side adapters for the existing replay JSON contract. Shared payloads
//! use wire's parser; no new durable or RunEvent vocabulary is introduced.
use super::*;
use serde::{Deserialize, Deserializer, de::Error};

pub(super) fn blocks<'de, D: Deserializer<'de>>(
    de: D,
) -> Result<Vec<crate::ContentBlock>, D::Error> {
    Vec::<Value>::deserialize(de)?
        .iter()
        .map(|value| {
            crate::wire::content_block_from_json(value, "replay")
                .map_err(|_| D::Error::custom("invalid replay content block"))
        })
        .collect()
}

pub(super) fn decision<'de, D: Deserializer<'de>>(de: D) -> Result<PermissionDecision, D::Error> {
    crate::wire::permission_decision_from_json(&Value::deserialize(de)?, "replay")
        .map_err(|_| D::Error::custom("invalid replay permission decision"))
}

pub(super) fn turn_end<'de, D: Deserializer<'de>>(de: D) -> Result<ReplayTurnEnd, D::Error> {
    let value = Value::deserialize(de)?;
    match value.as_str() {
        Some("completed") => Ok(ReplayTurnEnd::Completed),
        Some("blocked") => Ok(ReplayTurnEnd::Blocked),
        Some("max_tokens") => Ok(ReplayTurnEnd::MaxTokens),
        Some("interrupted") => Ok(ReplayTurnEnd::Interrupted),
        _ => {
            if let Some(cause) = value.get("aborted").and_then(Value::as_str) {
                return Ok(ReplayTurnEnd::Aborted {
                    cause: cause.into(),
                });
            }
            if let Some(message) = value.get("error").and_then(Value::as_str) {
                return Ok(ReplayTurnEnd::Error {
                    message: message.into(),
                });
            }
            Err(D::Error::custom("unknown replay turn termination"))
        }
    }
}
