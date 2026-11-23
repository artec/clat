use crate::CancelToken;
use crate::event::{EventSink, RunEvent};
use crate::i18n::{PresentedText, UiMessage};
use crate::model::{ModelConfig, ProviderCredentials};
use crate::plugins::services::{
    CompactionIntent, CompactionNode, CompactionOutcome, CompactionRequest, HistoryCompactor,
};
use crate::session::event::payloads;
use crate::session::recorder::SessionRecorder;
use crate::session::run_journal::{NewSessionEvent, RunJournal};
use crate::session::use_cases::SessionService;
use serde_json::json;
use std::sync::{Arc, Mutex};

mod active;
mod alignment;
pub(in crate::application) mod commit;
pub(in crate::application) mod maintenance;
mod manual;
mod recovery;
pub(super) use active::ActiveContext;
pub use manual::CompactHandle;

use super::*;

/// Lock-backed handle so the recorder can be driven as an `EventSink`
/// while the worker keeps access for the final `finish()`.
///
/// FP-09（架构层）：frontend sink **不在 recorder 锁内执行**——journal
/// 变换（锁内）完成后取走待转发事件、释放 recorder 锁，再驱动 sink
///（锁外，独立 mutex）。frontend panic 从此无法毒化 recorder：核心
/// persistence 与 frontend 故障域隔离（桌面端/第二客户端路线的地基）。
/// 顺序不变量保持：事件按到达顺序先记账再转发（handle 单线程串行）。
pub(super) struct RecorderHandle {
    pub(super) inputs: crate::input::InputQueue,
    pub(super) drafts: crate::application::DraftImages,
    pub(super) recorder: Arc<Mutex<SessionRecorder>>,
    pub(super) sink: Arc<Mutex<Box<dyn EventSink + Send>>>,
}

impl EventSink for RecorderHandle {
    fn emit(&mut self, event: RunEvent) {
        // 毒锁恢复（防御）：journal 变换必须继续。
        let forwarded = {
            let mut recorder = self
                .recorder
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            recorder.emit_and_forward(event)
        };
        if forwarded.is_empty() {
            return;
        }
        let mut sink = self
            .sink
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        for event in forwarded {
            if let RunEvent::SteeringApplied {
                client_message_id: Some(id),
                receipt: Some(receipt),
                request_digest: Some(digest),
                ..
            } = &event
                && receipt.state == crate::message::AdmissionState::Committed
                && let Some(draft) = self.inputs.committed(id, digest)
                && let Some(scope) = draft.draft_scope_id
            {
                self.drafts.commit_uploads(&scope, &draft.uploads);
            }
            sink.emit(event);
        }
    }
}

fn source_candidate<'a>(
    sessions: &SessionService,
    plan: &crate::plugins::CompactionPlan<'a>,
    summary: &str,
) -> Result<crate::plugins::CompactionCandidate<'a>, String> {
    let session = sessions.active_id();
    plan.candidate_with_sources(
        summary,
        session.as_ref().map(crate::SessionId::as_str),
        &|seq| sessions.original_tool_result(seq),
    )
}

struct CompactionCommit {
    replace_seq: u64,
    summary: String,
    disclosure: crate::session::replay::CompactionDisclosure,
}

impl CompactionCommit {
    fn notify(
        self,
        session: Option<crate::SessionId>,
        subscribers: &Arc<Mutex<Vec<std::sync::mpsc::Sender<ApplicationEvent>>>>,
    ) {
        if let Some(session) = session {
            broadcast_to(
                subscribers,
                ApplicationEvent::CompactionRecorded {
                    session,
                    seq: self.replace_seq - 1,
                    summary: self.summary,
                    disclosure: self.disclosure,
                },
            );
        }
    }
}

/// INV-CP2：连续自动摘要失败熔断阈值——达到即暂停本 run 的自动压缩；
/// 任一次成功复位（新用户轮由 per-run 计数器结构性复位）。
pub(in crate::application) const SUMMARY_FAILURE_LIMIT: u32 = 3;

/// 事件族 + replace 载体一次原子提交并 flush（plan §13.4；compaction
/// 事件族与 user/message 的邻接是契约）。闲置路径带 committed_seq CAS；
/// 活动路径见 `write_compaction_events_span_anchored`（INV-CP1 无 CAS）。
fn write_compaction_events(
    journal: &dyn RunJournal,
    shadowed: &[CompactionNode],
    summary: &str,
    outcome: &CompactionOutcome,
    config: &ModelConfig,
    turn: u64,
    revision: Option<u64>,
) -> Result<CompactionCommit, String> {
    let family = compaction_event_family(shadowed, summary, outcome, config, turn)?;
    let disclosure = crate::session::replay::CompactionDisclosure::from_data(&family[1].data);
    let committed = journal.append_atomic_durable_at(revision, &family)?;
    Ok(commit(committed, shadowed, summary, disclosure))
}

