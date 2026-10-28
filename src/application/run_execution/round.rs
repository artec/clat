//! One round owns bindings, spend reservation, invocation and durable settlement.
use super::*;

pub(super) struct RoundExecution {
    process_generation: Result<u64, String>,
    file_capture_error: Option<String>,
    subagent_round_error: Option<String>,
    recorder: Arc<Mutex<SessionRecorder>>,
    approver: Arc<dyn PermissionApprover>,
    spend_ledger: Arc<crate::model::RunSpendLedger>,
}
struct ExecutionOutcome {
    outcome: Option<Result<crate::RunOutput, AgentFailure>>,
    panic_text: Option<String>,
    process_bind_error: Option<String>,
    process_cleanup_error: Option<String>,
    cancelled: bool,
}
/// A settled round keeps its resources alive until accounting has consumed them.
pub(super) struct SettledRound {
    pub(super) result: ApplicationRunResult,
    pub(super) cancelled: bool,
    execution: RoundExecution,
}
impl SettledRound {
    pub(super) fn spent_tokens(&self) -> u64 {
        self.execution.spend_ledger.used()
    }
}
impl RoundExecution {
    pub(super) fn execute(
        worker: &mut RunWorker,
        context: super::super::run_context::RoundContext,
    ) -> SettledRound {
        let execution = Self::begin(worker, context.header, context.reason);
        let outcome = execution.invoke(worker, context.workflow, context.error);
        if let Some(target) = &worker.deps.target {
            target.meter.store(
                execution.spend_ledger.used(),
                std::sync::atomic::Ordering::Release,
            );
        }
        let cancelled = outcome.cancelled;
        let result = execution.finish(worker, outcome);
        SettledRound {
            result,
            cancelled,
            execution,
        }
    }

