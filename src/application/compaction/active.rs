//! Application-owned compaction at complete model/tool request boundaries.
use super::*;
use crate::i18n::{PresentedText, UiMessage};
use crate::model::{ModelItem, ModelOptions, RunSpendLedger, Usage};
use crate::run::RequestContextManager;
use crate::tool::ToolDefinition;
use std::collections::BTreeMap;
use std::sync::atomic::Ordering;

pub(in crate::application) struct ActiveContext {
    pub(in crate::application) compactor: Option<Arc<dyn HistoryCompactor>>,
    pub(in crate::application) pruner: Option<Arc<crate::plugins::ResultPruner>>,
    pub(in crate::application) image_offload: Option<Arc<crate::plugins::ImageOffloadPolicy>>,
    pub(in crate::application) protected_floor: u64,
    pub(in crate::application) sessions: Arc<SessionService>,
    pub(in crate::application) journal: Arc<dyn RunJournal>,
    pub(in crate::application) recorder: Arc<Mutex<SessionRecorder>>,
    pub(in crate::application) config: ModelConfig,
    pub(in crate::application) credentials: ProviderCredentials,
    pub(in crate::application) cancel: CancelToken,
    pub(in crate::application) turn: u64,
    pub(in crate::application) subscribers:
        Arc<Mutex<Vec<std::sync::mpsc::Sender<ApplicationEvent>>>>,
    pub(in crate::application) ledger: Arc<RunSpendLedger>,
    pub(in crate::application) checkpoints: BTreeMap<u64, String>,
    /// INV-CP2：连续自动摘要失败计数——达到 `SUMMARY_FAILURE_LIMIT` 暂停
    /// 本 run 的自动压缩；任一次成功清零（新用户轮由 per-run 计数器
    /// 结构性复位，不存在"一次失败永久停摆"）。
    pub(in crate::application) automatic_summary_failures: Arc<std::sync::atomic::AtomicU32>,
    /// One-shot signal to the Run's evidence tracker: durable history was
    /// rewritten, so reading the same tool result again may be productive.
    pub(in crate::application) history_changed: bool,
}

impl RequestContextManager for ActiveContext {
    fn take_history_change(&mut self) -> bool {
        std::mem::take(&mut self.history_changed)
    }

    fn stage_request(&mut self, snapshot: &crate::plugins::services::RuntimeSnapshot) {
        self.recorder
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .stage_model_request(snapshot);
    }

    fn flush_request(&mut self) -> Result<(), String> {
        self.recorder
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .flush_model_request()
    }

    fn observe(
        &mut self,
        instructions: Option<&str>,
        items: &[ModelItem],
        tools: &[ToolDefinition],
        usage: &Usage,
        tail_tokens: u64,
    ) {
        let estimated_input = crate::model::estimate_request_tokens(instructions, items, tools)
            .saturating_add(tail_tokens);
        self.sessions
            .observe_request(&self.config, instructions, tools, usage, estimated_input);
    }

    fn failed_capacity_attempt(
        &mut self,
        kind: crate::model::ModelErrorKind,
    ) -> Result<(), String> {
        self.recorder
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .finish_capacity_failure(kind)
    }
    fn recover(
        &mut self,
        kind: crate::model::ModelErrorKind,
        items: &mut Vec<ModelItem>,
        usage: &mut Usage,
        instructions: Option<&str>,
        definitions: &[ToolDefinition],
        options: &ModelOptions,
    ) -> Result<bool, String> {
        self.recover_capacity(kind, items, usage, instructions, definitions, options)
    }

    fn maintain(
        &mut self,
        items: &mut Vec<ModelItem>,
        usage: &mut Usage,
        instructions: Option<&str>,
        definitions: &[ToolDefinition],
        options: &ModelOptions,
    ) -> Result<(), String> {
        self.history_changed = false;
        // New runs always start from the explicitly selected target model window.
        let target = crate::model::ContextBudget::resolve(&self.config)?
            .map_or(u64::MAX, |budget| budget.threshold);
        let sessions = self.sessions.clone();
        let config = self.config.clone();
        let cost = |items: &[ModelItem]| {
            sessions
                .measure_request(
                    &config,
                    instructions,
                    items,
                    definitions,
                    options.request_tail_tokens(),
                )
                .input
        };
        if self.cancel.is_cancelled() {
            return Ok(());
        }
        let pruned = if cost(items) > target {
            self.boundary()?;
            // INV-CP5：trim（单条超限剪枝）之后、付钱摘要之前，先做
            // 无模型清组泄压——清后压力若已低于阈值，下面的摘要检查
            // 自然短路（CP-C4 红②）。
            let pruned = self.prune_results(items)?;
            pruned + self.sweep_tool_groups(items)?
        } else {
            0
        };
        let images = self.maintain_images(items, instructions, definitions, options, false)?;
        self.history_changed |= pruned > 0 || images > 0;
        self.notify_maintenance(pruned, images);
        if cost(items) <= target || !self.can_summarize() {
            return Ok(());
        }
        broadcast_to(
            &self.subscribers,
            ApplicationEvent::CompactionUpdated(CompactionStatus::Started),
        );
        let result = self.reduce(items, usage, instructions, definitions, &cost, target);
        self.report_summary(&result);
        if result.as_ref().is_ok_and(|(count, _)| *count > 0) {
            self.history_changed = true;
        }
        result?;
        if !self.cancel.is_cancelled() {
            crate::model::project_items_for_image_budget(items, instructions, definitions, options)
                .map_err(|error| {
                    format!("context could not be reduced to the request budget: {error}")
                })?;
        }
        Ok(())
    }
}

