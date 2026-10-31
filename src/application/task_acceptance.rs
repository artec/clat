//! User authority and read-only views for the shared Task acceptance owner.
use super::*;
use crate::task::{AcceptanceContract, TaskState, VerificationReport};
use std::collections::BTreeMap;
impl TrustedProjectApplication {
    pub fn task_set_acceptance(
        &self,
        id: &str,
        revision: u64,
        contract: AcceptanceContract,
    ) -> Result<TaskState, ApplicationError> {
        self.task_service()?;
        self.reject_session_switch_while_busy()?;
        self.task_service()?
            .set_acceptance(id, revision, contract)
            .map_err(ApplicationError::new)
    }
    pub fn task_attest(
        &self,
        id: &str,
        revision: u64,
        clause: &str,
        note: &str,
    ) -> Result<TaskState, ApplicationError> {
        self.task_service()?;
        self.reject_session_switch_while_busy()?;
        self.task_service()?
            .attest(id, revision, clause, note)
            .map_err(ApplicationError::new)
    }
    pub fn task_verify(
        &self,
        evidence: &BTreeMap<String, String>,
    ) -> Result<VerificationReport, ApplicationError> {
        self.task_service()?
            .check_acceptance(evidence, &crate::CancelToken::new())
            .map_err(ApplicationError::new)
    }
    pub fn task_save_verification(
        &self,
        id: &str,
        revision: u64,
        evidence: &BTreeMap<String, String>,
    ) -> Result<TaskState, ApplicationError> {
        self.task_service()?;
        self.reject_session_switch_while_busy()?;
        self.task_service()?
            .save_verification(id, revision, evidence, false, &crate::CancelToken::new())
            .map_err(ApplicationError::new)
    }
    pub fn task_complete_verified(
        &self,
        id: &str,
        revision: u64,
        report: &str,
    ) -> Result<TaskState, ApplicationError> {
        self.task_service()?;
        self.reject_session_switch_while_busy()?;
        self.task_service()?
            .finish_verified(id, revision, report)
            .map_err(ApplicationError::new)
    }
    pub fn task_complete_override(
        &self,
        id: &str,
        revision: u64,
        note: &str,
    ) -> Result<TaskState, ApplicationError> {
        self.task_service()?;
        self.reject_session_switch_while_busy()?;
        self.task_service()?
            .finish_human(id, revision, Some(note))
            .map_err(ApplicationError::new)
    }
    pub fn task_authorize_recheck(
        &self,
        id: &str,
        revision: u64,
        clause: &str,
    ) -> Result<TaskState, ApplicationError> {
        self.task_service()?;
        self.reject_session_switch_while_busy()?;
        self.task_service()?
            .authorize_recheck(id, revision, clause)
            .map_err(ApplicationError::new)
    }
    pub fn task_verification_status(
        &self,
        report: Option<&str>,
    ) -> Result<Value, ApplicationError> {
        self.task_service()?
            .verification_status(report)
            .map_err(ApplicationError::new)
    }
}
