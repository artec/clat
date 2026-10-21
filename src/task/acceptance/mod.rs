//! User-owned acceptance contracts and committed verification provenance.
mod checker;
mod command;
mod completion;
pub(crate) mod interface;
mod model;
mod service;
mod transition;
mod validation;
mod verification;
pub(crate) use checker::{AcceptanceChecker, CheckSnapshot, CheckerOutcome};
pub use model::*;
pub(super) use transition::validate_change;
pub(super) use validation::validate_snapshot;

pub(crate) use super::state::relative as validate_relative;

pub(crate) use command::{immutable_command, validate_command};