impl ActiveContext {
    fn report_summary(&self, result: &Result<(usize, Option<PresentedText>), String>) {
        let (note, succeeded): (PresentedText, bool) = match result {
            Ok((count, _)) if *count > 0 => (
                UiMessage::new("command.compacted_active")
                    .arg("count", *count)
                    .into(),
                true,
            ),
            Ok((_, reason)) => (
                UiMessage::new("command.compaction_deferred")
                    .arg_message(
                        "reason",
                        reason
                            .as_ref()
                            .map(|reason| reason.message())
                            .unwrap_or_else(|| UiMessage::new("command.compaction_no_range")),
                    )
                    .into(),
                false,
            ),
            Err(error) => (
                UiMessage::new("command.compaction_failed")
                    .arg("error", error.clone())
                    .into(),
                false,
            ),
        };
        broadcast_to(
            &self.subscribers,
            ApplicationEvent::CompactionUpdated(CompactionStatus::Finished { note, succeeded }),
        );
    }

    pub(super) fn can_summarize(&self) -> bool {
        self.compactor.is_some()
            && self.automatic_summary_failures.load(Ordering::Relaxed)
                < super::SUMMARY_FAILURE_LIMIT
    }

    pub(super) fn boundary(&self) -> Result<(), String> {
        self.recorder
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .prepare_context_boundary()
            .map_err(|error| format!("session journal failed: {error}"))
    }

    pub(super) fn reduce(
        &mut self,
        items: &mut Vec<ModelItem>,
        usage: &mut Usage,
        instructions: Option<&str>,
        definitions: &[ToolDefinition],
        cost: &dyn Fn(&[ModelItem]) -> u64,
        target: u64,
    ) -> Result<(usize, Option<PresentedText>), String> {
        let mut count = 0;
        let mut reason = None;
        // Each applied checkpoint must strictly shrink the request; stop on no progress.
        loop {
            if self.cancel.is_cancelled() || cost(items) <= target {
                break;
            }
            let nodes = self
                .sessions
                .compaction_nodes()
                .map_err(|error| error.to_string())?
                .into_iter()
                .map(|(seq, item, checkpoint)| CompactionNode {
                    seq,
                    item,
                    checkpoint,
                })
                .collect::<Vec<_>>();
            let checkpoints = self.checkpoints.keys().copied().collect::<Vec<_>>();
            // Compaction uses the same explicitly configured model window as
            // the next request; no provider-specific capacity inference.
            let effective = self.config.clone();
            let outcome = self
                .compactor
                .as_ref()
                .ok_or("compaction is unavailable")?
                .compact(CompactionRequest {
                    config: &effective,
                    credentials: &self.credentials,
                    nodes: &nodes,
                    measured_input: Some(cost(items)),
                    protected_boundaries: self
                        .sessions
                        .compaction_boundaries()
                        .map_err(|error| error.to_string())?,
                    active_checkpoints: Some(&checkpoints),
                    protected_head: self.sessions.last_system_head().map(|(seq, _)| seq),
                    spend_ledger: Some(Arc::clone(&self.ledger)),
                    instructions: instructions.unwrap_or_default().to_owned(),
                    tool_definitions: definitions.to_vec(),
                    force: target == 0,
                    cancel: self.cancel.clone(),
                });
            usage.add_assign(&outcome.usage);
            if self.cancel.is_cancelled() {
                self.record_unapplied(&outcome, "compaction cancelled")?;
                break;
            }
            let Some(summary) = &outcome.summary else {
                reason = outcome.degraded.clone();
                if outcome.attempted {
                    self.count_summary_failure();
                }
                self.record_unapplied(
                    &outcome,
                    reason.as_deref().unwrap_or("compaction deferred"),
                )?;
                break;
            };
            if !self.apply(items, &nodes, summary, &outcome, cost)? {
                self.count_summary_failure();
                break;
            }
            // INV-CP2：任一次成功复位熔断计数。
            self.automatic_summary_failures
                .store(0, std::sync::atomic::Ordering::Relaxed);
            count += 1;
        }
        self.boundary()?;
        Ok((count, reason))
    }

