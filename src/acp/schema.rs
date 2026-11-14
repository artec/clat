//! Baseline wire discriminators from stable ACP schema-v1.31.0.
use serde::Deserialize;
use serde_json::Value;

pub(super) const WIRE_VERSION: u64 = 1;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct Initialize {
    pub protocol_version: u64,
    #[serde(default = "empty_object")]
    pub agent_capabilities: Value,
    #[serde(default)]
    pub auth_methods: Vec<Value>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct Prompt {
    pub stop_reason: StopReason,
}

#[derive(Copy, Clone, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum StopReason {
    EndTurn,
    MaxTokens,
    #[serde(alias = "max_turns")]
    MaxTurnRequests,
    Refusal,
    Cancelled,
}

impl StopReason {
    pub fn name(self) -> &'static str {
        match self {
            Self::EndTurn => "end_turn",
            Self::MaxTokens => "max_tokens",
            Self::MaxTurnRequests => "max_turn_requests",
            Self::Refusal => "refusal",
            Self::Cancelled => "cancelled",
        }
    }
}

fn empty_object() -> Value {
    serde_json::Value::Object(Default::default())
}
