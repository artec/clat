use crate::CancelToken;
use crate::event::{EventSink, RunEvent};
use crate::model::{ModelConfig, ProviderCredentials};
use crate::plugins::services::{
    CompactionNode, CompactionOutcome, CompactionRequest, HistoryCompactor,
};
use crate::session::event::payloads;
use crate::session::recorder::SessionRecorder;
use crate::session::run_journal::{NewSessionEvent, RunJournal};
use crate::session::use_cases::SessionService;
use serde_json::json;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use super::*;

impl TrustedProjectApplication {
    /// 手动 `/compact`：异步 worker 内执行（含网络摘要），立即返回可取
    /// 消的 handle（INV-C11）；与活动 Run 互斥。完成经
    /// `ApplicationEvent::CompactionUpdated` 报告。
    pub fn compact_session(&mut self) -> Result<CompactHandle, ApplicationError> {
        if self
            .active_run
            .as_ref()
            .is_some_and(|handle| !handle.is_finished())
        {
            return Err(ApplicationError::new("another run is already active"));
        }
        if let Some(previous) = self.active_compaction.take() {
            previous.join()?;
        }
        let session_id = self
            .sessions
            .active_id()
            .ok_or_else(|| ApplicationError::new("no conversation to compact"))?;
        let turn = self.sessions.active_turns().map_err(session_error)?;
        let (config, credentials) = self.model_state()?;
        if !config.is_configured() {
            return Err(ApplicationError::new(
                "model is not configured; configure a model and endpoint first",
            ));
        }
        let compactor = self
            .compactor
            .clone()
            .ok_or_else(|| ApplicationError::new("compaction service is not available"))?;
        let sessions = Arc::clone(&self.sessions);
        let journal = self.sessions.journal().map_err(session_error)?;
        let subscribers = Arc::clone(&self.subscribers);
        let cancel = CancelToken::new();
        let busy = Arc::new(AtomicBool::new(true));
        let join_slot = Arc::new(Mutex::new(None));
        let report_slot: Arc<Mutex<Option<Result<CompactReport, String>>>> =
            Arc::new(Mutex::new(None));
        let handle = CompactHandle {
            cancel: cancel.clone(),
            busy: Arc::clone(&busy),
            join: Arc::clone(&join_slot),
            report: Arc::clone(&report_slot),
        };
        broadcast_to(
            &subscribers,
            ApplicationEvent::CompactionUpdated(CompactionStatus::Started),
        );
        let _ = session_id;
        let worker = std::thread::Builder::new()
            .name("clat-compact".into())
            .spawn(move || {
                let result = (|| -> Result<CompactReport, String> {
                    let nodes = sessions
                        .surface_nodes()
                        .map_err(|error| error.to_string())?;
                    let compaction_nodes: Vec<CompactionNode> = nodes
                        .iter()
                        .map(|(seq, item)| CompactionNode {
                            seq: *seq,
                            item: item.clone(),
                        })
                        .collect();
                    let outcome = compactor.compact(CompactionRequest {
                        config: &config,
                        credentials: &credentials,
                        nodes: &compaction_nodes,
                        instructions: String::new(),
                        tool_definitions: Vec::new(),
                        force: true,
                        cancel: cancel.clone(),
                    });
                    let summary = outcome.summary.as_deref().ok_or_else(|| {
                        outcome
                            .degraded
                            .clone()
                            .unwrap_or_else(|| "nothing to compact".into())
                    })?;
                    let shadowed = &compaction_nodes[..outcome.shadowed_count];
                    write_compaction_events(
                        journal.as_ref(),
                        shadowed,
                        summary,
                        &outcome,
                        &config,
                        turn,
                    )?;
                    let _ = sessions.sync_active();
                    Ok(CompactReport {
                        shadowed_count: outcome.shadowed_count,
                        degraded: outcome.degraded,
                    })
                })();
                // CB1-11：结构化结果存入 handle，供 join_report 消费。
                if let Ok(mut slot) = report_slot.lock() {
                    *slot = Some(result.clone());
                }
                let note = match &result {
                    Ok(report) => report.status_text(),
                    Err(error) => format!("compaction failed: {error}"),
                };
                broadcast_to(
                    &subscribers,
                    ApplicationEvent::CompactionUpdated(CompactionStatus::Finished {
                        note,
                        succeeded: result.is_ok(),
                    }),
                );
                busy.store(false, Ordering::Release);
            })
            .map_err(|error| ApplicationError::new(format!("spawn compaction worker: {error}")))?;
        *join_slot
            .lock()
            .map_err(|_| ApplicationError::new("compaction join lock poisoned"))? = Some(worker);
        self.active_compaction = Some(handle.clone());
        Ok(handle)
    }
}

