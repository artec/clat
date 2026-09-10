//! Owns serialized append/flush registration and committed projection catch-up.
use super::{
    Arc, JsonlBackend, Mutex, ProjectionRegistry, RunJournal, SessionEvent, SessionKey, now_ms,
};

pub(super) struct ProjectionFoldJournal {
    inner: Arc<dyn RunJournal>,
    /// Couples queue admission with pending-fold registration, and the
    /// durable flush with draining that registration. A concurrent flush
    /// must not commit an event in the small gap after the inner append
    /// returns but before its projection copy enters `pending`.
    lane: Mutex<()>,
    /// Events appended through this handle since its last flush, already
    /// carrying their committed seqs: folding them directly avoids a whole
    /// physical log re-read per explicit flush (audit P1-13). Bounded by
    /// one turn's events — the same order the recorder accumulates its
    /// own text; folding earlier would violate fold-after-Committed.
    pending: Mutex<Vec<SessionEvent>>,
    projections: Arc<Mutex<ProjectionRegistry>>,
    /// Contiguity fallback: some events reach the coordinator without
    /// passing through this handle (the resume seed marker enqueued by
    /// `SessionCoordinator::start`). When the pending batch does not start
    /// exactly at the projections' floor, direct folding would create a
    /// seq gap and break `events[i].seq == i` — fold the committed tail
    /// from the physical log instead.
    backend: Arc<JsonlBackend>,
    active_key: SessionKey,
}

impl ProjectionFoldJournal {
    pub(super) fn new(
        inner: Arc<dyn RunJournal>,
        projections: Arc<Mutex<ProjectionRegistry>>,
        backend: Arc<JsonlBackend>,
        active_key: SessionKey,
    ) -> Self {
        Self {
            inner,
            projections,
            backend,
            active_key,
            lane: Mutex::new(()),
            pending: Mutex::new(Vec::new()),
        }
    }

    fn build_events(
        &self,
        events: &[crate::session::run_journal::NewSessionEvent],
    ) -> Result<Vec<SessionEvent>, String> {
        let time = now_ms();
        events
            .iter()
            .map(|new_event| {
                Ok(SessionEvent {
                    event_type: new_event.event_type.clone(),
                    seq: 0,
                    time,
                    data: new_event.data.clone(),
                    ignorable: new_event.ignorable,
                    surface_op: new_event.surface_op.clone(),
                    source_event_seqs: new_event.source_event_seqs.clone(),
                    extra: serde_json::Map::new(),
                })
            })
            .collect()
    }

    /// Fold the already-durable subset of this shared handle's pending
    /// projection copies. Caller holds `lane`, so append registration and
    /// flush publication cannot interleave.
    fn fold_committed_pending(&self) -> Result<(), String> {
        let mut pending = std::mem::take(&mut *self.pending.lock().expect("fold journal"));
        if pending.is_empty() {
            return Ok(());
        }
        if let Some(committed) = self.inner.committed_seq()
            && let split = pending.partition_point(|event| event.seq <= committed)
            && split < pending.len()
        {
            let retained = pending.split_off(split);
            self.pending.lock().expect("fold journal").extend(retained);
        }
        if pending.is_empty() {
            return Ok(());
        }
        let floor = {
            let projections = self.projections.lock().expect("projections");
            projections.live_floor()
        };
        let contiguous = pending.first().is_some_and(|event| event.seq == floor);
        let mut projections = self.projections.lock().expect("projections");
        if contiguous {
            return projections.fold_all(&pending);
        }
        let (_, tail) = self
            .backend
            .read_from(&self.active_key, floor)
            .map_err(|error| error.to_string())?;
        projections.fold_all(&tail)
    }
}

impl RunJournal for ProjectionFoldJournal {
    fn append_atomic(
        &self,
        events: &[crate::session::run_journal::NewSessionEvent],
    ) -> Result<crate::session::run_journal::SeqRange, String> {
        let _lane = self.lane.lock().expect("projection fold lane");
        let range = self.inner.append_atomic(events)?;
        if let Ok(mut built) = self.build_events(events) {
            for (offset, event) in built.iter_mut().enumerate() {
                event.seq = range.start + offset as u64;
            }
            self.pending.lock().expect("fold journal").extend(built);
        }
        Ok(range)
    }
    fn flush(&self) -> Result<(), String> {
        let _lane = self.lane.lock().expect("projection fold lane");
        self.inner.flush()?;
        // Fold exactly what is durably committed since the last flush —
        // the physical log is the authority, but re-reading it per flush
        // made cost grow with total log size (P1-13). The lane prevents
        // append/registration from interleaving with this flush; the
        // committed-cursor cutoff still protects direct coordinator events
        // and any future producer that bypasses this wrapper (folding an
        // uncommitted event that later rolls back would push projections
        // ahead of the log and open a seq hole).
        self.fold_committed_pending()
    }

    fn append_atomic_durable(
        &self,
        events: &[crate::session::run_journal::NewSessionEvent],
    ) -> Result<crate::session::run_journal::SeqRange, String> {
        let _lane = self.lane.lock().expect("projection fold lane");
        let mut built = self.build_events(events)?;
        let range = self.inner.append_atomic_durable(events)?;
        for (offset, event) in built.iter_mut().enumerate() {
            event.seq = range.start + offset as u64;
        }
        self.pending.lock().expect("fold journal").extend(built);
        // Durability is already decided. Projection/checkpoint state is a
        // rebuildable cache, so a fold refresh failure must not turn a
        // committed permission change into a reported denial.
        if let Err(error) = self.fold_committed_pending() {
            eprintln!(
                "clat: warning: projection refresh failed after durable journal transaction: {error}"
            );
        }
        Ok(range)
    }

    fn committed_seq(&self) -> Option<u64> {
        self.inner.committed_seq()
    }
}
