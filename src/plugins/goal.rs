use super::services::{
    COMMAND_SERVICE, COMMAND_SERVICE_ID, GOAL_SERVICE, GOAL_SERVICE_ID, SESSION_SERVICE,
    SESSION_SERVICE_ID, TOOL_SERVICE, TOOL_SERVICE_ID,
};
use crate::application::TrustedProjectApplication;
use crate::command::{CommandError, CommandHandler, CommandOutcome, CommandSpec};
use crate::goal::{GoalAcceptance, GoalLimits, GoalService};
use crate::plugin::{
    DisposeError, Plugin, PluginContext, PluginDescriptor, PluginError, PluginId, ScopeKind,
    ServiceId,
};
use crate::{CancelToken, Project, Tool, ToolDefinition, ToolEffect, ToolError};
use serde_json::{Value, json};
use std::path::PathBuf;
use std::sync::Arc;

const ID: PluginId = PluginId::new("builtin.goal");
const REQUIRES: &[ServiceId] = &[SESSION_SERVICE_ID, TOOL_SERVICE_ID, COMMAND_SERVICE_ID];
const DESCRIPTOR: PluginDescriptor = PluginDescriptor {
    id: ID,
    scope: ScopeKind::TrustedProject,
    provides: &[GOAL_SERVICE_ID],
    requires: REQUIRES,
    optional: &[],
};

pub(crate) struct GoalPlugin {
    project_root: PathBuf,
}

impl GoalPlugin {
    pub(crate) fn new(project_root: PathBuf) -> Self {
        Self { project_root }
    }
}

impl Plugin for GoalPlugin {
    fn descriptor(&self) -> &'static PluginDescriptor {
        &DESCRIPTOR
    }

    fn mount(&self, context: &mut PluginContext<'_>) -> Result<(), PluginError> {
        let sessions = context
            .require(SESSION_SERVICE)
            .map_err(|error| PluginError::new(error.to_string()))?;
        let tools = context
            .require(TOOL_SERVICE)
            .map_err(|error| PluginError::new(error.to_string()))?;
        let commands = context
            .require(COMMAND_SERVICE)
            .map_err(|error| PluginError::new(error.to_string()))?;
        let service = Arc::new(GoalService::new(sessions, self.project_root.clone()));
        let tool_lease = tools
            .register(
                context.owner(),
                Arc::new(UpdateGoalTool {
                    service: Arc::clone(&service),
                }),
            )
            .map_err(|error| PluginError::new(error.to_string()))?;
        context.defer(move || {
            tool_lease
                .revoke()
                .map_err(|error| DisposeError::new(error.to_string()))
        });
        let command_lease = commands
            .register(
                context.owner(),
                CommandSpec {
                    names: vec!["goal".into()],
                    description: "create, inspect, control, or explicitly run one bounded goal"
                        .into(),
                    takes_args: true,
                    group: crate::command::CommandGroup::Experiments,
                    order: 12,
                    listed: true,
                    handler: Arc::new(GoalCommand),
                },
            )
            .map_err(|error| PluginError::new(error.to_string()))?;
        context.defer(move || {
            command_lease
                .revoke()
                .map_err(|error| DisposeError::new(error.to_string()))
        });
        context
            .provide(GOAL_SERVICE, service)
            .map_err(|error| PluginError::new(error.to_string()))
    }
}

struct UpdateGoalTool {
    service: Arc<GoalService>,
}

impl Tool for UpdateGoalTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "update_goal".into(),
            description: "Update the current durable goal using revision/CAS. The model may record progress, report a blocker, or submit a completion candidate. Completion succeeds only when the pre-registered read-only verifier passes; user-only goals require /goal complete."
                .into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "operation": {"type": "string", "enum": ["progress", "blocked", "complete"]},
                    "expected_revision": {"type": "integer", "minimum": 1},
                    "summary": {"type": "string", "minLength": 1, "maxLength": 16384},
                    "code": {"type": "string", "pattern": "^[a-z][a-z0-9]*(?:-[a-z0-9]+)*$"}
                },
                "required": ["operation", "expected_revision", "summary"],
                "additionalProperties": false
            }),
            effect: ToolEffect::SessionWrite,
            strict: true,
        }
    }

    fn invoke(
        &self,
        arguments: &Value,
        _project: &Project,
        _cancel: &CancelToken,
    ) -> Result<Value, ToolError> {
        let operation = arguments
            .get("operation")
            .and_then(Value::as_str)
            .ok_or_else(|| ToolError::new("update_goal requires operation"))?;
        let revision = arguments
            .get("expected_revision")
            .and_then(Value::as_u64)
            .ok_or_else(|| ToolError::new("update_goal requires expected_revision"))?;
        let summary = arguments
            .get("summary")
            .and_then(Value::as_str)
            .ok_or_else(|| ToolError::new("update_goal requires summary"))?;
        let view = match operation {
            "progress" => self.service.update_progress(revision, summary),
            "blocked" => self.service.block(
                revision,
                arguments
                    .get("code")
                    .and_then(Value::as_str)
                    .unwrap_or("blocked"),
                summary,
            ),
            "complete" => self.service.complete_candidate(revision, summary),
            _ => return Err(ToolError::new("invalid update_goal operation")),
        }
        .map_err(ToolError::new)?;
        Ok(goal_json(&view))
    }
}

