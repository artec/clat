use super::state::*;
use super::*;
use crate::session::use_cases::SessionService;
use std::sync::{Arc, Mutex};

pub(crate) struct TaskService {
    pub(super) sessions: Arc<SessionService>,
    pub(super) project: crate::Project,
    pub(super) root: super::root::Root,
    pub(super) lane: Mutex<()>,
    pub(super) live: Mutex<Option<LiveRun>>,
    pub(super) query: Mutex<Option<Arc<dyn JobQuery>>>,
    pub(super) failure: Mutex<Option<String>>,
    pub(super) checkers:
        std::collections::BTreeMap<(String, u32), Arc<dyn super::acceptance::AcceptanceChecker>>,
}
pub(super) struct LiveRun {
    pub task: String,
    pub session: String,
    pub run: String,
    pub tokens: u64,
    pub started: std::time::Instant,
    pub deadline: std::time::Instant,
    pub recovery: String,
}
impl TaskService {
    pub(crate) fn new(
        sessions: Arc<SessionService>,
        project: crate::Project,
    ) -> Result<Self, String> {
        let root = super::root::Root::open(project.root())?;
        Ok(Self {
            root,
            sessions,
            project,
            lane: Mutex::new(()),
            live: Mutex::new(None),
            query: Mutex::new(None),
            failure: Mutex::new(None),
            checkers: Default::default(),
        })
    }
    pub(crate) fn current(&self) -> Result<Option<TaskState>, String> {
        let state = self.sessions.task_state()?;
        if state.is_some() {
            self.root.check()?;
        }
        if let Some(s) = &state
            && (s.project_anchor != self.root.identity
                || s.project != self.project.root().to_string_lossy()
                || self.sessions.active_id().as_ref().map(|id| id.as_str())
                    != Some(s.session.as_str()))
        {
            return Err("task project/session ownership mismatch".into());
        }
        Ok(state)
    }
    pub(crate) fn ensure_start_allowed(&self) -> Result<(), String> {
        if self.cap().is_some() {
            return Ok(());
        }
        if self
            .current()?
            .is_some_and(|s| s.operations.iter().any(|o| o.outcome == "unknown"))
        {
            return Err(
                "task has unresolved operations; use /task check and /task resolve before continuing, or open a separate session"
                    .into(),
            );
        }
        Ok(())
    }
    pub(crate) fn create(
        &self,
        objective: String,
        acceptance: Vec<String>,
        budget: TaskBudget,
    ) -> Result<TaskState, String> {
        let _lane = self.lane.lock().map_err(|_| "task write lock poisoned")?;
        self.root.check()?;
        let old = self.current()?;
        if old.as_ref().is_some_and(|s| !s.terminal()) {
            return Err("unfinished task already exists".into());
        }
        let mut s = self.draft(objective, acceptance, budget);
        s.session = self
            .sessions
            .active_id()
            .ok_or("task requires a selected session")?
            .to_string();
        self.sessions.commit_task(old.as_ref(), &s)?;
        Ok(s)
    }
    pub(crate) fn validate_new(
        &self,
        objective: &str,
        acceptance: &[String],
        budget: &TaskBudget,
    ) -> Result<(), String> {
        self.root.check()?;
        self.draft(objective.into(), acceptance.to_vec(), budget.clone())
            .validate()
    }
    fn draft(&self, objective: String, acceptance: Vec<String>, budget: TaskBudget) -> TaskState {
        TaskState {
            id: format!("task-{}", uuid::Uuid::new_v4()),
            project: self.project.root().to_string_lossy().into_owned(),
            project_anchor: self.root.identity.clone(),
            session: "pending".into(),
            revision: 1,
            objective,
            acceptance,
            phase: "paused".into(),
            budget,
            progress: Progress::default(),
            runs: Vec::new(),
            operations: Vec::new(),
            acceptance_state: None,
        }
    }
    pub(super) fn update(
        &self,
        id: &str,
        revision: u64,
        change: impl FnOnce(&mut TaskState) -> Result<(), String>,
    ) -> Result<TaskState, String> {
        let old = self.current()?.ok_or("no current task")?;
        if old.id != id || old.revision != revision {
            return Err("stale task identity/revision; inspect again".into());
        }
        let mut next = old.clone();
        change(&mut next)?;
        next.revision = next
            .revision
            .checked_add(1)
            .ok_or("task revision overflow")?;
        self.sessions.commit_task(Some(&old), &next)?;
        Ok(next)
    }
    pub(crate) fn save(
        &self,
        id: &str,
        revision: u64,
        mut progress: Progress,
        model: bool,
    ) -> Result<TaskState, String> {
        validate_progress(&progress)?;
        let _lane = self.lane.lock().map_err(|_| "task write lock poisoned")?;
        if model {
            self.require_binding(id)?;
        }
        let mut bytes_budget = 32 * 1024 * 1024;
        for a in &mut progress.artifacts {
            let bytes = self
                .read_artifact_budget(&a.path, &mut bytes_budget)?
                .ok_or("task artifact missing")?;
            let actual = digest(&bytes);
            if a.digest != actual {
                return Err("artifact digest changed before checkpoint".into());
            }
        }
        self.update(id, revision, |s| {
            for a in &progress.artifacts {
                for evidence in &a.evidence {
                    if !s
                        .operations
                        .iter()
                        .any(|o| &o.id == evidence && o.outcome == "confirmed")
                    {
                        return Err(
                            "evidence must reference a committed confirmed task operation".into(),
                        );
                    }
                }
            }
            for artifact in &s.progress.artifacts {
                if !progress.artifacts.iter().any(|a| a.path == artifact.path) {
                    progress.artifacts.push(artifact.clone());
                }
            }
            for job in &s.progress.jobs {
                if !progress.jobs.iter().any(|j| {
                    (j.provider.as_str(), j.scope.as_str(), j.id.as_str())
                        == (job.provider.as_str(), job.scope.as_str(), job.id.as_str())
                }) {
                    progress.jobs.push(job.clone());
                }
            }
            s.progress = progress;
            Ok(())
        })
    }
    pub(crate) fn finish(&self, id: &str, revision: u64, phase: &str) -> Result<TaskState, String> {
        if phase == "complete" {
            return self.finish_human(id, revision, None);
        }
        let _lane = self.lane.lock().map_err(|_| "task write lock poisoned")?;
        if self
            .live
            .lock()
            .map_err(|_| "task binding lock poisoned")?
            .is_some()
        {
            return Err("pause/cancel the active run first".into());
        }
        self.update(id, revision, |s| {
            if !matches!(phase, "paused" | "complete" | "cancelled") {
                return Err("invalid task phase".into());
            }
            if phase == "complete" && s.operations.iter().any(|o| o.outcome == "unknown") {
                return Err("resolve unknown operations before completing".into());
            }
            s.phase = phase.into();
            Ok(())
        })
    }
    pub(crate) fn resolve(
        &self,
        id: &str,
        revision: u64,
        operation: &str,
        outcome: &str,
        note: &str,
    ) -> Result<TaskState, String> {
        bounded(note, 2048)?;
        let _lane = self.lane.lock().map_err(|_| "task write lock poisoned")?;
        if self
            .live
            .lock()
            .map_err(|_| "task binding lock poisoned")?
            .is_some()
        {
            return Err("cannot resolve an active task run".into());
        }
        self.update(id, revision, |s| {
            if !matches!(outcome, "confirmed" | "not_executed") {
                return Err("resolution must be confirmed or not_executed".into());
            }
            let op = s
                .operations
                .iter_mut()
                .find(|o| o.id == operation)
                .ok_or("unknown operation identity")?;
            if op.outcome != "unknown" {
                return Err("operation is already resolved".into());
            }
            op.outcome = outcome.into();
            op.resolution = Some(note.into());
            Ok(())
        })
    }
    pub(super) fn require_binding(&self, id: &str) -> Result<String, String> {
        let live = self.live.lock().map_err(|_| "task binding lock poisoned")?;
        let live = live.as_ref().ok_or("no active task run")?;
        if live.task != id
            || self.sessions.active_id().as_ref().map(|s| s.as_str()) != Some(live.session.as_str())
        {
            return Err("task binding does not own this session".into());
        }
        Ok(live.run.clone())
    }
    pub(crate) fn injection(&self) -> Result<Option<String>, String> {
        let Some(s) = self.current()? else {
            return Ok(None);
        };
        if self.cap().is_none() {
            return Ok(None);
        }
        self.require_binding(&s.id)?;
        let recovery = self
            .live
            .lock()
            .map_err(|_| "task binding lock poisoned")?
            .as_ref()
            .map(|l| l.recovery.clone())
            .unwrap_or_default();
        Ok(Some(format!(
            "{recovery}\nTask recovery state (progress is a claim, never verification). Do not replay confirmed operations. Use update_task for complete-boundary checkpoints.\n{}",
            serde_json::to_string(&s).map_err(|e| e.to_string())?
        )))
    }
}

impl TaskService {
    pub(crate) fn for_session(&self, sessions: Arc<SessionService>) -> Result<Arc<Self>, String> {
        self.root.check()?;
        let mut target = Self::new(sessions, self.project.clone())?;
        if target.root.identity != self.root.identity {
            return Err("target project identity changed".into());
        }
        target.checkers = self.checkers.clone();
        *target
            .query
            .lock()
            .map_err(|_| "task query lock poisoned")? = self
            .query
            .lock()
            .map_err(|_| "task query lock poisoned")?
            .clone();
        Ok(Arc::new(target))
    }
}
