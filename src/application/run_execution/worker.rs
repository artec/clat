use super::*;

/// Mutable worker progress. Only this owner advances rounds and releases run resources.
pub(super) struct RunWorker {
    pub(super) deps: RunWorkerDeps,
    pub(super) input: PreparedRun,
    pub(super) context: RunContextSnapshot,
    header_reason: Option<&'static str>,
    pub(super) durable_request_header: RequestHeaderData,
    first_round: bool,
    pub(super) task_run: Option<String>,
    pub(super) task_terminal: Option<Arc<Mutex<SessionRecorder>>>,
    pub(super) goal_mode: bool,
    round_started: std::time::Instant,
    aggregate_usage: Usage,
    aggregate_turns: usize,
    pub(super) subagent_bind_error: Option<String>,
    pub(super) captured_text: Arc<Mutex<String>>,
    pub(super) ui_sink: Arc<Mutex<Box<dyn EventSink + Send>>>,
}
struct RoundAccounting {
    usage: Usage,
    turns: usize,
    text: String,
    succeeded: bool,
    cancelled: bool,
}
impl RunWorker {
    pub(super) fn new(
        deps: RunWorkerDeps,
        start: RunExecutionStart,
        events: Box<dyn EventSink + Send>,
    ) -> Self {
        let RunExecutionStart {
            prepared: input,
            request_header,
            header_reason,
            context,
        } = start;
        deps.jobs.bind(input.session_id.clone());
        if let Some(todo) = &deps.todo_service {
            todo.bind_run(&input.session_id, Arc::clone(&input.journal));
        }
        let subagent_bind_error = deps
            .subagent_service
            .bind_run(
                &input.session_id,
                input.turn,
                Arc::clone(&input.journal),
                deps.config.clone(),
                deps.credentials.clone(),
            )
            .err();
        let capture = CapturingEventSink::new(events);
        let captured_text = Arc::clone(&capture.text);
        let ui_sink = Arc::new(Mutex::new(Box::new(capture) as Box<dyn EventSink + Send>));
        let goal_mode = input.goal_round_started.is_some();
        let round_started = input
            .goal_round_started
            .unwrap_or_else(std::time::Instant::now);
        let task_run = deps.task_service.binding_run();
        Self {
            task_run,
            task_terminal: None,
            deps,
            input,
            context,
            header_reason,
            durable_request_header: request_header,
            first_round: true,
            goal_mode,
            round_started,
            aggregate_usage: Usage::default(),
            aggregate_turns: 0,
            subagent_bind_error,
            captured_text,
            ui_sink,
        }
    }
    fn clear_run_slots(&self) {
        self.deps.jobs.unbind();
        self.deps.slots.clear();
    }
    pub(super) fn run(mut self) {
        let result = self.rounds();
        self.finish(result);
    }
    fn rounds(&mut self) -> ApplicationRunResult {
        loop {
            self.prepare_round();
            let mut round = self.execute_round();
            let accounting = self.account_round(&round.result, round.cancelled);
            let tokens = round.spent_tokens();
            if self.goal_mode && self.continue_goal(&mut round.result, accounting, tokens)? {
                continue;
            }
            return round
                .result
                .map(|mut done| {
                    done.turns = self.aggregate_turns;
                    done.usage = self.aggregate_usage.clone();
                    done
                })
                .map_err(|mut failure| {
                    failure.turns = self.aggregate_turns;
                    failure.usage = self.aggregate_usage.clone();
                    failure
                });
        }
    }
    fn prepare_round(&mut self) {
        self.deps.subagent_service.update_turn(self.input.turn);
        if let Some(compactor) = &self.deps.compactor
            && self.deps.task_service.cap().is_none()
        {
            let note = run_auto_compaction(
                compactor.as_ref(),
                self.deps.sessions.as_ref(),
                self.input.journal.as_ref(),
                &self.deps.config,
                &self.deps.credentials,
                &self.deps.cancel,
                self.input.turn,
            );
            if let Some(note) = note {
                broadcast_to(
                    &self.deps.subscribers,
                    ApplicationEvent::CompactionUpdated(CompactionStatus::Finished {
                        note: note.0,
                        succeeded: note.1,
                    }),
                );
            }
            if let Ok(nodes) = self.deps.sessions.surface_nodes() {
                self.input.history = nodes.into_iter().map(|(_, item)| item).collect();
            }
        }
        self.deps
            .slots
            .plugin_host
            .update_run_metadata(&self.input.session_id.to_string(), &self.input.history);
    }
    fn execute_round(&mut self) -> round::SettledRound {
        let context = self.context.round_context(
            &self.durable_request_header,
            self.header_reason,
            self.first_round,
            self.goal_mode.then(|| self.deps.goal_service.injection()),
        );
        self.durable_request_header = context.header.clone();
        RoundExecution::execute(self, context)
    }
    fn account_round(
        &mut self,
        result: &ApplicationRunResult,
        was_cancelled: bool,
    ) -> RoundAccounting {
        let (mut usage, turns, text, succeeded) = match result {
            Ok(done) => (
                done.usage.clone(),
                done.turns,
                done.output.clone(),
                !done.cancelled,
            ),
            Err(failure) => (
                failure.usage.clone(),
                failure.turns,
                failure.error.clone(),
                false,
            ),
        };
        usage.add_assign(&self.deps.subagent_service.take_round_usage());
        let cancelled = was_cancelled || matches!(result, Ok(done) if done.cancelled);
        self.aggregate_usage.add_assign(&usage);
        self.aggregate_turns = self.aggregate_turns.saturating_add(turns);
        RoundAccounting {
            usage,
            turns,
            text,
            succeeded,
            cancelled,
        }
    }
    fn continue_goal(
        &mut self,
        result: &mut ApplicationRunResult,
        accounting: RoundAccounting,
        tokens: u64,
    ) -> Result<bool, ApplicationRunFailure> {
        match self.deps.goal_service.finish_round(
            tokens,
            self.round_started.elapsed(),
            accounting.succeeded,
            accounting.cancelled,
            &accounting.text,
        ) {
            Ok(crate::goal::GoalContinuation::Continue) if !accounting.cancelled => {
                self.advance_goal(accounting)?;
                Ok(true)
            }
            Ok(_) => Ok(false),
            Err(error) => {
                self.deps.goal_service.disarm();
                *result = Err(ApplicationRunFailure {
                    receipt: None,
                    error: format!("goal progress could not commit after the round: {error}"),
                    turns: accounting.turns,
                    usage: accounting.usage,
                });
                Ok(false)
            }
        }
    }
    fn advance_goal(&mut self, accounting: RoundAccounting) -> Result<(), ApplicationRunFailure> {
        let next = self.deps.goal_service.next_round().map_err(|error| {
            self.deps.goal_service.disarm();
            ApplicationRunFailure {
                receipt: None,
                error: format!("goal continuation reservation failed: {error}"),
                turns: accounting.turns,
                usage: accounting.usage,
            }
        })?;
        let next_turn = self
            .deps
            .sessions
            .active_turns()
            .map(|turns| turns.saturating_add(1))
            .map_err(|error| {
                self.continuation_failure(format!(
                    "goal continuation could not read turn state: {error}"
                ))
            })?;
        let batch = [
            NewSessionEvent::new("turn/start", payloads::turn_start(next_turn)),
            NewSessionEvent::new("user/message", next.message).append(Vec::new()),
        ];
        self.round_started = next.started_at;
        self.input
            .journal
            .append_atomic(&batch)
            .and_then(|_| self.input.journal.flush())
            .map_err(|error| {
                self.continuation_failure(format!(
                    "goal continuation durable prelude failed: {error}"
                ))
            })?;
        let _ = self.deps.sessions.sync_active();
        self.input.history = self
            .deps
            .sessions
            .surface_nodes()
            .map_err(|error| {
                self.continuation_failure(format!(
                    "goal continuation history rebuild failed: {error}"
                ))
            })?
            .into_iter()
            .map(|(_, item)| item)
            .collect();
        self.input.turn = next_turn;
        self.input.message = crate::message::MessageContent::text(next.prompt);
        self.input.client_message_id = None;
        self.first_round = false;
        Ok(())
    }
    fn continuation_failure(&self, error: String) -> ApplicationRunFailure {
        self.deps.goal_service.disarm();
        ApplicationRunFailure {
            receipt: None,
            error,
            turns: self.aggregate_turns,
            usage: self.aggregate_usage.clone(),
        }
    }
    fn finish(mut self, result: ApplicationRunResult) {
        if let Some(todo_service) = &self.deps.todo_service {
            todo_service.unbind();
        }
        self.deps.subagent_service.unbind();
        let close_result = self.deps.run_scope.close();
        self.deps.monitor.refresh();
        let result = match (result, close_result) {
            (result, Ok(())) => result,
            (Ok(done), Err(error)) => Err(ApplicationRunFailure {
                receipt: None,
                error: format!("run scope cleanup failed: {error}"),
                turns: done.turns,
                usage: done.usage,
            }),
            (Err(mut failure), Err(error)) => {
                failure
                    .error
                    .push_str(&format!("; run scope cleanup failed: {error}"));
                Err(failure)
            }
        };
        self.clear_run_slots();
        let sampled = self
            .deps
            .sampling_usage
            .lock()
            .map(|mut cell| std::mem::take(&mut *cell))
            .unwrap_or_default();
        let result = result
            .map(|mut done| {
                done.usage.add_assign(&sampled);
                done
            })
            .map_err(|mut failure| {
                failure.usage.add_assign(&sampled);
                failure
            });
        let result = if let (Some(run), Some(recorder)) =
            (self.task_run.as_deref(), self.task_terminal.take())
        {
            super::terminal::finish(
                result,
                &self.deps.task_service,
                run,
                self.input.turn,
                &self.deps.cancel,
                &recorder,
                &self.ui_sink,
            )
        } else {
            result
        };
        if let Ok(done) = &result
            && !done.cancelled
            && self.deps.titler.is_some()
            && let Some(sender) = &self.deps.title_sender
        {
            let _ = sender.try_send(AutotitleJob {
                session_id: self.input.session_id.clone(),
                config: self.deps.config.clone(),
                credentials: self.deps.credentials.clone(),
            });
        }
        let result = self.close_target(result);
        let result = result
            .map(|mut done| {
                done.receipt = self.input.receipt.clone();
                done
            })
            .map_err(|mut failure| {
                failure.receipt = self.input.receipt.clone();
                failure
            });
        self.deps.busy.store(false, Ordering::Release);
        let _ = self.deps.completion.send(result);
    }
}

impl RunWorker {
    fn close_target(&self, result: ApplicationRunResult) -> ApplicationRunResult {
        match (
            result,
            self.deps.target.as_ref().map_or(Ok(()), |t| t.close()),
        ) {
            (result, Ok(())) => result,
            (Ok(done), Err(error)) => Err(ApplicationRunFailure {
                receipt: done.receipt,
                error: format!("target cleanup failed: {error}"),
                turns: done.turns,
                usage: done.usage,
            }),
            (Err(mut failure), Err(error)) => {
                failure
                    .error
                    .push_str(&format!("; target cleanup failed: {error}"));
                Err(failure)
            }
        }
    }
}
