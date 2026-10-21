//! Explicit task checker adapters; no execution or ambient reads occur here.
mod command;
mod local;
use crate::task::acceptance::{AcceptanceChecker, CheckSnapshot, CheckerOutcome, Clause};
use crate::{Project, task::TaskState};
pub(crate) use command::{command_observation, command_plan, prior_command_matches};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::sync::Arc;
pub(crate) fn task_checkers() -> Vec<Arc<dyn AcceptanceChecker>> {
    vec![
        Arc::new(local::TextChecker),
        Arc::new(local::JsonChecker),
        Arc::new(local::RecordsChecker),
        Arc::new(command::CommandChecker),
    ]
}
fn files(clause: &Clause, expected: &[&str]) -> Result<(), String> {
    let actual: std::collections::BTreeSet<_> = clause.files.iter().map(String::as_str).collect();
    let required: std::collections::BTreeSet<_> = expected.iter().copied().collect();
    if clause.source != "project_files" || actual != required {
        return Err("checker must read exactly the declared project files".into());
    }
    Ok(())
}
fn parse<T: serde::de::DeserializeOwned>(v: &Value) -> Result<T, String> {
    serde_json::from_value(v.clone())
        .map_err(|_| "unsupported or invalid checker parameters".into())
}
