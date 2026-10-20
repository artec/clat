use super::service::LiveRun;
use super::state::*;
use super::*;
use crate::tool::{ToolInvocation, ToolMiddleware, ToolNext};
use crate::{ToolEffect, ToolError};
use serde_json::Value;

impl TaskService {
    pub(crate) fn begin(
        &self,
        request: &TaskResume,
        check: &RecoveryCheck,
        configured_cap: Option<u64>,
    ) -> Result<(), String> {
        let _lane = self.lane.lock().map_err(|_| "task write lock poisoned")?;
        self.verify_check(request, check)?;
        let mut live = self.live.lock().map_err(|_| "task binding lock poisoned")?;
        if live.is_some() {
            return Err("task is already running".into());
        }
        let run = format!("run-{}", uuid::Uuid::new_v4());
        let tokens = configured_cap
            .unwrap_or(check.remaining_tokens)
            .min(check.remaining_tokens)
            .min(check.task.budget.run_tokens);
        let duration = check
            .remaining_ms
            .min(u64::from(check.task.budget.run_seconds) * 1000);
        self.update(&request.task_id, request.revision, |s| {
            s.phase = "running".into();
            s.runs.push(TaskRun {
                id: run.clone(),
                turn: None,
                tokens,
                elapsed_ms: duration,
                settled: false,
            });
            Ok(())
        })?;
        *self
            .failure
            .lock()
            .map_err(|_| "task failure lock poisoned")? = None;
        let now = std::time::Instant::now();
        *live = Some(LiveRun {
            task: request.task_id.clone(),
            session: check.task.session.clone(),
            run,
            tokens,
            started: now,
            deadline: now + std::time::Duration::from_millis(duration),
            recovery: format!(
                "Recovery observations (read-only; hashes do not prove correctness): {}",
                serde_json::json!({"artifacts":check.artifacts,"jobs":check.jobs,"permission":check.permission,"unknown_consumption":check.unknown_consumption})
            ),
        });
        Ok(())
    }
    pub(crate) fn admit(&self, turn: u64) -> Result<(), String> {
        let _lane = self.lane.lock().map_err(|_| "task write lock poisoned")?;
        let Some(task) = self
            .live
            .lock()
            .map_err(|_| "task binding lock poisoned")?
            .as_ref()
            .map(|l| l.task.clone())
        else {
            return Ok(());
        };
        let run = self.require_binding(&task)?;
        let s = self.current()?.ok_or("task disappeared before admission")?;
        self.update(&s.id, s.revision, |s| {
            let r = s.runs.last_mut().ok_or("task run missing")?;
            if r.id != run || r.turn.is_some() {
                return Err("task admission identity already bound or changed".into());
            }
            r.turn = Some(turn);
            Ok(())
        })?;
        Ok(())
    }
    pub(crate) fn cap(&self) -> Option<u64> {
        self.live.lock().ok()?.as_ref().map(|l| l.tokens)
    }
    pub(crate) fn deadline(&self) -> Option<std::time::Instant> {
        self.live.lock().ok()?.as_ref().map(|l| l.deadline)
    }
    pub(crate) fn settle(&self, tokens: u64) -> Result<(), String> {
        let _lane = self.lane.lock().map_err(|_| "task write lock poisoned")?;
        let live = self
            .live
            .lock()
            .map_err(|_| "task binding lock poisoned")?
            .take();
        let Some(live) = live else {
            return Ok(());
        };
        let s = self
            .current()?
            .ok_or("task disappeared during settlement")?;
        if s.id != live.task || s.session != live.session {
            return Err("task settlement ownership mismatch".into());
        }
        self.update(&s.id, s.revision, |s| {
            let run = s
                .runs
                .iter_mut()
                .find(|r| r.id == live.run)
                .ok_or("task run missing")?;
            run.tokens = tokens;
            run.elapsed_ms = u64::try_from(live.started.elapsed().as_millis()).unwrap_or(u64::MAX);
            run.settled = true;
            s.phase = "paused".into();
            Ok(())
        })?;
        Ok(())
    }
    fn intent(&self, invocation: &ToolInvocation<'_>) -> Result<Option<String>, String> {
        let Some(task) = self
            .live
            .lock()
            .map_err(|_| "task binding lock poisoned")?
            .as_ref()
            .map(|l| l.task.clone())
        else {
            return Ok(None);
        };
        let _lane = self.lane.lock().map_err(|_| "task write lock poisoned")?;
        let run = self.require_binding(&task)?;
        let s = self.current()?.ok_or("task disappeared")?;
        if !s.runs.iter().any(|r| r.id == run && r.turn.is_some()) {
            return Err("task run has not committed session admission".into());
        }
        let name = invocation.tool.definition().name;
        let arguments_digest =
            digest(&serde_json::to_vec(invocation.arguments).map_err(|e| e.to_string())?);
        if !matches!(
            invocation.tool.definition().effect,
            ToolEffect::Pure | ToolEffect::Read
        ) && s.operations.iter().any(|o| {
            o.run != run
                && o.tool == name
                && o.arguments_digest == arguments_digest
                && o.outcome == "confirmed"
        }) {
            return Err("a prior task run already confirmed this operation; inspect its result instead of replaying it".into());
        }
        let id = format!("op-{}", uuid::Uuid::new_v4());
        self.update(&s.id, s.revision, |s| {
            if s.operations.len() >= 128 {
                return Err(
                    "task operation capacity reached; checkpoint and finish this task".into(),
                );
            }
            s.operations.push(Operation {
                id: id.clone(),
                run,
                tool: name,
                arguments_digest,
                call_id: invocation.call_id.map(str::to_owned),
                outcome: "unknown".into(),
                result_digest: None,
                result_kind: None,
                resolution: None,
            });
            Ok(())
        })?;
        Ok(Some(id))
    }
    fn outcome(
        &self,
        id: &str,
        result: &Result<Value, ToolError>,
        outcome: &str,
    ) -> Result<(), String> {
        let _lane = self.lane.lock().map_err(|_| "task write lock poisoned")?;
        let s = self.current()?.ok_or("task disappeared")?;
        self.require_binding(&s.id)?;
        self.update(&s.id, s.revision, |s| {
            let o = s
                .operations
                .iter_mut()
                .find(|o| o.id == id)
                .ok_or("task operation missing")?;
            o.outcome = outcome.into();
            o.result_kind = Some(if result.is_ok() { "ok" } else { "error" }.into());
            o.result_digest = Some(digest(&match result {
                Ok(v) => serde_json::to_vec(v).map_err(|e| e.to_string())?,
                Err(_) => b"tool returned error; side effects not disproven".to_vec(),
            }));
            Ok(())
        })?;
        Ok(())
    }
}
impl ToolMiddleware for TaskService {
    fn execute(
        &self,
        invocation: &ToolInvocation<'_>,
        next: &dyn ToolNext,
    ) -> Result<Value, ToolError> {
        self.check_failure()?;
        // Session metadata (including update_task) cannot be mistaken for an external effect.
        let effect = invocation.tool.definition().effect;
        if matches!(effect, ToolEffect::SessionWrite) {
            return next.execute(invocation);
        }
        if invocation.cancel.is_cancelled() {
            return Err(ToolError::new("cancelled before task invocation"));
        }
        let id = self.intent(invocation).map_err(|e| self.poison(e))?;
        if invocation.cancel.is_cancelled() {
            if let Some(id) = id {
                let result = Err(ToolError::new("cancelled before actual invocation"));
                self.outcome(&id, &result, "not_executed")
                    .map_err(|e| self.poison(e))?;
            }
            return Err(ToolError::new("cancelled before actual task invocation"));
        }
        let result = next.execute(invocation);
        self.check_failure()?;
        if let Some(id) = id {
            self.outcome(
                &id,
                &result,
                if invocation.cancel.is_cancelled() {
                    "unknown"
                } else {
                    "confirmed"
                },
            )
            .map_err(|e| self.poison(e))?;
        }
        result
    }
}

impl TaskService {
    fn poison(&self, error: String) -> ToolError {
        *self
            .failure
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(error.clone());
        ToolError::fatal(error)
    }
    fn check_failure(&self) -> Result<(), ToolError> {
        if self.cap().is_some()
            && let Some(error) = self
                .failure
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .as_ref()
        {
            return Err(ToolError::fatal(error));
        }
        Ok(())
    }
}