/// INV-CP1（CP-C1）：活动路径提交——跨度锚定校验已在调用方完成，这里
/// 只做**无 CAS 的原子组追加**：摘要网络阶段（分钟级窗口）内的跨度外
/// 追加（surface 追加与 log-only 事件，含 `job/*`、回执）合法存活，
/// 不再作为提交门。
fn write_compaction_events_span_anchored(
    journal: &dyn RunJournal,
    shadowed: &[CompactionNode],
    summary: &str,
    outcome: &CompactionOutcome,
    config: &ModelConfig,
    turn: u64,
) -> Result<CompactionCommit, String> {
    let family = compaction_event_family(shadowed, summary, outcome, config, turn)?;
    let disclosure = crate::session::replay::CompactionDisclosure::from_data(&family[1].data);
    let expected = shadowed
        .iter()
        .map(|node| (node.seq, node.item.clone()))
        .collect::<Vec<_>>();
    let committed = journal.append_compaction_durable_span(&expected, &family)?;
    Ok(commit(committed, shadowed, summary, disclosure))
}

fn commit(
    committed: crate::session::run_journal::SeqRange,
    _shadowed: &[CompactionNode],
    summary: &str,
    disclosure: crate::session::replay::CompactionDisclosure,
) -> CompactionCommit {
    CompactionCommit {
        replace_seq: committed.start + 2,
        summary: summary.to_owned(),
        disclosure,
    }
}

fn compaction_event_family(
    shadowed: &[CompactionNode],
    summary: &str,
    outcome: &CompactionOutcome,
    config: &ModelConfig,
    turn: u64,
) -> Result<Vec<NewSessionEvent>, String> {
    if shadowed.is_empty() {
        return Err("compaction shadowed an empty range".into());
    }
    let compaction_id = uuid::Uuid::new_v4().to_string();
    let mut seqs: Vec<u64> = shadowed.iter().map(|node| node.seq).collect();
    seqs.dedup();
    let range = (seqs[0], seqs[seqs.len() - 1]);
    let usage = json!({
        "inputTokens": outcome.usage.input_tokens,
        "outputTokens": outcome.usage.output_tokens,
    });
    Ok(vec![
        NewSessionEvent::new(
            "compaction/start",
            payloads::compaction_start(&compaction_id, turn),
        )
        .log_only(),
        NewSessionEvent::new(
            "compaction/summary",
            payloads::compaction_summary(
                &compaction_id,
                summary,
                range,
                &seqs,
                outcome.shadowed_token_count,
                &config.protocol.to_string(),
                &config.model,
                outcome.summary_output_limit,
                usage,
            ),
        )
        .log_only(),
        NewSessionEvent::new("user/message", payloads::compaction_user_message(summary))
            .replace(range.0, range.1, seqs),
        NewSessionEvent::new(
            "compaction/end",
            payloads::compaction_end(&compaction_id, turn, None),
        )
        .log_only(),
    ])
}

/// 手动压缩结果报告。
#[derive(Clone, Debug)]
pub struct CompactReport {
    pub shadowed_count: usize,
    pub degraded: Option<PresentedText>,
}

impl CompactReport {
    pub fn status_presentation(&self) -> UiMessage {
        let base = UiMessage::new("command.compacted").arg("count", self.shadowed_count);
        match &self.degraded {
            Some(reason) => UiMessage::new("command.compacted_degraded")
                .arg_message("summary", base)
                .arg_message("reason", reason.message()),
            None => base,
        }
    }
}

/// Failed candidates still leave durable accounting, without any context replacement.
fn record_failed_attempt(
    journal: &dyn RunJournal,
    turn: u64,
    outcome: &CompactionOutcome,
    reason: &str,
) -> Result<(), String> {
    if outcome.usage.input_tokens == 0 && outcome.usage.output_tokens == 0 {
        return Ok(());
    }
    let id = uuid::Uuid::new_v4().to_string();
    let mut end = payloads::compaction_end(&id, turn, Some(reason));
    end["usage"] = json!({"inputTokens":outcome.usage.input_tokens,"outputTokens":outcome.usage.output_tokens});
    journal
        .append_atomic_durable(&[
            NewSessionEvent::new("compaction/start", payloads::compaction_start(&id, turn))
                .log_only(),
            NewSessionEvent::new("compaction/end", end).log_only(),
        ])
        .map(|_| ())
}