/// Lock-backed handle so the recorder can be driven as an `EventSink`
/// while the worker keeps access for the final `finish()`.
pub(super) struct RecorderHandle {
    pub(super) recorder: Arc<Mutex<SessionRecorder>>,
}

impl EventSink for RecorderHandle {
    fn emit(&mut self, event: RunEvent) {
        if let Ok(mut recorder) = self.recorder.lock() {
            recorder.emit(event);
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
) -> Result<(), String> {
    if shadowed.is_empty() {
        return Err("compaction shadowed an empty range".into());
    }
    let compaction_id = uuid::Uuid::new_v4().to_string();
    let seqs: Vec<u64> = shadowed.iter().map(|node| node.seq).collect();
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
    journal.append_atomic(&family)?;
    journal.flush()
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
) -> Option<(String, bool)> {
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
    let outcome = compactor.compact(CompactionRequest {
        config,
        credentials,
        nodes: &compaction_nodes,
        instructions: String::new(),
        tool_definitions: Vec::new(),
        force: false,
        cancel: cancel.clone(),
    });
    match &outcome.summary {
        Some(summary) => {
            let shadowed = &compaction_nodes[..outcome.shadowed_count.min(compaction_nodes.len())];
            let written =
                write_compaction_events(journal, shadowed, summary, &outcome, config, turn);
            let _ = sessions.sync_active();
            match written {
                Ok(()) => Some((
                    format!(
                        "compacted history: shadowed {} events",
                        outcome.shadowed_count
                    ),
                    true,
                )),
                Err(error) => Some((format!("compaction could not be persisted: {error}"), false)),
            }
        }
        None => outcome
            .degraded
            .as_ref()
            .map(|reason| (format!("compaction degraded: {reason}"), false)),
    }
}

/// `/compact` 的可取消句柄：取消令牌 + 幂等 join + 业务结果
/// （CB1-11：headless 调用方经 `join_report` 拿到结构化 CompactReport）。
#[derive(Clone)]
pub struct CompactHandle {
    cancel: CancelToken,
    busy: Arc<AtomicBool>,
    join: Arc<Mutex<Option<JoinHandle<()>>>>,
    report: Arc<Mutex<Option<Result<CompactReport, String>>>>,
}

impl CompactHandle {
    pub fn cancel(&self) {
        self.cancel.cancel();
    }

    pub fn is_finished(&self) -> bool {
        !self.busy.load(Ordering::Acquire)
    }

    pub fn join(&self) -> Result<(), ApplicationError> {
        let handle = self
            .join
            .lock()
            .map_err(|_| ApplicationError::new("compaction join lock poisoned"))?
            .take();
        if let Some(handle) = handle {
            handle
                .join()
                .map_err(|_| ApplicationError::new("compaction worker panicked"))?;
        }
        Ok(())
    }

    /// join 后返回压缩业务结果（Ok=事件族已耐久落盘；Err=失败原因）。
    /// ApplicationEvent 只是展示通道，这里才是结构化结果。
    pub fn join_report(&self) -> Result<Result<CompactReport, String>, ApplicationError> {
        self.join()?;
        self.report
            .lock()
            .map_err(|_| ApplicationError::new("compaction report lock poisoned"))
            .map(|slot| slot.clone().unwrap_or(Err("no compaction result".into())))
    }
}

/// 手动压缩结果报告。
#[derive(Clone, Debug)]
pub struct CompactReport {
    pub shadowed_count: usize,
    pub degraded: Option<String>,
}

impl CompactReport {
    pub(crate) fn status_text(&self) -> String {
        let base = format!("compacted: shadowed {} events", self.shadowed_count);
        match &self.degraded {
            Some(reason) => format!("{base} (degraded: {reason})"),
            None => base,
        }
    }
}
