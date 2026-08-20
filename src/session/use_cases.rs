//! `SessionUseCases` facade (plan §8.3): the Application-facing surface over
//! backend + projections + checkpoints + coordinator. Application never
//! touches raw persistence; frontends never see this file's internals.
//!
//! Stage-5 note: `normalize_selection` / `current_selection` live in the
//! control DB (`project_workspace_state`) and land with the cutover; this
//! facade owns everything session-log-shaped.

use crate::model::ModelItem;
use crate::permission::PermissionMode;
use crate::session::checkpoint::CheckpointStore;
use crate::session::event::{SessionEvent, now_ms, payloads};
use crate::session::header::SessionHeader;
use crate::session::id::SessionId;
use crate::session::key::{ProjectKey, SessionKey};
use crate::session::persistence::{JsonlBackend, JsonlCompression, SessionError};
use crate::session::projection::{CheckpointIdentity, ProjectionRegistry};
use crate::session::replay::{ReplayAdapter, ReplayEvent};
use crate::session::root_dir::SessionRootDir;
use crate::session::run_journal::{RunJournal, SessionCoordinator};
use serde_json::Value;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SessionSummary {
    pub id: SessionId,
    pub title: Option<String>,
    pub created_at_ms: i64,
    pub last_activity_ms: i64,
    pub message_count: u64,
    pub turns: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TranscriptLine {
    pub kind: String,
    pub text: String,
    pub is_error: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SessionView {
    pub header: SessionHeader,
    pub title: Option<String>,
    pub todos: Vec<(String, String)>,
    pub transcript: Vec<TranscriptLine>,
    /// Structured replay of the whole journal (frontend transcript rebuild).
    /// Assembled in `arm_session` from the durable log, so repair closers are
    /// included; the resume seed marker (enqueued at install) never is — it
    /// is on the replay skip list anyway.
    pub replay: Vec<ReplayEvent>,
    pub model_items: Vec<ModelItem>,
    pub turns: u64,
    /// Journal-derived usage stats (DSH `assistant/message.usage`), folded
    /// in the same streaming pass as `replay`: the status bar's Cache and
    /// Context restore from them at startup without a second log stream.
    pub usage: UsageStats,
}

/// Usage stats folded from one journal pass: the session aggregate (cache
/// ratio numerator/denominator) and the most recent report (the current
/// context watermark).
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct UsageStats {
    pub session: crate::model::Usage,
    pub last_request: Option<crate::model::Usage>,
}

/// Extract a usage report from an `assistant/message` event's DSH-shaped
/// `usage` object. Messages without a report (adapter did not report) and
/// unknown shapes are skipped.
fn usage_from_event(event: &SessionEvent) -> Option<crate::model::Usage> {
    let usage = event.data.get("usage")?;
    let number = |field: &str| usage.get(field).and_then(serde_json::Value::as_u64);
    Some(crate::model::Usage {
        input_tokens: number("inputTokens")?,
        output_tokens: number("outputTokens").unwrap_or(0),
        cached_input_tokens: number("cacheReadTokens"),
        reasoning_tokens: number("reasoningTokens"),
    })
}

/// Use-case-level CAS for title writes (plan §13.2).
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SetTitleExpectation {
    /// Matches only while no title event exists.
    NoTitle,
    /// Matches the seq of the title event being updated.
    Exact(u64),
    /// Explicit user override; always matches.
    Force,
}

/// Who derived the title: model-generated titles cite their provider and
/// model (catalog §2.2); user renames do not.
pub(crate) enum TitleSource<'a> {
    User,
    Provider { provider: &'a str, model: &'a str },
}

struct ActiveSession {
    key: SessionKey,
    coordinator: Arc<SessionCoordinator>,
    projections: Arc<Mutex<ProjectionRegistry>>,
    /// The one shared folding journal for this session. Every producer
    /// (run recorder, todo, compaction, titles) must append through the
    /// SAME instance: per-handle pending lists interleaving across
    /// flushes created seq gaps that desynced the surface projection
    /// from `events[i].seq == i`.
    journal: Mutex<Option<Arc<dyn RunJournal>>>,
    generation: AtomicU64,
}

/// A bounded, read-only resume witness. Full streaming projection restore and
/// writer preparation happen in `arm_session` before workspace CAS; staging
/// itself never starts a worker or mutates the target log.
pub(crate) struct StagedSession {
    key: SessionKey,
    header: SessionHeader,
}

/// A resume target with every fallible storage/projection operation
/// complete, but not yet published as active and with no resume seed
/// queued. It may be closed and discarded if the workspace CAS loses.
pub(crate) struct ArmedSession {
    active: ActiveSession,
    view: SessionView,
}

/// 单遍 resume 的汇聚点（R-1）：一次物理扫描同时喂三个消费者——
/// 前端转录回放、usage 统计，以及（由调用方持有并传入的）投影注册
/// 表。`pushed` 是已喂入的事件数；arm 期间的尾部追平从它继续，
/// 依赖 `events[i].seq == i` 的日志不变量。
struct ResumeSink {
    adapter: ReplayAdapter,
    replay: Vec<ReplayEvent>,
    usage: UsageStats,
    pushed: u64,
}

impl ResumeSink {
    fn new() -> Self {
        Self {
            adapter: ReplayAdapter::new(),
            replay: Vec::new(),
            usage: UsageStats::default(),
            pushed: 0,
        }
    }

    fn push(
        &mut self,
        event: &SessionEvent,
        registry: &mut ProjectionRegistry,
    ) -> Result<(), String> {
        registry.fold_one(event)?;
        self.adapter.push(event, &mut self.replay);
        if event.event_type == "assistant/message"
            && let Some(report) = usage_from_event(event)
        {
            self.usage.session.add_assign(&report);
            self.usage.last_request = Some(report);
        }
        self.pushed += 1;
        Ok(())
    }
}

pub(crate) struct SessionService {
    backend: Arc<JsonlBackend>,
    checkpoints: CheckpointStore,
    active: Mutex<Option<ActiveSession>>,
}

impl SessionService {
    pub(crate) fn new(
        session_root: std::path::PathBuf,
        compression: JsonlCompression,
    ) -> Result<Self, SessionError> {
        let root = SessionRootDir::open_or_create(&session_root).map_err(|error| {
            SessionError::Io(format!("cannot open capability-held session root: {error}"))
        })?;
        Ok(Self {
            backend: Arc::new(JsonlBackend::with_root(
                Arc::clone(&root),
                compression,
                true,
            )),
            checkpoints: CheckpointStore::new(root),
            active: Mutex::new(None),
        })
    }

    /// `/new`: a lazy session — nothing on disk until the first durable
    /// batch (plan §13.1). The caller quiesces the previous session
    /// itself; creation here never touches another session's state.
    pub(crate) fn new_session(&self, project: &ProjectKey) -> Result<SessionSummary, SessionError> {
        let mut active_slot = self.active.lock().expect("active");
        if active_slot.is_some() {
            return Err(SessionError::Conflict(
                "cannot create a new active session before quiescing the current one".into(),
            ));
        }
        let key = SessionKey {
            project: project.clone(),
            id: SessionId::generate(),
        };
        let header = SessionHeader::new(key.id.clone(), project.header_cwd.clone(), now_ms());
        let coordinator =
            SessionCoordinator::start(Arc::clone(&self.backend), key.clone(), header.clone())?;
        *active_slot = Some(ActiveSession {
            key: key.clone(),
            journal: Mutex::new(None),
            coordinator,
            projections: Arc::new(Mutex::new(ProjectionRegistry::clat())),
            generation: AtomicU64::new(0),
        });
        Ok(SessionSummary {
            id: key.id,
            title: None,
            created_at_ms: header.created_at,
            last_activity_ms: header.created_at,
            message_count: 0,
            turns: 0,
        })
    }

    /// `/resume` one-shot: bounded stage → arm → quiesce → infallible install.
    pub(crate) fn resume(&self, key: &SessionKey) -> Result<SessionView, SessionError> {
        let staged = self.stage_resume(key)?;
        let armed = self.arm_session(staged)?;
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
        let StagedSession { key, header } = staged;
        let arm_header = SessionHeader::new(key.id.clone(), key.project.header_cwd.clone(), 0);
        let mut registry = ProjectionRegistry::clat();
        let mut sink = ResumeSink::new();
        let (coordinator, visitor_applied) = SessionCoordinator::start_unseeded_with_visitor(
            Arc::clone(&self.backend),
            key.clone(),
            arm_header,
            &mut |event| sink.push(event, &mut registry),
        )?;
        if !visitor_applied {
            // 撕裂尾部在 prepare 内修复：visitor 的部分输出跨过了截断
            // 点，不可信——丢弃后从修复好的日志重读一遍（崩溃路径，
            // R-1 允许这一遍）。
            let mut repaired_registry = ProjectionRegistry::clat();
            let mut repaired = ResumeSink::new();
            if let Err(error) = self.backend.visit_from(&key, 0, &mut |event| {
                repaired.push(event, &mut repaired_registry)
            }) {
                let _ = coordinator.close();
                return Err(error);
            }
            registry = repaired_registry;
            sink = repaired;
        }
        let projections = Arc::new(Mutex::new(registry));
        let active = ActiveSession {
            key: key.clone(),
            journal: Mutex::new(None),
            coordinator: Arc::clone(&coordinator),
            projections: Arc::clone(&projections),
            generation: AtomicU64::new(0),
        };
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
            let tail = self
                .backend
                .visit_from(&key, floor, &mut |event| sink.push(event, &mut guard));
            drop(guard);
            if let Err(error) = tail {
                let _ = coordinator.close();
                return Err(error);
            }
        }
        let ResumeSink { replay, usage, .. } = sink;
        let mut view =
            match self.view_from(&header, &projections.lock().expect("projections"), replay) {
                Ok(view) => view,
                Err(error) => {
                    let _ = coordinator.close();
                    return Err(error);
                }
            };
        view.usage = usage;
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
        active.coordinator.enqueue_seed_marker_if_needed();
        // The seed marker must be durable before `Application::open`
        // returns: mount 的第一次 snapshot 走 mounted_replay 暂存、不
        // 再重流日志，但后续任何全量读者（下一次 snapshot、下一次冷
        // resume 的 prepare 扫描）都会看到磁盘——marker 还在 200ms
        // 写后窗口里时，"open 已返回但日志缺 marker"对它们就是种族。
        // Best effort on purpose: a failed flush keeps the batch on the
        // normal retry lane, and install must stay infallible.
        let _ = active.coordinator.flush();
        *self.active.lock().expect("active") = Some(active);
        view
    }

    /// Headers + cached projections for the `/resume` picker; never decodes
    /// log bodies (plan §12.1).
    pub(crate) fn list_sessions(
        &self,
        project: &ProjectKey,
    ) -> Result<Vec<SessionSummary>, SessionError> {
        let mut summaries = Vec::new();
        for (key, header, _revision) in self.backend.list_snapshots()? {
            // Bucket filtering alone cannot distinguish a lossy collision:
            // the header's own cwd is the witness (plan §4.1).
            if key.project.bucket != project.bucket
                || header.cwd.as_deref() != project.header_cwd.as_deref()
            {
                continue;
            }
            let identity = CheckpointIdentity::of(&header);
            let record = self
                .checkpoints
                .load(&key)
                .filter(|record| record.identity_matches(&identity));
            let (title, message_count, turns, last_activity) = match &record {
                Some(record) => {
                    let stats = &record.rows.get("stats").map(|row| &row.val);
                    let title = record
                        .rows
                        .get("title")
                        .and_then(|row| row.val.get("title"))
                        .and_then(Value::as_str)
                        .map(str::to_owned);
                    (
                        title,
                        stats
                            .and_then(|stats| stats.get("messages"))
                            .and_then(Value::as_u64)
                            .unwrap_or(0),
                        stats
                            .and_then(|stats| stats.get("turns"))
                            .and_then(Value::as_u64)
                            .unwrap_or(0),
                        stats
                            .and_then(|stats| stats.get("lastActivityMs"))
                            .and_then(Value::as_i64)
                            .unwrap_or(header.created_at),
                    )
                }
                None => (None, 0, 0, header.created_at),
            };
            summaries.push(SessionSummary {
                id: header.id,
                title,
                created_at_ms: header.created_at,
                last_activity_ms: last_activity,
                message_count,
                turns,
            });
        }
        summaries.sort_by_key(|summary| std::cmp::Reverse(summary.last_activity_ms));
        Ok(summaries)
    }

    /// The active session's journal (Run Scope acquisition, plan §14.4).
    /// One shared instance per session — see `ActiveSession::journal`.
    pub(crate) fn journal(&self) -> Result<Arc<dyn RunJournal>, SessionError> {
        let active = self.active.lock().expect("active");
        let session = active
            .as_ref()
            .ok_or_else(|| SessionError::NotFound("no active session".into()))?;
        let mut journal = session.journal.lock().expect("session journal");
        if journal.is_none() {
            *journal = Some(journal_with_projection_fold(session, &self.backend));
        }
        Ok(Arc::clone(journal.as_ref().expect("just initialized")))
    }

    /// 导入图片附件（M4，2026-08-19）：先整体校验（存在、扩展名合法、
    /// ≤4MB），再复制进会话目录的 `attachments/` 子目录（uuid 文件名
    /// 保留原扩展名）。返回 (绝对路径, MIME) 列表——绝对引用随后进
    /// journal，回放零换算；原件此后可删可改，会话自包含。
    /// 校验失败在任何复制之前返回错误（不留半套附件）。
    pub(crate) fn import_attachments(
        &self,
        sources: &[std::path::PathBuf],
    ) -> Result<Vec<(String, String)>, SessionError> {
        if sources.is_empty() {
            return Ok(Vec::new());
        }
        // 预检：全部合法才动手。
        for source in sources {
            crate::media::media_type_for_path(source).ok_or_else(|| {
                SessionError::Io(format!("unsupported image type: {}", source.display()))
            })?;
            let metadata = std::fs::metadata(source)
                .map_err(|error| SessionError::Io(format!("{}: {error}", source.display())))?;
            if !metadata.is_file() {
                return Err(SessionError::Io(format!(
                    "not a file: {}",
                    source.display()
                )));
            }
            if metadata.len() > crate::media::MAX_ATTACHMENT_BYTES {
                return Err(SessionError::Io(format!(
                    "image too large ({} bytes > {}): {}",
                    metadata.len(),
                    crate::media::MAX_ATTACHMENT_BYTES,
                    source.display()
                )));
            }
        }
        let attachments_dir = {
            let active = self.active.lock().expect("active");
            let session = active
                .as_ref()
                .ok_or_else(|| SessionError::NotFound("no active session".into()))?;
            crate::session::path_layout::session_dir(
                self.backend.root_path(),
                session.key.project.header_cwd.as_deref(),
                &session.key.id,
            )
            .join("attachments")
        };
        std::fs::create_dir_all(&attachments_dir)
            .map_err(|error| SessionError::Io(format!("create attachments dir: {error}")))?;
        let mut imported = Vec::new();
        for source in sources {
            let media_type =
                crate::media::media_type_for_path(source).unwrap_or("application/octet-stream");
            let extension = source
                .extension()
                .and_then(|extension| extension.to_str())
                .unwrap_or("png");
            let name = format!("{}.{extension}", uuid::Uuid::new_v4().simple());
            let destination = attachments_dir.join(&name);
            std::fs::copy(source, &destination)
                .map_err(|error| SessionError::Io(format!("copy {}: {error}", source.display())))?;
            imported.push((
                destination.to_string_lossy().into_owned(),
                media_type.to_owned(),
            ));
        }
        Ok(imported)
    }

    /// Whether a session log is materialized on disk (Materializing
    /// normalization, plan §13.1).
    pub(crate) fn has_log(&self, key: &SessionKey) -> bool {
        self.backend.has_log(key)
    }

    /// 测试仪表：透传 backend 的全量流计数（性能回归测试用）。
    #[cfg(test)]
    pub(crate) fn stream_probe(&self) -> usize {
        self.backend.stream_probe()
    }

    /// The active session id, if any.
    pub(crate) fn active_id(&self) -> Option<SessionId> {
        self.active
            .lock()
            .expect("active")
            .as_ref()
            .map(|session| session.key.id.clone())
    }

    /// Completed turn count of the active session (stats projection).
    pub(crate) fn active_turns(&self) -> Result<u64, SessionError> {
        let guard = self.active.lock().expect("active");
        let Some(active) = guard.as_ref() else {
            return Ok(0);
        };
        let projections = active.projections.lock().expect("projections");
        Ok(projections
            .state_snapshot("stats")
            .and_then(|stats| stats.get("turns").cloned())
            .and_then(|turns| turns.as_u64())
            .unwrap_or(0))
    }

    /// The last `request/header` body in the active session's log (the
    /// dedupe authority for catalog §2.7).
    pub(crate) fn last_request_header(&self) -> Option<Value> {
        let guard = self.active.lock().expect("active");
        let active = guard.as_ref()?;
        let projections = active.projections.lock().expect("projections");
        projections
            .state_snapshot("requestHeader")
            .and_then(|state| state.get("header").cloned())
            .filter(|header| !header.is_null())
    }

    /// Current title state of the active session: `(title, title-event seq)`
    /// from the title projection.
    pub(crate) fn title_state(&self) -> (Option<String>, Option<u64>) {
        let guard = self.active.lock().expect("active");
        let Some(active) = guard.as_ref() else {
            return (None, None);
        };
        let projections = active.projections.lock().expect("projections");
        let state = projections.state_snapshot("title").unwrap_or_default();
        (
            state
                .get("title")
                .and_then(Value::as_str)
                .map(str::to_owned),
            state.get("eventSeq").and_then(Value::as_u64),
        )
    }

    /// 活跃会话的已落档档位（`sandbox/mode` latest-wins fold）。None =
    /// 该会话从未记录过（档位系统之前的遗留会话，或无活跃会话）——
    /// 调用方回落编译期默认，绝不继承其他会话的档位（PS1/PS3）。
    pub(crate) fn permission_mode_state(&self) -> Option<PermissionMode> {
        let guard = self.active.lock().expect("active");
        let active = guard.as_ref()?;
        let projections = active.projections.lock().expect("projections");
        let state = projections
            .state_snapshot("permission-mode")
            .unwrap_or_default();
        state
            .get("mode")
            .and_then(Value::as_str)
            .and_then(PermissionMode::from_journal_value)
    }

    /// 向活跃会话追加一条 `sandbox/mode` 事件（DSH setSandboxMode 形状：
    /// append + flush + checkpoint，latest-wins，无 CAS——档位切换只有
    /// UI 同步路径一个写者）。同值切换零事件（DSH apply() no-op 语义）。
    /// 无活跃会话返回 Ok(false)：不落任何 journal——内存 cell 继续作为
    /// 未物化会话的出生档（PS7）。
    pub(crate) fn record_permission_mode(
        &self,
        mode: PermissionMode,
    ) -> Result<bool, SessionError> {
        let guard = self.active.lock().expect("active");
        let Some(active) = guard.as_ref() else {
            return Ok(false);
        };
        {
            let projections = active.projections.lock().expect("projections");
            let state = projections
                .state_snapshot("permission-mode")
                .unwrap_or_default();
            let current = state.get("mode").and_then(Value::as_str);
            if current == Some(mode.journal_value()) {
                return Ok(true);
            }
        }
        let event = crate::session::run_journal::NewSessionEvent::new(
            "sandbox/mode",
            payloads::sandbox_mode(&mode),
        );
        // The shared session journal (same instance as every producer).
        let mut journal_slot = active.journal.lock().expect("session journal");
        if journal_slot.is_none() {
            *journal_slot = Some(journal_with_projection_fold(active, &self.backend));
        }
        let journal = Arc::clone(journal_slot.as_ref().expect("just initialized"));
        drop(journal_slot);
        journal.append(event).map_err(SessionError::Corruption)?;
        journal.flush().map_err(SessionError::Corruption)?;
        checkpoint_active(active, &self.checkpoints)?;
        Ok(true)
    }

    /// The first user message text of the active session (transcript
    /// projection, compaction-safe) — autotitle input.
    pub(crate) fn first_user_text(&self) -> Option<String> {
        let guard = self.active.lock().expect("active");
        let active = guard.as_ref()?;
        let projections = active.projections.lock().expect("projections");
        let transcript = projections.state_snapshot("transcript").unwrap_or_default();
        transcript
            .get("entries")
            .and_then(Value::as_array)?
            .iter()
            .filter(|entry| entry.get("kind").and_then(Value::as_str) == Some("user"))
            .find_map(|entry| entry.get("text").and_then(Value::as_str))
            .map(str::to_owned)
    }

    /// The model-facing history of the active session: surface nodes with
    /// their seqs (surface projection → ModelItem adapter). The first
    /// element of each pair is the durable event seq, which compaction
    /// needs for `shadowedRange`/`shadowedSeqs`.
    pub(crate) fn surface_nodes(&self) -> Result<Vec<(u64, ModelItem)>, SessionError> {
        let guard = self.active.lock().expect("active");
        let Some(active) = guard.as_ref() else {
            return Ok(Vec::new());
        };
        let projections = active.projections.lock().expect("projections");
        projections
            .surface_nodes()
            .map_err(SessionError::Corruption)
    }

    /// Fold everything durably committed into the active projections and
    /// refresh the checkpoint (call after run boundaries). The fold is
    /// skipped when the projections already cover the committed cursor —
    /// explicit journal flushes fold their batches directly (P1-13), so
    /// this only replays what a 200 ms write-behind deadline committed
    /// without an observing flush.
    pub(crate) fn sync_active(&self) -> Result<(), SessionError> {
        let guard = self.active.lock().expect("active");
        let Some(active) = guard.as_ref() else {
            return Ok(());
        };
        fold_if_behind(active, &self.backend)?;
        checkpoint_active(active, &self.checkpoints)
    }

    /// Title writes with the use-case CAS (plan §13.2): the payload is
    /// always a plain DSH `session/title` event — the CAS never leaks into
    /// the durable format. Session-scoped on purpose (fourth-pass F-A): a
    /// background autotitle job captured its expectation from one session
    /// and must never apply to whichever session happens to be active
    /// when it finishes — a stale job is a silent no-op, not a write.
    pub(crate) fn set_title(
        &self,
        session: &SessionId,
        expectation: SetTitleExpectation,
        title: &str,
        source: TitleSource<'_>,
    ) -> Result<bool, SessionError> {
        let guard = self.active.lock().expect("active");
        let Some(active) = guard.as_ref() else {
            return Err(SessionError::NotFound("no active session".into()));
        };
        if active.key.id != *session {
            return Ok(false);
        }
        // 目录 §2.2：provider 派生的标题引用其依据的 user/message seq
        //（首条消息）并携带 provider/model；手工重命名为 []。
        let message_seqs;
        {
            let projections = active.projections.lock().expect("projections");
            let state = projections.state_snapshot("title").unwrap_or_default();
            let current_seq = state.get("eventSeq").and_then(Value::as_u64);
            // NoTitle 匹配"尚无 title 事件"——派生的 fallback 标题
            // （首条消息派生）不是显式标题，不参与 CAS。
            let matches = match expectation {
                SetTitleExpectation::NoTitle => current_seq.is_none(),
                SetTitleExpectation::Exact(seq) => current_seq == Some(seq),
                SetTitleExpectation::Force => true,
            };
            if !matches {
                return Ok(false);
            }
            message_seqs = match &source {
                TitleSource::Provider { .. } => state
                    .get("firstUserSeq")
                    .and_then(Value::as_u64)
                    .into_iter()
                    .collect(),
                TitleSource::User => Vec::new(),
            };
        }
        let payload = match &source {
            TitleSource::Provider { provider, model } => {
                payloads::session_title_provider(title, message_seqs, provider, model)
            }
            TitleSource::User => payloads::session_title(title, message_seqs, "user"),
        };
        let event = crate::session::run_journal::NewSessionEvent::new("session/title", payload);
        // The shared session journal (same instance as every producer).
        let mut journal_slot = active.journal.lock().expect("session journal");
        if journal_slot.is_none() {
            *journal_slot = Some(journal_with_projection_fold(active, &self.backend));
        }
        let journal = Arc::clone(journal_slot.as_ref().expect("just initialized"));
        drop(journal_slot);
        journal.append(event).map_err(SessionError::Corruption)?;
        journal.flush().map_err(SessionError::Corruption)?;
        checkpoint_active(active, &self.checkpoints)?;
        Ok(true)
    }

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
                if event.event_type == "assistant/message"
                    && let Some(report) = usage_from_event(event)
                {
                    usage.session.add_assign(&report);
                    usage.last_request = Some(report);
                }
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
        let key = {
            let guard = self.active.lock().expect("active");
            match guard.as_ref() {
                Some(active) => active.key.clone(),
                None => return Ok((Vec::new(), UsageStats::default())),
            }
        };
        self.replay_with_usage(&key)
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

    /// Flush + checkpoint + join the writer (session detach). This is the
    /// thread-retirement boundary: after it returns, no writer thread for
    /// the session remains (audit P1-07).
    pub(crate) fn quiesce_active(&self) -> Result<(), SessionError> {
        let mut guard = self.active.lock().expect("active");
        if let Some(active) = guard.take() {
            // Detach must retire the writer even if folding/checkpointing
            // fails. An early `?` here used to drop the JoinHandle and leave
            // a detached writer thread alive on the exact error path where
            // shutdown guarantees matter most.
            let mut errors = Vec::new();
            if let Err(error) = fold_if_behind(&active, &self.backend) {
                errors.push(error.to_string());
            } else if let Err(error) = checkpoint_active(&active, &self.checkpoints) {
                errors.push(error.to_string());
            }
            if let Err(error) = active.coordinator.close() {
                errors.push(format!("session writer close failed: {error}"));
            }
            if !errors.is_empty() {
                return Err(SessionError::Corruption(errors.join("; ")));
            }
        }
        Ok(())
    }

    fn view_from(
        &self,
        header: &SessionHeader,
        projections: &ProjectionRegistry,
        replay: Vec<ReplayEvent>,
    ) -> Result<SessionView, SessionError> {
        let row = |unit: &str| projections.state_snapshot(unit).unwrap_or_default();
        let title = row("title")
            .get("title")
            .and_then(Value::as_str)
            .map(str::to_owned);
        let todos = row("todo")
            .get("todos")
            .and_then(Value::as_array)
            .map(|todos| {
                todos
                    .iter()
                    .filter_map(|todo| {
                        Some((
                            todo.get("content")?.as_str()?.to_owned(),
                            todo.get("status")?.as_str()?.to_owned(),
                        ))
                    })
                    .collect()
            })
            .unwrap_or_default();
        let transcript = row("transcript")
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
            .unwrap_or_default();
        let turns = row("stats")
            .get("turns")
            .and_then(Value::as_u64)
            .unwrap_or(0);
        // Model history from the surface projection only.
        let model_items = projections
            .surface_nodes()
            .map_err(SessionError::Corruption)?
            .into_iter()
            .map(|(_, item)| item)
            .collect();
        Ok(SessionView {
            header: header.clone(),
            title,
            todos,
            transcript,
            replay,
            model_items,
            turns,
            usage: UsageStats::default(),
        })
    }
}

