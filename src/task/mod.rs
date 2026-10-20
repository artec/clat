//! Explicit task progress and conservative recovery; the session journal is authority.
mod check;
mod execution;
pub(crate) mod plugin;
pub(crate) mod projection;
mod root;
mod service;
mod state;
#[cfg(test)]
mod tests;
pub use check::{JobObservation, JobQuery, RecoveryCheck, TaskResume};
pub(crate) use plugin::TaskPlugin;
pub(crate) use service::TaskService;
pub use state::{Artifact, JobReference, Progress, TaskBudget, TaskState};
pub(crate) use state::{decode_change, fold_change};
