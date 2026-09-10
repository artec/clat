//! history use cases, internal to SessionService.
use super::*;

impl SessionService {
    /// Structured replay of a session journal — the frontend transcript
    /// rebuild input. A pure fold of the durable log: deleting checkpoints
    /// changes nothing (invariant I4). Lazy sessions without a log replay
    /// empty. The read streams through `visit_from` so decoded memory stays
    /// bounded to the output items plus one record (audit F4: the earlier
    /// `read_from` version materialized the whole decoded log); incremental
    /// reads are a future scale layer.
    ///
    /// The "callers run at quiescent points" assumption does not hold for
    /// same-process late writers (a pending write-behind batch, a straggler
    /// title event), so an Io failure — the stat→stream→stat mismatch of
    /// `stream_events` — is retried with locally rebuilt state instead of
    /// failing the caller (mirrors `read_stable`). A truly external writer
    /// exhausts the budget and the error still surfaces.
    pub(crate) fn replay(&self, key: &SessionKey) -> Result<Vec<ReplayEvent>, SessionError> {
        self.replay_with_usage(key).map(|(replay, _)| replay)
    }

    /// One streaming pass producing both the structured replay and the
    /// journal-derived usage stats: the startup path needs both, and a
    /// second pass would double the zstd decode cost the mount-time reuse
    /// exists to avoid.
    pub(crate) fn replay_with_usage(
        &self,
        key: &SessionKey,
    ) -> Result<(Vec<ReplayEvent>, UsageStats), SessionError> {
        if !self.has_log(key) {
            return Ok((Vec::new(), UsageStats::default()));
        }
        let mut last = None;
        for _ in 0..3 {
            let mut adapter = ReplayAdapter::new();
            let mut out = Vec::new();
            let mut usage = UsageStats::default();
            match self.backend.visit_from(key, 0, &mut |event| {
                adapter.push(event, &mut out);
                usage.record(event);
                Ok(())
            }) {
                Ok(_) => return Ok((out, usage)),
                Err(SessionError::Io(message)) => last = Some(SessionError::Io(message)),
                Err(error) => return Err(error),
            }
        }
        Err(last.expect("at least one attempt ran"))
    }

    /// Structured replay of the active session, if any. Callers run at
    /// quiescent points (mount, switch); a concurrently appending writer is
    /// not expected here.
    pub(crate) fn replay_active(&self) -> Result<Vec<ReplayEvent>, SessionError> {
        self.replay_active_with_usage().map(|(replay, _)| replay)
    }

    /// [`Self::replay_active`] + usage stats in the same single pass.
    pub(crate) fn replay_active_with_usage(
        &self,
    ) -> Result<(Vec<ReplayEvent>, UsageStats), SessionError> {
        let (key, committed, replay) = {
            let guard = self.active.lock().expect("active");
            match guard.as_ref() {
                Some(active) => (
                    active.key.clone(),
                    active.coordinator.committed_seq(),
                    Arc::clone(&active.replay),
                ),
                None => return Ok((Vec::new(), UsageStats::default())),
            }
        };
        catch_up_replay(&self.backend, &key, committed, &replay)?;
        let replay = replay.lock().expect("replay");
        Ok((replay.replay.clone(), replay.usage.clone()))
    }

    /// Tail-oriented replay page for the active session. The arming scan is
    /// retained in core, so older-page reads never touch the journal again;
    /// only a newly committed suffix is decoded before slicing.
    pub(crate) fn history_active(
        &self,
        before_seq: Option<u64>,
        max_messages: usize,
    ) -> Result<SessionHistoryPage, SessionError> {
        self.history_active_with_usage(before_seq, max_messages)
            .map(|(page, _)| page)
    }

