//! Application-owned compaction at complete model/tool request boundaries.
use super::*;
use crate::i18n::{PresentedText, UiMessage};
use crate::model::{ModelItem, ModelOptions, RunSpendLedger, Usage};
use crate::run::RequestContextManager;
use crate::tool::ToolDefinition;
use std::collections::BTreeMap;

pub(in crate::application) struct ActiveContext {
    pub(in crate::application) compactor: Arc<dyn HistoryCompactor>,
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
}

impl RequestContextManager for ActiveContext {
    fn maintain(
        &mut self,
        items: &mut Vec<ModelItem>,
        usage: &mut Usage,
        instructions: Option<&str>,
        definitions: &[ToolDefinition],
        options: &ModelOptions,
    ) -> Result<(), String> {
        let Some(window) = self.config.max_context_tokens.map(u64::from) else {
            return Ok(());
        };
        let target = options
            .image_projection
            .as_ref()
            .and_then(|budget| budget.request_token_limit())
            .unwrap_or_else(|| window.saturating_mul(4) / 5);
        let cost = |items: &[ModelItem]| {
            crate::model::estimate_request_tokens(instructions, items, definitions)
                .saturating_add(u64::from(options.output_limit.unwrap_or(4096)))
                .saturating_add(512)
        };
        if self.cancel.is_cancelled() || cost(items) <= target {
            return Ok(());
        }
        self.boundary()?;
        broadcast_to(
            &self.subscribers,
            ApplicationEvent::CompactionUpdated(CompactionStatus::Started),
        );
        let result = self.reduce(items, usage, instructions, definitions, &cost, target);
        let (note, succeeded): (PresentedText, bool) = match &result {
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
                            .map(|reason| {
                                UiMessage::new("common.diagnostic").arg("detail", reason.clone())
                            })
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
    fn boundary(&self) -> Result<(), String> {
        self.recorder
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .prepare_context_boundary()
            .map_err(|error| format!("session journal failed: {error}"))
    }

    fn reduce(
        &mut self,
        items: &mut Vec<ModelItem>,
        usage: &mut Usage,
        instructions: Option<&str>,
        definitions: &[ToolDefinition],
        cost: &dyn Fn(&[ModelItem]) -> u64,
        target: u64,
    ) -> Result<(usize, Option<String>), String> {
        let mut count = 0;
        let mut reason = None;
        // Each pass must shrink the actual request. Bound network work even with many steering turns.
        for _ in 0..4 {
            if self.cancel.is_cancelled() || cost(items) <= target {
                break;
            }
            if self.ledger.exceeds_cap() {
                return Err("run token budget exhausted during compaction".into());
            }
            let nodes = self
                .sessions
                .surface_nodes()
                .map_err(|error| error.to_string())?
                .into_iter()
                .map(|(seq, item)| CompactionNode { seq, item })
                .collect::<Vec<_>>();
            let checkpoints = self.checkpoints.keys().copied().collect::<Vec<_>>();
            let outcome = self.compactor.compact(CompactionRequest {
                config: &self.config,
                credentials: &self.credentials,
                nodes: &nodes,
                active_checkpoints: Some(&checkpoints),
                spend_ledger: Some(Arc::clone(&self.ledger)),
                instructions: instructions.unwrap_or_default().to_owned(),
                tool_definitions: definitions.to_vec(),
                force: true,
                cancel: self.cancel.clone(),
            });
            usage.add_assign(&outcome.usage);
            if self.cancel.is_cancelled() {
                self.record_unapplied(&outcome, "compaction cancelled")?;
                break;
            }
            let Some(summary) = &outcome.summary else {
                reason = outcome.degraded.clone();
                self.record_unapplied(
                    &outcome,
                    reason.as_deref().unwrap_or("compaction deferred"),
                )?;
                break;
            };
            if !self.apply(items, &nodes, summary, &outcome, cost)? {
                break;
            }
            count += 1;
        }
        self.boundary()?;
        Ok((count, reason))
    }

    fn apply(
        &mut self,
        items: &mut Vec<ModelItem>,
        nodes: &[CompactionNode],
        summary: &str,
        outcome: &CompactionOutcome,
        cost: &dyn Fn(&[ModelItem]) -> u64,
    ) -> Result<bool, String> {
        let summary =
            format!("CLAT task checkpoint (context only, not a new user command):\n{summary}");
        let (shadowed_range, updated) = match self.replacement(items, nodes, &summary, outcome) {
            Ok(replacement) => replacement,
            Err(error) => {
                self.record_unapplied(outcome, &error)?;
                return Err(error);
            }
        };
        let shadowed = &nodes[shadowed_range];
        if cost(&updated) >= cost(items) {
            self.record_unapplied(outcome, "summary would not reduce the active context")?;
            return Ok(false);
        }
        let seq = write_compaction_events(
            self.journal.as_ref(),
            shadowed,
            &summary,
            outcome,
            &self.config,
            self.turn,
        )
        .map_err(|error| format!("compaction could not be persisted: {error}"))?;
        *items = updated;
        for node in shadowed {
            self.checkpoints.remove(&node.seq);
        }
        self.checkpoints.insert(seq, summary);
        Ok(true)
    }

    fn replacement(
        &self,
        items: &[ModelItem],
        nodes: &[CompactionNode],
        summary: &str,
        outcome: &CompactionOutcome,
    ) -> Result<(std::ops::Range<usize>, Vec<ModelItem>), String> {
        let end = outcome
            .shadowed_start
            .checked_add(outcome.shadowed_count)
            .ok_or("compaction range overflow")?;
        let shadowed_range = outcome.shadowed_start..end;
        let shadowed = nodes
            .get(shadowed_range.clone())
            .ok_or("invalid compaction range")?;
        let range = alignment::live_range(items, shadowed, &self.checkpoints)?;
        let mut updated = items[..range.start].to_vec();
        updated.push(ModelItem::user_text(summary.to_owned()));
        updated.extend_from_slice(&items[range.end..]);
        Ok((shadowed_range, updated))
    }

    /// Account attempted summaries without publishing a replacement or inventing a model step.
    fn record_unapplied(&self, outcome: &CompactionOutcome, reason: &str) -> Result<(), String> {
        if outcome.usage.input_tokens == 0 && outcome.usage.output_tokens == 0 {
            return Ok(());
        }
        let id = uuid::Uuid::new_v4().to_string();
        let mut end = payloads::compaction_end(&id, self.turn, Some(reason));
        end["usage"] = json!({"inputTokens": outcome.usage.input_tokens, "outputTokens": outcome.usage.output_tokens});
        self.journal
            .append_atomic_durable(&[
                NewSessionEvent::new(
                    "compaction/start",
                    payloads::compaction_start(&id, self.turn),
                )
                .log_only(),
                NewSessionEvent::new("compaction/end", end).log_only(),
            ])
            .map(|_| ())
            .map_err(|error| format!("compaction accounting could not be persisted: {error}"))
    }
}
