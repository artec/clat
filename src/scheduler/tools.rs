//! Models may propose drafts; confirmation capabilities never enter tool output.
use super::*;
use crate::session::use_cases::SessionService;
use crate::{CancelToken, Project, Tool, ToolDefinition, ToolEffect, ToolError};
use serde_json::{Value, json};
#[derive(Clone)]
pub(super) struct Scope {
    pub scheduler: Arc<SchedulerService>,
    pub sessions: Arc<SessionService>,
    pub config: Arc<dyn crate::plugins::services::ConfigStore>,
    pub project: String,
}
#[derive(Clone, Copy)]
enum Operation {
    Create,
    List,
    Update,
    Delete,
}
struct ScheduleTool {
    scope: Scope,
    operation: Operation,
    bound: Option<Option<String>>,
}
pub(super) fn tools(scope: Scope) -> Vec<Arc<dyn Tool>> {
    [
        Operation::Create,
        Operation::List,
        Operation::Update,
        Operation::Delete,
    ]
    .into_iter()
    .map(|operation| {
        Arc::new(ScheduleTool {
            scope: scope.clone(),
            operation,
            bound: None,
        }) as Arc<dyn Tool>
    })
    .collect()
}
impl ScheduleTool {
    fn run(&self, args: &Value, cancel: &CancelToken) -> Result<Value, ScheduleError> {
        if cancel.is_cancelled() {
            return Err(ScheduleError::new("cancelled"));
        }
        let session = self
            .bound
            .clone()
            .unwrap_or_else(|| self.scope.sessions.active_id().map(|s| s.to_string()))
            .ok_or_else(|| ScheduleError::new("session_unavailable"))?;
        if self
            .scope
            .sessions
            .active_id()
            .is_none_or(|s| s.as_str() != session)
        {
            return Err(ScheduleError::new("session_unavailable"));
        }
        let project = &self.scope.project;
        let rules = self.scope.scheduler.list(project)?;
        let mut visible: Vec<_> = rules
            .into_iter()
            .filter(|r| r.scope.session == session)
            .collect();
        for rule in &mut visible {
            rule.review = None;
        }
        if matches!(self.operation, Operation::List) {
            if args.as_object().is_none_or(|o| !o.is_empty()) {
                return Err(ScheduleError::new("invalid_request"));
            }
            return Ok(model_list(visible));
        }
        let (id, revision, draft) = self.parse_edit(args)?;
        if let Some(id) = &id
            && !visible.iter().any(|r| r.id == *id)
        {
            return Err(ScheduleError::new("not_found"));
        }
        let Some(draft) = draft else {
            self.scope
                .scheduler
                .delete(project, id.as_deref().unwrap(), revision)?;
            return Ok(json!({"deleted":true}));
        };
        let (config, _) = model::load(self.scope.config.as_ref(), draft.model_profile.as_deref())?;
        let spec = WorkSpec {
            title: None,
            notification: None,
            instruction: draft.instruction,
            trigger: Trigger::Time {
                rule: super::schema::parse_time(draft.time)?,
            },
            budget: draft.budget,
            model: model::approved(&config, draft.model_profile),
        };
        let rule = match id {
            Some(id) => self.scope.scheduler.edit(project, &id, revision, spec)?,
            None => self.scope.scheduler.create(
                WorkScope {
                    project: project.clone(),
                    session,
                },
                spec,
            )?,
        };
        Ok(
            json!({"id":rule.id,"revision":rule.revision,"phase":rule.phase,"trigger":rule.spec.trigger,"budget":rule.spec.budget,"requires_user_confirmation":true}),
        )
    }
    fn parse_edit(
        &self,
        args: &Value,
    ) -> Result<(Option<String>, u64, Option<Draft>), ScheduleError> {
        let parsed = match self.operation {
            Operation::Create => (
                None,
                0,
                Some(
                    serde_json::from_value::<Draft>(args.clone())
                        .map_err(|_| ScheduleError::new("invalid_request"))?,
                ),
            ),
            Operation::Update => {
                let edit: Edit = serde_json::from_value(args.clone())
                    .map_err(|_| ScheduleError::new("invalid_request"))?;
                (Some(edit.id), edit.revision, Some(edit.draft))
            }
            Operation::Delete => {
                let edit: Identity = serde_json::from_value(args.clone())
                    .map_err(|_| ScheduleError::new("invalid_request"))?;
                (Some(edit.id), edit.revision, None)
            }
            Operation::List => unreachable!(),
        };
        Ok(parsed)
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Draft {
    instruction: String,
    time: Value,
    budget: WorkBudget,
    model_profile: Option<String>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Edit {
    id: String,
    revision: u64,
    draft: Draft,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Identity {
    id: String,
    revision: u64,
}
impl Tool for ScheduleTool {
    fn capability_availability(&self) -> Option<(&'static str, &'static str)> {
        Some(if self.scope.scheduler.list(&self.scope.project).is_ok() {
            ("available", "host_owned_draft_authorization")
        } else {
            ("unavailable", "scheduler_control_invalid")
        })
    }
    fn definition(&self) -> ToolDefinition {
        let draft = super::schema::draft();
        let (name, description, schema, effect) = match self.operation {
            Operation::Create => (
                "schedule_create",
                "Propose a schedule draft for this session. Does not enable automatic execution; user must review exact instruction/time/model/budget in CLAT. No cron or subagent scheduling.",
                draft,
                ToolEffect::Write,
            ),
            Operation::List => (
                "schedule_list",
                "List this session's schedules and blocked states. Does not expose confirmation capabilities or other sessions.",
                json!({"type":"object","properties":{},"additionalProperties":false}),
                ToolEffect::Read,
            ),
            Operation::Update => (
                "schedule_update",
                "Replace this session's schedule with a draft. Revokes automatic authorization; user must confirm again. Revision is required.",
                json!({"type":"object","properties":{"id":{"type":"string"},"revision":{"type":"integer"},"draft":draft},"required":["id","revision","draft"],"additionalProperties":false}),
                ToolEffect::Write,
            ),
            Operation::Delete => (
                "schedule_delete",
                "Delete this session's schedule. Already admitted work may still finish; deletion never replays work.",
                json!({"type":"object","properties":{"id":{"type":"string"},"revision":{"type":"integer"}},"required":["id","revision"],"additionalProperties":false}),
                ToolEffect::Write,
            ),
        };
        ToolDefinition {
            name: name.into(),
            description: description.into(),
            input_schema: schema,
            effect,
            strict: true,
        }
    }
    fn prepare_execution(&self, _: &Value) -> Option<crate::tool::PreparedTool> {
        Some(crate::tool::PreparedTool {
            workspace_enforced: false,
            status: "host-schedule-control".into(),
            tool: Arc::new(Self {
                scope: self.scope.clone(),
                operation: self.operation,
                bound: Some(self.scope.sessions.active_id().map(|s| s.to_string())),
            }),
        })
    }
    fn invoke(&self, args: &Value, _: &Project, cancel: &CancelToken) -> Result<Value, ToolError> {
        self.run(args, cancel).map_err(|e| {
            ToolError::new(e.presentation().english()).with_recovery(
                json!({"presentation":e.presentation(),"code":format!("schedule.{}",e.code)}),
            )
        })
    }
    fn error_output(&self, error: &ToolError) -> Value {
        error
            .recovery()
            .cloned()
            .unwrap_or_else(|| json!({"error":error.to_string()}))
    }
}
fn model_list(rules: Vec<Rule>) -> Value {
    let count = rules.len();
    let mut rows = vec![];
    for r in rules {
        let row = json!({"id":r.id,"revision":r.revision,"phase":r.phase,"trigger":r.spec.trigger,"next_at":r.next_at,"used":r.used,"blocked":r.blocked});
        if serde_json::to_string(&rows).unwrap().len() + row.to_string().len() > 6000 {
            break;
        }
        rows.push(row);
    }
    json!({"truncated":rows.len()<count,"total":count,"rules":rows})
}
