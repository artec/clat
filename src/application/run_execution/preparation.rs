//! Capacity acceptance precedes the recorder's target header and target provider request.
use super::*;
impl worker::RunWorker {
    pub(super) fn prepare_capacity(&mut self) -> Result<(), String> {
        let snapshot = self
            .durable_request_header
            .dynamic_instructions
            .as_ref()
            .map(|source| source.snapshot())
            .transpose()?
            .flatten();
        let instructions = self.durable_request_header.base_system.clone();
        let options = crate::ModelOptions {
            output_limit: self.deps.config.output_limit,
            image_projection: Some(crate::model::ImageProjectionBudget::for_config(
                &self.deps.config,
            )),
            request_tail: self
                .context
                .runtime_context
                .snapshot(snapshot)
                .request_tail(None),
            ..Default::default()
        };
        let tail_tokens = options.request_tail_tokens();
        let switched = self
            .deps
            .sessions
            .last_request_header()
            .is_some_and(|header| {
                header["config"]["model"] != self.deps.config.model
                    || header["config"]["provider"] != self.deps.config.protocol.to_string()
            });
        let target = crate::model::ContextBudget::resolve(&self.deps.config)?;
        self.maintain_before_round(&instructions, &options)?;
        let over = target.is_some_and(|budget| {
            needs_compaction_before_request(
                self.measured_input(&instructions, tail_tokens),
                budget,
                switched,
            )
        });
        // Startup and active requests both prune/offload before paying for a summary.
        // A switch must fit before the target request header can be published.
        if over
            && self
                .automatic_summary_failures
                .load(std::sync::atomic::Ordering::Relaxed)
                < super::super::compaction::SUMMARY_FAILURE_LIMIT
            && let Some(compactor) = &self.deps.compactor
            && self.deps.task_service.cap().is_none()
        {
            self.summarize_before_request(
                compactor.as_ref(),
                &instructions,
                tail_tokens,
                switched,
            )?;
            self.reload_maintained_history()?;
        }
        if let Some(budget) = target
            && self.measured_input(&instructions, tail_tokens) > budget.capacity
        {
            return Err(
                "conversation cannot fit the configured model capacity; model request was not dispatched".into()
            );
        }
        Ok(())
    }
    fn measured_input(&self, instructions: &str, tail_tokens: u64) -> u64 {
        self.deps
            .sessions
            .measure_request(
                &self.deps.config,
                Some(instructions),
                &self.input.history,
                &self.context.tool_definitions,
                tail_tokens,
            )
            .input
    }

