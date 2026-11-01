//! The only idle compaction transaction: snapshot, summarize, validate, durable publish.
use super::*;
use crate::tool::ToolDefinition;

pub(in crate::application) struct IdleCompaction<'a> {
    pub sessions: &'a SessionService,
    pub journal: &'a dyn RunJournal,
    pub compactor: &'a dyn HistoryCompactor,
    pub config: &'a ModelConfig,
    pub credentials: &'a ProviderCredentials,
    pub cancel: &'a CancelToken,
    pub turn: u64,
    pub instructions: String,
    pub definitions: Vec<ToolDefinition>,
    pub force: bool,
}
impl IdleCompaction<'_> {
    pub fn execute(&self) -> Result<CompactReport, PresentedText> {
        self.journal.flush()?;
        self.sessions
            .sync_active()
            .map_err(|error| error.to_string())?;
        let session = self.sessions.active_id();
        let revision = self.journal.committed_seq();
        let nodes = self.nodes()?;
        let outcome = self.summarize(&nodes)?;
        let result = self.publish(session, revision, &nodes, &outcome);
        if let Err(reason) = &result {
            super::record_failed_attempt(self.journal, self.turn, &outcome, &reason.raw)?;
        }
        result
    }
    fn summarize(&self, nodes: &[CompactionNode]) -> Result<CompactionOutcome, String> {
        Ok(self.compactor.compact(CompactionRequest {
            config: self.config,
            credentials: self.credentials,
            nodes,
            measured_input: Some(
                self.sessions
                    .measure_request(
                        self.config,
                        Some(&self.instructions),
                        &nodes
                            .iter()
                            .map(|node| node.item.clone())
                            .collect::<Vec<_>>(),
                        &self.definitions,
                    )
                    .input,
            ),
            protected_boundaries: self
                .sessions
                .compaction_boundaries()
                .map_err(|error| error.to_string())?,
            active_checkpoints: None,
            protected_head: self.sessions.last_system_head().map(|(seq, _)| seq),
            spend_ledger: None,
            instructions: self.instructions.clone(),
            tool_definitions: self.definitions.clone(),
            force: self.force,
            cancel: self.cancel.clone(),
        }))
    }
    fn publish(
        &self,
        session: Option<crate::SessionId>,
        revision: Option<u64>,
        nodes: &[CompactionNode],
        outcome: &CompactionOutcome,
    ) -> Result<CompactReport, PresentedText> {
        if self.cancel.is_cancelled() {
            return Err(UiMessage::new("compaction.cancelled").into());
        }
        if outcome.summary.is_none() && outcome.degraded.is_none() {
            return Ok(CompactReport {
                shadowed_count: 0,
                degraded: None,
            });
        }
        let summary = outcome.summary.as_deref().ok_or_else(|| {
            outcome
                .degraded
                .clone()
                .unwrap_or_else(|| "nothing to compact".into())
        })?;
        if session != self.sessions.active_id() || nodes != self.nodes()?.as_slice() {
            return Err("session changed during context maintenance".into());
        }
        let end = outcome
            .shadowed_start
            .checked_add(outcome.shadowed_count)
            .ok_or("compaction range overflow")?;
        let plan = crate::plugins::CompactionPlan::new(nodes, outcome.shadowed_start..end)?;
        plan.validate_summary(summary)?;
        let shadowed = nodes
            .get(plan.range.clone())
            .ok_or("invalid compaction range")?;
        if self
            .sessions
            .last_system_head()
            .is_some_and(|(head, _)| shadowed.iter().any(|node| node.seq == head))
        {
            return Err("compaction would shadow the protected system head".into());
        }
        let framed = crate::plugins::frame_checkpoint(summary);
        write_compaction_events(
            self.journal,
            shadowed,
            &framed,
            outcome,
            self.config,
            self.turn,
            revision,
        )?;
        self.sessions
            .sync_active()
            .map_err(|error| error.to_string())?;
        Ok(CompactReport {
            shadowed_count: plan.range.len(),
            degraded: outcome.degraded.clone(),
        })
    }
    fn nodes(&self) -> Result<Vec<CompactionNode>, String> {
        self.sessions
            .compaction_nodes()
            .map(|nodes| {
                nodes
                    .into_iter()
                    .map(|(seq, item)| CompactionNode { seq, item })
                    .collect()
            })
            .map_err(|error| error.to_string())
    }
}

impl TrustedProjectApplication {
    pub(super) fn compaction_inputs(
        &self,
        config: &ModelConfig,
    ) -> Result<(String, Vec<ToolDefinition>), ApplicationError> {
        let snapshot = self
            .dynamic_instructions
            .snapshot()
            .map_err(ApplicationError::new)?;
        let context = self.run_context_snapshot(
            config,
            self.skills_snapshot()?,
            None,
            crate::memory::MemoryInjection::default(),
            self.goal_port()
                .injection()
                .map_err(ApplicationError::new)?,
            self.task_port()
                .injection()
                .map_err(ApplicationError::new)?,
        );
        Ok((
            crate::plugins::services::compose_instructions(
                &context.instructions.with_goal,
                snapshot.as_ref(),
            ),
            context.tool_definitions.to_vec(),
        ))
    }
}
