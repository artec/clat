//! Active-session resources and the unique writer acquisition point.
use super::*;

pub(super) struct ActiveSession {
    pub(super) key: SessionKey,
    pub(super) coordinator: Arc<SessionCoordinator>,
    pub(super) projections: Arc<Mutex<ProjectionRegistry>>,
    /// The replay fold produced by the same physical scan that arms a
    /// session. Later readers reuse this prefix and only visit a newly
    /// committed tail.
    pub(super) replay: Arc<Mutex<ResumeSink>>,
    /// The one shared folding journal for this session. Every producer
    /// (run recorder, todo, compaction, titles) must append through the
    /// SAME instance: per-handle pending lists interleaving across
    /// flushes created seq gaps that desynced the surface projection
    /// from `events[i].seq == i`.
    journal: Mutex<Option<Arc<dyn RunJournal>>>,
    generation: AtomicU64,
}

impl ActiveSession {
    pub(super) fn new(
        key: SessionKey,
        coordinator: Arc<SessionCoordinator>,
        projections: Arc<Mutex<ProjectionRegistry>>,
        replay: Arc<Mutex<ResumeSink>>,
    ) -> Self {
        Self {
            key,
            coordinator,
            projections,
            replay,
            journal: Mutex::new(None),
            generation: AtomicU64::new(0),
        }
    }
    #[cfg(test)]
    pub(super) fn replace_journal_for_test(&self, journal: Option<Arc<dyn RunJournal>>) {
        *self.journal.lock().expect("journal") = journal;
    }

    /// Every producer borrows the same append/flush lane; no caller assembles it.
    pub(super) fn shared_journal(&self, backend: &Arc<JsonlBackend>) -> Arc<dyn RunJournal> {
        let mut slot = self.journal.lock().expect("session journal");
        Arc::clone(slot.get_or_insert_with(|| journal_with_projection_fold(self, backend)))
    }
}

pub(super) fn active_floor(active: &ActiveSession) -> u64 {
    let projections = active.projections.lock().expect("projections");
    projections.live_floor()
}

/// Fold only when the projections lag the durable cursor: explicit journal
/// flushes fold their committed batches directly (P1-13), so a whole-file
/// physical read is reserved for events that reached the log outside the
/// folding journal (write-behind deadlines, resume seed markers, repairs).
pub(super) fn fold_if_behind(
    active: &ActiveSession,
    backend: &JsonlBackend,
) -> Result<(), SessionError> {
    let floor = active_floor(active);
    let committed = active.coordinator.committed_seq();
    if committed.is_some_and(|committed| floor <= committed) {
        return fold_committed(active, backend, floor);
    }
    Ok(())
}

pub(super) fn fold_committed(
    active: &ActiveSession,
    backend: &JsonlBackend,
    floor: u64,
) -> Result<(), SessionError> {
    let mut projections = active.projections.lock().expect("projections");
    backend.visit_from(&active.key, floor, &mut |event| projections.fold_one(event))?;
    Ok(())
}

pub(super) fn checkpoint_active(
    active: &ActiveSession,
    checkpoints: &CheckpointStore,
) -> Result<(), SessionError> {
    if active.coordinator.is_read_only() {
        return Ok(());
    }
    // A lazy session with no committed event has no authoritative log and
    // therefore must not materialize a checkpoint-only ghost directory.
    if active.coordinator.committed_seq().is_none() {
        return Ok(());
    }
    // Identity comes from the coordinator's header — no extra full-log
    // read just to learn what we already hold (P1-13).
    let identity = CheckpointIdentity::of(active.coordinator.header());
    let projections = active.projections.lock().expect("projections");
    let generation = active.generation.fetch_add(1, Ordering::Relaxed) + 1;
    let record = projections.checkpoint_bounded(identity, generation, CHECKPOINT_BYTE_CAP);
    drop(projections);
    // Cache writes are fail-soft: a failure means a longer replay next time.
    let _ = checkpoints.save(&active.key, &record);
    Ok(())
}

fn journal_with_projection_fold(
    active: &ActiveSession,
    backend: &Arc<JsonlBackend>,
) -> Arc<dyn RunJournal> {
    Arc::new(ProjectionFoldJournal::new(
        active.coordinator.journal(),
        Arc::clone(&active.projections),
        Arc::clone(backend),
        active.key.clone(),
    ))
}

/// Byte budget for one checkpoint record. Every row is derived and may be
/// omitted; the authoritative log rebuilds it. This caps the final file,
/// not merely the surface row: transcript/request data can also be large.
pub(super) const CHECKPOINT_BYTE_CAP: usize = 8 * 1024 * 1024;
