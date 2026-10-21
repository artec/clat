use super::*;
use crate::task::{TaskService, TaskState};
impl TaskService {
    pub(crate) fn set_acceptance(
        &self,
        id: &str,
        revision: u64,
        contract: AcceptanceContract,
    ) -> Result<TaskState, String> {
        let _lane = self.lane.lock().map_err(|_| "task write lock poisoned")?;
        self.require_idle()?;
        let s = self.current()?.ok_or("no current task")?;
        validation::validate_contract(&contract, &s.acceptance)?;
        let expected = s
            .acceptance_state
            .as_ref()
            .and_then(|a| a.contract.as_ref())
            .map_or(Some(1), |c| c.version.checked_add(1))
            .ok_or("contract version exhausted")?;
        if contract.version != expected {
            return Err("contract version must advance exactly once".into());
        }
        for clause in &contract.clauses {
            if let Some(checker) = self
                .checkers
                .get(&(clause.checker.clone(), clause.checker_version))
            {
                checker.validate(clause)?;
            }
        }
        self.update(id, revision, |s| {
            let a = s.acceptance_state.get_or_insert_with(Default::default);
            a.contract = Some(contract);
            a.attestations.clear();
            a.candidate = None;
            a.grants.clear();
            Ok(())
        })
    }
    pub(crate) fn attest(
        &self,
        id: &str,
        revision: u64,
        clause: &str,
        note: &str,
    ) -> Result<TaskState, String> {
        crate::task::state::bounded(note, 2048)?;
        let _lane = self.lane.lock().map_err(|_| "task write lock poisoned")?;
        self.require_idle()?;
        self.update(id, revision, |s| {
            let a = s
                .acceptance_state
                .as_mut()
                .ok_or("no acceptance contract")?;
            let c = a.contract.as_ref().ok_or("no acceptance contract")?;
            if a.attestations.iter().any(|h| h.clause == clause) {
                return Err("human clause already confirmed".into());
            }
            a.attestations.push(Attestation {
                clause: clause.into(),
                contract_version: c.version,
                note: note.into(),
            });
            Ok(())
        })
    }
    pub(crate) fn require_idle(&self) -> Result<(), String> {
        if self
            .live
            .lock()
            .map_err(|_| "task binding lock poisoned")?
            .is_some()
        {
            return Err("task must be idle".into());
        }
        Ok(())
    }
}
