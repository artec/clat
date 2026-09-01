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
use crate::session::projection::{
    CheckpointIdentity, ProjectionRegistry, committed_admission_from_event,
};
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
pub(crate) struct ApprovedPlanWrite {
    pub(crate) text: String,
    pub(crate) digest: String,
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

/// 已通过当前会话可达性与 no-follow 文件栅栏的不可变附件快照。它只在
/// 受信应用壳与同进程前端之间流转；绝不投影到 journal、SSE 或模型请求，
/// 也不暴露任何路径。serve 只能按已验证的固定长度分块读取该快照，底层
/// store inode 在验证后被改写也不会改变已经授权的响应字节。
pub(crate) struct ActiveAttachmentReader {
    pub(crate) descriptor: crate::message::AttachmentDescriptor,
    pub(crate) bytes: u64,
    pub(crate) file: std::io::Cursor<Vec<u8>>,
}

/// Usage stats folded from one journal pass: the session aggregate (cache
/// ratio numerator/denominator), the most recent report (the current
/// context watermark), and per-route aggregates (INV-C1: the status-bar
/// cache ratio is scoped to the current model route — switching models
/// neither mixes nor clears buckets; provider-side caches survive detours,
/// so the accounting must too).
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct UsageStats {
    pub session: crate::model::Usage,
    pub last_request: Option<crate::model::Usage>,
    /// 按 `model_route_key`（journal source 的 provider/model）分桶的
    /// 累计；显示端取"当前配置路由"的桶。
    pub routes: std::collections::BTreeMap<String, crate::model::Usage>,
}

impl UsageStats {
    /// 单条事件入账（两处折叠共用，保证 live/replay 平价，INV-C2）。
    fn record(&mut self, event: &SessionEvent) {
        if event.event_type != "assistant/message" {
            return;
        }
        let Some(report) = usage_from_event(event) else {
            return;
        };
        self.session.add_assign(&report);
        if let Some(key) = route_key_of_event(event) {
            self.routes.entry(key).or_default().add_assign(&report);
        }
        self.last_request = Some(report);
    }
}

/// journal `assistant/message.message.source {kind: model, provider,
/// model}` → 路由键；无 source 或非模型来源（旧日志/异常形状）不入桶
/// （session 口径仍计，Cache 显示按 `--%` 兜底）。
fn route_key_of_event(event: &SessionEvent) -> Option<String> {
    let source = event.data.get("message")?.get("source")?;
    if source.get("kind").and_then(serde_json::Value::as_str) != Some("model") {
        return None;
    }
    let provider = source.get("provider")?.as_str()?;
    let model = source.get("model")?.as_str()?;
    Some(crate::model::model_route_key(provider, model))
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
        self.usage.record(event);
        self.pushed += 1;
        Ok(())
    }
}

fn committed_admission_from_projections(
    projections: &ProjectionRegistry,
    client_message_id: &str,
) -> Option<crate::message::CommittedAdmission> {
    let state = projections.state_snapshot("receipts")?;
    let entries = state.get("entries")?.as_array()?;
    let entry = entries.iter().find(|entry| {
        entry.get("client_message_id").and_then(Value::as_str) == Some(client_message_id)
    })?;
    Some(crate::message::CommittedAdmission {
        receipt: crate::message::AdmissionReceipt::committed(
            client_message_id.to_owned(),
            entry
                .get("message_id")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned(),
            entry
                .get("attachment_ids")
                .and_then(Value::as_array)
                .map(|ids| {
                    ids.iter()
                        .filter_map(|id| id.as_str().map(str::to_owned))
                        .collect()
                })
                .unwrap_or_default(),
        ),
        request_digest: entry
            .get("request_digest")
            .and_then(Value::as_str)
            .map(str::to_owned),
    })
}