    /// History page plus the usage fold captured by the same replay cache
    /// lock. Windowed snapshots therefore never need a second full replay
    /// clone merely to restore status-bar counters.
    pub(crate) fn history_active_with_usage(
        &self,
        before_seq: Option<u64>,
        max_messages: usize,
    ) -> Result<(SessionHistoryPage, UsageStats), SessionError> {
        let (key, committed, replay) = {
            let guard = self.active.lock().expect("active");
            match guard.as_ref() {
                Some(active) => (
                    active.key.clone(),
                    active.coordinator.committed_seq(),
                    Arc::clone(&active.replay),
                ),
                None => {
                    return Ok((
                        SessionHistoryPage {
                            events: Vec::new(),
                            has_more: false,
                        },
                        UsageStats::default(),
                    ));
                }
            }
        };
        catch_up_replay(&self.backend, &key, committed, &replay)?;
        let replay = replay.lock().expect("replay");
        Ok((
            paginate_replay(&replay.replay, before_seq, max_messages),
            replay.usage.clone(),
        ))
    }

    pub(crate) fn message_outline_active(&self) -> Result<Vec<MessageOutlineItem>, SessionError> {
        let guard = self.active.lock().expect("active");
        let Some(active) = guard.as_ref() else {
            return Ok(Vec::new());
        };
        fold_if_behind(active, &self.backend)?;
        let projections = active.projections.lock().expect("projections");
        Ok(projections
            .state_snapshot("message-outline")
            .unwrap_or_default()
            .get("entries")
            .and_then(Value::as_array)
            .map(|entries| {
                entries
                    .iter()
                    .filter_map(|entry| {
                        Some(MessageOutlineItem {
                            seq: entry.get("seq")?.as_u64()?,
                            turn: entry.get("turn")?.as_u64()?,
                            role: entry.get("role")?.as_str()?.to_owned(),
                            preview: entry
                                .get("preview")
                                .and_then(Value::as_str)
                                .unwrap_or_default()
                                .to_owned(),
                        })
                    })
                    .collect()
            })
            .unwrap_or_default())
    }

    /// The display transcript of the active session (projection-backed,
    /// compaction-safe).
    pub(crate) fn transcript_lines(&self) -> Result<Vec<TranscriptLine>, SessionError> {
        let guard = self.active.lock().expect("active");
        let Some(active) = guard.as_ref() else {
            return Ok(Vec::new());
        };
        let projections = active.projections.lock().expect("projections");
        Ok(projections
            .state_snapshot("transcript")
            .unwrap_or_default()
            .get("entries")
            .and_then(Value::as_array)
            .map(|entries| {
                entries
                    .iter()
                    .filter_map(|entry| {
                        Some(TranscriptLine {
                            kind: entry.get("kind")?.as_str()?.to_owned(),
                            text: entry
                                .get("text")
                                .and_then(Value::as_str)
                                .unwrap_or_default()
                                .to_owned(),
                            is_error: entry
                                .get("isError")
                                .and_then(Value::as_bool)
                                .unwrap_or(false),
                        })
                    })
                    .collect()
            })
            .unwrap_or_default())
    }

    /// Input recall from the transcript projection (compaction-safe,
    /// plan §13.5).
    pub(crate) fn recent_inputs(&self, limit: usize) -> Result<Vec<String>, SessionError> {
        let guard = self.active.lock().expect("active");
        let Some(active) = guard.as_ref() else {
            return Ok(Vec::new());
        };
        let projections = active.projections.lock().expect("projections");
        let transcript = projections.state_snapshot("transcript").unwrap_or_default();
        let mut inputs = transcript
            .get("entries")
            .and_then(Value::as_array)
            .map(|entries| {
                entries
                    .iter()
                    .filter(|entry| entry.get("kind").and_then(Value::as_str) == Some("user"))
                    .filter_map(|entry| {
                        entry.get("text").and_then(Value::as_str).map(str::to_owned)
                    })
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        if inputs.len() > limit {
            inputs.drain(..inputs.len() - limit);
        }
        Ok(inputs)
    }
}