struct GoalCommand;

impl CommandHandler for GoalCommand {
    fn run(
        &self,
        application: &mut TrustedProjectApplication,
        args: &str,
    ) -> Result<CommandOutcome, CommandError> {
        let (verb, rest) = split_once(args);
        match verb {
            "" | "show" => application
                .goal_overview()
                .map(CommandOutcome::ShowGoal)
                .map_err(failed),
            "create" => {
                let parsed = parse_create(rest)?;
                let view = application
                    .goal_create(
                        &parsed.objective,
                        parsed.acceptance,
                        parsed.limits,
                        parsed.run,
                    )
                    .map_err(failed)?;
                if parsed.run {
                    Ok(CommandOutcome::StartGoalRun {
                        message: application.goal_run_message(),
                    })
                } else {
                    Ok(CommandOutcome::Status(format!(
                        "Goal {} created at revision {} (disarmed). Use /goal run to start.",
                        view.state.id, view.state.revision
                    )))
                }
            }
            "run" => {
                let view = application
                    .goal()
                    .map_err(failed)?
                    .ok_or_else(|| failed("no current goal"))?;
                application.goal_arm(view.state.revision).map_err(failed)?;
                Ok(CommandOutcome::StartGoalRun {
                    message: application.goal_run_message(),
                })
            }
            "pause" => {
                let view = current(application)?;
                let view = application
                    .goal_pause(view.state.revision)
                    .map_err(failed)?;
                Ok(CommandOutcome::Status(render_goal(&view)))
            }
            "resume" => {
                let view = current(application)?;
                let view = application
                    .goal_resume(view.state.revision)
                    .map_err(failed)?;
                Ok(CommandOutcome::Status(render_goal(&view)))
            }
            "complete" => {
                let view = current(application)?;
                let summary = if rest.is_empty() {
                    "Completed by user."
                } else {
                    rest
                };
                let view = application
                    .goal_complete(view.state.revision, summary)
                    .map_err(failed)?;
                Ok(CommandOutcome::Status(render_goal(&view)))
            }
            "cancel" | "clear" => {
                let view = current(application)?;
                application
                    .goal_clear(view.state.revision)
                    .map_err(failed)?;
                Ok(CommandOutcome::Status("Goal cleared and disarmed.".into()))
            }
            _ => Err(usage()),
        }
    }
}

struct CreateArgs {
    objective: String,
    acceptance: GoalAcceptance,
    limits: GoalLimits,
    run: bool,
}

fn parse_create(value: &str) -> Result<CreateArgs, CommandError> {
    let mut words = value.split_whitespace().peekable();
    let mut objective = Vec::new();
    let mut acceptance = GoalAcceptance::User;
    let mut limits = GoalLimits::default();
    let mut run = false;
    while let Some(word) = words.next() {
        if !word.starts_with("--") {
            objective.push(word);
            continue;
        }
        match word {
            "--run" => run = true,
            "--rounds" => limits.max_rounds = parse_next(&mut words, "--rounds")?,
            "--tokens" => limits.max_tokens = parse_next(&mut words, "--tokens")?,
            "--seconds" => limits.max_time_secs = parse_next(&mut words, "--seconds")?,
            "--failures" => limits.max_failures = parse_next(&mut words, "--failures")?,
            "--accept" => {
                let spec = words.next().ok_or_else(usage)?;
                acceptance = parse_acceptance(spec)?;
            }
            _ => return Err(usage()),
        }
    }
    let objective = objective.join(" ");
    if objective.is_empty() {
        return Err(usage());
    }
    Ok(CreateArgs {
        objective,
        acceptance,
        limits,
        run,
    })
}

fn parse_next<T: std::str::FromStr>(
    words: &mut std::iter::Peekable<std::str::SplitWhitespace<'_>>,
    _name: &str,
) -> Result<T, CommandError> {
    words.next().ok_or_else(usage)?.parse().map_err(|_| usage())
}