    /// INV-CP2：失败只递增计数（达到阈值由 `can_summarize` 拦截），不再
    /// 一次性停摆。
    fn count_summary_failure(&self) {
        self.automatic_summary_failures
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }

    fn apply(
        &mut self,
        items: &mut Vec<ModelItem>,
        nodes: &[CompactionNode],
        summary: &str,
        outcome: &CompactionOutcome,
        cost: &dyn Fn(&[ModelItem]) -> u64,
    ) -> Result<bool, String> {
        let range = outcome.shadowed_start
            ..outcome
                .shadowed_start
                .checked_add(outcome.shadowed_count)
                .ok_or("compaction range overflow")?;
        // INV-CP1（CP-C1）：跨度锚定提交——重取 surface 后只校验**被选
        // 跨度**未变（DSH `assertSelectedSpanStable` 同义）；跨度外的
        // 追加（surface 追加与 log-only 事件，含 `job/*`、回执）必须
        // 容忍，压缩照常落地、追加内容照常存活。全量 surface 相等与
        // `committed_seq` CAS 已从活动路径移除（提交走无 CAS 原子组）。
        // 提交失败只降级本次压缩，绝不向 turn 上抛。
        let current = self.nodes_after_summary()?;
        if !span_unchanged(&current, &nodes[range.clone()]) {
            self.record_unapplied(outcome, "selected span changed during summarization")?;
            return Ok(false);
        }
        if self.cancel.is_cancelled() {
            self.record_unapplied(outcome, "compaction cancelled")?;
            return Ok(false);
        }
        let plan = crate::plugins::CompactionPlan::new(nodes, range)?;
        let candidate = plan.candidate(summary)?;
        let aligned = alignment::live_range(items, candidate.shadowed(), &self.checkpoints);
        let updated = match aligned.and_then(|range| candidate.replace_live(items, range)) {
            Ok(replacement) => replacement,
            Err(error) => {
                self.record_unapplied(outcome, &error)?;
                return Ok(false);
            }
        };
        let shadowed = candidate.shadowed();
        if cost(&updated) >= cost(items) {
            self.record_unapplied(outcome, "summary would not reduce the active context")?;
            return Ok(false);
        }
        let session = self.sessions.active_id();
        let seq = match write_compaction_events_span_anchored(
            self.journal.as_ref(),
            shadowed,
            candidate.checkpoint(),
            outcome,
            &self.config,
            self.turn,
        ) {
            Ok(seq) => seq,
            Err(error) if error.starts_with("compaction selected span invalidated:") => {
                // A concurrent in-span replace won the final journal lane.
                // Keep the original model context and downgrade this summary,
                // rather than applying a stale checkpoint or killing the Run.
                self.record_unapplied(outcome, &error)?;
                return Ok(false);
            }
            Err(error) => return Err(format!("compaction could not be persisted: {error}")),
        };
        *items = updated;
        for node in shadowed {
            self.checkpoints.remove(&node.seq);
        }
        self.checkpoints
            .insert(seq.replace_seq, candidate.checkpoint().to_owned());
        seq.notify(session, &self.subscribers);
        Ok(true)
    }

    fn nodes_after_summary(&self) -> Result<Vec<CompactionNode>, String> {
        Ok(self
            .sessions
            .compaction_nodes()
            .map_err(|error| error.to_string())?
            .into_iter()
            .map(|(seq, item, checkpoint)| CompactionNode {
                seq,
                item,
                checkpoint,
            })
            .collect())
    }

    /// Account attempted summaries without publishing a replacement or inventing a model step.
    fn record_unapplied(&self, outcome: &CompactionOutcome, reason: &str) -> Result<(), String> {
        super::record_failed_attempt(self.journal.as_ref(), self.turn, outcome, reason)
    }
}

/// INV-CP1 跨度锚定校验（DSH `assertSelectedSpanStable` 同义）：被选跨度
/// 的节点必须在当前 surface 原样在位——同 seq、同 item、同 checkpoint
/// 标记、仍连续。跨度外的任何追加/变更不影响判定；跨度内节点缺失或被
/// 改写（并发 prune/replace）即失败。
fn span_unchanged(current: &[CompactionNode], span: &[CompactionNode]) -> bool {
    let Some(first) = span.first() else {
        return true;
    };
    let Some(offset) = current.iter().position(|node| node.seq == first.seq) else {
        return false;
    };
    let end = offset + span.len();
    if end > current.len() {
        return false;
    }
    current[offset..end]
        .iter()
        .zip(span.iter())
        .all(|(a, b)| a.seq == b.seq && a.item == b.item && a.checkpoint == b.checkpoint)
}
