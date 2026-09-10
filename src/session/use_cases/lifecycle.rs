//! lifecycle use cases, internal to SessionService.
use super::*;

impl SessionService {
    /// `/resume` one-shot: bounded stage → arm → quiesce → infallible install.
    /// Reopening the already-active key retires that writer before staging so
    /// the lease is never acquired twice for one physical session.
    pub(crate) fn resume(&self, key: &SessionKey) -> Result<SessionView, SessionError> {
        self.resume_with_view(key, true)
    }

    /// Resume while keeping the active replay/projection caches authoritative,
    /// but do not clone their full frontend views. A windowed frontend can ask
    /// [`Self::history_active`] for only its first visible page after install.
    pub(crate) fn resume_windowed(&self, key: &SessionKey) -> Result<SessionView, SessionError> {
        self.resume_with_view(key, false)
    }

    fn resume_with_view(
        &self,
        key: &SessionKey,
        materialize_full_view: bool,
    ) -> Result<SessionView, SessionError> {
        // Reopening the session that is already active must first retire its
        // writer.  Otherwise arming the same physical log would require a
        // second lease in this process and, more importantly, would race the
        // existing write-behind coordinator.  A different target still uses
        // the stage → arm → CAS path below so a failed switch leaves the
        // current session untouched.
        let same_active = self
            .active
            .lock()
            .expect("active")
            .as_ref()
            .is_some_and(|active| active.key == *key);
        if same_active {
            self.quiesce_active()?;
        }
        let staged = self.stage_resume(key)?;
        let armed = self.arm_session_with_view(staged, materialize_full_view)?;
        if let Err(error) = self.quiesce_active() {
            return match self.discard_armed(armed) {
                Ok(()) => Err(error),
                Err(close_error) => Err(SessionError::Corruption(format!(
                    "{error}; staged session close failed: {close_error}"
                ))),
            };
        }
        Ok(self.install_armed(armed))
    }

    /// Read-only target admission without deactivating the current session.
    /// Only the bounded first-frame Header is decoded here.
    pub(crate) fn stage_resume(&self, key: &SessionKey) -> Result<StagedSession, SessionError> {
        let header = self.backend.header_snapshot(key)?;
        Ok(StagedSession {
            key: key.clone(),
            header,
        })
    }

    /// Arm a staged target before workspace CAS, but do not enqueue its
    /// resume seed or publish it as active. `prepare` performs pending
    /// recovery here; single-pass (R-1): the recovery scan's events feed
    /// projection folding, the structured replay, and usage stats in the
    /// same physical read — only the torn-tail crash path re-reads once
    /// after repair. Any failure closes the just-created writer.
    pub(crate) fn arm_session(&self, staged: StagedSession) -> Result<ArmedSession, SessionError> {
        self.arm_session_with_view(staged, true)
    }

    /// Prepare a resume target without cloning its full replay/transcript into
    /// the return view. The active session still owns the complete folded
    /// state, so a later full consumer remains lossless.
    pub(crate) fn arm_session_windowed(
        &self,
        staged: StagedSession,
    ) -> Result<ArmedSession, SessionError> {
        self.arm_session_with_view(staged, false)
    }