pub(crate) struct SessionService {
    backend: Arc<JsonlBackend>,
    checkpoints: CheckpointStore,
    active: Mutex<Option<ActiveSession>>,
    #[cfg(test)]
    fail_next_plan_checkpoint: std::sync::atomic::AtomicBool,
    #[cfg(test)]
    fail_next_permission_checkpoint: std::sync::atomic::AtomicBool,
    #[cfg(test)]
    fail_next_admission_owner_scan: std::sync::atomic::AtomicBool,
    #[cfg(test)]
    fail_next_quiesce: std::sync::atomic::AtomicBool,
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
            #[cfg(test)]
            fail_next_plan_checkpoint: std::sync::atomic::AtomicBool::new(false),
            #[cfg(test)]
            fail_next_permission_checkpoint: std::sync::atomic::AtomicBool::new(false),
            #[cfg(test)]
            fail_next_admission_owner_scan: std::sync::atomic::AtomicBool::new(false),
            #[cfg(test)]
            fail_next_quiesce: std::sync::atomic::AtomicBool::new(false),
        })
    }

    #[cfg(test)]
    pub(crate) fn inject_persistence_faults(&self, hooks: crate::session::persistence::FaultHooks) {
        self.backend.inject_faults(hooks);
    }

    #[cfg(test)]
    pub(crate) fn inject_next_admission_owner_scan_failure(&self) {
        self.fail_next_admission_owner_scan
            .store(true, Ordering::Release);
    }

    #[cfg(test)]
    pub(crate) fn inject_next_quiesce_failure(&self) {
        self.fail_next_quiesce.store(true, Ordering::Release);
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
        // INV-MM1-4：会话打开时的有界 orphan 回收（引用集合来自上方
        // 单遍收集；附件域不存在则跳过——全新会话无附件；失败静默：
        // 回收是增益，不得阻塞会话打开）。
        let attachments_root = crate::session::path_layout::session_dir(
            self.backend.root_path(),
            key.project.header_cwd.as_deref(),
            &key.id,
        )
        .join("attachments");
        if let Ok(session_dir) = self.backend.open_session_dir(&key)
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
    /// ≤8MiB），再复制进会话目录的 `attachments/` 子目录（uuid 文件名
    /// 保留原扩展名）。返回 (绝对路径, MIME) 列表——绝对引用随后进
    /// journal，回放零换算；原件此后可删可改，会话自包含。
    /// 校验失败在任何复制之前返回错误（不留半套附件）。
    /// 导入图片附件（M4 → MM-1A 元数据化 → MM-1 S2/S3 接线 store）：
    /// [`crate::session::attachments::AttachmentStore::admit`] 完成
    /// S1 校验 + 批次上限 + 完整解码规范化 + 内容寻址发布——返回
    /// [`JournalImage`]：attachmentId = 规范化字节的 sha256（opaque）、
    /// 宽高 = 规范化后值（original_* 记源尺寸）、字节 = 规范化计数，
    /// 全部随 journal 落盘（回放重建 descriptor 零文件 I/O）。批次
    /// 失败在任何 journal 写入之前整体失败（零可达半成品，INV-MM1-3）。
    pub(crate) fn import_attachments(
        &self,
        sources: &[std::path::PathBuf],
    ) -> Result<Vec<crate::message::JournalImage>, SessionError> {
        if sources.is_empty() {
            return Ok(Vec::new());
        }
        let (key, attachments_dir) = {
            let active = self.active.lock().expect("active");
            let session = active
                .as_ref()
                .ok_or_else(|| SessionError::NotFound("no active session".into()))?;
            (
                session.key.clone(),
                crate::session::path_layout::session_dir(
                    self.backend.root_path(),
                    session.key.project.header_cwd.as_deref(),
                    &session.key.id,
                )
                .join("attachments"),
            )
        };
        let session_dir = self.backend.create_session_dir(&key)?;
        let store = crate::session::attachments::AttachmentStore::open_in_session(
            &session_dir,
            attachments_dir,
        )
        .map_err(|error| SessionError::Io(format!("open attachment store: {error}")))?;
        let stored = store
            .admit(sources)
            .map_err(|error| SessionError::Io(error.to_string()))?;
        Ok(stored
            .into_iter()
            .map(|stored| crate::message::JournalImage {
                descriptor: crate::message::AttachmentDescriptor {
                    attachment_id: stored.id,
                    media_type: stored.media_type.to_owned(),
                    width: stored.width,
                    height: stored.height,
                    bytes: stored.bytes,
                    display_name: stored.display_name,
                    original_width: Some(stored.original_width),
                    original_height: Some(stored.original_height),
                },
                path: stored.blob_path,
            })
            .collect())
    }

    /// MM-2/W5 byte admission for sources already read through a narrower
    /// capability (project-relative no-follow reads or core-minted run
    /// scratch). It deliberately reuses AttachmentStore normalization and
    /// content-addressed publication; callers never get to mint an arbitrary
    /// descriptor/path pair.
    pub(crate) fn import_attachment_bytes(
        &self,
        bytes: &[u8],
        display_name: &str,
    ) -> Result<crate::message::JournalImage, SessionError> {
        let store = self.active_attachment_store()?;
        let stored = store
            .admit_bytes(bytes, display_name)
            .map_err(|error| SessionError::Io(error.to_string()))?;
        Ok(journal_image(stored))
    }

    /// Resolve an attachment id only when it is reachable from the active
    /// model surface. An orphan blob whose digest is guessed is not authority.
    /// The returned path has already crossed the session fence; provider reads
    /// still use their final no-follow open to close replacement races.
    pub(crate) fn resolve_active_attachment(
        &self,
        attachment_id: &str,
    ) -> Result<crate::message::JournalImage, SessionError> {
        if attachment_id.is_empty()
            || attachment_id.len() > 128
            || !attachment_id
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
        {
            return Err(SessionError::NotFound("invalid attachment id".into()));
        }
        for (_, item) in self.surface_nodes()? {
            match item {
                ModelItem::User { content } | ModelItem::Assistant { content, .. } => {
                    for part in content {
                        if let crate::model::ContentPart::Image { path, media_type } = part
                            && attachment_path_matches(&path, attachment_id)
                        {
                            return journal_image_from_path(
                                attachment_id,
                                &path,
                                &media_type,
                                None,
                            );
                        }
                    }
                }
                ModelItem::ToolResult(result) => {
                    let image_blocks = result.blocks.iter().filter_map(|block| match block {
                        crate::message::ContentBlock::Image { attachment } => Some(attachment),
                        crate::message::ContentBlock::Text { .. } => None,
                    });
                    let image_parts = result.image_parts.iter().filter_map(|part| match part {
                        crate::model::ContentPart::Image { path, .. } => Some(path),
                        crate::model::ContentPart::Text(_) => None,
                    });
                    if image_blocks.clone().count() != image_parts.clone().count() {
                        // Descriptor authority and fenced paths are parallel
                        // projections of one ordered durable content array.
                        // A cardinality drift means their pairing is no longer
                        // provable, so fail closed for the whole tool result.
                        continue;
                    }
                    for (attachment, path) in image_blocks.zip(image_parts) {
                        if attachment.attachment_id == attachment_id {
                            return Ok(crate::message::JournalImage {
                                descriptor: attachment.clone(),
                                path: path.clone(),
                            });
                        }
                    }
                }
                _ => {}
            }
        }
        Err(SessionError::NotFound(format!(
            "attachment `{attachment_id}` is not reachable from the active session"
        )))
    }

    /// 读取当前会话中**可达**的图片内容。先走同一 reachability 判定，
    /// 再以 no-follow 打开并生成有界、摘要已验证的不可变快照，避免
    /// guessed blob id、软链、前端传入路径或 verify→stream 原位改写成为
    /// 读取权限。返回的 reader 不带路径，调用方只能以受限块大小消费它。
    pub(crate) fn open_active_attachment(
        &self,
        attachment_id: &str,
    ) -> Result<ActiveAttachmentReader, SessionError> {
        let image = self.resolve_active_attachment(attachment_id)?;
        let content_addressed = image.descriptor.attachment_id.len() == 64
            && image
                .descriptor
                .attachment_id
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase());
        let (snapshot, bytes) = if content_addressed {
            self.active_attachment_store()?
                .open_blob_verified(&image.descriptor.attachment_id)
                .map_err(|error| {
                    SessionError::NotFound(format!(
                        "attachment integrity verification failed: {error}"
                    ))
                })?
        } else {
            open_attachment_file(&image.path)?
        };
        if !crate::media::media_type_matches_bytes(&image.descriptor.media_type, &snapshot) {
            return Err(SessionError::NotFound(
                "attachment media type verification failed: attachment media type does not match its bytes".into(),
            ));
        }
        // New durable descriptors are minted from normalized content and must
        // describe those exact bytes. Legacy ids may legitimately carry
        // bytes=0 (unknown), but a content-addressed descriptor has no such
        // compatibility allowance: bind its count to the same verified file
        // handle before any frontend can consume it.
        if content_addressed && image.descriptor.bytes != bytes {
            return Err(SessionError::NotFound(
                "attachment descriptor byte count does not match its bytes".into(),
            ));
        }
        if content_addressed {
            let expected = (image.descriptor.width, image.descriptor.height);
            if crate::media::image_dimensions_bytes(&snapshot) != Some(expected) {
                return Err(SessionError::NotFound(
                    "attachment dimension verification failed: attachment dimensions do not match its bytes".into(),
                ));
            }
        }
        Ok(ActiveAttachmentReader {
            descriptor: image.descriptor,
            bytes,
            file: std::io::Cursor::new(snapshot),
        })
    }

    fn active_attachment_store(
        &self,
    ) -> Result<crate::session::attachments::AttachmentStore, SessionError> {
        let active = self.active.lock().expect("active");
        let session = active
            .as_ref()
            .ok_or_else(|| SessionError::NotFound("no active session".into()))?;
        let key = session.key.clone();
        let attachments_dir = crate::session::path_layout::session_dir(
            self.backend.root_path(),
            session.key.project.header_cwd.as_deref(),
            &session.key.id,
        )
        .join("attachments");
        drop(active);
        let session_dir = self.backend.create_session_dir(&key)?;
        crate::session::attachments::AttachmentStore::open_in_session(&session_dir, attachments_dir)
            .map_err(|error| SessionError::Io(format!("open attachment store: {error}")))
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
    /// append + flush 是事实提交点，checkpoint 是可重建缓存；latest-wins，无 CAS——档位切换只有
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
        journal
            .append_atomic_durable(&[event])
            .map_err(SessionError::Corruption)?;
        #[cfg(test)]
        let checkpoint = if self
            .fail_next_permission_checkpoint
            .swap(false, Ordering::AcqRel)
        {
            Err(SessionError::Io(
                "intentional permission-mode checkpoint failure".into(),
            ))
        } else {
            checkpoint_active(active, &self.checkpoints)
        };
        #[cfg(not(test))]
        let checkpoint = checkpoint_active(active, &self.checkpoints);
        if let Err(error) = checkpoint {
            eprintln!(
                "clat: warning: permission-mode checkpoint refresh failed after durable commit: {error}"
            );
        }
        Ok(true)
    }

    /// Active session Plan Mode projection. Missing state is inactive.
    pub(crate) fn plan_mode_state(&self) -> crate::plan_mode::PlanModeState {
        let guard = self.active.lock().expect("active");
        let Some(active) = guard.as_ref() else {
            return crate::plan_mode::PlanModeState::default();
        };
        let projections = active.projections.lock().expect("projections");
        let state = projections.state_snapshot("plan-mode").unwrap_or_default();
        crate::plan_mode::PlanModeState {
            active: state
                .get("active")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            approved: state.get("approved").and_then(|approved| {
                Some(crate::plan_mode::ApprovedPlan {
                    text: approved.get("text")?.as_str()?.to_owned(),
                    digest: approved.get("digest")?.as_str()?.to_owned(),
                    event_seq: approved.get("eventSeq")?.as_u64()?,
                })
            }),
        }
    }

    /// Append+flush a DSH `plan/mode` fact. The append+flush is the commit
    /// point; a checkpoint refresh failure is rebuildable and does not roll
    /// back a user-approved plan that is already durable.
    pub(crate) fn record_plan_mode(
        &self,
        active_mode: bool,
        approved: Option<ApprovedPlanWrite>,
    ) -> Result<Option<u64>, SessionError> {
        if active_mode && approved.is_some() {
            return Err(SessionError::Corruption(
                "approved plan is valid only when plan mode becomes inactive".into(),
            ));
        }
        let guard = self.active.lock().expect("active");
        let Some(active) = guard.as_ref() else {
            return Ok(None);
        };
        let current = {
            let projections = active.projections.lock().expect("projections");
            projections.state_snapshot("plan-mode").unwrap_or_default()
        };
        if approved.is_none()
            && current
                .get("active")
                .and_then(Value::as_bool)
                .unwrap_or(false)
                == active_mode
            && current.get("approved").is_none()
        {
            return Ok(None);
        }
        let mut data = serde_json::json!({ "active": active_mode });
        if let Some(approved) = approved {
            data.as_object_mut().expect("plan payload object").insert(
                "approved".into(),
                serde_json::json!({ "text": approved.text, "digest": approved.digest }),
            );
        }
        let mut journal_slot = active.journal.lock().expect("session journal");
        if journal_slot.is_none() {
            *journal_slot = Some(journal_with_projection_fold(active, &self.backend));
        }
        let journal = Arc::clone(journal_slot.as_ref().expect("just initialized"));
        drop(journal_slot);
        let seq = journal
            .append(crate::session::run_journal::NewSessionEvent::new(
                "plan/mode",
                data,
            ))
            .map_err(SessionError::Corruption)?;
        journal.flush().map_err(SessionError::Corruption)?;
        #[cfg(test)]
        let checkpoint = if self.fail_next_plan_checkpoint.swap(false, Ordering::AcqRel) {
            Err(SessionError::Io(
                "intentional plan-mode checkpoint failure".into(),
            ))
        } else {
            checkpoint_active(active, &self.checkpoints)
        };
        #[cfg(not(test))]
        let checkpoint = checkpoint_active(active, &self.checkpoints);
        if let Err(error) = checkpoint {
            eprintln!(
                "clat: warning: plan-mode checkpoint refresh failed after durable commit: {error}"
            );
        }
        Ok(Some(seq))
    }

    /// Current whole-value goal projection. `None` means no active session or
    /// no current goal (before create / after clear). The projection is the
    /// only durable reader; GoalService keeps only process-local activation.
    pub(crate) fn goal_state_json(&self) -> Result<Option<Value>, SessionError> {
        let guard = self.active.lock().expect("active");
        let Some(active) = guard.as_ref() else {
            return Ok(None);
        };
        let projections = active.projections.lock().expect("projections");
        Ok(projections
            .state_snapshot("goal")
            .filter(|value| !value.is_null()))
    }

    /// Commit one already-CAS-validated whole-value `goal/change` fact.
    /// GoalService serializes every writer through its write lane; this method
    /// owns append -> flush -> projection checkpoint and never publishes a
    /// speculative in-memory state ahead of the log.
    pub(crate) fn record_goal_change(&self, data: Value) -> Result<u64, SessionError> {
        crate::goal::validate_change_payload(&data).map_err(SessionError::Corruption)?;
        let guard = self.active.lock().expect("active");
        let active = guard.as_ref().ok_or_else(|| {
            SessionError::Corruption("goal mutation requires an active session".into())
        })?;
        let mut journal_slot = active.journal.lock().expect("session journal");
        if journal_slot.is_none() {
            *journal_slot = Some(journal_with_projection_fold(active, &self.backend));
        }
        let journal = Arc::clone(journal_slot.as_ref().expect("just initialized"));
        drop(journal_slot);
        let seq = journal
            .append(crate::session::run_journal::NewSessionEvent::new(
                "goal/change",
                data,
            ))
            .map_err(SessionError::Corruption)?;
        journal.flush().map_err(SessionError::Corruption)?;
        // Checkpoints are disposable caches. A failure does not roll back a
        // durable goal mutation, mirroring record_plan_mode.
        let _ = checkpoint_active(active, &self.checkpoints);
        Ok(seq)
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
    /// INV-MM1-5（legacy reader 围栏）：journal 投影出的模型项里，
    /// Image part 的路径必须落在本会话 attachments root 内且路径上
    /// 无 symlink——越界/篡改路径原位替换为稳定占位（诊断不回显路径），
    /// journal 不动、不崩溃。这是模型内容的唯一入口栅栏（run 历史
    /// 与 `/context` 同走 `surface_nodes`）。
    pub(crate) fn surface_nodes(&self) -> Result<Vec<(u64, ModelItem)>, SessionError> {
        let (nodes, attachments_root) = {
            let guard = self.active.lock().expect("active");
            let Some(active) = guard.as_ref() else {
                return Ok(Vec::new());
            };
            let projections = active.projections.lock().expect("projections");
            let nodes = projections
                .surface_nodes()
                .map_err(SessionError::Corruption)?;
            let root = crate::session::path_layout::session_dir(
                self.backend.root_path(),
                active.key.project.header_cwd.as_deref(),
                &active.key.id,
            )
            .join("attachments");
            (nodes, root)
        };
        let mut nodes = nodes;
        for (_, item) in &mut nodes {
            fence_attachment_parts(item, &attachments_root);
        }
        Ok(nodes)
    }

    /// MM-1A：committed 回执门面（application/run_lifecycle 消费——
    /// serve 幂等重试与 completion outcome 附带）。
    pub(crate) fn committed_receipt(
        &self,
        client_message_id: &str,
    ) -> Option<crate::message::AdmissionReceipt> {
        self.committed_admission(client_message_id)
            .map(|admission| admission.receipt)
    }

    /// MM-1A：按客户端幂等键查询 committed 回执 + 落盘 digest（M-02
    /// 的生产判别路径——serve 幂等重试经 `Application::committed_admission`
    /// 消费，不得在 serve 复刻投影逻辑）。journal 投影是权威（INV-M1A-4
    /// 的重启重建路径），进程内状态不参与。无活动会话或键不在回执
    /// 窗口内返回 None。
    pub(crate) fn committed_admission(
        &self,
        client_message_id: &str,
    ) -> Option<crate::message::CommittedAdmission> {
        let guard = self.active.lock().expect("active");
        let active = guard.as_ref()?;
        let projections = active.projections.lock().expect("projections");
        committed_admission_from_projections(&projections, client_message_id)
    }

    /// Resolve a durable client admission across every materialized session
    /// in one project without changing the active selection. Frontend
    /// recovery uses this when its own mapping write can lag the session
    /// journal: zero matches means no admission committed, one identifies
    /// its owner, and multiple matches are corruption.
    pub(crate) fn find_committed_admission_session(
        &self,
        project: &ProjectKey,
        client_message_id: &str,
    ) -> Result<Option<(SessionId, crate::message::CommittedAdmission)>, SessionError> {
        #[cfg(test)]
        if self
            .fail_next_admission_owner_scan
            .swap(false, Ordering::AcqRel)
        {
            return Err(SessionError::Io(
                "intentional admission-owner scan failure".into(),
            ));
        }
        let mut found = None;
        for summary in self.list_sessions(project)? {
            let key = SessionKey {
                project: project.clone(),
                id: summary.id,
            };
            let mut session_admission = None;
            self.backend.visit_from(&key, 0, &mut |event| {
                let Some(admission) = committed_admission_from_event(event, client_message_id)
                else {
                    return Ok(());
                };
                if session_admission.replace(admission).is_some() {
                    return Err("client delivery is committed more than once in one session".into());
                }
                Ok(())
            })?;
            let Some(admission) = session_admission else {
                continue;
            };
            if found.is_some() {
                return Err(SessionError::Corruption(
                    "client delivery is committed in multiple project sessions".into(),
                ));
            }
            found = Some((key.id, admission));
        }
        Ok(found)
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
            #[cfg(test)]
            if self.fail_next_quiesce.swap(false, Ordering::AcqRel) {
                errors.push("injected session quiesce failure".into());
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
        // INV-MM1-5 记档（2026-08-27 审计 M1-E）：这里直读投影的
        // surface_nodes，**未过附件栅栏**（fence_attachment_parts）。
        // 当前无生产消费者；任何消费者接入前必须改走带栅栏的
        // SessionService::surface_nodes（模型内容的唯一入口栅栏，
        // run 历史与 /context 同源）——届时本注释必须随之删除。
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

/// INV-MM1-5 + INV-MM2-6：模型项里的 Image part 围栏与 ref 解析点。
/// 新图块来自 adapter 的**相对 ref** `blobs/<attachmentId>`（journal
/// 不持久化绝对路径）——这里先验证 ref 形状（`blobs/` 下恰好一个
/// 十六进制组件，无穿越面），重写为会话 attachments root 内的绝对
/// 路径，再走既有围栏（词法位于 root 内 + 祖先组件无 symlink）。
/// legacy 绝对路径（MM-1 桥接期/平铺时代）直接走围栏。越界/篡改 →
/// 原位替换为稳定占位（不回显路径）。User/Assistant 之外的 item
/// 不携带内容，跳过。
fn fence_attachment_parts(item: &mut ModelItem, attachments_root: &std::path::Path) {
    let parts = match item {
        ModelItem::User { content } | ModelItem::Assistant { content, .. } => content,
        ModelItem::ToolResult(result) => &mut result.image_parts,
        _ => return,
    };
    for part in parts.iter_mut() {
        if let crate::model::ContentPart::Image { path, .. } = part {
            if let Some(resolved) = resolve_blob_reference(path, attachments_root) {
                *path = resolved;
            }
            if !path_is_within_attachment_root(path, attachments_root) {
                *part = crate::model::ContentPart::Text(
                    "[image unavailable: the referenced attachment is outside this session's \
                     attachment store]"
                        .into(),
                );
            }
        }
    }
}

fn journal_image(
    stored: crate::session::attachments::StoredAttachment,
) -> crate::message::JournalImage {
    crate::message::JournalImage {
        descriptor: crate::message::AttachmentDescriptor {
            attachment_id: stored.id,
            media_type: stored.media_type.to_owned(),
            width: stored.width,
            height: stored.height,
            bytes: stored.bytes,
            display_name: stored.display_name,
            original_width: Some(stored.original_width),
            original_height: Some(stored.original_height),
        },
        path: stored.blob_path,
    }
}

fn attachment_path_matches(path: &str, attachment_id: &str) -> bool {
    let path = std::path::Path::new(path);
    path.file_name().and_then(|name| name.to_str()) == Some(attachment_id)
        || crate::message::legacy_attachment_id(path.to_string_lossy().as_ref()) == attachment_id
}

fn journal_image_from_path(
    attachment_id: &str,
    path: &str,
    media_type: &str,
    display_name: Option<String>,
) -> Result<crate::message::JournalImage, SessionError> {
    let bytes = read_attachment_bytes(path)?;
    let decoded = image::load_from_memory(&bytes)
        .map_err(|_| SessionError::NotFound("attachment dimensions are unavailable".into()))?;
    let (width, height) = (u64::from(decoded.width()), u64::from(decoded.height()));
    Ok(crate::message::JournalImage {
        descriptor: crate::message::AttachmentDescriptor {
            attachment_id: attachment_id.to_owned(),
            media_type: media_type.to_owned(),
            width,
            height,
            bytes: bytes.len() as u64,
            display_name,
            original_width: None,
            original_height: None,
        },
        path: path.to_owned(),
    })
}

fn open_attachment_file(path: &str) -> Result<(Vec<u8>, u64), SessionError> {
    let path = std::path::Path::new(path);
    let (mut file, metadata) =
        crate::session::attachments::open_private_regular_file_no_follow(path)
            .map_err(|error| SessionError::Io(format!("open attachment no-follow: {error}")))?;
    let bytes = metadata.len();
    if bytes > crate::media::MAX_ATTACHMENT_BYTES {
        return Err(SessionError::NotFound(
            "attachment exceeds the image byte limit".into(),
        ));
    }
    let snapshot =
        crate::session::attachments::read_open_file_verified_snapshot(&mut file, path, bytes)
            .map_err(|error| {
                SessionError::NotFound(format!("attachment integrity verification failed: {error}"))
            })?;
    Ok((snapshot, bytes))
}

/// Legacy attachment metadata reconstruction still needs bytes for image
/// decoding; it reuses the same no-follow/length fence as the streaming web
/// reader rather than re-opening an unconstrained path.
fn read_attachment_bytes(path: &str) -> Result<Vec<u8>, SessionError> {
    open_attachment_file(path).map(|(snapshot, _)| snapshot)
}

/// 相对 ref `blobs/<hex-id>` → root 内绝对路径；非该形状（legacy
/// 绝对路径/已被解析过）返回 None 交由围栏按原语义处理。id 限定
/// 十六进制字符——词法上无 `..`/分隔符/空件的穿越面。
fn resolve_blob_reference(path: &str, root: &std::path::Path) -> Option<String> {
    let rest = path.strip_prefix("blobs/")?;
    if rest.is_empty()
        || !rest.bytes().all(|byte| byte.is_ascii_hexdigit())
        || std::path::Path::new(rest).components().count() != 1
    {
        return None;
    }
    Some(root.join("blobs").join(rest).to_string_lossy().into_owned())
}

/// Durable image-bearing event → referenced attachment ids for the orphan
/// mark phase. User and assistant messages carry blocks directly; tool
/// results nest their typed blocks inside the DSH `tool-result` message part.
/// All three are durable attachment authority and must survive cold-open GC.
fn collect_event_attachment_ids(
    event: &SessionEvent,
    referenced: &mut std::collections::HashSet<String>,
) {
    match event.event_type.as_str() {
        "user/message" => collect_content_attachment_ids(event.data.get("content"), referenced),
        "assistant/message" => {
            collect_content_attachment_ids(event.data.pointer("/message/content"), referenced)
        }
        "tool/result" => {
            let Some(parts) = event
                .data
                .pointer("/message/content")
                .and_then(Value::as_array)
            else {
                return;
            };
            for part in parts
                .iter()
                .filter(|part| part.get("type").and_then(Value::as_str) == Some("tool-result"))
            {
                collect_content_attachment_ids(part.get("content"), referenced);
            }
        }
        _ => {}
    }
}

fn collect_content_attachment_ids(
    content: Option<&Value>,
    referenced: &mut std::collections::HashSet<String>,
) {
    let Some(blocks) = content.and_then(Value::as_array) else {
        return;
    };
    for block in blocks
        .iter()
        .filter(|block| block.get("type").and_then(Value::as_str) == Some("image"))
    {
        if let Some(id) = block.get("attachmentId").and_then(Value::as_str) {
            referenced.insert(id.to_owned());
        }
    }
}

/// 词法归一化（组件级处理 `.`/`..`）后判断是否位于 root 前缀内；
/// 再对已存在的祖先组件做 symlink 检查（缺失组件留给读取端按既有
/// 降级语义处理——栅栏管授权形状，不管存在性）。
fn path_is_within_attachment_root(path: &str, root: &std::path::Path) -> bool {
    let path = std::path::Path::new(path);
    let Some(normalized) = lexically_within(path, root) else {
        return false;
    };
    // root 之下逐级检查（root 自身由 store 建权，不必复查）。
    let root_len = root.components().count();
    let mut ancestor = root.to_path_buf();
    for component in normalized.components().skip(root_len) {
        ancestor.push(component.as_os_str());
        if let Ok(metadata) = std::fs::symlink_metadata(&ancestor)
            && metadata.file_type().is_symlink()
        {
            return false;
        }
    }
    true
}

/// 词法归一化：把 `path` 的组件逐个压栈（`.` 跳过、`..` 弹栈，下溢
/// 即失败）。返回的路径以 `root` 的组件为前缀（否则 None）。
fn lexically_within(path: &std::path::Path, root: &std::path::Path) -> Option<std::path::PathBuf> {
    let root_components: Vec<_> = root.components().collect();
    let mut stack: Vec<std::path::Component> = Vec::new();
    for component in path.components() {
        match component {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                stack.pop()?;
            }
            other => stack.push(other),
        }
    }
    if stack.len() < root_components.len() {
        return None;
    }
    for (have, want) in stack.iter().zip(root_components.iter()) {
        if have != want {
            return None;
        }
    }
    let mut normalized = std::path::PathBuf::new();
    for component in &stack {
        normalized.push(component.as_os_str());
    }
    Some(normalized)
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

    fn service(tag: &str) -> (SessionService, std::path::PathBuf) {
        let root = std::env::temp_dir().join(format!(
            "clat-usecases-{tag}-{}",
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

    /// MM-I9 / attachment reachability: durable tool results may carry more
    /// than one image. Descriptor authority and provider-facing paths are two
    /// parallel projections of the same ordered content, so resolving the
    /// second opaque id must return the second path, never the first image in
    /// the result. The pre-fix implementation searched `image_parts` from the
    /// beginning for every descriptor and therefore aliased every id to image
    /// zero.
    #[test]
    fn tool_result_attachment_resolution_preserves_multi_image_pairing() {
        let (service, root) = service("tool-result-image-pairing");
        service.new_session(&project()).expect("session");
        let make_source = |name: &str, color: [u8; 3]| {
            let path = root.join(format!("{name}.png"));
            let image = image::RgbImage::from_pixel(8, 8, image::Rgb(color));
            image::DynamicImage::ImageRgb8(image)
                .save_with_format(&path, image::ImageFormat::Png)
                .expect("write image fixture");
            path
        };
        let images = service
            .import_attachments(&[
                make_source("first", [10, 20, 30]),
                make_source("second", [40, 50, 60]),
            ])
            .expect("admit image pair");
        let blocks = images
            .iter()
            .map(|image| crate::message::ContentBlock::Image {
                attachment: image.descriptor.clone(),
            })
            .collect::<Vec<_>>();
        let journal = service.journal().expect("journal");
        journal
            .append(
                crate::session::run_journal::NewSessionEvent::new(
                    "tool/result",
                    payloads::tool_result(
                        1,
                        1,
                        "multi-image-call",
                        payloads::tool_result_content_with_blocks(
                            &serde_json::json!("two images"),
                            &blocks,
                        ),
                        false,
                    ),
                )
                .append(Vec::new()),
            )
            .expect("append tool result");
        journal.flush().expect("flush tool result");

        let second = service
            .resolve_active_attachment(&images[1].descriptor.attachment_id)
            .expect("resolve second image");
        assert_eq!(second.descriptor, images[1].descriptor);
        assert_eq!(
            second.path, images[1].path,
            "the second opaque id must not alias the first tool image path"
        );
        assert_ne!(second.path, images[0].path);
        std::fs::remove_dir_all(root).ok();
    }

    /// INV-MM1-3/5: no-follow prevents a final symlink, but a second hardlink
    /// name can still mutate the same inode after publication. Provider/PWA
    /// reads must therefore reject multiply-linked attachment files instead
    /// of treating their regular-file type as sufficient authority.
    #[cfg(unix)]
    #[test]
    fn attachment_reader_rejects_a_multiply_linked_blob() {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let root = std::env::temp_dir().join(format!("clat-attachment-read-link-{unique}"));
        std::fs::create_dir_all(&root).expect("root");
        let blob = root.join("blob");
        let alias = root.join("alias");
        std::fs::write(&blob, b"normalized image bytes").expect("blob");
        std::fs::hard_link(&blob, &alias).expect("hardlink alias");

        assert!(
            open_attachment_file(blob.to_str().expect("utf8 path")).is_err(),
            "a multiply-linked attachment must fail before any bytes are exposed"
        );
        assert_eq!(
            std::fs::read(alias).expect("alias remains intact"),
            b"normalized image bytes"
        );
        std::fs::remove_dir_all(root).ok();
    }

    /// The PWA/session reader must enforce the same content-address integrity
    /// as provider projection. A writable same-user process can alter a 0600
    /// inode in place without adding a link or changing its length.
    #[test]
    fn attachment_reader_rejects_a_content_address_mismatch() {
        use sha2::Digest as _;

        let original = b"original-image";
        let tampered = b"tampered-image";
        assert_eq!(original.len(), tampered.len(), "same-length attack fixture");
        let name = sha2::Sha256::digest(original)
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        let root = std::env::temp_dir().join(format!(
            "clat-attachment-read-digest-{}",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&root).expect("root");
        let blob = root.join(name);
        std::fs::write(&blob, tampered).expect("write corrupted blob");

        assert!(
            open_attachment_file(blob.to_str().expect("utf8 path")).is_err(),
            "content-address mismatch must fail before any bytes are exposed"
        );

        std::fs::remove_dir_all(root).ok();
    }

    /// PWA headers are derived from durable descriptor metadata. Before a
    /// blob reader is exposed, that MIME claim must match the normalized blob
    /// magic; a content-address match alone cannot authorize relabeling PNG
    /// bytes as JPEG.
    #[test]
    fn active_attachment_reader_rejects_a_media_type_magic_mismatch() {
        let (service, root) = service("attachment-media-mismatch");
        service.new_session(&project()).expect("session");
        let source = root.join("source.png");
        image::DynamicImage::ImageRgb8(image::RgbImage::from_pixel(8, 8, image::Rgb([7, 8, 9])))
            .save_with_format(&source, image::ImageFormat::Png)
            .expect("write PNG fixture");
        let mut image = service
            .import_attachments(&[source])
            .expect("admit image")
            .pop()
            .expect("stored image");
        image.descriptor.media_type = "image/jpeg".into();
        let attachment_id = image.descriptor.attachment_id.clone();
        let journal = service.journal().expect("journal");
        journal
            .append(
                crate::session::run_journal::NewSessionEvent::new(
                    "user/message",
                    payloads::admitted_user_message("m-mime", "", &[image], None, None),
                )
                .append(Vec::new()),
            )
            .expect("append forged durable descriptor");
        journal.flush().expect("flush descriptor");

        let error = match service.open_active_attachment(&attachment_id) {
            Ok(_) => panic!("reader must reject a durable MIME/blob mismatch"),
            Err(error) => error,
        };
        assert!(
            error.to_string().contains("media type"),
            "failure identifies the descriptor/blob mismatch: {error}"
        );

        std::fs::remove_dir_all(root).ok();
    }

    /// Durable tool-result descriptors are paired with provider-facing paths
    /// during replay, but their byte count is still untrusted journal
    /// metadata. The PWA reader must compare it with the already-open blob so
    /// a forged descriptor cannot make UI/policy consumers under-report the
    /// normalized image while serving different bytes.
    #[test]
    fn active_attachment_reader_rejects_a_descriptor_byte_count_mismatch() {
        let (service, root) = service("attachment-byte-count-mismatch");
        service.new_session(&project()).expect("session");
        let source = root.join("source.png");
        image::DynamicImage::ImageRgb8(image::RgbImage::from_pixel(8, 8, image::Rgb([7, 8, 9])))
            .save_with_format(&source, image::ImageFormat::Png)
            .expect("write PNG fixture");
        let image = service
            .import_attachments(&[source])
            .expect("admit image")
            .pop()
            .expect("stored image");
        let attachment_id = image.descriptor.attachment_id.clone();
        let mut forged = image.descriptor.clone();
        forged.bytes = forged.bytes.saturating_sub(1);
        let journal = service.journal().expect("journal");
        journal
            .append(
                crate::session::run_journal::NewSessionEvent::new(
                    "tool/result",
                    payloads::tool_result(
                        1,
                        1,
                        "forged-image-metadata",
                        payloads::tool_result_content_with_blocks(
                            &serde_json::json!("image"),
                            &[crate::message::ContentBlock::Image { attachment: forged }],
                        ),
                        false,
                    ),
                )
                .append(Vec::new()),
            )
            .expect("append forged durable descriptor");
        journal.flush().expect("flush descriptor");

        let error = match service.open_active_attachment(&attachment_id) {
            Ok(_) => panic!("reader must reject a durable descriptor/blob byte mismatch"),
            Err(error) => error,
        };
        assert!(
            error.to_string().contains("byte count"),
            "failure identifies the descriptor/blob mismatch: {error}"
        );

        std::fs::remove_dir_all(root).ok();
    }

    /// Width and height are also durable facts for new content-addressed
    /// attachments. A tool-result producer drift must not let the transcript
    /// advertise dimensions that do not describe the bytes served by PWA.
    #[test]
    fn active_attachment_reader_rejects_a_descriptor_dimension_mismatch() {
        let (service, root) = service("attachment-dimension-mismatch");
        service.new_session(&project()).expect("session");
        let source = root.join("source.png");
        image::DynamicImage::ImageRgb8(image::RgbImage::from_pixel(8, 8, image::Rgb([7, 8, 9])))
            .save_with_format(&source, image::ImageFormat::Png)
            .expect("write PNG fixture");
        let image = service
            .import_attachments(&[source])
            .expect("admit image")
            .pop()
            .expect("stored image");
        let attachment_id = image.descriptor.attachment_id.clone();
        let mut forged = image.descriptor.clone();
        forged.width = forged.width.saturating_add(1);
        let journal = service.journal().expect("journal");
        journal
            .append(
                crate::session::run_journal::NewSessionEvent::new(
                    "tool/result",
                    payloads::tool_result(
                        1,
                        1,
                        "forged-image-dimensions",
                        payloads::tool_result_content_with_blocks(
                            &serde_json::json!("image"),
                            &[crate::message::ContentBlock::Image { attachment: forged }],
                        ),
                        false,
                    ),
                )
                .append(Vec::new()),
            )
            .expect("append forged durable descriptor");
        journal.flush().expect("flush descriptor");

        let error = match service.open_active_attachment(&attachment_id) {
            Ok(_) => panic!("reader must reject a durable descriptor/blob dimension mismatch"),
            Err(error) => error,
        };
        assert!(
            error.to_string().contains("dimensions"),
            "failure identifies the descriptor/blob mismatch: {error}"
        );

        std::fs::remove_dir_all(root).ok();
    }

    /// Digest verification and HTTP streaming cannot be two reads of a
    /// writable inode: a same-user process can change that inode after the
    /// first pass and make the browser receive bytes that were never
    /// verified. The application boundary therefore returns an immutable,
    /// bounded snapshot rather than the live store descriptor.
    #[test]
    fn active_attachment_reader_streams_the_verified_snapshot_after_blob_mutation() {
        use std::io::Read as _;

        let (service, root) = service("attachment-immutable-snapshot");
        service.new_session(&project()).expect("session");
        let source = root.join("source.png");
        image::DynamicImage::ImageRgb8(image::RgbImage::from_pixel(8, 8, image::Rgb([7, 8, 9])))
            .save_with_format(&source, image::ImageFormat::Png)
            .expect("write PNG fixture");
        let image = service
            .import_attachments(&[source])
            .expect("admit image")
            .pop()
            .expect("stored image");
        let attachment_id = image.descriptor.attachment_id.clone();
        let journal = service.journal().expect("journal");
        journal
            .append(
                crate::session::run_journal::NewSessionEvent::new(
                    "user/message",
                    payloads::admitted_user_message(
                        "m-snapshot",
                        "",
                        std::slice::from_ref(&image),
                        None,
                        None,
                    ),
                )
                .append(Vec::new()),
            )
            .expect("append durable descriptor");
        journal.flush().expect("flush descriptor");
        let original = std::fs::read(&image.path).expect("read admitted blob");

        let mut reader = service
            .open_active_attachment(&attachment_id)
            .expect("open verified attachment snapshot");
        let mut mutated = original.clone();
        let last = mutated.last_mut().expect("non-empty PNG");
        *last ^= 0xff;
        std::fs::write(&image.path, &mutated).expect("mutate store inode after verification");

        let mut exposed = Vec::new();
        reader
            .file
            .read_to_end(&mut exposed)
            .expect("consume application reader");
        assert_eq!(
            exposed, original,
            "browser bytes must be the exact snapshot that passed digest verification"
        );

        std::fs::remove_dir_all(root).ok();
    }

    /// INV-MM1-4 + MM-I9: `view_image` stores its descriptor inside the
    /// nested DSH tool-result content, not a top-level user message. The cold
    /// orphan mark must protect that blob while still reclaiming a genuinely
    /// unreferenced peer. Removing the `tool/result` collector makes the
    /// referenced blob disappear in this test.
    #[test]
    fn orphan_mark_preserves_tool_result_image_attachments() {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let root = std::env::temp_dir().join(format!("clat-tool-image-gc-{unique}"));
        let store = crate::session::attachments::AttachmentStore::open(root.clone())
            .expect("open attachment store");
        let make_source = |name: &str, color: [u8; 3]| {
            let path = root.join(format!("{name}.png"));
            let image = image::RgbImage::from_pixel(8, 8, image::Rgb(color));
            image::DynamicImage::ImageRgb8(image)
                .save_with_format(&path, image::ImageFormat::Png)
                .expect("write image fixture");
            path
        };
        let referenced_source = make_source("referenced", [10, 20, 30]);
        let orphan_source = make_source("orphan", [40, 50, 60]);
        let stored = store
            .admit(&[referenced_source, orphan_source])
            .expect("admit image pair");
        let referenced_image = crate::message::AttachmentDescriptor {
            attachment_id: stored[0].id.clone(),
            media_type: stored[0].media_type.to_owned(),
            width: stored[0].width,
            height: stored[0].height,
            bytes: stored[0].bytes,
            display_name: stored[0].display_name.clone(),
            original_width: Some(stored[0].original_width),
            original_height: Some(stored[0].original_height),
        };
        let event = SessionEvent::new(
            "tool/result",
            0,
            0,
            payloads::tool_result(
                1,
                1,
                "view-image-call",
                payloads::tool_result_content_with_blocks(
                    &serde_json::json!("ok"),
                    &[crate::message::ContentBlock::Image {
                        attachment: referenced_image,
                    }],
                ),
                false,
            ),
        );
        let mut referenced = std::collections::HashSet::new();
        collect_event_attachment_ids(&event, &mut referenced);
        assert_eq!(
            referenced,
            std::collections::HashSet::from([stored[0].id.clone()])
        );

        let future = std::time::SystemTime::now() + std::time::Duration::from_secs(48 * 60 * 60);
        let sweep = store.sweep_orphans(&referenced, future);
        assert_eq!(sweep.removed_blobs, 1, "the unrelated orphan is reclaimed");
        assert!(
            std::path::Path::new(&stored[0].blob_path).is_file(),
            "the durable tool-result image remains readable"
        );
        assert!(
            !std::path::Path::new(&stored[1].blob_path).exists(),
            "the unreferenced control blob proves the sweep actually ran"
        );
        std::fs::remove_dir_all(root).ok();
    }

    /// INV-MM2-6（MM-2 W6 红测）：相对 ref `blobs/<hex>` 在栅栏处
    /// 解析为会话 root 内绝对路径；畸形 ref（空 id/非十六进制/多
    /// 组件）不解析且按围栏语义占位。删 resolve_blob_reference 即红
    ///（合法 ref 无法解析 → 围栏拒绝 → 占位）。
    #[test]
    fn fence_resolves_blob_references_within_the_root() {
        let root = std::path::PathBuf::from("/store/sessions/p/s/attachments");
        let mut item = crate::model::ModelItem::User {
            content: vec![
                crate::model::ContentPart::Text("look".into()),
                crate::model::ContentPart::Image {
                    path: "blobs/0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"
                        .into(),
                    media_type: "image/png".into(),
                },
            ],
        };
        fence_attachment_parts(&mut item, &root);
        let crate::model::ModelItem::User { content } = &item else {
            panic!("user item");
        };
        let crate::model::ContentPart::Image { path, .. } = &content[1] else {
            panic!("the valid ref must resolve, not placeholder");
        };
        assert_eq!(
            path,
            &root
                .join("blobs")
                .join("0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef")
                .to_string_lossy()
                .into_owned()
        );

        // 畸形 ref：非十六进制 id / 多组件 / 绝对越界路径 → 占位。
        for malformed in ["blobs/not-hex!", "blobs/aaaa/bbbb", "blobs/", "/etc/passwd"] {
            let mut item = crate::model::ModelItem::User {
                content: vec![crate::model::ContentPart::Image {
                    path: malformed.into(),
                    media_type: "image/png".into(),
                }],
            };
            fence_attachment_parts(&mut item, &root);
            let crate::model::ModelItem::User { content } = &item else {
                panic!("user item");
            };
            assert!(
                matches!(content[0], crate::model::ContentPart::Text(ref note) if note.contains("image unavailable")),
                "malformed reference {malformed} degrades to a stable placeholder"
            );
        }
    }

    /// MM-1 围栏五腿判别（git checkout 事故丢失，2026-08-27 研究员
    /// 按 MM-1 审查记录还原；规格见 mm2 实施计划 §事故记档）：
    /// ①root 内 legacy 平铺文件保留 ②blobs/ 内合法引用保留
    /// ③`..` 词法逃逸占位 ④root 内 symlink 绝不跟随 ⑤界外绝对
    /// 路径占位。删 path_is_within_attachment_root 任一闸即红。
    #[test]
    fn fence_rejects_paths_outside_the_session_store() {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let root = std::env::temp_dir().join(format!("clat-fence-{unique}"));
        std::fs::create_dir_all(root.join("blobs")).expect("blobs dir");

        std::fs::write(root.join("flat.png"), b"legacy").expect("flat file");
        std::fs::write(root.join("blobs").join("deadbeef"), b"blob").expect("blob file");

        let fenced = |path: String| -> crate::model::ContentPart {
            let mut item = crate::model::ModelItem::User {
                content: vec![crate::model::ContentPart::Image {
                    path,
                    media_type: "image/png".into(),
                }],
            };
            fence_attachment_parts(&mut item, &root);
            let crate::model::ModelItem::User { content } = &item else {
                panic!("user item");
            };
            content.first().expect("one part").clone()
        };

        // 腿 ①②：root 内平铺（legacy）与 blobs/ 内引用照常保留。
        for keep in [
            root.join("flat.png").to_string_lossy().into_owned(),
            root.join("blobs")
                .join("deadbeef")
                .to_string_lossy()
                .into_owned(),
        ] {
            match fenced(keep.clone()) {
                crate::model::ContentPart::Image { path, .. } => {
                    assert_eq!(path, keep, "in-root references must stay readable");
                }
                other => panic!("in-root reference must stay an image part, got {other:?}"),
            }
        }

        // 腿 ③：`..` 组件词法逃逸（下溢 / 弹出 root）→ 稳定占位。
        for escape in [
            root.join("..")
                .join("secret.png")
                .to_string_lossy()
                .into_owned(),
            root.join("blobs")
                .join("..")
                .join("..")
                .join("x.png")
                .to_string_lossy()
                .into_owned(),
        ] {
            assert!(
                matches!(fenced(escape), crate::model::ContentPart::Text(ref note) if note.contains("image unavailable")),
                "a `..` escape must degrade to a stable placeholder"
            );
        }

        // 腿 ④：root 内 symlink 指向界外 → 拒绝（词法在界内也不跟随）。
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(std::env::temp_dir(), root.join("link")).expect("symlink");
            let linked = root
                .join("link")
                .join("outside.png")
                .to_string_lossy()
                .into_owned();
            assert!(
                matches!(fenced(linked), crate::model::ContentPart::Text(ref note) if note.contains("image unavailable")),
                "a symlink inside the root must never be followed"
            );
            let _ = std::fs::remove_file(root.join("link"));
        }

        // 腿 ⑤：界外绝对路径 → 占位（词法栅栏，无需目标存在）。
        assert!(
            matches!(fenced("/etc/passwd".into()), crate::model::ContentPart::Text(ref note) if note.contains("image unavailable")),
            "an absolute path outside the store must degrade to a stable placeholder"
        );

        let _ = std::fs::remove_dir_all(&root);
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

    #[test]
    fn global_admission_owner_scan_outlives_receipt_window_and_rejects_duplicates() {
        let (service, root) = service("global-admission-owner");
        let project = project();
        let first = service.new_session(&project).expect("first session");
        let journal = service.journal().expect("journal");
        let digest = "a".repeat(64);
        let mut events = vec![
            crate::session::run_journal::NewSessionEvent::new(
                "user/message",
                payloads::admitted_user_message(
                    "message-target",
                    "target",
                    &[],
                    Some("delivery-target"),
                    Some(&digest),
                ),
            )
            .append(Vec::new()),
        ];
        for index in 0..1024 {
            events.push(
                crate::session::run_journal::NewSessionEvent::new(
                    "user/message",
                    payloads::admitted_user_message(
                        &format!("message-{index}"),
                        "filler",
                        &[],
                        Some(&format!("delivery-{index}")),
                        Some(&digest),
                    ),
                )
                .append(Vec::new()),
            );
        }
        journal
            .append_atomic(&events)
            .expect("append admission window");
        journal.flush().expect("commit admission window");
        assert!(
            service.committed_admission("delivery-target").is_none(),
            "the ordinary retry projection is intentionally bounded"
        );
        let owner = service
            .find_committed_admission_session(&project, "delivery-target")
            .expect("scan journals")
            .expect("historical owner");
        assert_eq!(owner.0, first.id);
        assert_eq!(owner.1.request_digest.as_deref(), Some(digest.as_str()));

        service.quiesce_active().expect("quiesce first");
        service.new_session(&project).expect("second session");
        let journal = service.journal().expect("second journal");
        journal
            .append(
                crate::session::run_journal::NewSessionEvent::new(
                    "user/message",
                    payloads::admitted_user_message(
                        "duplicate-message",
                        "duplicate",
                        &[],
                        Some("delivery-target"),
                        Some(&digest),
                    ),
                )
                .append(Vec::new()),
            )
            .expect("append duplicate owner");
        journal.flush().expect("commit duplicate owner");
        let error = service
            .find_committed_admission_session(&project, "delivery-target")
            .expect_err("multiple durable owners must fail closed");
        assert!(error.to_string().contains("multiple project sessions"));

        service.quiesce_active().expect("cleanup");
        crate::test_support::cleanup_tree(&root);
    }

    struct FailingPlanJournal {
        append_error: bool,
        flush_error: bool,
    }

    impl crate::session::run_journal::RunJournal for FailingPlanJournal {
        fn append_atomic(
            &self,
            _events: &[crate::session::run_journal::NewSessionEvent],
        ) -> Result<crate::session::run_journal::SeqRange, String> {
            if self.append_error {
                Err("intentional plan append failure".into())
            } else {
                Ok(crate::session::run_journal::SeqRange {
                    start: 99,
                    end_inclusive: 99,
                })
            }
        }

        fn flush(&self) -> Result<(), String> {
            if self.flush_error {
                Err("intentional plan flush failure".into())
            } else {
                Ok(())
            }
        }
    }

    #[test]
    fn plan_mode_append_and_flush_failures_never_publish_approval() {
        let (service, root) = service("plan-commit-failures");
        service.new_session(&project()).expect("session");
        service
            .record_plan_mode(true, None)
            .expect("enter plan mode");
        assert!(service.plan_mode_state().active);

        let replace_journal = |journal: Arc<dyn crate::session::run_journal::RunJournal>| {
            let guard = service.active.lock().expect("active");
            let active = guard.as_ref().expect("active session");
            *active.journal.lock().expect("journal") = Some(journal);
        };

        replace_journal(Arc::new(FailingPlanJournal {
            append_error: true,
            flush_error: false,
        }));
        let approved = ApprovedPlanWrite {
            text: "approved only after durable commit".into(),
            digest: crate::plan_mode::plan_digest("approved only after durable commit"),
        };
        assert!(
            service
                .record_plan_mode(false, Some(approved.clone()))
                .is_err()
        );
        assert!(service.plan_mode_state().active);
        assert!(service.plan_mode_state().approved.is_none());

        replace_journal(Arc::new(FailingPlanJournal {
            append_error: false,
            flush_error: true,
        }));
        assert!(service.record_plan_mode(false, Some(approved)).is_err());
        assert!(service.plan_mode_state().active);
        assert!(service.plan_mode_state().approved.is_none());

        // Restore the real folding journal so normal teardown can flush/close.
        {
            let guard = service.active.lock().expect("active");
            let active = guard.as_ref().expect("active session");
            *active.journal.lock().expect("journal") =
                Some(journal_with_projection_fold(active, &service.backend));
        }
        service.quiesce_active().expect("quiesce");
        crate::test_support::cleanup_tree(&root);
    }

    #[test]
    fn plan_mode_checkpoint_failure_after_commit_is_rebuildable_degradation() {
        let (service, root) = service("plan-checkpoint-degradation");
        let project = project();
        let summary = service.new_session(&project).expect("session");
        let key = SessionKey {
            project: project.clone(),
            id: summary.id,
        };
        service
            .record_plan_mode(true, None)
            .expect("enter plan mode");
        service
            .fail_next_plan_checkpoint
            .store(true, Ordering::Release);
        let text = "checkpoint failure must not undo a durable approval";
        let seq = service
            .record_plan_mode(
                false,
                Some(ApprovedPlanWrite {
                    text: text.into(),
                    digest: crate::plan_mode::plan_digest(text),
                }),
            )
            .expect("checkpoint failure is non-fatal")
            .expect("durable approval seq");
        let state = service.plan_mode_state();
        assert!(!state.active);
        assert_eq!(
            state.approved.as_ref().map(|plan| plan.event_seq),
            Some(seq)
        );
        assert_eq!(
            state.approved.as_ref().map(|plan| plan.text.as_str()),
            Some(text)
        );
        service.quiesce_active().expect("quiesce");
        drop(service);

        let reopened = SessionService::new(root.clone(), JsonlCompression::Zstd).expect("reopen");
        reopened
            .resume(&key)
            .expect("resume from stale checkpoint + journal tail");
        let rebuilt = reopened.plan_mode_state();
        assert!(!rebuilt.active);
        assert_eq!(
            rebuilt.approved.as_ref().map(|plan| plan.event_seq),
            Some(seq)
        );
        assert_eq!(
            rebuilt.approved.as_ref().map(|plan| plan.text.as_str()),
            Some(text)
        );
        reopened.quiesce_active().expect("reopen quiesce");
        crate::test_support::cleanup_tree(&root);
    }

    #[test]
    fn permission_mode_checkpoint_failure_after_commit_rebuilds_from_journal() {
        let (service, root) = service("permission-checkpoint-degradation");
        let project = project();
        let summary = service.new_session(&project).expect("session");
        let key = SessionKey {
            project,
            id: summary.id,
        };
        service
            .record_permission_mode(PermissionMode::ProjectWrite)
            .expect("initial mode");
        service
            .fail_next_permission_checkpoint
            .store(true, Ordering::Release);
        service
            .record_permission_mode(PermissionMode::FullAccess)
            .expect("checkpoint failure is non-fatal after journal commit");
        assert_eq!(
            service.permission_mode_state(),
            Some(PermissionMode::FullAccess)
        );
        service.quiesce_active().expect("quiesce");
        drop(service);

        let reopened = SessionService::new(root.clone(), JsonlCompression::Zstd).expect("reopen");
        reopened.resume(&key).expect("rebuild from journal tail");
        assert_eq!(
            reopened.permission_mode_state(),
            Some(PermissionMode::FullAccess)
        );
        reopened.quiesce_active().expect("quiesce reopened");
        crate::test_support::cleanup_tree(&root);
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
        crate::test_support::cleanup_tree(&root);
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
        crate::test_support::cleanup_tree(&root);
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
        crate::test_support::cleanup_tree(&root);
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
        crate::test_support::cleanup_tree(&root);
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
        crate::test_support::cleanup_tree(&root);
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
        crate::test_support::cleanup_tree(&root);
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

        crate::test_support::cleanup_tree(&root);
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
        crate::test_support::cleanup_tree(&root);
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
        crate::test_support::cleanup_tree(&root);
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
        crate::test_support::cleanup_tree(&root);
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
        crate::test_support::cleanup_tree(&root);
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
        crate::test_support::cleanup_tree(&root);
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
        crate::test_support::cleanup_tree(&root);
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
        crate::test_support::cleanup_tree(&root);
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
        crate::test_support::cleanup_tree(&root);
    }

    /// INV-C1/C2：usage 折叠按 journal `source {provider, model}` 路由
    /// 分桶——Cache 口径归属当前模型路由（切换不混合不清零），session
    /// 口径仍是全会话累计（TUI-L04 不变），last_request 取最近一次。
    /// 修复前该测试无处安放：UsageStats 没有路由桶，跨模型的缓存命中
    /// 会混进同一个百分比（用户报告：GLM→DeepSeek 切换后 Cache 残留）。
    #[test]
    fn usage_fold_buckets_by_model_route() {
        let (service, root) = service("usage-routes");
        let summary = service.new_session(&project()).expect("session");
        let key = SessionKey {
            id: summary.id.clone(),
            project: project(),
        };
        let glm_usage = crate::model::Usage {
            input_tokens: 1000,
            cached_input_tokens: Some(800),
            ..crate::model::Usage::default()
        };
        let ds_usage = crate::model::Usage {
            input_tokens: 200,
            cached_input_tokens: Some(0),
            ..crate::model::Usage::default()
        };
        let journal = service.journal().expect("journal");
        journal
            .append_atomic(&[
                crate::session::run_journal::NewSessionEvent::new(
                    "assistant/message",
                    payloads::assistant_message(
                        1,
                        1,
                        vec![payloads::text_block("glm answer")],
                        "OpenAI Compatible",
                        "glm-5.3",
                        Some(&glm_usage),
                    ),
                )
                .append(Vec::new()),
                crate::session::run_journal::NewSessionEvent::new(
                    "assistant/message",
                    payloads::assistant_message(
                        2,
                        2,
                        vec![payloads::text_block("deepseek answer")],
                        "OpenAI Compatible",
                        "deepseek-v4-flash",
                        Some(&ds_usage),
                    ),
                )
                .append(Vec::new()),
            ])
            .expect("append");
        journal.flush().expect("flush");

        let (_replay, usage) = service.replay_with_usage(&key).expect("replay");
        let glm = usage
            .routes
            .get("OpenAI Compatible/glm-5.3")
            .expect("glm bucket survives the switch");
        assert_eq!(glm.input_tokens, 1000);
        assert_eq!(glm.cached_input_tokens, Some(800));
        let ds = usage
            .routes
            .get("OpenAI Compatible/deepseek-v4-flash")
            .expect("deepseek bucket");
        assert_eq!(ds.input_tokens, 200);
        assert_eq!(ds.cached_input_tokens, Some(0));
        // session 口径跨路由累计；最近一次是 deepseek。
        assert_eq!(usage.session.input_tokens, 1200);
        assert_eq!(usage.session.cached_input_tokens, Some(800));
        assert_eq!(
            usage
                .last_request
                .as_ref()
                .and_then(|u| u.cached_input_tokens),
            Some(0)
        );
        crate::test_support::cleanup_tree(&root);
    }
}