fn active_floor(active: &ActiveSession) -> u64 {
    let projections = active.projections.lock().expect("projections");
    projections.live_floor()
}

/// Fold only when the projections lag the durable cursor: explicit journal
/// flushes fold their committed batches directly (P1-13), so a whole-file
/// physical read is reserved for events that reached the log outside the
/// folding journal (write-behind deadlines, resume seed markers, repairs).
fn fold_if_behind(active: &ActiveSession, backend: &JsonlBackend) -> Result<(), SessionError> {
    let floor = active_floor(active);
    let committed = active.coordinator.committed_seq();
    if committed.is_some_and(|committed| floor <= committed) {
        return fold_committed(active, backend, floor);
    }
    Ok(())
}

fn fold_committed(
    active: &ActiveSession,
    backend: &JsonlBackend,
    floor: u64,
) -> Result<(), SessionError> {
    let mut projections = active.projections.lock().expect("projections");
    backend.visit_from(&active.key, floor, &mut |event| projections.fold_one(event))?;
    Ok(())
}

/// Byte budget for one checkpoint record. Every row is derived and may be
/// omitted; the authoritative log rebuilds it. This caps the final file,
/// not merely the surface row: transcript/request data can also be large.
const CHECKPOINT_BYTE_CAP: usize = 8 * 1024 * 1024;