    fn reload_maintained_history(&mut self) -> Result<(), String> {
        self.input.history = self
            .deps
            .sessions
            .surface_nodes()
            .map_err(|error| error.to_string())?
            .into_iter()
            .map(|(_, item)| item)
            .collect();
        Ok(())
    }
    fn maintain_before_round(
        &mut self,
        instructions: &str,
        options: &crate::ModelOptions,
    ) -> Result<(), String> {
        self.input.journal.flush()?;
        self.deps
            .sessions
            .sync_active()
            .map_err(|error| error.to_string())?;
        let maintenance = super::super::compaction::maintenance::DurableMaintenance {
            sessions: &self.deps.sessions,
            journal: self.input.journal.as_ref(),
            cancel: &self.deps.cancel,
            config: &self.deps.config,
            pruner: self.deps.pruner.as_deref(),
            image_offload: self.deps.image_offload.as_deref(),
            protected_floor: self.input.replay_before_seq,
        };
        let pressure =
            crate::model::ContextBudget::resolve(&self.deps.config)?.is_some_and(|budget| {
                self.measured_input(instructions, options.request_tail_tokens()) > budget.threshold
            });
        let pruned = if pressure {
            // INV-CP5：trim 之后先无模型清组泄压，再考虑付钱摘要。
            let pruned = maintenance.prune_results(&mut self.input.history)?;
            pruned + maintenance.sweep_tool_groups(&mut self.input.history)?
        } else {
            0
        };
        let images = maintenance.maintain_images(
            &mut self.input.history,
            Some(instructions),
            &self.context.tool_definitions,
            options,
            false,
        )?;
        if pruned + images > 0 {
            broadcast_to(
                &self.deps.subscribers,
                ApplicationEvent::CompactionUpdated(CompactionStatus::Finished {
                    note: crate::i18n::UiMessage::new("command.context_maintained")
                        .arg("pruned", pruned)
                        .arg("images", images)
                        .into(),
                    succeeded: true,
                }),
            );
        }
        Ok(())
    }
    fn summarize_before_request(
        &self,
        compactor: &dyn crate::plugins::services::HistoryCompactor,
        instructions: &str,
        tail_tokens: u64,
        switched: bool,
    ) -> Result<(), String> {
        let previous = if switched {
            self.previous_compaction_route()?
        } else {
            None
        };
        // Prefer the previous route when saved; otherwise use the configured
        // target route for bounded summarization before target dispatch.
        let (config, credentials) = previous.as_ref().map_or(
            (&self.deps.config, &self.deps.credentials),
            |(config, credentials)| (config, credentials),
        );
        let report = super::super::compaction::commit::IdleCompaction {
            sessions: &self.deps.sessions,
            subscribers: &self.deps.subscribers,
            journal: self.input.journal.as_ref(),
            compactor,
            config,
            credentials,
            cancel: &self.deps.cancel,
            turn: self.input.turn,
            instructions: instructions.to_owned(),
            tail_tokens,
            definitions: self.context.tool_definitions.to_vec(),
            intent: crate::plugins::services::CompactionIntent::automatic(switched),
            automatic_summary_failures: Some(&self.automatic_summary_failures),
        }
        .execute();
        if switched {
            report.map_err(|error| error.to_string())?;
        } else if let Ok(report) = &report
            && report.shadowed_count > 0
        {
            broadcast_to(
                &self.deps.subscribers,
                ApplicationEvent::CompactionUpdated(CompactionStatus::Finished {
                    note: report.status_presentation().into(),
                    succeeded: true,
                }),
            );
        } else if let Err(reason) = report {
            broadcast_to(
                &self.deps.subscribers,
                ApplicationEvent::CompactionUpdated(CompactionStatus::Finished {
                    note: crate::i18n::UiMessage::new("command.compaction_degraded")
                        .arg_message("reason", reason.message())
                        .into(),
                    succeeded: false,
                }),
            );
        }
        Ok(())
    }
    fn previous_compaction_route(
        &self,
    ) -> Result<Option<(ModelConfig, ProviderCredentials)>, String> {
        let Some(header) = self.deps.sessions.last_request_header() else {
            return Ok(None);
        };
        for profile in self
            .deps
            .config_store
            .list_profiles()
            .map_err(|e| e.to_string())?
        {
            if let Some(route) = self
                .deps
                .config_store
                .load_profile(&profile.name)
                .map_err(|e| e.to_string())?
                && route.0.is_configured()
                && header["config"]["model"] == route.0.model
                && header["config"]["provider"] == route.0.protocol.to_string()
            {
                return Ok(Some(route));
            }
        }
        Ok(None)
    }
}
/// Model switching uses the target's hard input capacity, not the old
/// model's pressure threshold. Ongoing turns use the target threshold.
fn needs_compaction_before_request(
    measured_input: u64,
    budget: crate::model::ContextBudget,
    switched: bool,
) -> bool {
    measured_input
        > if switched {
            budget.capacity
        } else {
            budget.threshold
        }
}

#[cfg(test)]
mod capacity_switch_tests {
    use super::*;

    #[test]
    fn generic_switch_uses_configured_500k_256k_and_smaller_windows() {
        let mut config = ModelConfig {
            max_context_tokens: Some(1_048_576),
            output_limit: Some(16_384),
            ..Default::default()
        };
        for (window, compact) in [
            (1_048_576, false),
            (500_000, true),
            (262_144, true),
            (131_072, true),
            (65_536, true),
        ] {
            config.max_context_tokens = Some(window);
            let budget = crate::model::ContextBudget::resolve(&config)
                .unwrap()
                .unwrap();
            assert_eq!(
                needs_compaction_before_request(490_000, budget, true),
                compact,
                "target model window {window}"
            );
        }
        // Switching back to a bigger model does not carry forward the old
        // limit or force a summary; any earlier compacted history stays
        // compacted in the durable journal.
        config.max_context_tokens = Some(1_048_576);
        let large = crate::model::ContextBudget::resolve(&config)
            .unwrap()
            .unwrap();
        assert!(!needs_compaction_before_request(490_000, large, true));
        // A same-model follow-up uses proactive pressure threshold.
        assert!(needs_compaction_before_request(900_000, large, false));
        assert!(!needs_compaction_before_request(100_000, large, false));
    }
}
