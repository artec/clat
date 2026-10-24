use super::*;
use crate::process::{ByteRing, ProcessService};
use std::sync::atomic::{AtomicBool, Ordering};
pub(crate) struct JobCapacity {
    root: String,
    counts: Mutex<(usize, usize)>,
}
pub(super) struct Permit {
    owner: Arc<JobCapacity>,
    active: AtomicBool,
}
impl JobCapacity {
    pub(crate) fn new(root: &std::path::Path) -> Self {
        Self {
            root: root
                .canonicalize()
                .unwrap_or_else(|_| root.to_path_buf())
                .to_string_lossy()
                .into_owned(),
            counts: Mutex::new((0, 0)),
        }
    }
    fn acquire(self: &Arc<Self>) -> Result<Arc<Permit>, JobError> {
        let mut counts = self
            .counts
            .lock()
            .map_err(|_| JobError::new("unavailable"))?;
        if counts.0 >= 16 || counts.1 >= 128 {
            return Err(JobError::new("capacity"));
        }
        counts.0 += 1;
        counts.1 += 1;
        Ok(Arc::new(Permit {
            owner: self.clone(),
            active: AtomicBool::new(true),
        }))
    }
}
impl Permit {
    pub(super) fn settle(&self) {
        if self.active.swap(false, Ordering::AcqRel) {
            self.owner.counts.lock().unwrap().0 -= 1;
        }
    }
}
impl Drop for Permit {
    fn drop(&mut self) {
        self.settle();
        self.owner.counts.lock().unwrap().1 -= 1;
    }
}
pub(super) struct Entry {
    pub view: Mutex<JobView>,
    pub output: Mutex<ByteRing>,
    pub agent_offset: Mutex<u64>,
    pub cancel: CancelToken,
    pub kernel: Mutex<Option<(Arc<ProcessService>, u64)>>,
    pub join: Mutex<Option<std::thread::JoinHandle<()>>>,
    pub permit: Arc<Permit>,
    pub kill_requested: AtomicBool,
    pub lost: AtomicBool,
    pub finished: AtomicBool,
}
impl Entry {
    fn unresolved(&self) -> bool {
        !self.finished.load(Ordering::Acquire) || {
            let view = self.view();
            view.persistence_error.is_some() || view.cleanup_error.is_some()
        }
    }
    pub(super) fn view(&self) -> std::sync::MutexGuard<'_, JobView> {
        self.view
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}
struct RegistryState {
    binding: Option<SessionId>,
    closed: bool,
    entries: BTreeMap<String, Arc<Entry>>,
}
type JobNotice = Arc<dyn Fn(JobView) + Send + Sync>;
pub(crate) struct JobRegistry {
    pub(super) scope: String,
    #[cfg(test)]
    pub(super) panic_next_monitor: AtomicBool,
    pub(super) sessions: Arc<SessionService>,
    pub(super) process: Arc<ProcessService>,
    pub(super) project: ProjectKey,
    capacity: Arc<JobCapacity>,
    recovery_error: Mutex<Option<JobError>>,
    state: Mutex<RegistryState>,
    pub(super) admission: Mutex<()>,
    pub(super) notice: Mutex<Option<JobNotice>>,
}
impl JobRegistry {
    pub(crate) fn new(
        sessions: Arc<SessionService>,
        process: Arc<ProcessService>,
        project: ProjectKey,
        capacity: Arc<JobCapacity>,
    ) -> Self {
        use sha2::{Digest, Sha256};
        let scope = format!(
            "{:x}",
            Sha256::digest(serde_json::to_vec(&(&capacity.root, &project.header_cwd)).unwrap())
        );
        Self {
            scope,
            #[cfg(test)]
            panic_next_monitor: AtomicBool::new(false),
            sessions,
            process,
            project,
            capacity,
            recovery_error: Mutex::new(None),
            state: Mutex::new(RegistryState {
                binding: None,
                closed: false,
                entries: BTreeMap::new(),
            }),
            admission: Mutex::new(()),
            notice: Mutex::new(None),
        }
    }
    pub(crate) fn recovery_failed(&self, error: JobError) {
        *self.recovery_error.lock().unwrap() = Some(error);
    }
    pub(crate) fn ready(&self) -> Result<(), JobError> {
        self.recovery_error
            .lock()
            .map_err(|_| JobError::new("unavailable"))?
            .clone()
            .map_or(Ok(()), Err)
    }
    pub(crate) fn try_list(&self) -> Result<Vec<JobView>, JobError> {
        self.ready()?;
        Ok(self.list())
    }
    pub(super) fn binding(&self) -> Option<SessionId> {
        self.state.lock().unwrap().binding.clone()
    }
    pub(crate) fn bind(&self, id: SessionId) {
        self.state.lock().unwrap().binding = Some(id);
    }
    pub(crate) fn unbind(&self) {
        self.state.lock().unwrap().binding = None;
    }
    pub(crate) fn set_notice(&self, notice: JobNotice) {
        *self.notice.lock().unwrap() = Some(notice);
    }
    pub(crate) fn has_active(&self) -> bool {
        if self.ready().is_err() {
            return true;
        }
        self.state
            .lock()
            .unwrap()
            .entries
            .values()
            .any(|e| e.unresolved())
    }
    pub(crate) fn has_session_work(&self, id: &str) -> bool {
        self.state.lock().unwrap().entries.values().any(|e| {
            let matches = e.view().session == id;
            matches && e.unresolved()
        })
    }
    pub(crate) fn list(&self) -> Vec<JobView> {
        self.state
            .lock()
            .unwrap()
            .entries
            .values()
            .map(|e| e.view().clone())
            .collect()
    }
    pub(super) fn try_entry(&self, id: &str) -> Result<Arc<Entry>, JobError> {
        self.state
            .try_lock()
            .map_err(|_| JobError::new("unavailable"))?
            .entries
            .get(id)
            .cloned()
            .ok_or_else(|| JobError::new("not_found"))
    }
    pub(super) fn entry(&self, id: &str) -> Result<Arc<Entry>, JobError> {
        self.state
            .lock()
            .map_err(|_| JobError::new("unavailable"))?
            .entries
            .get(id)
            .cloned()
            .ok_or_else(|| JobError::new("not_found"))
    }
    pub(super) fn prepare_entry(
        &self,
        label: String,
        expected: &SessionId,
    ) -> Result<Arc<Entry>, JobError> {
        self.ready()?;
        let mut state = self
            .state
            .lock()
            .map_err(|_| JobError::new("unavailable"))?;
        if state.closed {
            return Err(JobError::new("closed"));
        }
        let session = state
            .binding
            .clone()
            .ok_or_else(|| JobError::new("unbound"))?;
        if &session != expected {
            return Err(JobError::new("unbound"));
        }
        let mut completed: Vec<_> = state
            .entries
            .iter()
            .filter(|(_, e)| !e.unresolved())
            .map(|(id, e)| (e.view().started_at, id.clone()))
            .collect();
        completed.sort();
        while state.entries.len() >= 64 {
            let Some((_, id)) = completed.first().cloned() else {
                return Err(JobError::new("capacity"));
            };
            completed.remove(0);
            state.entries.remove(&id);
        }
        let permit = match self.capacity.acquire() {
            Ok(p) => p,
            Err(e) => {
                if let Some((_, id)) = completed.first() {
                    state.entries.remove(id);
                }
                self.capacity.acquire().map_err(|_| e)?
            }
        };
        let id = uuid::Uuid::new_v4().to_string();
        let view = JobView {
            reference: self.reference(&id, "registered"),
            id: id.clone(),
            project: self
                .project
                .header_cwd
                .clone()
                .ok_or_else(|| JobError::new("unbound"))?,
            session: session.to_string(),
            label,
            state: "registered".into(),
            started_at: now(),
            finished_at: None,
            observation: None,
            persistence_error: None,
            cleanup_error: None,
            notification_id: None,
            notification_error: None,
        };
        Ok(make_entry(view, permit, false))
    }
    pub(super) fn restore_entry(&self, view: JobView) -> Result<(), JobError> {
        let permit = self.capacity.acquire()?;
        self.insert(make_entry(view, permit, true));
        Ok(())
    }
    pub(super) fn insert(&self, entry: Arc<Entry>) {
        let id = entry.view().id.clone();
        self.state.lock().unwrap().entries.insert(id, entry);
    }
    pub(crate) fn kill(&self, id: &str) -> Result<JobView, JobError> {
        let entry = self.entry(id)?;
        entry.kill_requested.store(true, Ordering::Release);
        entry.cancel.cancel();
        let view = entry.view().clone();
        Ok(view)
    }
    pub(crate) fn read_output(&self, id: &str, offset: Option<u64>) -> Result<JobOutput, JobError> {
        let entry = self.entry(id)?;
        let agent = entry
            .agent_offset
            .lock()
            .map_err(|_| JobError::new("unavailable"))?;
        let offset = offset.unwrap_or(*agent);
        let ring = entry
            .output
            .lock()
            .map_err(|_| JobError::new("unavailable"))?;
        let (text, next, lost, more) = output::read_text(&ring, offset);
        let view = entry.view().clone();
        Ok(JobOutput {
            job: view,
            text,
            next_offset: next,
            lost: lost || entry.lost.load(Ordering::Acquire),
            more,
        })
    }
    pub(crate) fn agent_output(&self, id: &str) -> Result<JobOutput, JobError> {
        let result = self.read_output(id, None)?;
        let entry = self.entry(id)?;
        *entry.agent_offset.lock().unwrap() = result.next_offset;
        Ok(result)
    }
    pub(super) fn publish(&self, entry: &Entry) {
        let view = entry.view().clone();
        if let Some(notice) = self.notice.lock().unwrap().clone()
            && std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| notice(view))).is_err()
        {
            entry
                .view
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .notification_error = Some("notification sink panicked".into());
        }
    }
    pub(crate) fn close(&self) -> Result<(), JobError> {
        let _admission = self
            .admission
            .lock()
            .map_err(|_| JobError::new("unavailable"))?;
        let entries = {
            let mut state = self
                .state
                .lock()
                .map_err(|_| JobError::new("unavailable"))?;
            state.closed = true;
            state.entries.values().cloned().collect::<Vec<_>>()
        };
        for entry in &entries {
            entry.cancel.cancel();
        }
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut errors = Vec::new();
        for entry in entries {
            let handle = entry.join.lock().unwrap().take();
            if let Some(handle) = handle {
                while !handle.is_finished() && Instant::now() < deadline {
                    std::thread::sleep(Duration::from_millis(10));
                }
                if handle.is_finished() {
                    if handle.join().is_err() {
                        errors.push("job monitor panicked".to_owned());
                    }
                } else {
                    if let Some((kernel, _)) = entry.kernel.lock().unwrap().as_ref()
                        && let Err(e) = kernel.close()
                    {
                        errors.push(e);
                    }
                    errors.push("job monitor did not stop".into());
                }
            }
            if entry.finished.load(Ordering::Acquire) {
                let _ = self.retry_entry(&entry);
            }
            let view = entry.view();
            if let Some(e) = &view.persistence_error {
                errors.push(e.clone());
            }
            if let Some(e) = &view.cleanup_error {
                errors.push(e.clone());
            }
        }
        if errors.is_empty() {
            Ok(())
        } else {
            Err(JobError::new("cleanup_failed").detail(errors.join("; ")))
        }
    }
}

fn make_entry(view: JobView, permit: Arc<Permit>, recovered: bool) -> Arc<Entry> {
    if recovered {
        permit.settle();
    }
    Arc::new(Entry {
        view: Mutex::new(view),
        output: Mutex::new(ByteRing::new(MAX_OUTPUT)),
        agent_offset: Mutex::new(0),
        cancel: CancelToken::new(),
        kernel: Mutex::new(None),
        join: Mutex::new(None),
        permit,
        kill_requested: AtomicBool::new(false),
        lost: AtomicBool::new(recovered),
        finished: AtomicBool::new(recovered),
    })
}
