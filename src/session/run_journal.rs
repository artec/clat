//! `RunJournal` + per-session coordinator (plan §8.5, stage-4 harness).
//! The journal owns seq assignment under one lock: an atomic group gets
//! contiguous seqs and enters the write-behind queue under the same lock,
//! so the batch window can never split it. Clients observe durability via
//! `flush()`; the final RunEvent may only be published after it succeeds.

use crate::session::event::{SessionEvent, SurfaceOp};
use crate::session::header::SessionHeader;
use crate::session::key::SessionKey;
use crate::session::persistence::{AppendFailure, JsonlBackend, PreparedSession, SessionError};
use crate::session::write_behind::{SessionWriteBehind, WRITE_BATCH_MAX_DELAY};
use serde_json::Value;
use std::sync::{Arc, Mutex};

/// An event awaiting its seq — what producers build.
#[derive(Clone, Debug)]
pub(crate) struct NewSessionEvent {
    pub(crate) event_type: String,
    pub(crate) data: Value,
    pub(crate) ignorable: Option<bool>,
    pub(crate) surface_op: Option<SurfaceOp>,
    pub(crate) source_event_seqs: Option<Vec<u64>>,
}

impl NewSessionEvent {
    pub(crate) fn new(event_type: &str, data: Value) -> Self {
        Self {
            event_type: event_type.into(),
            data,
            ignorable: None,
            surface_op: None,
            source_event_seqs: None,
        }
    }

    pub(crate) fn append(mut self, sources: Vec<u64>) -> Self {
        self.surface_op = Some(SurfaceOp::Append);
        self.source_event_seqs = (!sources.is_empty()).then_some(sources);
        self
    }

    /// A surface replacement shadowing the closed seq range `start..=end`
    /// (surface-node seqs); every shadowed node seq must appear in sources.
    pub(crate) fn replace(mut self, start: u64, end: u64, sources: Vec<u64>) -> Self {
        self.surface_op = Some(SurfaceOp::Replace { start, end });
        self.source_event_seqs = (!sources.is_empty()).then_some(sources);
        self
    }

