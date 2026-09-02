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
    /// Append one atomic group and synchronously decide its durability.
    /// Production journals override this to remove a proven-not-committed
    /// tail from write-behind; the default preserves simple test doubles.
    fn append_atomic_durable(&self, events: &[NewSessionEvent]) -> Result<SeqRange, String> {
        let range = self.append_atomic(events)?;
        self.flush()?;
        Ok(range)
    }
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
    /// Serializes normal queue admission with synchronous reversible
    /// transactions without blocking the worker's `CoordinatorCore` lock.
    transaction: Mutex<()>,
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
    /// 测试探针：本协调器 writer 线程的存活标志句柄——drop 协调器后
    /// 仍可轮询，验证 Drop 安全网真的 join 了线程。
    #[cfg(test)]
    pub(crate) fn writer_alive_handle_for_test(&self) -> Arc<std::sync::atomic::AtomicBool> {
        self.writer.worker_alive_handle_for_test()
    }

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
        Self::start_unseeded_with_visitor(backend, key, header, &mut |_| Ok(()))
            .map(|(coordinator, _)| coordinator)
    }

    /// [`Self::start_unseeded`] 的单遍变体（R-1）：prepare 的物理扫描
    /// 同时把每个事件交给 `visitor`（冷 resume 在同一次扫描里折叠投
    /// 影、构建回放、累计 usage）。返回的 `visitor_applied` 为 false 时
    /// 走过撕裂修复路径，visitor 输出不可信、调用方必须重读。
    /// `NotFound`（全新会话）下 visitor 一事件未见，视为已应用。
    pub(crate) fn start_unseeded_with_visitor(
        backend: Arc<JsonlBackend>,
        key: SessionKey,
        header: SessionHeader,
        visitor: &mut dyn FnMut(&SessionEvent) -> Result<(), String>,
    ) -> Result<(Arc<Self>, bool), SessionError> {
        // Resume an existing log, or keep the freshly created lazy handle
        // (materialization happens on the first durable batch).
        let (handle, visitor_applied) = match backend.prepare_with_visitor(&key, visitor) {
            Ok((handle, applied)) => (handle, applied),
            Err(SessionError::NotFound(_)) => (backend.create(key.clone(), header)?, true),
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
            transaction: Mutex::new(()),
            writer,
            needs_seed_marker: std::sync::atomic::AtomicBool::new(needs_seed_marker),
        });
        Ok((coordinator, visitor_applied))
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
        let _transaction = self.transaction.lock().expect("journal transaction");
        self.enqueue_atomic_locked(events)
    }

    fn enqueue_atomic_locked(&self, events: Vec<SessionEvent>) -> Result<SeqRange, String> {
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

    fn enqueue_atomic_durable(&self, events: Vec<SessionEvent>) -> Result<SeqRange, String> {
        let _transaction = self.transaction.lock().expect("journal transaction");
        let range = self.enqueue_atomic_locked(events)?;
        if let Err(error) = self.writer.flush() {
            let fatal = self.inner.lock().expect("coordinator lock").fatal.clone();
            if fatal.is_none() {
                {
                    let core = self.inner.lock().expect("coordinator lock");
                    if core.next_seq != range.end_inclusive.saturating_add(1) {
                        return Err(format!(
                            "cannot roll back a non-tail session transaction after: {error}"
                        ));
                    }
                }
                self.writer
                    .discard_pending_tail(range.start, range.end_inclusive)?;
                self.inner.lock().expect("coordinator lock").next_seq = range.start;
            }
            return Err(error);
        }
        let core = self.inner.lock().expect("coordinator lock");
        if let Some(fatal) = &core.fatal {
            return Err(fatal.clone());
        }
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

impl Drop for SessionCoordinator {
    fn drop(&mut self) {
        // 安全网（2026-08-19 CI 失败）：显式 close 之外的最后一个
        // `Arc` 落下时也必须退役 writer——JoinHandle 的 drop 是分离
        // 而非 join，worker 会在 condvar 上永生（真实事故：一个测试
        // 忘了 quiesce，泄漏的 writer 把并行套件里任何
        // `wait_for_writer_baseline` 的 30s 窗口顶红，慢速 CI 上必现）。
        // close 幂等（显式路径先走，这里只兜底）；flush 尽力而为，
        // 失败无处上报，静默。
        let _ = self.close();
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

    fn append_atomic_durable(&self, events: &[NewSessionEvent]) -> Result<SeqRange, String> {
        let time = crate::session::event::now_ms();
        let built = events
            .iter()
            .map(|new_event| {
                validate_new_event(new_event)?;
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
            .collect::<Result<Vec<_>, String>>()?;
        self.coordinator.enqueue_atomic_durable(built)
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
        crate::test_support::cleanup_tree(&root);
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
        crate::test_support::cleanup_tree(&root);
    }

    /// DU-1（2026-09-02，对照 DSH 2026-08-30 外部插件 ignorable 事件
    /// 保留决策）：未知类型 + `ignorable: true` 信封（DSH 外部插件事件
    /// 的词表形态）在 CLAT 日志里的**可重载行为**——写入侧信封字段透传
    /// （round-trip 还原 `Some(true)`）、读取侧准入放行、重放跳过不重
    /// 建、resume 的 seq 游标跨过外部事件继续追加、重载后外部事件与
    /// CLAT 事件共存且 seq 连续。fail-closed 对照腿（未知且无
    /// ignorable → resume 拒绝）由 persistence 的
    /// `inspect_preserves_required_unknown_but_resume_rejects_it` 钉住。
    #[test]
    fn external_ignorable_events_are_reloadable_across_resume() {
        let (backend, key, coordinator, root) = setup("external-ignorable");
        let journal = coordinator.journal();
        journal
            .append_atomic(&[turn_start(1), user_message("hello")])
            .expect("first");
        journal
            .append(
                NewSessionEvent::new("cordis-plugin/foreign-note", json!({"vendor": "tree-out"}))
                    .log_only(),
            )
            .expect("external ignorable event journals");
        // 干净收尾本轮：resume 侧才不会先走撕裂修复的合成关闭。
        journal
            .append(NewSessionEvent::new(
                "turn/end",
                payloads::turn_end(1, &crate::session::event::TurnEndReason::Completed),
            ))
            .expect("turn end");
        journal.flush().expect("durable");
        coordinator.close().expect("close");

        // 读取侧：未知 + ignorable 准入放行，信封字段经物理文件
        // round-trip 还原。
        let loaded = backend.load(&key, false).expect("admission passes");
        assert_eq!(loaded.events.len(), 4);
        let external = &loaded.events[2];
        assert_eq!(external.event_type, "cordis-plugin/foreign-note");
        assert_eq!(external.ignorable, Some(true), "the envelope flag survives");

        // 重放：外部事件跳过不重建，用户消息正常还原。
        let replay = crate::session::replay::ReplayAdapter::fold(&loaded.events);
        assert!(replay.iter().any(|event| matches!(
            event,
            crate::session::replay::ReplayEvent::UserMessage { text, .. } if text == "hello"
        )));

        // resume：coordinator 在同一日志上重开。日志末尾不是
        // session/end-seed，resume 协议先补一个 seed marker（seq 4），
        // 新事件从 seq 5 继续——seq 游标跨过外部事件与 seed 正常前进。
        let coordinator =
            SessionCoordinator::start(Arc::clone(&backend), key.clone(), loaded.header.clone())
                .expect("resume past the external event");
        let journal = coordinator.journal();
        let next_seq = journal.append(turn_start(2)).expect("append continues");
        assert_eq!(
            next_seq, 5,
            "resume appends its end-seed (seq 4), then continues past the external event"
        );
        journal.flush().expect("flush");
        coordinator.close().expect("close");

        let reloaded = backend.load(&key, false).expect("reload");
        let seqs: Vec<u64> = reloaded.events.iter().map(|event| event.seq).collect();
        assert_eq!(
            seqs,
            vec![0, 1, 2, 3, 4, 5],
            "seqs stay contiguous across the external event and the resume seed"
        );
        assert_eq!(reloaded.events[2].event_type, "cordis-plugin/foreign-note");
        assert_eq!(reloaded.events[2].ignorable, Some(true));
        assert_eq!(reloaded.events[4].event_type, "session/end-seed");
        crate::test_support::cleanup_tree(&root);
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
        crate::test_support::cleanup_tree(&root);
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
        crate::test_support::cleanup_tree(&root);
    }
}
