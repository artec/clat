use super::*;
use crate::event::{EventSink, RunEvent};
use crate::model::{ModelItem, ModelOptions, RunSpendLedger, estimate_request_tokens};
use crate::permission::AllowAll;
use crate::tool::{ToolAccessPolicy, ToolDefinition};
use crate::{Run, RunError, RunOutput};

impl SubagentService {
    pub(super) fn run_one(
        self: &Arc<Self>,
        binding: RunBinding,
        id: String,
        input_digest: String,
        task: SubagentTask,
        parent_cancel: &CancelToken,
    ) -> Result<SubagentResult, String> {
        let deadline = Instant::now() + Duration::from_secs(task.timeout_secs);
        let cancel = parent_cancel.child_with_deadline(deadline);
        let _worker = self.register_worker(id.clone(), cancel.clone())?;
        let started = Instant::now();
        let definitions: Arc<[ToolDefinition]> = self
            .tools
            .definitions_for(&ToolAccessPolicy::readonly_child())
            .into();
        let items = vec![ModelItem::user_text(child_prompt(&task))];
        let input = estimate_request_tokens(Some(task.role.instructions()), &items, &definitions);
        let budget = Arc::new(RunSpendLedger::new(Some(task.max_tokens)));
        let mut events = ChildEvents::default();
        let (output, usage, stop_reason) = if input >= task.max_tokens {
            let message = format!(
                "child request estimate {input} reaches token cap {}; narrow the task or increase delegate_readonly max_tokens",
                task.max_tokens
            );
            (message, Usage::default(), "error")
        } else {
            self.execute_child(
                &binding,
                &task,
                ChildInput { items, definitions },
                &cancel,
                &budget,
                &mut events,
            )
        };
        // A provider panic can unwind between reserve and settlement.
        budget.commit_pending();
        let output = truncate_utf8(&output, MAX_CHILD_OUTPUT_BYTES);
        let result = SubagentResult {
            id,
            role: task.role,
            output_digest: sha256(output.as_bytes()),
            output,
            stop_reason: stop_reason.into(),
            usage,
            charged_tokens: budget.committed(),
            elapsed_ms: started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64,
            tools: events.tools.into_iter().collect(),
            input_digest,
        };
        binding.journal.append(
            NewSessionEvent::new("clat/subagent", lifecycle_end(&result, &binding, &task))
                .log_only(),
        )?;
        binding.journal.flush()?;
        Ok(result)
    }

    fn execute_child(
        &self,
        binding: &RunBinding,
        task: &SubagentTask,
        input: ChildInput,
        cancel: &CancelToken,
        ledger: &Arc<RunSpendLedger>,
        events: &mut ChildEvents,
    ) -> (String, Usage, &'static str) {
        let config = super::budget::child_model_config(binding.config.clone());
        let options = ModelOptions {
            output_limit: config.output_limit,
            temperature: config.temperature,
            parallel_tool_calls: Some(false),
            ..ModelOptions::default()
        };
        let providers = Arc::clone(&self.providers);
        let ledger = Arc::clone(ledger);
        let credentials = binding.credentials.clone();
        let capabilities = config.capabilities.clone();
        let mut model = crate::providers::retry_model(
            config.protocol.to_string(),
            config.model.clone(),
            Box::new(move || {
                let model = providers.build(&config, &credentials)?;
                Ok(Box::new(super::budget::BudgetedModel::new(
                    model,
                    Arc::clone(&ledger),
                )))
            }),
        );
        let mut run = Run::new(model.as_mut(), &self.tools, &AllowAll, &self.project)
            .with_model_options(options)
            .with_cancel_token(cancel.clone())
            .with_tool_access(ToolAccessPolicy::readonly_child())
            .with_tool_definitions(input.definitions)
            .with_instructions(task.role.instructions())
            .with_capabilities(capabilities)
            // OpenCode assigns a child's requests to the parent's gateway session.
            .with_session_id(Some(binding.session.to_string()));
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            run.execute_with_items(
                input.items,
                crate::message::MessageContent::text(task.task.clone()),
                None,
                events,
            )
        }));
        child_outcome(outcome, cancel.is_cancelled() || events.cancelled)
    }
}

struct ChildInput {
    items: Vec<ModelItem>,
    definitions: Arc<[ToolDefinition]>,
}

fn child_outcome(
    outcome: std::thread::Result<Result<RunOutput, RunError>>,
    cancelled: bool,
) -> (String, Usage, &'static str) {
    match outcome {
        Ok(Ok(done)) => (
            done.text,
            done.usage,
            if cancelled { "aborted" } else { "completed" },
        ),
        Ok(Err(error)) => {
            let (message, _, usage, _) = error.into_parts();
            (message, usage, if cancelled { "aborted" } else { "error" })
        }
        Err(payload) => (
            format!(
                "subagent panicked: {}",
                crate::plugin::panic_message(payload)
            ),
            Usage::default(),
            "error",
        ),
    }
}

#[derive(Default)]
struct ChildEvents {
    tools: BTreeSet<String>,
    cancelled: bool,
}

impl EventSink for ChildEvents {
    fn emit(&mut self, event: RunEvent) {
        match event {
            RunEvent::ToolStarted { tool, .. } => {
                self.tools.insert(tool);
            }
            RunEvent::RunCancelled { .. } => self.cancelled = true,
            _ => {}
        }
    }
}
