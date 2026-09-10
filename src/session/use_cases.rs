//! `SessionUseCases` facade (plan §8.3): the Application-facing surface over
//! backend + projections + checkpoints + coordinator. Application never
//! touches raw persistence; frontends never see this file's internals.
//!
//! Stage-5 note: `normalize_selection` / `current_selection` live in the
//! control DB (`project_workspace_state`) and land with the cutover; this
//! facade owns everything session-log-shaped.

mod active;
mod attachments;
mod folding_journal;
mod history;
mod lifecycle;
use active::{ActiveSession, checkpoint_active, fold_if_behind};
use folding_journal::ProjectionFoldJournal;

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

/// A message-aligned, backwards page over the active session's structured
/// replay. `before_seq` is an exclusive journal cursor.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SessionHistoryPage {
    pub events: Vec<ReplayEvent>,
    pub has_more: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MessageOutlineItem {
    pub seq: u64,
    pub turn: u64,
    pub role: String,
    pub preview: String,
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
        *active_slot = Some(ActiveSession::new(
            key.clone(),
            coordinator,
            Arc::new(Mutex::new(ProjectionRegistry::clat())),
            Arc::new(Mutex::new(ResumeSink::new())),
        ));
        Ok(SessionSummary {
            id: key.id,
            title: None,
            created_at_ms: header.created_at,
            last_activity_ms: header.created_at,
            message_count: 0,
            turns: 0,
        })
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
        if session.coordinator.is_read_only() {
            return Err(SessionError::UnsupportedFormat(
                "legacy session is read-only; use /update to upgrade or /new".into(),
            ));
        }
        Ok(session.shared_journal(&self.backend))
    }

    pub(crate) fn is_read_only(&self) -> bool {
        self.active
            .lock()
            .expect("active")
            .as_ref()
            .is_some_and(|active| active.coordinator.is_read_only())
    }

    pub(crate) fn upgrade_active(&self) -> Result<SessionView, SessionError> {
        let key = {
            let guard = self.active.lock().expect("active");
            let active = guard
                .as_ref()
                .ok_or_else(|| SessionError::NotFound("no active session".into()))?;
            if !active.coordinator.is_read_only() {
                return Err(SessionError::UnsupportedFormat(
                    "/update is only available in legacy sessions".into(),
                ));
            }
            active.key.clone()
        };
        self.backend.upgrade_legacy(&key)?;
        let armed = self
            .stage_resume(&key)
            .and_then(|staged| self.arm_session(staged))
            .map_err(|error| {
                SessionError::Io(format!(
                    "v2 has been published; reopen the session or retry /update: {error}"
                ))
            })?;
        if let Err(error) = self.quiesce_active() {
            let cleanup = self.discard_armed(armed);
            return Err(SessionError::Io(format!(
                "v2 has been published; reopen the session after detach failure: {error}; target close: {cleanup:?}"
            )));
        }
        Ok(self.install_armed(armed))
    }

    /// Whether a session log is materialized on disk (Materializing normalization).
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
        let journal = active.shared_journal(&self.backend);
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
            && current.get("approved").is_none_or(Value::is_null)
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
        let journal = active.shared_journal(&self.backend);
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
        let journal = active.shared_journal(&self.backend);
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
        let journal = active.shared_journal(&self.backend);
        journal.append(event).map_err(SessionError::Corruption)?;
        journal.flush().map_err(SessionError::Corruption)?;
        checkpoint_active(active, &self.checkpoints)?;
        Ok(true)
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
        materialize_full_view: bool,
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
        let transcript = if materialize_full_view {
            row("transcript")
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
                .unwrap_or_default()
        } else {
            Vec::new()
        };
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
        let model_items = if materialize_full_view {
            projections
                .surface_nodes()
                .map_err(SessionError::Corruption)?
                .into_iter()
                .map(|(_, item)| item)
                .collect()
        } else {
            Vec::new()
        };
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

fn catch_up_replay(
    backend: &JsonlBackend,
    key: &SessionKey,
    committed: Option<u64>,
    replay: &Mutex<ResumeSink>,
) -> Result<(), SessionError> {
    let mut replay = replay.lock().expect("replay");
    if !committed.is_some_and(|committed| replay.pushed <= committed) {
        return Ok(());
    }
    let floor = replay.pushed;
    let ResumeSink {
        adapter,
        replay: events,
        usage,
        pushed,
    } = &mut *replay;
    backend.visit_from(key, floor, &mut |event| {
        adapter.push(event, events);
        usage.record(event);
        *pushed += 1;
        Ok(())
    })?;
    Ok(())
}

fn paginate_replay(
    replay: &[ReplayEvent],
    before_seq: Option<u64>,
    max_messages: usize,
) -> SessionHistoryPage {
    if max_messages == 0 || replay.is_empty() {
        return SessionHistoryPage {
            events: Vec::new(),
            has_more: !replay.is_empty(),
        };
    }
    let mut end = before_seq
        .map(|before| replay.partition_point(|event| event.seq() < before))
        .unwrap_or(replay.len());
    // A caller may present any journal cursor, including one between two
    // replay-visible events of the same turn. Rewind the upper edge as well
    // as the lower edge so a malformed or stale cursor can never expose a
    // partial turn.
    if end > 0 && end < replay.len() && replay[end - 1].turn() == replay[end].turn() {
        let turn = replay[end].turn();
        while end > 0 && replay[end - 1].turn() == turn {
            end -= 1;
        }
    }
    if end == 0 {
        return SessionHistoryPage {
            events: Vec::new(),
            has_more: false,
        };
    }

    let mut start = end;
    let mut messages = 0usize;
    while start > 0 {
        start -= 1;
        if replay[start].is_message() {
            messages += 1;
            if messages == max_messages {
                let turn = replay[start].turn();
                while start > 0 && replay[start - 1].turn() == turn {
                    start -= 1;
                }
                break;
            }
        }
    }
    SessionHistoryPage {
        events: replay[start..end].to_vec(),
        has_more: start > 0,
    }
}

#[cfg(test)]
mod tests;
