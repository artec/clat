//! Application-owned compaction at complete model/tool request boundaries.
use super::*;
use crate::i18n::{PresentedText, UiMessage};
use crate::model::{ModelItem, ModelOptions, RunSpendLedger, Usage};
use crate::run::RequestContextManager;
use crate::tool::ToolDefinition;
use std::collections::BTreeMap;

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
}

impl RequestContextManager for ActiveContext {
    fn observe(
        &mut self,
        instructions: Option<&str>,
        items: &[ModelItem],
        tools: &[ToolDefinition],
        usage: &Usage,
    ) {
        self.sessions
            .observe_request(&self.config, instructions, items, tools, usage);
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
        let target = crate::model::ContextBudget::resolve(&self.config)?
            .map_or(u64::MAX, |budget| budget.threshold);
        let sessions = self.sessions.clone();
        let config = self.config.clone();
        let cost = |items: &[ModelItem]| {
            sessions
                .measure_request(&config, instructions, items, definitions)
                .input
        };
        if self.cancel.is_cancelled() {
            return Ok(());
        }
        let pruned = if cost(items) > target {
            self.boundary()?;
            self.prune_results(items)?
        } else {
            0
        };
        let images = self.maintain_images(items, instructions, definitions, options, false)?;
        self.notify_maintenance(pruned, images);
        if cost(items) <= target || self.compactor.is_none() {
            return Ok(());
        }
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
                .map(|(seq, item)| CompactionNode { seq, item })
                .collect::<Vec<_>>();
            let revision = self.journal.committed_seq();
            let checkpoints = self.checkpoints.keys().copied().collect::<Vec<_>>();
            let outcome = self
                .compactor
                .as_ref()
                .ok_or("compaction is unavailable")?
                .compact(CompactionRequest {
                    config: &self.config,
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
                self.record_unapplied(
                    &outcome,
                    reason.as_deref().unwrap_or("compaction deferred"),
                )?;
                break;
            };
            if !self.apply(items, &nodes, summary, &outcome, cost, revision)? {
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
        revision: Option<u64>,
    ) -> Result<bool, String> {
        let current = self
            .sessions
            .compaction_nodes()
            .map_err(|error| error.to_string())?
            .into_iter()
            .map(|(seq, item)| CompactionNode { seq, item })
            .collect::<Vec<_>>();
        if current != nodes || self.cancel.is_cancelled() {
            return Err("session changed during context maintenance".into());
        }
        let range = outcome.shadowed_start
            ..outcome
                .shadowed_start
                .checked_add(outcome.shadowed_count)
                .ok_or("compaction range overflow")?;
        let plan = crate::plugins::CompactionPlan::new(nodes, range)?;
        let candidate = plan.candidate(summary)?;
        let aligned = alignment::live_range(items, candidate.shadowed(), &self.checkpoints);
        let updated = match aligned.and_then(|range| candidate.replace_live(items, range)) {
            Ok(replacement) => replacement,
            Err(error) => {
                self.record_unapplied(outcome, &error)?;
                return Err(error);
            }
        };
        let shadowed = candidate.shadowed();
        if cost(&updated) >= cost(items) {
            self.record_unapplied(outcome, "summary would not reduce the active context")?;
            return Ok(false);
        }
        let seq = write_compaction_events(
            self.journal.as_ref(),
            shadowed,
            candidate.checkpoint(),
            outcome,
            &self.config,
            self.turn,
            revision,
        )
        .map_err(|error| format!("compaction could not be persisted: {error}"))?;
        *items = updated;
        for node in shadowed {
            self.checkpoints.remove(&node.seq);
        }
        self.checkpoints
            .insert(seq, candidate.checkpoint().to_owned());
        Ok(true)
    }

    /// Account attempted summaries without publishing a replacement or inventing a model step.
    fn record_unapplied(&self, outcome: &CompactionOutcome, reason: &str) -> Result<(), String> {
        super::record_failed_attempt(self.journal.as_ref(), self.turn, outcome, reason)
    }
}
