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
        let instructions = crate::plugins::services::compose_instructions(
            &self.durable_request_header.base_system,
            snapshot.as_ref(),
        );
        let switched = self
            .deps
            .sessions
            .last_request_header()
            .is_some_and(|header| {
                header["config"]["model"] != self.deps.config.model
                    || header["config"]["provider"] != self.deps.config.protocol.to_string()
            });
        let target = crate::model::ContextBudget::resolve(&self.deps.config)?;
        self.maintain_before_round(&instructions)?;
        let over = target.is_some_and(|budget| {
            self.deps
                .sessions
                .measure_request(
                    &self.deps.config,
                    Some(&instructions),
                    &self.input.history,
                    &self.context.tool_definitions,
                )
                .input
                > if switched {
                    budget.capacity
                } else {
                    budget.threshold
                }
        });
        // Startup and active requests both prune/offload before paying for a summary.
        // A switch must fit before the target request header can be published.
        if over
            && let Some(compactor) = &self.deps.compactor
            && self.deps.task_service.cap().is_none()
        {
            self.summarize_before_request(compactor.as_ref(), &instructions, switched)?;
            self.reload_maintained_history()?;
        }
        if switched
            && let Some(budget) = target
            && self
                .deps
                .sessions
                .measure_request(
                    &self.deps.config,
                    Some(&instructions),
                    &self.input.history,
                    &self.context.tool_definitions,
                )
                .input
                > budget.capacity
        {
            return Err("conversation cannot fit the target model capacity; target request was not dispatched".into());
        }
        Ok(())
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
    fn maintain_before_round(&mut self, instructions: &str) -> Result<(), String> {
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
        let options = crate::ModelOptions {
            output_limit: self.deps.config.output_limit,
            image_projection: Some(crate::model::ImageProjectionBudget::for_config(
                &self.deps.config,
            )),
            ..Default::default()
        };
        let pressure =
            crate::model::ContextBudget::resolve(&self.deps.config)?.is_some_and(|budget| {
                self.deps
                    .sessions
                    .measure_request(
                        &self.deps.config,
                        Some(instructions),
                        &self.input.history,
                        &self.context.tool_definitions,
                    )
                    .input
                    > budget.threshold
            });
        let pruned = if pressure {
            maintenance.prune_results(&mut self.input.history)?
        } else {
            0
        };
        let images = maintenance.maintain_images(
            &mut self.input.history,
            Some(instructions),
            &self.context.tool_definitions,
            &options,
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
        switched: bool,
    ) -> Result<(), String> {
        let previous = if switched {
            self.previous_compaction_route()?
        } else {
            None
        };
        if switched && previous.is_none() {
            return Err(
                "target model capacity requires compaction but the previous route is unavailable"
                    .into(),
            );
        }
        let (config, credentials) = previous.as_ref().map_or(
            (&self.deps.config, &self.deps.credentials),
            |(config, credentials)| (config, credentials),
        );
        let report = super::super::compaction::commit::IdleCompaction {
            sessions: &self.deps.sessions,
            journal: self.input.journal.as_ref(),
            compactor,
            config,
            credentials,
            cancel: &self.deps.cancel,
            turn: self.input.turn,
            instructions: instructions.to_owned(),
            definitions: self.context.tool_definitions.to_vec(),
            force: switched,
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
