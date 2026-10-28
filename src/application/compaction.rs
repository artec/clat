use crate::CancelToken;
use crate::event::{EventSink, RunEvent};
use crate::i18n::{PresentedText, UiMessage};
use crate::model::{ModelConfig, ProviderCredentials};
use crate::plugins::services::{
    CompactionNode, CompactionOutcome, CompactionRequest, HistoryCompactor,
};
use crate::session::event::payloads;
use crate::session::recorder::SessionRecorder;
use crate::session::run_journal::{NewSessionEvent, RunJournal};
use crate::session::use_cases::SessionService;
use serde_json::json;
use std::sync::{Arc, Mutex};

mod active;
mod alignment;
mod manual;
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
    pub(super) drafts: Arc<crate::draft::DraftImageStore>,
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

/// 事件族 + replace 载体一次原子提交并 flush（plan §13.4；compaction
/// 事件族与 user/message 的邻接是契约）。
fn write_compaction_events(
    journal: &dyn RunJournal,
    shadowed: &[CompactionNode],
    summary: &str,
    outcome: &CompactionOutcome,
    config: &ModelConfig,
    turn: u64,
) -> Result<u64, String> {
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
    let family = vec![
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
    ];
    let committed = journal.append_atomic_durable(&family)?;
    Ok(committed.start + 2)
}

fn compact_history(
    compactor: &dyn HistoryCompactor,
    config: &ModelConfig,
    credentials: &ProviderCredentials,
    nodes: &[CompactionNode],
    force: bool,
    cancel: &CancelToken,
) -> CompactionOutcome {
    compactor.compact(CompactionRequest {
        config,
        credentials,
        nodes,
        active_checkpoints: None,
        spend_ledger: None,
        instructions: String::new(),
        tool_definitions: Vec::new(),
        force,
        cancel: cancel.clone(),
    })
}

/// 自动压缩的 worker 侧执行；返回 `(note, succeeded)` 或 None（无事发生）。
pub(super) fn run_auto_compaction(
    compactor: &dyn HistoryCompactor,
    sessions: &SessionService,
    journal: &dyn RunJournal,
    config: &ModelConfig,
    credentials: &ProviderCredentials,
    cancel: &CancelToken,
    turn: u64,
) -> Option<(PresentedText, bool)> {
    let nodes = sessions.surface_nodes().ok()?;
    if nodes.is_empty() {
        return None;
    }
    let compaction_nodes: Vec<CompactionNode> = nodes
        .iter()
        .map(|(seq, item)| CompactionNode {
            seq: *seq,
            item: item.clone(),
        })
        .collect();
    let outcome = compact_history(
        compactor,
        config,
        credentials,
        &compaction_nodes,
        false,
        cancel,
    );
    match &outcome.summary {
        Some(summary) => {
            let shadowed = &compaction_nodes[..outcome.shadowed_count.min(compaction_nodes.len())];
            // SV 最小适配行（S6 例外，docs/todo/session-v3-execution.md）：
            // V3 受保护 system 头不得被压缩 shadow——头之前的前缀节点
            // （至多首条用户消息）从遮蔽前缀剔除。
            let shadowed = match sessions.last_system_head() {
                Some((head, _)) => {
                    let skip = shadowed.iter().take_while(|node| node.seq <= head).count();
                    &shadowed[skip..]
                }
                None => shadowed,
            };
            let written =
                write_compaction_events(journal, shadowed, summary, &outcome, config, turn);
            let _ = sessions.sync_active();
            match written {
                Ok(_) => Some((
                    UiMessage::new("command.compacted_history")
                        .arg("count", outcome.shadowed_count)
                        .into(),
                    true,
                )),
                Err(error) => Some((
                    UiMessage::new("command.compaction_persistence_failed")
                        .arg("error", error)
                        .into(),
                    false,
                )),
            }
        }
        None => outcome.degraded.as_ref().map(|reason| {
            (
                UiMessage::new("command.compaction_degraded")
                    .arg_message("reason", reason.message())
                    .into(),
                false,
            )
        }),
    }
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
