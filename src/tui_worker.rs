use crate::permission::{InteractivePermissionPolicy, PermissionDecision, PermissionRequest};
use crate::providers::ProviderRuntime;
use crate::{
    CancelToken, EventSink, ModelConfig, ModelItem, ModelOptions, Project, Run, RunEvent,
    RunOutput, SafeByDefault, ToolRegistry, Usage, register_native_read_tools,
};
use std::sync::Mutex;
use std::sync::mpsc::{self, Sender};

pub(crate) fn execute_run(
    project: Project,
    config: ModelConfig,
    provider_runtime: ProviderRuntime,
    history_items: Vec<ModelItem>,
    prompt: String,
    sender: Sender<WorkerMessage>,
    cancel: CancelToken,
) -> Result<RunDone, String> {
    let mut model = provider_runtime
        .build_model(&config)
        .map_err(|error| error.to_string())?;
    let mut tools = ToolRegistry::new();
    register_native_read_tools(&mut tools);

    // Side-effecting tools are classified by SafeByDefault and then resolved
    // interactively: the policy posts the request to the UI and blocks until
    // the user answers. If the UI goes away, the recv fails and the call is
    // denied so the model can adapt instead of hanging.
    let (decision_tx, decision_rx) = mpsc::channel();
    let decision_rx = Mutex::new(decision_rx);
    let ask_sender = sender.clone();
    let ask = move |request: PermissionRequest| {
        let _ = ask_sender.send(WorkerMessage::PermissionRequest {
            request,
            decision_tx: decision_tx.clone(),
        });
        decision_rx
            .lock()
            .ok()
            .and_then(|receiver| receiver.recv().ok())
            .unwrap_or_else(|| PermissionDecision::Deny {
                reason: "no permission decision available".into(),
            })
    };
    let permissions = InteractivePermissionPolicy::new(SafeByDefault, Box::new(ask));

    let options = ModelOptions {
        output_limit: config.output_limit,
        temperature: config.temperature,
        parallel_tool_calls: Some(config.parallel_tool_calls),
        ..ModelOptions::default()
    };
    let mut sink = ChannelEventSink(sender);
    let history_len = history_items.len();
    let output = Run::new(model.as_mut(), &tools, &permissions, &project)
        .with_model_options(options)
        .with_cancel_token(cancel.clone())
        .with_instructions(
            "You are CLAT, a command line agent operating on the current project. Use project tools to inspect real files when needed. Use project-relative paths and recover from tool errors instead of guessing.",
        )
        .execute_with_items(history_items, prompt, &mut sink)
        .map_err(|error| error.to_string())?;
    let RunOutput {
        text,
        turns,
        items,
        usage,
        ..
    } = output;
    Ok(RunDone {
        output: text,
        turns,
        usage,
        cancelled: cancel.is_cancelled(),
        // Everything the run appended after the supplied history is new
        // conversation context for the UI to persist.
        new_items: items.into_iter().skip(history_len).collect(),
    })
}

struct ChannelEventSink(Sender<WorkerMessage>);

impl EventSink for ChannelEventSink {
    fn emit(&mut self, event: RunEvent) {
        let _ = self.0.send(WorkerMessage::Event(event));
    }
}

pub(crate) enum WorkerMessage {
    Event(RunEvent),
    PermissionRequest {
        request: PermissionRequest,
        decision_tx: Sender<PermissionDecision>,
    },
    Done(Result<RunDone, String>),
}

pub(crate) struct RunDone {
    pub output: String,
    pub turns: usize,
    pub usage: Usage,
    pub cancelled: bool,
    pub new_items: Vec<ModelItem>,
}