    fn begin(worker: &RunWorker, header: RequestHeaderData, reason: Option<&'static str>) -> Self {
        let (process_generation, file_capture_error) = bind_round_services(
            &worker.deps.process_service,
            &worker.deps.slots,
            worker.input.session_id.as_str(),
            worker.input.turn,
            &worker.deps.cancel,
        );
        let (mut recorder, approver) = SessionRecorder::with_approver(
            Arc::clone(&worker.input.journal),
            Arc::clone(&worker.deps.approver),
            header,
            &worker.deps.config.protocol.to_string(),
            &worker.deps.config.model,
            worker.input.turn,
            reason,
        );
        recorder.attach_aux_usage(Arc::clone(&worker.deps.sampling_usage));
        let configured = worker.deps.config.effective_run_token_budget();
        let budget = if worker.goal_mode {
            let remaining = worker.deps.goal_service.remaining_tokens().unwrap_or(1);
            Some(configured.map_or(remaining, |cap| cap.min(remaining)))
        } else {
            configured
        };
        let budget = match worker.deps.task_service.cap() {
            Some(cap) => Some(budget.map_or(cap, |b| b.min(cap))),
            None => budget,
        };
        let spend_ledger = Arc::new(crate::model::RunSpendLedger::new(budget));
        let subagent_round_error = worker
            .deps
            .subagent_service
            .begin_round(Arc::clone(&spend_ledger))
            .err();
        recorder.set_run_ledger(Arc::clone(&spend_ledger));
        Self {
            process_generation,
            file_capture_error,
            subagent_round_error,
            recorder: Arc::new(Mutex::new(recorder)),
            spend_ledger,
            approver: Arc::new(approver),
        }
    }
    fn invoke(
        &self,
        worker: &RunWorker,
        workflow: Option<String>,
        goal_error: Option<String>,
    ) -> ExecutionOutcome {
        let execution = self.execute_agent(worker, workflow, goal_error);
        let process_bind_error = self.process_generation.as_ref().err().cloned();
        let process_cleanup_error = self
            .process_generation
            .as_ref()
            .ok()
            .and_then(|generation| worker.deps.process_service.unbind_run(*generation).err());
        if let Some(error) = process_bind_error
            .as_ref()
            .or(process_cleanup_error.as_ref())
        {
            self.recorder
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .force_terminal_failure(format!(
                    "process lifecycle failed before run completion: {error}"
                ));
        }
        let (outcome, panic_text) = match execution {
            Ok(outcome) => (Some(outcome), None),
            Err(payload) => (
                None,
                Some(format!(
                    "{}\npartial output: {}",
                    panic_message(payload),
                    worker
                        .captured_text
                        .lock()
                        .map(|text| text.clone())
                        .unwrap_or_default()
                )),
            ),
        };
        let cancelled = worker.deps.cancel.is_cancelled()
            || outcome
                .as_ref()
                .is_some_and(|result| result.as_ref().is_ok_and(|done| done.cancelled));
        ExecutionOutcome {
            outcome,
            panic_text,
            process_bind_error,
            process_cleanup_error,
            cancelled,
        }
    }
    fn execute_agent(
        &self,
        worker: &RunWorker,
        workflow: Option<String>,
        goal_error: Option<String>,
    ) -> std::thread::Result<Result<crate::RunOutput, AgentFailure>> {
        let recorder_sink = Box::new(RecorderHandle {
            inputs: worker.deps.steering.inputs().clone(),
            drafts: worker.deps.draft_images.clone(),
            recorder: Arc::clone(&self.recorder),
            sink: Arc::clone(&worker.ui_sink),
        });
        catch_unwind(AssertUnwindSafe(|| {
            check_round_bindings(
                goal_error.as_ref(),
                worker.subagent_bind_error.as_ref(),
                self.subagent_round_error.as_ref(),
                self.process_generation.as_ref().err(),
                self.file_capture_error.as_ref(),
            )?;
            worker.deps.agent.execute(AgentRequest {
                runtime: worker.deps.target.as_ref().map(|t| t.runtime.clone()),
                context_manager: self.context_manager(worker),
                config: worker.deps.config.clone(),
                spend_ledger: Some(Arc::clone(&self.spend_ledger)),
                credentials: worker.deps.credentials.clone(),
                history_items: worker.input.history.clone(),
                message: worker.input.message.clone(),
                client_message_id: worker.input.client_message_id.clone(),
                cancel: worker.deps.cancel.clone(),
                steering: worker.deps.steering.clone(),
                approver: Arc::clone(&self.approver),
                events: recorder_sink,
                tool_access: worker.context.tool_access.clone(),
                tool_definitions: Arc::clone(&worker.context.tool_definitions),
                workflow_instructions: worker.deps.process_service.workflow(workflow),
                permission_mode: worker.deps.permission_mode_snapshot,
            })
        }))
    }
    fn context_manager(
        &self,
        worker: &RunWorker,
    ) -> Option<Box<dyn crate::run::RequestContextManager>> {
        Some(Box::new(super::super::compaction::ActiveContext {
            compactor: worker.deps.compactor.clone()?,
            sessions: Arc::clone(&worker.deps.sessions),
            journal: Arc::clone(&worker.input.journal),
            recorder: Arc::clone(&self.recorder),
            config: worker.deps.config.clone(),
            credentials: worker.deps.credentials.clone(),
            cancel: worker.deps.cancel.clone(),
            turn: worker.input.turn,
            subscribers: Arc::clone(&worker.deps.subscribers),
            ledger: Arc::clone(&self.spend_ledger),
            checkpoints: Default::default(),
        }))
    }
    fn finish(&self, worker: &mut RunWorker, outcome: ExecutionOutcome) -> ApplicationRunResult {
        let task_error = worker
            .deps
            .task_service
            .settle(self.spend_ledger.used())
            .err()
            .map(|e| format!("task progress settlement failed: {e}"));
        if let Some(error) = &task_error {
            self.recorder
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .force_terminal_failure(error.clone());
        }
        if worker.task_run.is_some() {
            let error = self
                .recorder
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .prepare_terminal(outcome.cancelled);
            worker.task_terminal = Some(Arc::clone(&self.recorder));
            return outcome.into_result(
                error
                    .map(|e| format!("session journal failed: {e}"))
                    .or(task_error),
            );
        }
        let (finish_error, published) = self
            .recorder
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .finish(outcome.reason());
        let journal_error = finish_error.map(|error| format!("session journal failed: {error}"));
        for event in published {
            let mut sink = worker
                .ui_sink
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let forwarded = catch_unwind(AssertUnwindSafe(|| sink.emit(event)));
            if forwarded.is_err() {
                eprintln!(
                    "clat: warning: frontend event sink panicked while publishing the terminal event"
                );
            }
        }
        let _ = worker.deps.sessions.sync_active();
        if let Some(header) = worker.deps.sessions.last_request_header() {
            worker.durable_request_header.header = header;
        }
        outcome.into_result(journal_error.or(task_error))
    }
}
impl ExecutionOutcome {
    fn reason(&self) -> TurnEndReason {
        match (
            &self.outcome,
            &self.process_bind_error,
            &self.process_cleanup_error,
        ) {
            (_, Some(error), _) => TurnEndReason::Error {
                error: json!({"message":format!("process bind failed: {error}")}),
            },
            (_, None, Some(error)) => TurnEndReason::Error {
                error: json!({"message":format!("process cleanup failed: {error}")}),
            },
            (Some(Ok(_)), None, None) if self.cancelled => TurnEndReason::Aborted {
                reason: TurnEndCancelCause::User,
            },
            (Some(Ok(_)), None, None) => TurnEndReason::Completed,
            (Some(Err(failure)), None, None) => TurnEndReason::Error {
                error: json!({"message":failure.error.to_string()}),
            },
            (None, None, None) => TurnEndReason::Error {
                error: json!({"message":"run worker panicked"}),
            },
        }
    }
    fn into_result(self, journal_error: Option<String>) -> ApplicationRunResult {
        let Some(result) = self.outcome else {
            return Err(ApplicationRunFailure {
                receipt: None,
                error: match (self.panic_text, journal_error) {
                    (Some(text), Some(error)) => format!("{text}; {error}"),
                    (Some(text), None) => text,
                    (None, Some(error)) => error,
                    (None, None) => self
                        .process_cleanup_error
                        .map(|error| format!("process cleanup failed: {error}"))
                        .unwrap_or_else(|| "run worker panicked".into()),
                },
                turns: 0,
                usage: Usage::default(),
            });
        };
        let base = result
            .map(|done| ApplicationRunDone {
                receipt: None,
                output: done.text,
                turns: done.turns,
                usage: done.usage,
                cancelled: self.cancelled,
            })
            .map_err(|failure| {
                let (message, turns, usage, _) = failure.error.into_parts();
                ApplicationRunFailure {
                    receipt: None,
                    error: message,
                    turns,
                    usage,
                }
            });
        let base = match (base, self.process_cleanup_error) {
            (Ok(done), Some(error)) => Err(ApplicationRunFailure {
                receipt: None,
                error: format!("process cleanup failed: {error}"),
                turns: done.turns,
                usage: done.usage,
            }),
            (base, _) => base,
        };
        merge_terminal(base, journal_error, self.panic_text)
    }
}
fn merge_terminal(
    base: ApplicationRunResult,
    journal_error: Option<String>,
    panic_text: Option<String>,
) -> ApplicationRunResult {
    match (base, journal_error, panic_text) {
        (base, None, None) => base,
        (Ok(done), Some(error), _) => Err(ApplicationRunFailure {
            receipt: None,
            error,
            turns: done.turns,
            usage: done.usage,
        }),
        (Ok(done), None, Some(text)) => Err(ApplicationRunFailure {
            receipt: None,
            error: format!("{text} (run had completed: {})", done.output),
            turns: done.turns,
            usage: done.usage,
        }),
        (Err(failure), Some(error), _) => Err(ApplicationRunFailure {
            receipt: None,
            error: format!("{}; {error}", failure.error),
            turns: failure.turns,
            usage: failure.usage,
        }),
        (Err(failure), None, Some(text)) => Err(ApplicationRunFailure {
            receipt: None,
            error: format!("{text}; {}", failure.error),
            turns: failure.turns,
            usage: failure.usage,
        }),
    }
}
