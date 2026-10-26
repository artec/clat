//! Host-owned, explicitly authorized automatic work. Journal owns execution facts.
pub(crate) mod command;
mod dispatch;
mod manual;
pub(crate) mod model;
pub(crate) mod plugin;
mod schema;
mod state;
mod store;
mod time;
mod tools;
use crate::i18n::UiMessage;
use serde::{Deserialize, Serialize};
pub use state::*;
use std::sync::Arc;
pub(crate) use store::SchedulerService;
pub use time::TimeRule;
#[derive(Clone, Debug)]
pub(crate) struct ScheduleError {
    code: &'static str,
    detail: String,
}
impl ScheduleError {
    fn new(code: &'static str) -> Self {
        Self {
            code,
            detail: String::new(),
        }
    }
    fn detail(mut self, detail: impl ToString) -> Self {
        self.detail = crate::redact::redact_secrets(&detail.to_string());
        self
    }
    pub(crate) fn presentation(&self) -> UiMessage {
        UiMessage::new(&format!("schedule.{}", self.code)).arg("detail", self.detail.clone())
    }
}
pub(crate) fn digest(value: &impl Serialize) -> String {
    use sha2::{Digest, Sha256};
    format!(
        "{:x}",
        Sha256::digest(serde_json::to_vec(value).expect("automatic control data"))
    )
}
pub(crate) fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis().min(i64::MAX as u128) as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests;