    fn arm_session_with_view(
        &self,
        staged: StagedSession,
        materialize_full_view: bool,
    ) -> Result<ArmedSession, SessionError> {
        let StagedSession { key, header } = staged;
        let arm_header = SessionHeader::new(key.id.clone(), key.project.header_cwd.clone(), 0);
        let mut registry = ProjectionRegistry::clat();
        let mut sink = ResumeSink::new();
        // INV-MM1-4：单遍 visitor 顺路收集 attachment 引用 id——
        // 会话打开时的有界 orphan 回收输入（见 arm 尾部 sweep）。
        let mut referenced_attachments = std::collections::HashSet::new();
        let (coordinator, visitor_applied) = SessionCoordinator::start_unseeded_with_visitor(
            Arc::clone(&self.backend),
            key.clone(),
            arm_header,
            &mut |event| {
                collect_event_attachment_ids(event, &mut referenced_attachments);
                sink.push(event, &mut registry)
            },
        )?;
        if !visitor_applied {
            // 撕裂尾部在 prepare 内修复：visitor 的部分输出跨过了截断
            // 点，不可信——丢弃后从修复好的日志重读一遍（崩溃路径，
            // R-1 允许这一遍）。
            let mut repaired_registry = ProjectionRegistry::clat();
            let mut repaired = ResumeSink::new();
            if let Err(error) = self.backend.visit_from(&key, 0, &mut |event| {
                collect_event_attachment_ids(event, &mut referenced_attachments);
                repaired.push(event, &mut repaired_registry)
            }) {
                let _ = coordinator.close();
                return Err(error);
            }
            registry = repaired_registry;
            sink = repaired;
        }
        let projections = Arc::new(Mutex::new(registry));
        // Catch up what arming committed behind the single pass (torn-tail
        // repair closers): the same channel keeps feeding projections, the
        // replay, and usage — a bounded tail read, never a full re-stream.
        // A failure here must close the just-armed writer before
        // propagating (dropping an armed coordinator detaches its thread).
        let floor = sink.pushed;
        if coordinator
            .committed_seq()
            .is_some_and(|committed| floor <= committed)
        {
            let mut guard = projections.lock().expect("projections");
            let tail = self.backend.visit_from(&key, floor, &mut |event| {
                collect_event_attachment_ids(event, &mut referenced_attachments);
                sink.push(event, &mut guard)
            });
            drop(guard);
            if let Err(error) = tail {
                let _ = coordinator.close();
                return Err(error);
            }
        }
        let replay = if materialize_full_view {
            sink.replay.clone()
        } else {
            Vec::new()
        };
        let usage = sink.usage.clone();
        let replay_state = Arc::new(Mutex::new(sink));
        let active = ActiveSession::new(
            key.clone(),
            Arc::clone(&coordinator),
            Arc::clone(&projections),
            replay_state,
        );
        let mut view = match self.view_from(
            &header,
            &projections.lock().expect("projections"),
            replay,
            materialize_full_view,
        ) {
            Ok(view) => view,
            Err(error) => {
                let _ = coordinator.close();
                return Err(error);
            }
        };
        view.usage = usage;
        // INV-MM1-4：会话打开时的有界 orphan 回收（引用集合来自上方
        // 单遍收集；附件域不存在则跳过——全新会话无附件；失败静默：
        // 回收是增益，不得阻塞会话打开）。
        let attachments_root = crate::session::path_layout::session_dir(
            self.backend.root_path(),
            key.project.header_cwd.as_deref(),
            &key.id,
        )
        .join("attachments");
        if !coordinator.is_read_only()
            && let Ok(session_dir) = self.backend.open_session_dir(&key)
            && session_dir.symlink_metadata("attachments").is_ok()
            && let Ok(store) = crate::session::attachments::AttachmentStore::open_in_session(
                &session_dir,
                attachments_root,
            )
        {
            let _ = store.sweep_orphans(&referenced_attachments, std::time::SystemTime::now());
        }
        Ok(ArmedSession { active, view })
    }

    /// Close a fully armed but unpublished target after a lost workspace
    /// CAS. No seed was queued, so this never grows an otherwise untouched
    /// session log.
    pub(crate) fn discard_armed(&self, armed: ArmedSession) -> Result<(), SessionError> {
        armed
            .active
            .coordinator
            .close()
            .map_err(SessionError::Corruption)
    }

    /// The post-CAS pointer swap is intentionally infallible: storage
    /// prepare, repair, projection catch-up, and view construction all
    /// completed in [`Self::arm_session`].
    pub(crate) fn install_armed(&self, armed: ArmedSession) -> SessionView {
        let ArmedSession { active, view } = armed;
        let seeded = active.coordinator.enqueue_seed_marker_if_needed();
        // The seed marker must be durable before `Application::open`
        // returns: active replay caches avoid a second disk stream, but later
        // cold-resume readers still observe the journal directly——marker 还在 200ms
        // 写后窗口里时，"open 已返回但日志缺 marker"对它们就是种族。
        // Best effort on purpose: a failed flush keeps the batch on the
        // normal retry lane, and install must stay infallible.
        let flushed = active.coordinator.flush().is_ok();
        if seeded
            && flushed
            && let Some(committed) = active.coordinator.committed_seq()
        {
            // session/end-seed is an explicit replay skip. It is the only
            // event admitted between the arming scan and publication, so the
            // cached replay can advance its source watermark without another
            // physical read.
            active.replay.lock().expect("replay").pushed = committed + 1;
            let seed = SessionEvent::new(
                "session/end-seed",
                committed,
                now_ms(),
                serde_json::json!({}),
            );
            let _ = active
                .projections
                .lock()
                .expect("projections")
                .fold_one(&seed);
        }
        *self.active.lock().expect("active") = Some(active);
        view
    }
}