fn checkpoint_active(
    active: &ActiveSession,
    checkpoints: &CheckpointStore,
) -> Result<(), SessionError> {
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

struct ProjectionFoldJournal {
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

    fn committed_seq(&self) -> Option<u64> {
        self.inner.committed_seq()
    }
}

fn journal_with_projection_fold(
    active: &ActiveSession,
    backend: &Arc<JsonlBackend>,
) -> Arc<dyn RunJournal> {
    Arc::new(ProjectionFoldJournal {
        inner: active.coordinator.journal(),
        lane: Mutex::new(()),
        pending: Mutex::new(Vec::new()),
        projections: Arc::clone(&active.projections),
        backend: Arc::clone(backend),
        active_key: active.key.clone(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::event::payloads;
    use crate::session::replay::ReplayTurnEnd;

    fn service(_tag: &str) -> (SessionService, std::path::PathBuf) {
        let root = std::env::temp_dir().join(format!(
            "clat-usecases-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        (
            SessionService::new(root.clone(), JsonlCompression::Zstd).expect("service"),
            root,
        )
    }

    fn project() -> ProjectKey {
        ProjectKey::from_cwd("/tmp/usecases")
    }

    fn run_turn(service: &SessionService, text: &str) -> Result<(), SessionError> {
        let journal = service.journal()?;
        journal
            .append_atomic(&[
                crate::session::run_journal::NewSessionEvent::new(
                    "turn/start",
                    payloads::turn_start(1),
                ),
                crate::session::run_journal::NewSessionEvent::new(
                    "user/message",
                    payloads::user_message(text),
                )
                .append(Vec::new()),
                crate::session::run_journal::NewSessionEvent::new(
                    "turn/end",
                    payloads::turn_end(1, &crate::session::event::TurnEndReason::Completed),
                ),
            ])
            .map_err(SessionError::Corruption)?;
        journal.flush().map_err(SessionError::Corruption)?;
        Ok(())
    }

    /// 启动加载的分阶段计时（诊断用，`--ignored` 运行）。模拟一个真实
    /// 长会话的日志形状：多轮 ×（用户消息 + 助手回复 + 带内容的工具
    /// 调用/结果），然后分别计时 resume 流水线的三个完整日志遍：
    /// 恢复扫描（prepare）、checkpoint 之后的投影折叠（visit_from）、
    /// 前端转录重建（replay_with_usage）。数字用于指导"合并遍数"类
    /// 优化；事件体量刻意接近 dogfood 会话（工具结果数 KB）。
    #[test]
    #[ignore = "diagnostic timing bench, run with --nocapture"]
    fn large_journal_resume_phase_timing() {
        let (service, root) = service("resume-timing");
        let summary = service.new_session(&project()).expect("session");
        let key = SessionKey {
            id: summary.id.clone(),
            project: project(),
        };
        let tool_content = "x".repeat(4 * 1024);
        let turns = 400u64;
        for turn in 0..turns {
            let journal = service.journal().expect("journal");
            let usage = crate::model::Usage {
                input_tokens: 32000,
                output_tokens: 900,
                cached_input_tokens: Some(24000),
                reasoning_tokens: None,
            };
            let step = turn + 1;
            journal
                .append_atomic(&[
                    crate::session::run_journal::NewSessionEvent::new(
                        "turn/start",
                        payloads::turn_start(step),
                    ),
                    crate::session::run_journal::NewSessionEvent::new(
                        "user/message",
                        payloads::user_message(&format!("turn {turn}: please inspect and fix")),
                    )
                    .append(Vec::new()),
                    crate::session::run_journal::NewSessionEvent::new(
                        "assistant/message",
                        payloads::assistant_message(
                            step,
                            step,
                            vec![payloads::text_block(&format!(
                                "turn {turn} plan:\n- read the module\n- patch\n- verify"
                            ))],
                            "deepseek",
                            "deepseek-v4-pro",
                            Some(&usage),
                        ),
                    )
                    .append(Vec::new()),
                    crate::session::run_journal::NewSessionEvent::new(
                        "tool/call",
                        payloads::tool_call(
                            step,
                            step,
                            &format!("call-{turn}"),
                            "read_file",
                            &serde_json::json!({ "path": "src/lib.rs" }),
                        ),
                    ),
                    crate::session::run_journal::NewSessionEvent::new(
                        "tool/result",
                        payloads::tool_result(
                            step,
                            step,
                            &format!("call-{turn}"),
                            payloads::tool_result_content(&serde_json::json!(tool_content)),
                            false,
                        ),
                    )
                    .append(Vec::new()),
                    crate::session::run_journal::NewSessionEvent::new(
                        "turn/end",
                        payloads::turn_end(step, &crate::session::event::TurnEndReason::Completed),
                    ),
                ])
                .expect("append turn");
        }
        // 刷新一次写出全部待写批次，并让 checkpoint 站在日志末尾——
        // 计时的是"最近一次干净关闭后重开"的冷启动路径。
        service
            .journal()
            .expect("journal")
            .flush()
            .expect("flush pending batches");
        {
            let guard = service.active.lock().expect("active");
            let active = guard.as_ref().expect("armed");
            checkpoint_active(active, &service.checkpoints).expect("checkpoint");
        }
        let log_size = dir_size(&root);
        eprintln!(
            "journal: {turns} turns, {} bytes ({:.1} MiB)",
            log_size,
            log_size as f64 / 1024.0 / 1024.0
        );

        let time = |label: &str, f: &mut dyn FnMut()| {
            let start = std::time::Instant::now();
            f();
            eprintln!("{label}: {:?}", start.elapsed());
        };

        // 阶段 1：prepare 的全量恢复扫描（start_unseeded 内部路径）。
        time("prepare scan (full pass)", &mut || {
            service.backend.prepare(&key).expect("prepare");
        });
        // 阶段 2：checkpoint 命中时的投影折叠（floor 之后应近零）。
        time("projection fold from checkpoint", &mut || {
            service
                .backend
                .visit_from(&key, 0, &mut |_| Ok(()))
                .expect("visit");
        });
        // 阶段 3：前端转录重建（replay，永远从 seq 0）。
        time("replay_with_usage (full pass)", &mut || {
            let (replay, usage) = service.replay_with_usage(&key).expect("replay");
            assert!(!replay.is_empty());
            assert!(usage.session.input_tokens > 0);
        });
        // 整体：一次真实 resume（= 单遍流读 + 协调器开销）。
        time("full resume()", &mut || {
            service.resume(&key).expect("resume");
        });
        // 泄漏纪律同上：quiesce 后再清理临时目录。
        service.quiesce_active().expect("quiesce");
        std::fs::remove_dir_all(&root).ok();
    }

    /// 递归累加目录字节数（诊断用：报告日志体量）。
    fn dir_size(root: &std::path::Path) -> u64 {
        let mut total = 0;
        if let Ok(entries) = std::fs::read_dir(root) {
            for entry in entries.flatten() {
                let Ok(file_type) = entry.file_type() else {
                    continue;
                };
                if file_type.is_dir() {
                    total += dir_size(&entry.path());
                } else if let Ok(metadata) = entry.metadata() {
                    total += metadata.len();
                }
            }
        }
        total
    }

    /// 不变量 R-1（2026-08-19 启动性能，DSH 对照）：干净日志的冷
    /// resume 对日志恰好发起**一次**物理流读——prepare 的恢复扫描
    /// 同时完成投影折叠、转录回放与 usage 统计。torn-tail 崩溃修复
    /// 路径另行允许丢弃重读。pre-fix 为 3 遍（prepare 扫描 + 投影
    /// 折叠 + replay），日志体量线性放大后即用户可感的启动延迟。
    #[test]
    fn cold_resume_streams_the_log_exactly_once() {
        let (service, root) = service("single-pass");
        let summary = service.new_session(&project()).expect("session");
        let key = SessionKey {
            id: summary.id.clone(),
            project: project(),
        };
        for i in 0..3 {
            run_turn(&service, &format!("turn {i}")).expect("turn");
        }
        service.journal().expect("journal").flush().expect("flush");
        let before = service.stream_probe();
        let view = service.resume(&key).expect("cold resume");
        let streams = service.stream_probe() - before;
        assert_eq!(
            streams, 1,
            "a clean cold resume must stream the log exactly once (got {streams})"
        );
        assert!(
            !view.replay.is_empty(),
            "the replay was built in that one pass"
        );
        // resume() 安装了活动会话：不 quiesce 就结束会泄漏一个 writer
        // 线程，并行套件里抬高的全局计数会把相邻的线程回收测试顶红
        //（a_hundred_session_switches 实测）。下面的计时诊断同因，也
        // 必须在清理前 quiesce。
        service.quiesce_active().expect("quiesce");
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn checkpoint_budget_never_snapshots_unbounded_event_units() {
        let mut registry = ProjectionRegistry::clat();
        registry
            .fold_all(&[SessionEvent::new(
                "user/message",
                0,
                1,
                payloads::user_message(&"x".repeat(4 * 1024 * 1024)),
            )
            .append(Vec::new())])
            .expect("fold huge event");
        let bounded = registry.checkpoint_bounded(
            CheckpointIdentity {
                created_at: 1,
                cwd: Some("/tmp/usecases".into()),
            },
            7,
            CHECKPOINT_BYTE_CAP,
        );
        assert!(serde_json::to_vec_pretty(&bounded).unwrap().len() <= CHECKPOINT_BYTE_CAP);
        assert_eq!(bounded.generation, 7);
        assert!(!bounded.rows.contains_key("surface"));
        assert!(!bounded.rows.contains_key("transcript"));
    }

    #[test]
    fn checkpoint_generation_advances_on_each_publish() {
        let (service, root) = service("generation");
        let project = project();
        let summary = service.new_session(&project).expect("new");
        run_turn(&service, "hello").expect("run");
        let key = SessionKey {
            project,
            id: summary.id,
        };
        service.sync_active().expect("checkpoint 1");
        let first = service.checkpoints.load(&key).expect("first checkpoint");
        service.sync_active().expect("checkpoint 2");
        let second = service.checkpoints.load(&key).expect("second checkpoint");
        assert!(second.generation > first.generation);
        service.quiesce_active().expect("close");
        std::fs::remove_dir_all(root).expect("cleanup");
    }

    /// 回归（真实事故）：重启后第一次启动以 "changed while streaming" 失败，
    /// 第二次成功——install_armed 把 resume seed 留在 write-behind 车道里，
    /// 而挂载期 snapshot() 的全量流式读把它的落盘当成了外部写入者。
    /// 不变量：install 返回时 seed 必须已经持久化（读屏障，见
    /// [`SessionCoordinator::flush`]）。修复前该测试失败：marker 最长
    /// 200ms 后落盘，而这里的磁盘读取发生在微秒级。
    #[test]
    fn installed_resume_seed_is_durable_before_install_returns() {
        let (service, root) = service("seed-durable");
        let project = project();
        let summary = service.new_session(&project).expect("new");
        run_turn(&service, "hello").expect("turn");
        let key = SessionKey {
            project: project.clone(),
            id: summary.id.clone(),
        };
        // 退役现有 writer；日志末事件是 turn/end 而非 end-seed，
        // 下一次 prepare 因此需要 seed marker。
        service.quiesce_active().expect("quiesce");

        let staged = service.stage_resume(&key).expect("stage");
        let armed = service.arm_session(staged).expect("arm");
        let _view = service.install_armed(armed);

        // 用独立 backend 观察磁盘真值（绕过本进程任何内存状态）。
        let observer = JsonlBackend::new(root.clone(), JsonlCompression::Zstd, false);
        let events = observer.load(&key, false).expect("durable read").events;
        assert_eq!(
            events.last().expect("session has events").event_type,
            "session/end-seed",
            "install_armed must make the resume seed durable before returning"
        );
        service.quiesce_active().expect("cleanup");
        std::fs::remove_dir_all(root).expect("cleanup");
    }

    /// I4：回放是事件日志的纯折叠——删 checkpoint、走完整 resume 冷读
    /// 阶梯，结果必须逐项相等。
    #[test]
    fn replay_is_a_pure_log_fold_checkpoints_change_nothing() {
        let (service, root) = service("replay-checkpoints");
        let project = project();
        let summary = service.new_session(&project).expect("new");
        run_turn(&service, "hello").expect("run 1");
        run_turn(&service, "again").expect("run 2");
        service.sync_active().expect("checkpoint");
        let key = SessionKey {
            project: project.clone(),
            id: summary.id.clone(),
        };
        let direct = service.replay(&key).expect("replay with checkpoint");
        // Shape sanity before the equivalence claims: two user turns, each
        // explained by a completed turn/end (time/turn metadata varies, so
        // project to the essentials).
        let essentials = |items: &[ReplayEvent]| {
            items
                .iter()
                .map(|item| match item {
                    ReplayEvent::UserMessage { text, .. } => format!("user:{text}"),
                    ReplayEvent::TurnEnded { reason, .. } => format!("end:{reason:?}"),
                    other => format!("other:{other:?}"),
                })
                .collect::<Vec<_>>()
        };
        assert_eq!(
            essentials(&direct),
            vec![
                "user:hello".to_owned(),
                "end:Completed".to_owned(),
                "user:again".to_owned(),
                "end:Completed".to_owned(),
            ]
        );

        // A lazy session with no log replays empty (never an error).
        service.checkpoints.drop(&key);
        let staged = service.stage_resume(&key).expect("stage");
        let armed = service.arm_session(staged).expect("arm");
        assert_eq!(&armed.view.replay, &direct, "resume without checkpoint");
        service.discard_armed(armed).expect("discard");
        assert!(
            service
                .replay(&SessionKey {
                    project,
                    id: SessionId::generate()
                })
                .unwrap()
                .is_empty(),
            "a session without a log replays empty"
        );
        std::fs::remove_dir_all(root).expect("cleanup");
    }

    /// I5：崩溃残留（open step + 无 result 的 tool/call）经恢复闭合器
    /// 补齐后，合成事件如常回放——工具名照常配对。
    #[test]
    fn interrupted_log_replays_recovery_synthetic_closers() {
        let (service, root) = service("replay-interrupted");
        let project = project();
        let summary = service.new_session(&project).expect("new");
        let journal = service.journal().expect("journal");
        journal
            .append_atomic(&[
                crate::session::run_journal::NewSessionEvent::new(
                    "turn/start",
                    payloads::turn_start(1),
                ),
                crate::session::run_journal::NewSessionEvent::new(
                    "user/message",
                    payloads::user_message("do things"),
                )
                .append(Vec::new()),
                crate::session::run_journal::NewSessionEvent::new(
                    "step/start",
                    payloads::step_start(1, 0),
                ),
                // Real producers always announce the call in the settled
                // assistant message before the durable tool/call (recovery
                // registers pending calls from these blocks).
                crate::session::run_journal::NewSessionEvent::new(
                    "assistant/message",
                    payloads::assistant_message(
                        1,
                        0,
                        vec![payloads::tool_call_block(
                            "call-crash",
                            "read_file",
                            &serde_json::json!({"path": "x"}),
                        )],
                        "application-test",
                        "deterministic",
                        None,
                    ),
                )
                .append(Vec::new()),
                crate::session::run_journal::NewSessionEvent::new(
                    "tool/call",
                    payloads::tool_call(
                        1,
                        0,
                        "call-crash",
                        "read_file",
                        &serde_json::json!({"path": "x"}),
                    ),
                )
                .log_only(),
            ])
            .map_err(SessionError::Corruption)
            .expect("interrupted batch");
        journal
            .flush()
            .map_err(SessionError::Corruption)
            .expect("durable");
        service.quiesce_active().expect("close");

        let key = SessionKey {
            project,
            id: summary.id,
        };
        let staged = service.stage_resume(&key).expect("stage");
        let armed = service.arm_session(staged).expect("arm repairs the log");
        let replay = armed.view.replay.clone();
        // arm performs recovery before the view is built, so the synthetic
        // isError tool/result and interrupted turn/end replay like any other
        // producer event.
        assert!(
            replay.iter().any(|item| matches!(item,
                ReplayEvent::ToolFinished { call_id, tool, is_error, .. }
                if call_id == "call-crash" && tool == "read_file" && *is_error)),
            "synthetic outcome-unknown tool result must replay, paired by callId: {replay:?}"
        );
        assert!(
            replay.iter().any(|item| matches!(
                item,
                ReplayEvent::TurnEnded {
                    reason: ReplayTurnEnd::Interrupted,
                    ..
                }
            )),
            "the interrupted turn must explain its stop: {replay:?}"
        );
        service.discard_armed(armed).expect("discard");
        std::fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn new_session_never_overwrites_an_unquiesced_active_writer() {
        let (service, root) = service("new-overwrite");
        service.new_session(&project()).expect("first");
        assert!(matches!(
            service.new_session(&project()),
            Err(SessionError::Conflict(message)) if message.contains("quiescing")
        ));
        service.quiesce_active().expect("close first");
        assert!(
            !root.join("--tmp-usecases--").exists(),
            "both lazy attempts create no session bucket"
        );
    }

    #[test]
    fn empty_lazy_session_quiesces_without_materializing_and_retires_writer() {
        let (service, root) = service("empty-close");
        let baseline = crate::session::write_behind::live_writers_for_test();
        service.new_session(&project()).expect("new");
        service.quiesce_active().expect("empty close");
        wait_for_writer_baseline(baseline);
        assert!(
            !root.join("--tmp-usecases--").exists(),
            "an empty lazy session creates no session bucket"
        );
    }

    /// 不变量（2026-08-19 CI 失败根因）：忘记显式 quiesce/close 而
    /// drop 的活动会话也必须退役 writer——JoinHandle 的 drop 是分离，
    /// worker 在 condvar 上永生；泄漏的 writer 把并行套件里任何
    /// `wait_for_writer_baseline` 的窗口顶红（慢速 CI 必现）。
    /// `SessionCoordinator` 的 Drop 安全网保证这一点。
    ///
    /// 观察手段必须是**每实例存活探针**而非全局 writer 计数：全局计
    /// 数在并行套件里随别家测试的 writer 生灭抖动（第二版测试的
    /// spawn 断言因此把"计数恰好持平"误报成失败，CI 二连红）。
    /// pre-fix（无 Drop 安全网）：worker 永生，2s 轮询后断言失败。
    #[test]
    fn dropping_the_active_session_retires_its_writer() {
        use std::sync::atomic::Ordering;
        let (service, root) = service("drop-retires");
        let summary = service.new_session(&project()).expect("new");
        let _ = summary;
        run_turn(&service, "leave the writer holding a pending batch").expect("run");
        let coordinator = {
            let guard = service.active.lock().expect("active");
            guard.as_ref().expect("active session").coordinator.clone()
        };
        let alive = coordinator.writer_alive_handle_for_test();
        assert!(
            alive.load(Ordering::SeqCst),
            "the active session spawned a writer"
        );
        // 故意不 quiesce：drop 路径自己必须收拾线程（并尽力 flush）。
        drop(coordinator);
        drop(service);
        // close 同步 join，drop 返回即已退出——轮询只是 CI 磁盘 hiccup
        // 的余量（fsync 慢过 5s 才会吃满）。
        let mut retired = false;
        for _ in 0..500 {
            if !alive.load(Ordering::SeqCst) {
                retired = true;
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(
            retired,
            "dropping the active session must retire its writer (still alive after 5s)"
        );
        std::fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn quiesce_fold_error_still_joins_the_writer() {
        let (service, root) = service("error-close");
        let baseline = crate::session::write_behind::live_writers_for_test();
        let summary = service.new_session(&project()).expect("new");
        let direct = {
            let guard = service.active.lock().expect("active");
            guard
                .as_ref()
                .expect("active session")
                .coordinator
                .journal()
        };
        direct
            .append(crate::session::run_journal::NewSessionEvent::new(
                "turn/start",
                payloads::turn_start(1),
            ))
            .expect("append");
        direct.flush().expect("commit outside folding journal");
        let log = root
            .join("--tmp-usecases--")
            .join(summary.id.as_str())
            .join("session.jsonl.zstd");
        std::fs::write(&log, b"corrupt").expect("corrupt after commit");
        assert!(service.quiesce_active().is_err());
        wait_for_writer_baseline(baseline);
        std::fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn new_resume_and_list_round_trip_with_checkpoints() {
        let (service, root) = service("roundtrip");
        let project = project();
        let summary = service.new_session(&project).expect("new");
        run_turn(&service, "hello world").expect("run");
        service.sync_active().expect("sync");
        let key = SessionKey {
            project: project.clone(),
            id: summary.id.clone(),
        };

        // Detach (flush + checkpoint), then resume: the view carries the
        // conversation, and the seed marker landed exactly once.
        service.quiesce_active().expect("detach");
        let view = service.resume(&key).expect("resume");
        assert!(
            view.transcript
                .iter()
                .any(|line| line.kind == "user" && line.text == "hello world")
        );
        assert_eq!(view.turns, 1);
        assert!(!view.model_items.is_empty());

        // Second resume does not grow the log with another seed marker.
        service.quiesce_active().expect("detach 2");
        service.resume(&key).expect("resume 2");
        service.quiesce_active().expect("detach 3");
        let loaded = service.backend.load(&key, false).expect("load");
        let seed_markers = loaded
            .events
            .iter()
            .filter(|event| event.event_type == "session/end-seed")
            .count();
        assert_eq!(seed_markers, 1, "untouched reopens do not grow the log");

        // List reads the checkpoint: title/stats present without decoding
        // log bodies.
        let summaries = service.list_sessions(&project).expect("list");
        assert_eq!(summaries.len(), 1);
        assert_eq!(summaries[0].turns, 1);
        assert!(
            summaries[0].last_activity_ms >= summaries[0].created_at_ms
                || summaries[0].last_activity_ms == summaries[0].created_at_ms
        );

        std::fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn staging_is_read_only_and_failed_resume_keeps_the_active_session() {
        let (service, root) = service("stage-read-only");
        let project = project();
        let first = service.new_session(&project).expect("first");
        run_turn(&service, "keep me active").expect("run first");

        let bad_key = SessionKey {
            project: project.clone(),
            id: SessionId::new("unsupported-target"),
        };
        let bad_header =
            SessionHeader::new(bad_key.id.clone(), bad_key.project.header_cwd.clone(), 7);
        let prepared = service
            .backend
            .create(bad_key.clone(), bad_header)
            .expect("register target");
        service
            .backend
            .append_batch(
                prepared,
                0,
                &[SessionEvent::new(
                    "future/required",
                    0,
                    8,
                    serde_json::json!({"opaque": true}),
                )],
            )
            .expect("materialize unsupported target");

        assert!(service.resume(&bad_key).is_err());
        assert_eq!(
            service.active_id().as_ref(),
            Some(&first.id),
            "target admission must finish before the current session is quiesced"
        );

        let first_key = SessionKey {
            project,
            id: first.id,
        };
        service.quiesce_active().expect("detach first");
        service.checkpoints.drop(&first_key);
        let staged = service.stage_resume(&first_key).expect("stage first");
        assert!(
            service.checkpoints.load(&first_key).is_none(),
            "staging must not publish a derived checkpoint before workspace CAS"
        );
        let armed = service.arm_session(staged).expect("arm first");
        service.install_armed(armed);
        service.quiesce_active().expect("cleanup session");
        std::fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn title_cas_rejects_stale_and_accepts_force() {
        let (service, root) = service("title");
        let project = project();
        let summary = service.new_session(&project).expect("new");
        let session = summary.id.clone();
        run_turn(&service, "fix the login bug").expect("run");
        service.sync_active().expect("sync");

        // A late automatic rename against NoTitle loses to the manual one.
        assert!(
            service
                .set_title(
                    &session,
                    SetTitleExpectation::NoTitle,
                    "manual name",
                    TitleSource::User
                )
                .expect("manual")
        );
        assert!(
            !service
                .set_title(
                    &session,
                    SetTitleExpectation::NoTitle,
                    "late auto",
                    TitleSource::Provider {
                        provider: "prov",
                        model: "mdl",
                    },
                )
                .expect("cas check"),
            "NoTitle no longer matches"
        );
        // Force always wins.
        assert!(
            service
                .set_title(
                    &session,
                    SetTitleExpectation::Force,
                    "forced",
                    TitleSource::User
                )
                .expect("force")
        );
        service.quiesce_active().expect("detach");

        let summaries = service.list_sessions(&project).expect("list");
        assert_eq!(summaries[0].title.as_deref(), Some("forced"));
        std::fs::remove_dir_all(root).expect("cleanup");
    }

    /// 第四轮复审 F-A：迟到的自动命名 job 绑定原会话——切换后不得把
    /// 标题写进当前活动会话（修复前：期望值来自旧会话、写入作用于新
    /// 会话，两个都 NoTitle 时标题落错日志）。
    #[test]
    fn stale_title_jobs_never_write_into_the_switched_to_session() {
        let (service, root) = service("title-race");
        let project = project();
        let first = service.new_session(&project).expect("first");
        run_turn(&service, "first session prompt").expect("run");
        service.sync_active().expect("sync");

        // Switch away (quiesce + new active session with no title).
        service.quiesce_active().expect("quiesce first");
        let second = service.new_session(&project).expect("second");
        assert_ne!(first.id, second.id);

        // The stale job for the first session: silent no-op, never a write.
        assert!(
            !service
                .set_title(
                    &first.id,
                    SetTitleExpectation::NoTitle,
                    "late title",
                    TitleSource::Provider {
                        provider: "prov",
                        model: "mdl",
                    },
                )
                .expect("stale job is a no-op, not an error")
        );
        // The active session stays untitled; a bound write still works.
        let (title, _) = service.title_state();
        assert_eq!(title, None, "no title leaked into the second session");
        assert!(
            service
                .set_title(
                    &second.id,
                    SetTitleExpectation::NoTitle,
                    "bound title",
                    TitleSource::User
                )
                .expect("bound write")
        );
        service.quiesce_active().expect("detach");

        let summaries = service.list_sessions(&project).expect("list");
        let by_id = |id: &crate::session::id::SessionId| {
            summaries
                .iter()
                .find(|summary| summary.id == *id)
                .expect("summary")
        };
        assert_eq!(by_id(&second.id).title.as_deref(), Some("bound title"));
        // The first session's summary shows its fallback title (derived
        // from the first user message) — the invariant is that NO explicit
        // `session/title` event was written to its log.
        let key = SessionKey {
            project: project.clone(),
            id: first.id.clone(),
        };
        let loaded = service.backend.load(&key, false).expect("load first");
        assert!(
            !loaded
                .events
                .iter()
                .any(|event| event.event_type == "session/title"),
            "the stale job never titled the first session either"
        );
        std::fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn deleting_checkpoints_changes_nothing_but_replay() {
        let (service, root) = service("drop-cache");
        let project = project();
        let summary = service.new_session(&project).expect("new");
        run_turn(&service, "one").expect("run");
        run_turn(&service, "two").expect("run 2");
        service.sync_active().expect("sync");
        let key = SessionKey {
            project: project.clone(),
            id: summary.id.clone(),
        };
        service.quiesce_active().expect("detach");
        let with_cache = service.resume(&key).expect("resume");
        service.quiesce_active().expect("detach");

        // Delete every checkpoint: cold resume must produce the same view.
        service.checkpoints.drop(&key);
        let without_cache = service.resume(&key).expect("resume from log");
        assert_eq!(without_cache.transcript, with_cache.transcript);
        assert_eq!(without_cache.turns, with_cache.turns);
        assert_eq!(without_cache.title, with_cache.title);
        service.quiesce_active().expect("cleanup detach");
        std::fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn recent_inputs_come_from_the_transcript() {
        let (service, root) = service("inputs");
        let project = project();
        service.new_session(&project).expect("new");
        run_turn(&service, "first question").expect("run");
        run_turn(&service, "second question").expect("run 2");
        let inputs = service.recent_inputs(10).expect("inputs");
        assert_eq!(inputs, vec!["first question", "second question"]);
        let limited = service.recent_inputs(1).expect("limited");
        assert_eq!(limited, vec!["second question"]);
        service.quiesce_active().expect("detach");
        std::fs::remove_dir_all(root).expect("cleanup");
    }

    /// 并行测试会同时持有各自的 writer：断言用"回到基线"的轮询形式。
    /// 预算 30s（正常路径立即返回）：CI 慢机上兄弟测试的 writer 可能
    /// 存活数秒；真泄漏时会留下约 100 个 writer 永不退休，照样超时。
    fn wait_for_writer_baseline(baseline: usize) {
        for _ in 0..1_200 {
            if crate::session::write_behind::live_writers_for_test() <= baseline {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(25));
        }
        panic!(
            "writer threads did not retire (baseline {baseline}, now {})",
            crate::session::write_behind::live_writers_for_test()
        );
    }

    #[test]
    fn a_hundred_session_switches_retire_every_writer_thread() {
        let (service, root) = service("switches");
        let project = project();
        let baseline = crate::session::write_behind::live_writers_for_test();
        let mut last_id = None;
        for round in 0..100 {
            let summary = service.new_session(&project).expect("new");
            run_turn(&service, &format!("round {round}")).expect("run");
            // The quiesce is the detach boundary: it must join the writer,
            // not leak it (审计 P1-07 的线程泄漏反例)。
            service
                .quiesce_active()
                .unwrap_or_else(|error| panic!("quiesce failed: {error}"));
            last_id = Some(summary.id);
        }
        wait_for_writer_baseline(baseline);
        // And the last session still resumes with its content intact.
        let key = SessionKey {
            project: project.clone(),
            id: last_id.expect("id"),
        };
        let view = service.resume(&key).expect("resume");
        assert!(
            view.transcript
                .iter()
                .any(|line| line.kind == "user" && line.text == "round 99")
        );
        service.quiesce_active().expect("cleanup");
        std::fs::remove_dir_all(root).expect("cleanup");
    }

    /// 复审第二轮：install 时 arm writer 会做 torn-tail 修复并追加合成
    /// closer；追赶折叠必须把这些事件折进 staged 投影，否则中断 turn 不
    /// 计数、下一轮 turn 号可能与中断轮撞号。
    /// 第三轮复审：并发生产者在 inner.flush 与 pending 取出之间 append 的
    /// 事件尚未提交——折叠必须止步于 committed 游标，否则一次 NotCommitted
    /// 回滚就会让投影越过 seq 空洞领先于磁盘。
    struct SteppedInnerJournal {
        next_seq: std::sync::atomic::AtomicU64,
        committed: std::sync::atomic::AtomicU64,
    }

    impl RunJournal for SteppedInnerJournal {
        fn append_atomic(
            &self,
            events: &[crate::session::run_journal::NewSessionEvent],
        ) -> Result<crate::session::run_journal::SeqRange, String> {
            use std::sync::atomic::Ordering;
            let start = self
                .next_seq
                .fetch_add(events.len() as u64, Ordering::SeqCst);
            Ok(crate::session::run_journal::SeqRange {
                start,
                end_inclusive: start + events.len() as u64 - 1,
            })
        }
        fn flush(&self) -> Result<(), String> {
            Ok(())
        }
        fn committed_seq(&self) -> Option<u64> {
            Some(self.committed.load(std::sync::atomic::Ordering::SeqCst))
        }
    }

    #[test]
    fn direct_folds_never_pass_the_committed_cursor() {
        let root = std::env::temp_dir().join(format!(
            "clat-foldcursor-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let backend = Arc::new(JsonlBackend::new(
            &root,
            crate::session::persistence::JsonlCompression::Zstd,
            true,
        ));
        let key = SessionKey {
            project: project(),
            id: SessionId::new("cursor"),
        };
        let inner = Arc::new(SteppedInnerJournal {
            next_seq: std::sync::atomic::AtomicU64::new(0),
            committed: std::sync::atomic::AtomicU64::new(0),
        });
        let projections = Arc::new(Mutex::new(ProjectionRegistry::clat()));
        let journal = ProjectionFoldJournal {
            inner: Arc::clone(&inner) as Arc<dyn RunJournal>,
            lane: Mutex::new(()),
            pending: Mutex::new(Vec::new()),
            projections: Arc::clone(&projections),
            backend: Arc::clone(&backend),
            active_key: key,
        };
        let event = |turn: u64| {
            crate::session::run_journal::NewSessionEvent::new(
                "turn/start",
                payloads::turn_start(turn),
            )
        };
        let floor = || projections.lock().expect("projections").live_floor();

        // Two events appended, both committed: both fold.
        journal
            .append_atomic(&[event(1), event(2)])
            .expect("append 1");
        inner
            .committed
            .store(1, std::sync::atomic::Ordering::SeqCst);
        journal.flush().expect("flush 1");
        assert_eq!(floor(), 2, "committed events fold");

        // Two more appended (seqs 2,3); the commit cursor only reaches 2 —
        // seq 3 must stay queued, not folded.
        journal
            .append_atomic(&[event(3), event(4)])
            .expect("append 2");
        inner
            .committed
            .store(2, std::sync::atomic::Ordering::SeqCst);
        journal.flush().expect("flush 2");
        assert_eq!(floor(), 3, "the uncommitted tail stays queued");

        // The commit lands: the next flush picks it up.
        inner
            .committed
            .store(3, std::sync::atomic::Ordering::SeqCst);
        journal.flush().expect("flush 3");
        assert_eq!(floor(), 4, "the committed event folds on the next flush");
        std::fs::remove_dir_all(root).ok();
    }

    struct AppendFlushOverlapJournal {
        append_active: std::sync::atomic::AtomicBool,
        overlap: std::sync::atomic::AtomicBool,
    }

    impl RunJournal for AppendFlushOverlapJournal {
        fn append_atomic(
            &self,
            events: &[crate::session::run_journal::NewSessionEvent],
        ) -> Result<crate::session::run_journal::SeqRange, String> {
            self.append_active
                .store(true, std::sync::atomic::Ordering::SeqCst);
            std::thread::sleep(std::time::Duration::from_millis(40));
            self.append_active
                .store(false, std::sync::atomic::Ordering::SeqCst);
            Ok(crate::session::run_journal::SeqRange {
                start: 0,
                end_inclusive: events.len() as u64 - 1,
            })
        }

        fn flush(&self) -> Result<(), String> {
            if self.append_active.load(std::sync::atomic::Ordering::SeqCst) {
                self.overlap
                    .store(true, std::sync::atomic::Ordering::SeqCst);
            }
            Ok(())
        }

        fn committed_seq(&self) -> Option<u64> {
            Some(0)
        }
    }

    #[test]
    fn projection_registration_is_atomic_against_concurrent_flush() {
        let root = std::env::temp_dir().join(format!(
            "clat-fold-lane-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let backend = Arc::new(JsonlBackend::new(
            &root,
            crate::session::persistence::JsonlCompression::Zstd,
            true,
        ));
        let inner = Arc::new(AppendFlushOverlapJournal {
            append_active: std::sync::atomic::AtomicBool::new(false),
            overlap: std::sync::atomic::AtomicBool::new(false),
        });
        let journal = Arc::new(ProjectionFoldJournal {
            inner: Arc::clone(&inner) as Arc<dyn RunJournal>,
            lane: Mutex::new(()),
            pending: Mutex::new(Vec::new()),
            projections: Arc::new(Mutex::new(ProjectionRegistry::clat())),
            backend,
            active_key: SessionKey {
                project: project(),
                id: SessionId::new("fold-lane"),
            },
        });
        let append_journal = Arc::clone(&journal);
        let append = std::thread::spawn(move || {
            append_journal.append_atomic(&[crate::session::run_journal::NewSessionEvent::new(
                "turn/start",
                payloads::turn_start(1),
            )])
        });
        while !inner
            .append_active
            .load(std::sync::atomic::Ordering::SeqCst)
        {
            std::thread::yield_now();
        }
        journal.flush().expect("flush");
        append.join().unwrap().expect("append");
        assert!(
            !inner.overlap.load(std::sync::atomic::Ordering::SeqCst),
            "flush cannot pass queue admission before pending registration"
        );
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn torn_tail_resume_counts_the_interrupted_turn() {
        let (service, root) = service("tornresume");
        let project = project();
        let summary = service.new_session(&project).expect("new");
        run_turn(&service, "first complete turn").expect("run");
        // Append an open second turn, then tear the file mid-frame.
        let journal = service.journal().expect("journal");
        journal
            .append_atomic(&[
                crate::session::run_journal::NewSessionEvent::new(
                    "turn/start",
                    payloads::turn_start(2),
                ),
                crate::session::run_journal::NewSessionEvent::new(
                    "user/message",
                    payloads::user_message("second"),
                )
                .append(Vec::new()),
            ])
            .expect("open turn");
        journal.flush().expect("flush");
        service.quiesce_active().expect("detach");
        let key = SessionKey {
            project: project.clone(),
            id: summary.id.clone(),
        };
        let log = root
            .join("--tmp-usecases--")
            .join(summary.id.as_str())
            .join("session.jsonl.zstd");
        let bytes = std::fs::read(&log).expect("read");
        std::fs::write(&log, &bytes[..bytes.len() - 3]).expect("tear");

        let view = service.resume(&key).expect("resume");
        assert_eq!(view.turns, 2, "the interrupted turn is closed and counted");
        assert_eq!(
            service.active_turns().expect("turns"),
            2,
            "the next run's turn number must not collide with turn 2"
        );
        service.quiesce_active().expect("cleanup");
        std::fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn staging_a_corrupt_target_fails_without_leaking_a_writer() {
        let (service, root) = service("corrupt");
        let project = project();
        let summary = service.new_session(&project).expect("new");
        run_turn(&service, "hello").expect("run");
        service.quiesce_active().expect("detach");
        let key = SessionKey {
            project: project.clone(),
            id: summary.id.clone(),
        };
        // Corrupt the log *semantically*: append a replace that cites
        // nonexistent surface nodes. prepare/admission pass (the bytes and
        // payloads are well-formed); cold restore fails and the read-only
        // stage path must leave no active session or writer behind.
        let loaded = service.backend.load(&key, false).expect("read len");
        let seq = loaded.events.len() as u64;
        let mut bad = crate::session::event::SessionEvent::new(
            "user/message",
            seq,
            9_999,
            crate::session::event::payloads::user_message("[bogus summary]"),
        );
        bad.surface_op = Some(crate::session::event::SurfaceOp::Replace { start: 99, end: 99 });
        bad.source_event_seqs = Some(vec![99]);
        let frame = crate::session::jsonl::append_batch_bytes(
            &[bad],
            crate::session::persistence::JsonlCompression::Zstd,
            true,
        )
        .expect("encode");
        let log = root
            .join("--tmp-usecases--")
            .join(summary.id.as_str())
            .join("session.jsonl.zstd");
        let mut bytes = std::fs::read(&log).expect("read");
        bytes.extend_from_slice(&frame);
        std::fs::write(&log, &bytes).expect("append");
        let baseline = crate::session::write_behind::live_writers_for_test();
        let staged = service.stage_resume(&key).expect("bounded header stage");
        assert!(service.arm_session(staged).is_err());
        assert!(
            service.active_id().is_none(),
            "a failed stage must not install anything"
        );
        wait_for_writer_baseline(baseline);
        std::fs::remove_dir_all(root).expect("cleanup");
    }
}
