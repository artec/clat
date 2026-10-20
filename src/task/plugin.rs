use super::*;
use crate::command::{CommandError, CommandHandler, CommandOutcome, CommandSpec};
use crate::plugin::*;
use crate::plugins::services::*;
use crate::{CancelToken, Project, Tool, ToolDefinition, ToolEffect, ToolError};
use serde::Deserialize;
use serde_json::{Value, json};
use std::sync::Arc;
const TASK_ID: ServiceId = ServiceId::new("core.task");
pub(crate) const TASK_SERVICE: ServiceKey<TaskService> = ServiceKey::new(TASK_ID);
const DESCRIPTOR: PluginDescriptor = PluginDescriptor {
    id: PluginId::new("builtin.task"),
    scope: ScopeKind::TrustedProject,
    provides: &[TASK_ID],
    requires: &[
        SESSION_SERVICE_ID,
        TOOL_SERVICE_ID,
        COMMAND_SERVICE_ID,
        TOOL_PIPELINE_SERVICE_ID,
    ],
    optional: &[],
};
pub(crate) struct TaskPlugin(pub(crate) Project);
impl Plugin for TaskPlugin {
    fn descriptor(&self) -> &'static PluginDescriptor {
        &DESCRIPTOR
    }
    fn mount(&self, context: &mut PluginContext<'_>) -> Result<(), PluginError> {
        let service = Arc::new(
            TaskService::new(
                context.require(SESSION_SERVICE).map_err(pe)?,
                Project::new(self.0.root().canonicalize().map_err(pe)?),
            )
            .map_err(pe)?,
        );
        let tools = context.require(TOOL_SERVICE).map_err(pe)?;
        for read in [true, false] {
            let lease = tools
                .register(
                    context.owner(),
                    Arc::new(ProgressTool {
                        service: service.clone(),
                        read,
                    }),
                )
                .map_err(pe)?;
            context.defer(move || lease.revoke().map_err(|e| DisposeError::new(e.to_string())));
        }
        let pipeline = context.require(TOOL_PIPELINE_SERVICE).map_err(pe)?;
        let lease = pipeline
            .register_middleware(context.owner(), service.clone())
            .map_err(pe)?;
        context.defer(move || lease.revoke().map_err(|e| DisposeError::new(e.to_string())));
        let commands = context.require(COMMAND_SERVICE).map_err(pe)?;
        let lease = commands
            .register(
                context.owner(),
                CommandSpec {
                    names: vec!["task".into()],
                    description: "inspect, checkpoint and explicitly resume one durable task"
                        .into(),
                    takes_args: true,
                    group: crate::command::CommandGroup::Conversation,
                    order: 18,
                    listed: true,
                    handler: Arc::new(TaskCommand),
                },
            )
            .map_err(pe)?;
        context.defer(move || lease.revoke().map_err(|e| DisposeError::new(e.to_string())));
        context.provide(TASK_SERVICE, service).map_err(pe)
    }
}
fn pe(e: impl ToString) -> PluginError {
    PluginError::new(e.to_string())
}
struct ProgressTool {
    service: Arc<TaskService>,
    read: bool,
}
impl Tool for ProgressTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: if self.read { "read_task" } else { "update_task" }.into(),
            description: if self.read { "Read current durable task revision, operation evidence and budget. Progress claims are not verified completion." }
                else { "Save a complete task checkpoint using task_id and current expected_revision from read_task. Progress fields: summary, steps, questions, next, artifacts [{path, summary, digest (SHA256), evidence (confirmed operation ids)}], jobs [{provider, scope, id, result_ref, last_observation}], required_tools. No raw outputs or secrets. Cannot create/complete tasks or resolve uncertain operations." }.into(),
            effect: ToolEffect::SessionWrite, strict: false,
            input_schema: if self.read { json!({"type":"object","properties":{},"additionalProperties":false}) }
                else { json!({"type":"object","properties":{"task_id":{"type":"string"},"expected_revision":{"type":"integer"},"progress":{"type":"object"}},"required":["task_id","expected_revision","progress"],"additionalProperties":false}) },
        }
    }
    fn invoke(
        &self,
        args: &Value,
        _project: &Project,
        cancel: &CancelToken,
    ) -> Result<Value, ToolError> {
        if cancel.is_cancelled() {
            return Err(ToolError::new("task checkpoint cancelled"));
        }
        let s = self
            .service
            .current()
            .map_err(ToolError::new)?
            .ok_or_else(|| ToolError::new("no current task"))?;
        self.service
            .require_binding(&s.id)
            .map_err(ToolError::new)?;
        if self.read {
            return serde_json::to_value(s).map_err(|e| ToolError::new(e.to_string()));
        }
        let id = args["task_id"]
            .as_str()
            .ok_or_else(|| ToolError::new("task_id required"))?;
        let rev = args["expected_revision"]
            .as_u64()
            .ok_or_else(|| ToolError::new("expected_revision required"))?;
        let p: Progress = serde_json::from_value(args["progress"].clone())
            .map_err(|e| ToolError::new(e.to_string()))?;
        let next = self
            .service
            .save(id, rev, p, true)
            .map_err(ToolError::new)?;
        serde_json::to_value(next).map_err(|e| ToolError::new(e.to_string()))
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CreateTask {
    pub objective: String,
    pub acceptance: Vec<String>,
    pub budget: TaskBudget,
}
struct TaskCommand;
impl CommandHandler for TaskCommand {
    fn run(
        &self,
        app: &mut crate::application::TrustedProjectApplication,
        args: &str,
    ) -> Result<CommandOutcome, CommandError> {
        if args.len() > 131072 {
            return Err(failed("task command too large"));
        }
        let (verb, rest) = args
            .trim()
            .split_once(char::is_whitespace)
            .unwrap_or((args.trim(), ""));
        let rest = rest.trim();
        match verb {
            "" | "show" => app
                .task_state()
                .map(|s| CommandOutcome::ShowTask(pretty(&s)))
                .map_err(failed),
            "check" => app
                .task_check()
                .map(|c| CommandOutcome::ShowTask(pretty(&c)))
                .map_err(failed),
            "create" => {
                let p: CreateTask = serde_json::from_str(rest).map_err(failed)?;
                app.task_create(p.objective, p.acceptance, p.budget)
                    .map(|s| CommandOutcome::ShowTask(pretty(&s)))
                    .map_err(failed)
            }
            "save" => {
                let (id, rev, body) = identity(rest)?;
                let p: Progress = serde_json::from_str(body).map_err(failed)?;
                app.task_save(id, rev, p)
                    .map(|s| CommandOutcome::ShowTask(pretty(&s)))
                    .map_err(failed)
            }
            "continue" => {
                let (id, rev, body) = identity(rest)?;
                let (digest, ack) = body.split_once(char::is_whitespace).unwrap_or((body, ""));
                if !matches!(ack.trim(), "" | "--accept-changes") {
                    return Err(failed("unexpected continuation argument"));
                }
                let request = TaskResume {
                    task_id: id.into(),
                    revision: rev,
                    check_digest: digest.into(),
                    accept_changes: ack.trim() == "--accept-changes",
                };
                app.task_validate_resume(&request).map_err(failed)?;
                Ok(CommandOutcome::StartTaskRun(request))
            }
            "pause" => {
                app.task_pause().map_err(failed)?;
                Ok(CommandOutcome::Status(
                    "Task paused; last committed checkpoint retained.".into(),
                ))
            }
            "complete" | "cancel" => {
                let (id, rev, _) = identity(rest)?;
                app.task_finish(
                    id,
                    rev,
                    if verb == "complete" {
                        "complete"
                    } else {
                        "cancelled"
                    },
                )
                .map(|s| CommandOutcome::ShowTask(pretty(&s)))
                .map_err(failed)
            }
            "resolve" => resolve(app, rest),
            _ => Err(failed(
                "/task show|check|create <JSON>|save <id> <revision> <JSON>|continue <id> <revision> <check_digest> [--accept-changes]|pause|complete <id> <revision>|cancel <id> <revision>|resolve <id> <revision> <operation> confirmed|not_executed <note>",
            )),
        }
    }
}
fn resolve(
    app: &mut crate::application::TrustedProjectApplication,
    rest: &str,
) -> Result<CommandOutcome, CommandError> {
    let (id, rev, body) = identity(rest)?;
    let mut words = body.splitn(3, ' ');
    let op = words.next().unwrap_or("");
    let result = words.next().unwrap_or("");
    let note = words.next().unwrap_or("");
    app.task_resolve(id, rev, op, result, note)
        .map(|s| CommandOutcome::ShowTask(pretty(&s)))
        .map_err(failed)
}
fn identity(rest: &str) -> Result<(&str, u64, &str), CommandError> {
    let mut parts = rest.splitn(3, ' ');
    let id = parts.next().unwrap_or("");
    let rev = parts.next().unwrap_or("").parse().map_err(failed)?;
    Ok((id, rev, parts.next().unwrap_or("").trim()))
}
fn pretty(s: &impl serde::Serialize) -> String {
    serde_json::to_string_pretty(s).unwrap_or_else(|e| e.to_string())
}
fn failed(e: impl ToString) -> CommandError {
    CommandError::Failed {
        message: e.to_string(),
    }
}