    /// Mark the event ignorable (readers may skip unknown types).
    pub(crate) fn log_only(mut self) -> Self {
        self.ignorable = Some(true);
        self
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct SeqRange {
    pub(crate) start: u64,
    pub(crate) end_inclusive: u64,
}

/// UI-independent persistence port for one run (plan §8.5).
pub(crate) trait RunJournal: Send + Sync {
    fn append_atomic(&self, events: &[NewSessionEvent]) -> Result<SeqRange, String>;
    fn append(&self, event: NewSessionEvent) -> Result<u64, String> {
        let range = self.append_atomic(&[event])?;
        Ok(range.start)
    }
    fn flush(&self) -> Result<(), String>;
    /// The last durably committed seq, when the journal can report it.
    /// Folding consumers use it to never fold an event ahead of its
    /// commit (fold-after-Committed, plan §11.2).
    fn committed_seq(&self) -> Option<u64> {
        None
    }
}

/// One authoritative writer per session (plan §8.4): assigns seqs, feeds
/// the write-behind queue, rotates the backend handle, and remembers an
/// Unknown outcome so the run must stop.
pub(crate) struct SessionCoordinator {
    key: SessionKey,
    header: SessionHeader,
    backend: Arc<JsonlBackend>,
    inner: Arc<Mutex<CoordinatorCore>>,
    writer: SessionWriteBehind,
    needs_seed_marker: std::sync::atomic::AtomicBool,
}

struct CoordinatorCore {
    handle: Option<PreparedSession>,
    next_seq: u64,
    /// Set after an Unknown commit outcome: the run must terminate; only a
    /// cold load(repair) clears the backend-side poison.
    fatal: Option<String>,
}

impl SessionCoordinator {
    pub(crate) fn start(
        backend: Arc<JsonlBackend>,
        key: SessionKey,
        header: SessionHeader,
    ) -> Result<Arc<Self>, SessionError> {
        let coordinator = Self::start_unseeded(backend, key, header)?;
        coordinator.enqueue_seed_marker_if_needed();
        Ok(coordinator)
    }

    /// Prepare the durable handle and writer without publishing the DSH
    /// resume seed. Session switching uses this before the workspace CAS:
    /// every fallible read/repair/restore step is complete, while a failed
    /// CAS can still close the empty writer without growing the target log.
    pub(crate) fn start_unseeded(
        backend: Arc<JsonlBackend>,
        key: SessionKey,
        header: SessionHeader,
    ) -> Result<Arc<Self>, SessionError> {
        // Resume an existing log, or keep the freshly created lazy handle
        // (materialization happens on the first durable batch).
        let handle = match backend.prepare(&key) {
            Ok(handle) => handle,
            Err(SessionError::NotFound(_)) => backend.create(key.clone(), header)?,
            Err(error) => return Err(error),
        };
        let needs_seed_marker = handle.needs_seed_marker;
        let header = handle.header.clone();
        let next_seq = handle.next_seq();
        let core = Arc::new(Mutex::new(CoordinatorCore {
            handle: Some(handle),
            next_seq,
            fatal: None,
        }));
        let writer_core = Arc::clone(&core);
        let writer_backend = Arc::clone(&backend);
        let writer = SessionWriteBehind::new(WRITE_BATCH_MAX_DELAY, move |batch| {
            write_batch(&writer_backend, &writer_core, batch)
        });
        let coordinator = Arc::new(Self {
            key,
            header,
            backend,
            inner: core,
            writer,
            needs_seed_marker: std::sync::atomic::AtomicBool::new(needs_seed_marker),
        });
        Ok(coordinator)
    }

    /// Publish the one resume seed only after the workspace selection CAS
    /// committed. Queue admission is infallible for a freshly armed,
    /// unclosed coordinator; durability remains on the normal batch lane.
    pub(crate) fn enqueue_seed_marker_if_needed(&self) {
        if self
            .needs_seed_marker
            .swap(false, std::sync::atomic::Ordering::AcqRel)
        {
            self.enqueue_atomic(vec![SessionEvent::new(
                "session/end-seed",
                0, // seq assigned under the coordinator lock
                crate::session::event::now_ms(),
                serde_json::json!({}),
            )])
            .expect("freshly armed session writer accepts its seed marker");
        }
    }

    pub(crate) fn journal(self: &Arc<Self>) -> Arc<dyn RunJournal> {
        Arc::new(JournalAdapter {
            coordinator: Arc::clone(self),
        })
    }

    pub(crate) fn key(&self) -> &SessionKey {
        &self.key
    }

    pub(crate) fn header(&self) -> &SessionHeader {
        &self.header
    }

    fn enqueue_atomic(&self, events: Vec<SessionEvent>) -> Result<SeqRange, String> {
        if events.is_empty() {
            return Err("cannot append an empty event group".into());
        }
        let mut events = events;
        let range = {
            // Assign contiguous seqs under the same lock that admits the
            // group into the queue: the window can never split it.
            let mut core = self.inner.lock().expect("coordinator lock");
            if let Some(fatal) = &core.fatal {
                return Err(fatal.clone());
            }
            let start = core.next_seq;
            let end_inclusive = start + events.len() as u64 - 1;
            for (offset, event) in events.iter_mut().enumerate() {
                event.seq = start + offset as u64;
            }
            // Queue admission and cursor publication are one transaction.
            // A closed writer must not consume an invisible seq range.
            self.writer.enqueue(events)?;
            core.next_seq = end_inclusive + 1;
            SeqRange {
                start,
                end_inclusive,
            }
        };
        Ok(range)
    }

    /// Drain the write-behind lane without retiring the writer (contrast
    /// [`Self::close`]): a read barrier so a same-process full-log stream
    /// (`replay` callers) cannot observe our own pending batch as a foreign
    /// mid-stream change. A failure keeps the batch queued for the normal
    /// retry lane (write-behind semantics).
    pub(crate) fn flush(&self) -> Result<(), String> {
        self.writer.flush()?;
        let core = self.inner.lock().expect("coordinator lock");
        if let Some(fatal) = &core.fatal {
            return Err(fatal.clone());
        }
        Ok(())
    }

    /// The last durable seq (batch starts assign seqs optimistically; the
    /// committed cursor is what the handle advanced to). Readers use this
    /// to skip physical re-reads when projections are already current.
    pub(crate) fn committed_seq(&self) -> Option<u64> {
        let core = self.inner.lock().expect("coordinator lock");
        core.handle
            .as_ref()
            .and_then(|handle| handle.next_seq().checked_sub(1))
    }

    /// Flush + join the writer; used at session detach / project close.
    /// The join is the anti-leak guarantee (audit P1-07): every session
    /// switch retires exactly one writer thread.
    pub(crate) fn close(&self) -> Result<(), String> {
        let result = self.writer.close();
        let mut core = self.inner.lock().expect("coordinator lock");
        if let Some(fatal) = &core.fatal {
            return Err(fatal.clone());
        }
        // Drop the handle so the backend state reflects the last durable
        // cursor on the next prepare.
        core.handle.take();
        result
    }
}

fn write_batch(
    backend: &JsonlBackend,
    core: &Mutex<CoordinatorCore>,
    batch: &[SessionEvent],
) -> Result<(), String> {
    let mut core = core.lock().expect("coordinator lock");
    if let Some(fatal) = &core.fatal {
        return Err(fatal.clone());
    }
    let Some(handle) = core.handle.take() else {
        return Err("session writer lost its handle".into());
    };
    let expected = batch
        .first()
        .map(|event| event.seq)
        .unwrap_or(handle.next_seq());
    match backend.append_batch(handle, expected, batch) {
        Ok(advanced) => {
            core.handle = Some(advanced);
            Ok(())
        }
        Err(AppendFailure::NotCommitted { session, error }) => {
            // Proven rollback: keep the handle, surface the error for the
            // flush boundary; the automatic path pauses until the next
            // enqueue (write-behind semantics).
            core.handle = Some(*session);
            Err(error)
        }
        Err(AppendFailure::Unknown { error }) => {
            // Indeterminate commit: the handle is consumed, the run must
            // stop, and only a cold load(repair) may continue.
            core.fatal = Some(format!(
                "session append outcome unknown, run must stop: {error}"
            ));
            Err(core.fatal.clone().expect("just set"))
        }
    }
}

struct JournalAdapter {
    coordinator: Arc<SessionCoordinator>,
}

impl RunJournal for JournalAdapter {
    fn append_atomic(&self, events: &[NewSessionEvent]) -> Result<SeqRange, String> {
        // Validate the whole group first, then assign contiguous seqs and
        // enqueue under the same lock (plan §8.5).
        let time = crate::session::event::now_ms();
        let built = events
            .iter()
            .map(|new_event| {
                validate_new_event(new_event)?;
                let seq_placeholder = 0u64;
                Ok(SessionEvent {
                    event_type: new_event.event_type.clone(),
                    seq: seq_placeholder,
                    time,
                    data: new_event.data.clone(),
                    ignorable: new_event.ignorable,
                    surface_op: new_event.surface_op.clone(),
                    source_event_seqs: new_event.source_event_seqs.clone(),
                    extra: serde_json::Map::new(),
                })
            })
            .collect::<Result<Vec<_>, String>>()?;
        self.coordinator.enqueue_atomic(built)
    }

    fn flush(&self) -> Result<(), String> {
        self.coordinator.flush()
    }

    fn committed_seq(&self) -> Option<u64> {
        self.coordinator.committed_seq()
    }
}

fn validate_new_event(event: &NewSessionEvent) -> Result<(), String> {
    let is_surface = crate::session::catalog::is_surface_type(&event.event_type);
    if is_surface && event.surface_op.is_none() {
        return Err(format!(
            "surface event `{}` requires a surfaceOp",
            event.event_type
        ));
    }
    if !is_surface && (event.surface_op.is_some() || event.source_event_seqs.is_some()) {
        return Err(format!(
            "non-surface event `{}` must not carry surface metadata",
            event.event_type
        ));
    }
    if !event.data.is_object() && !event.data.is_null() {
        return Err("event data must be a JSON object".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::event::payloads;
    use crate::session::header::SessionHeader;
    use crate::session::id::SessionId;
    use crate::session::key::ProjectKey;
    use crate::session::persistence::JsonlCompression;
    use serde_json::json;

    fn setup(
        tag: &str,
    ) -> (
        Arc<JsonlBackend>,
        SessionKey,
        Arc<SessionCoordinator>,
        std::path::PathBuf,
    ) {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let root = std::env::temp_dir().join(format!("clat-journal-{tag}-{unique}"));
        let backend = Arc::new(JsonlBackend::new(&root, JsonlCompression::Zstd, true));
        let key = SessionKey {
            project: ProjectKey::from_cwd("/tmp/journal-project"),
            id: SessionId::new("s1"),
        };
        let coordinator = SessionCoordinator::start(
            Arc::clone(&backend),
            key.clone(),
            SessionHeader::new(key.id.clone(), key.project.header_cwd.clone(), 1),
        )
        .expect("start coordinator");
        (backend, key, coordinator, root)
    }

    fn turn_start(turn: u64) -> NewSessionEvent {
        NewSessionEvent::new("turn/start", payloads::turn_start(turn))
    }

    fn user_message(text: &str) -> NewSessionEvent {
        NewSessionEvent::new("user/message", payloads::user_message(text)).append(Vec::new())
    }

    #[test]
    fn first_turn_batch_is_atomic_and_durable_before_the_model_runs() {
        let (backend, key, coordinator, root) = setup("first-batch");
        let journal = coordinator.journal();

        // The first logical group enters as one atomic append (A-02).
        let range = journal
            .append_atomic(&[turn_start(1), user_message("fix the bug")])
            .expect("atomic group");
        assert_eq!(
            range,
            SeqRange {
                start: 0,
                end_inclusive: 1
            }
        );
        journal.flush().expect("first-batch flush");

        // Durable before any model call: a fresh load sees both events.
        let loaded = backend.load(&key, false).expect("load");
        assert_eq!(
            loaded
                .events
                .iter()
                .map(|event| event.event_type.as_str())
                .collect::<Vec<_>>(),
            vec!["turn/start", "user/message"]
        );
        coordinator.close().expect("close");
        std::fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn window_batches_chunks_and_turn_end_flushes_everything() {
        let (backend, key, coordinator, root) = setup("window");
        let journal = coordinator.journal();
        journal
            .append_atomic(&[turn_start(1), user_message("hello")])
            .expect("first");
        journal.flush().expect("flush first");

        for index in 0..5 {
            journal
                .append(NewSessionEvent::new(
                    "assistant/chunk",
                    payloads::assistant_chunk(
                        1,
                        0,
                        json!({ "type": "text-delta", "index": 0, "text": format!("t{index}") }),
                    ),
                ))
                .expect("chunk");
        }
        // The window has not elapsed: nothing beyond the first batch is
        // durable yet (write-behind, provisional by design).
        let before = backend.load(&key, false).expect("load");
        assert_eq!(before.events.len(), 2, "chunks are provisional until flush");
        journal
            .append(NewSessionEvent::new(
                "turn/end",
                payloads::turn_end(1, &crate::session::event::TurnEndReason::Completed),
            ))
            .expect("turn end");
        journal.flush().expect("turn flush");

        let loaded = backend.load(&key, false).expect("load");
        assert_eq!(loaded.events.len(), 8);
        for (index, event) in loaded.events.iter().enumerate() {
            assert_eq!(event.seq, index as u64);
        }
        assert_eq!(loaded.events.last().unwrap().event_type, "turn/end");
        coordinator.close().expect("close");
        std::fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn unknown_outcome_stops_the_journal_and_requires_cold_recovery() {
        let (backend, key, coordinator, root) = setup("unknown");
        let journal = coordinator.journal();
        journal
            .append_atomic(&[turn_start(1), user_message("x")])
            .expect("first");
        journal.flush().expect("flush first");

        // fsync + rollback-fsync both fail on the NEXT batch → Unknown.
        backend.inject_faults(crate::session::persistence::FaultHooks {
            fail_batch_fsync: true,
            fail_rollback_fsync: true,
            ..Default::default()
        });
        journal
            .append(NewSessionEvent::new(
                "turn/end",
                payloads::turn_end(1, &crate::session::event::TurnEndReason::Completed),
            ))
            .expect("enqueue");
        let flush_error = journal.flush().expect_err("flush surfaces the outcome");
        assert!(
            flush_error.contains("unknown"),
            "flush error: {flush_error}"
        );

        // The run is over: any further journal use refuses.
        let refused = journal.append(turn_start(2));
        assert!(refused.is_err());

        // Cold recovery decides what actually committed and re-arms: the
        // rollback truncate ran before its fsync failed, so the uncertain
        // batch is gone and recovery closes the open turn synthetically.
        let recovered = backend.load(&key, true).expect("cold recovery");
        assert_eq!(recovered.events.len(), 3);
        let closer = recovered.events.last().expect("closer");
        assert_eq!(closer.event_type, "turn/end");
        assert_eq!(closer.data["reason"]["kind"], "interrupted");
        let _ = coordinator.close();
        std::fs::remove_dir_all(root).expect("cleanup");
    }

    /// Side-effect durability barrier (plan §9.2): the approval request is
    /// flushed BEFORE the approver sees it, and the allow decision pairs
    /// with tool/call in one atomic group flushed BEFORE invoke.
    #[test]
    fn approval_barrier_orders_durable_events_around_the_approver() {
        let (backend, key, coordinator, root) = setup("barrier");
        let journal = coordinator.journal();
        journal
            .append_atomic(&[turn_start(1), user_message("run the tool")])
            .expect("first");
        journal.flush().expect("flush first");

        // 1. approval/asked durable before asking the human.
        let asked = NewSessionEvent::new(
            "approval/asked",
            payloads::approval_asked("req-1", "write_file", Some("call-1"), "side effects"),
        );
        journal.append(asked).expect("asked");
        journal.flush().expect("asked durable");
        let after_ask = backend.load(&key, false).expect("load");
        assert!(
            after_ask
                .events
                .iter()
                .any(|event| event.event_type == "approval/asked")
        );

        // (The approver runs here — allow.)
        // 2. decision + tool/call as one atomic group, flushed pre-invoke.
        journal
            .append_atomic(&[
                NewSessionEvent::new(
                    "approval/decided",
                    payloads::approval_decided("req-1", "allowed-once"),
                ),
                NewSessionEvent::new(
                    "tool/call",
                    json!({ "turn": 1, "step": 0, "callId": "call-1", "name": "write_file", "arguments": "{\"path\":\"a\"}" }),
                ),
            ])
            .expect("decision + call");
        journal.flush().expect("pre-invoke durable");
        let after_allow = backend.load(&key, false).expect("load");
        let tail: Vec<&str> = after_allow.events[3..]
            .iter()
            .map(|e| e.event_type.as_str())
            .collect();
        assert_eq!(tail, vec!["approval/decided", "tool/call"]);

        // 3. (Tool::invoke happens here.) Result afterwards:
        journal
            .append(NewSessionEvent::new(
                "tool/result",
                json!({
                    "turn": 1, "step": 0,
                    "message": {
                        "id": "m1", "role": "user",
                        "content": [{ "type": "tool-result", "toolCallId": "call-1", "isError": false,
                                      "content": [{ "type": "text", "text": "written" }] }],
                        "source": { "kind": "tool", "callId": "call-1" },
                    },
                }),
            ).append(Vec::new()))
            .expect("result");
        journal.flush().expect("result durable");
        let loaded = backend.load(&key, true).expect("final load");
        assert_eq!(loaded.events.len(), 7);
        // Recovery closes the still-open turn with interrupted... wait: no
        // turn/end was written, so repair synthesizes one — proving the
        // three-way recovery distinction holds through the journal too.
        assert!(loaded.events.last().unwrap().data["reason"]["kind"] == "interrupted");
        coordinator.close().expect("close");
        std::fs::remove_dir_all(root).expect("cleanup");
    }
}