fn parse_acceptance(value: &str) -> Result<GoalAcceptance, CommandError> {
    if value == "user" {
        return Ok(GoalAcceptance::User);
    }
    if let Some(path) = value.strip_prefix("file-exists:") {
        return Ok(GoalAcceptance::FileExists { path: path.into() });
    }
    if let Some(value) = value.strip_prefix("file-contains:")
        && let Some((path, text)) = value.split_once(':')
    {
        return Ok(GoalAcceptance::FileContains {
            path: path.into(),
            text: text.into(),
        });
    }
    Err(usage())
}

fn current(application: &TrustedProjectApplication) -> Result<crate::goal::GoalView, CommandError> {
    application
        .goal()
        .map_err(failed)?
        .ok_or_else(|| failed("no current goal"))
}

fn split_once(value: &str) -> (&str, &str) {
    let value = value.trim();
    value
        .split_once(char::is_whitespace)
        .map(|(head, tail)| (head, tail.trim()))
        .unwrap_or((value, ""))
}

fn goal_json(view: &crate::goal::GoalView) -> Value {
    json!({
        "ok": true,
        "goal": view.state,
        "activation": if view.armed { "armed" } else { "disarmed" },
    })
}

fn render_goal(view: &crate::goal::GoalView) -> String {
    let state = &view.state;
    format!(
        "{} rev={} phase={} activation={} rounds={}/{} tokens={}/{} failures={}/{} elapsed={}ms/{}ms\n{}{}",
        state.id,
        state.revision,
        state.phase.as_str(),
        if view.armed { "armed" } else { "disarmed" },
        state.rounds_started,
        state.limits.max_rounds,
        state.tokens_used,
        state.limits.max_tokens,
        state.failures,
        state.limits.max_failures,
        state.elapsed_ms,
        state.limits.max_time_secs.saturating_mul(1000),
        state.objective,
        state
            .blocked_reason
            .as_ref()
            .map(|reason| format!("\nblocked {}: {}", reason.code, reason.message))
            .unwrap_or_default(),
    )
}

fn usage() -> CommandError {
    CommandError::Failed {
        message: "usage: /goal show | create <objective> [--run] [--rounds N] [--tokens N] [--seconds N] [--failures N] [--accept user|file-exists:path|file-contains:path:text] | run | pause | resume | complete [summary] | cancel".into(),
    }
}

fn failed(error: impl ToString) -> CommandError {
    CommandError::Failed {
        message: error.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugin::PluginManager;
    use crate::plugins::{CommandsPlugin, SessionPersistencePlugin, ToolRegistryPlugin};
    use crate::session::persistence::JsonlCompression;

    #[test]
    fn create_parser_keeps_bounds_and_acceptance_explicit() {
        let parsed = parse_create(
            "ship release --rounds 3 --tokens 5000 --seconds 60 --failures 2 --accept file-exists:done.txt --run",
        )
        .unwrap();
        assert_eq!(parsed.objective, "ship release");
        assert_eq!(parsed.limits.max_rounds, 3);
        assert!(parsed.run);
        assert_eq!(
            parsed.acceptance,
            GoalAcceptance::FileExists {
                path: "done.txt".into()
            }
        );
    }

    #[test]
    fn catalog_omission_and_scope_close_remove_goal_surfaces() {
        let (storage, project) = crate::test_support::roots("goal-plugin-lifecycle");
        std::fs::create_dir_all(&storage).unwrap();
        std::fs::create_dir_all(&project).unwrap();
        let sessions = Arc::new(
            crate::session::use_cases::SessionService::new(
                storage.join("sessions"),
                JsonlCompression::Zstd,
            )
            .unwrap(),
        );
        let foundations = || {
            vec![
                Arc::new(SessionPersistencePlugin::new(Arc::clone(&sessions))) as Arc<dyn Plugin>,
                Arc::new(ToolRegistryPlugin),
                Arc::new(CommandsPlugin),
            ]
        };
        let mut without = PluginManager::root(ScopeKind::TrustedProject);
        without.mount_all(foundations()).unwrap();
        let tools = without.require(TOOL_SERVICE).unwrap();
        let commands = without.require(COMMAND_SERVICE).unwrap();
        assert!(tools.get("update_goal").is_none());
        assert!(commands.lookup("goal").is_none());
        without.close().unwrap();

        let mut with = PluginManager::root(ScopeKind::TrustedProject);
        let mut catalog = foundations();
        catalog.push(Arc::new(GoalPlugin::new(project.clone())));
        with.mount_all(catalog).unwrap();
        let tools = with.require(TOOL_SERVICE).unwrap();
        let commands = with.require(COMMAND_SERVICE).unwrap();
        assert!(tools.get("update_goal").is_some());
        assert!(commands.lookup("goal").is_some());
        with.close().unwrap();
        assert!(tools.get("update_goal").is_none());
        assert!(commands.lookup("goal").is_none());
        crate::test_support::cleanup_tree(storage.parent().unwrap());
    }
}
