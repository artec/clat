use super::*;
use crate::process::BackgroundPlan;
use crate::{PreparedTool, Project, Tool, ToolCapability, ToolDefinition, ToolEffect, ToolError};
#[derive(Clone, Copy)]
pub(super) enum Operation {
    Start,
    Output,
    List,
    Kill,
}
pub(super) struct JobTool {
    pub registry: Arc<JobRegistry>,
    pub process: Arc<crate::process::ProcessService>,
    pub operation: Operation,
}
struct FrozenStart {
    source: JobTool,
    args: Value,
    plan: Mutex<Option<Result<(BackgroundPlan, SessionId), ToolError>>>,
}
impl JobTool {
    fn execute(&self, args: &Value, cancel: &CancelToken) -> Result<Value, ToolError> {
        if cancel.is_cancelled() {
            return Err(JobError::new("cancelled").tool());
        }
        match self.operation {
            Operation::Start => {
                self.prepare_execution(args)
                    .unwrap()
                    .tool
                    .invoke(args, &Project::new("."), cancel)
            }
            Operation::List => {
                keys(args, &[])?;
                Ok(model_view::list(
                    self.registry.try_list().map_err(|e| e.tool())?,
                ))
            }
            Operation::Output => {
                keys(args, &["job_id"])?;
                self.registry
                    .agent_output(id(args)?)
                    .map(model_view::page)
                    .map_err(|e| e.tool())
            }
            Operation::Kill => {
                keys(args, &["job_id"])?;
                self.registry
                    .kill(id(args)?)
                    .map(|o| json!({"job":model_view::job(&o),"stop_requested":true}))
                    .map_err(|e| e.tool())
            }
        }
    }
}
impl Tool for JobTool {
    fn definition(&self) -> ToolDefinition {
        let (name, description, properties, required, effect) = match self.operation {
            Operation::Start => (
                "job_start",
                "Start a non-interactive Host-owned background command after normal permission approval. Exactly one of cmd or argv. It survives this conversation turn and frontend disconnection. Use project files or CLAT_JOB_SCRATCH for temporary work, never the current Run scratch. Completion is not proof of acceptance; inspect the actual exit observation. Continuation requires separate explicit user authorization and budget.",
                json!({"label":{"type":"string","minLength":1,"maxLength":120},"cmd":{"type":"string"},"argv":{"type":"array","items":{"type":"string"}},"workdir":{"type":"string"},"timeout_ms":{"type":"integer","minimum":100,"maximum":86400000},"network":{"type":"boolean"},"sandbox":{"type":"string","enum":["auto","required","off"]},"verification":{"type":"object"}}),
                vec!["label"],
                ToolEffect::Execute,
            ),
            Operation::Output => (
                "job_output",
                "Read bounded redacted output and the actual state of a job in this project. Has an Agent-only output cursor; user reads do not consume it. Lost output is explicit. A stop request is not a terminal observation.",
                json!({"job_id":{"type":"string"}}),
                vec!["job_id"],
                ToolEffect::Read,
            ),
            Operation::List => (
                "job_list",
                "List bounded native jobs of this project, including original session and actual terminal observations. Does not adopt or restart external jobs.",
                json!({}),
                vec![],
                ToolEffect::Read,
            ),
            Operation::Kill => (
                "job_kill",
                "Request cancellation of a job in this project through normal execution permission. Poll its actual state until it is terminal; this call never claims that a request already stopped the process.",
                json!({"job_id":{"type":"string"}}),
                vec!["job_id"],
                ToolEffect::Execute,
            ),
        };
        ToolDefinition {
            name: name.into(),
            description: description.into(),
            input_schema: json!({"type":"object","properties":properties,"required":required,"additionalProperties":false}),
            effect,
            strict: true,
        }
    }
    fn capability_availability(&self) -> Option<(&'static str, &'static str)> {
        Some(if self.registry.ready().is_ok() {
            ("available", "host_owned")
        } else {
            ("unavailable", "recovery_failed")
        })
    }
    fn capability_metadata(&self) -> ToolCapability {
        ToolCapability::native(
            &["job", "background", "command", "后台"],
            &["current-project"],
            &["job-reference", "process-exit"],
            "Non-interactive Host lifetime. No replay after restart. Separate user consent and budget required for continuation.",
        )
    }
    fn prepare_execution(&self, args: &Value) -> Option<PreparedTool> {
        if !matches!(self.operation, Operation::Start) {
            return None;
        }
        let plan = validate_start(args)
            .and_then(|request| {
                crate::plugins::process::background_plan(
                    &self.process,
                    &request.command_arguments(),
                )
            })
            .and_then(|plan| {
                self.registry
                    .binding()
                    .map(|id| (plan, id))
                    .ok_or_else(|| JobError::new("unbound").tool())
            });
        Some(PreparedTool {
            workspace_enforced: false,
            status: "host-background-command".into(),
            tool: Arc::new(FrozenStart {
                source: Self {
                    registry: self.registry.clone(),
                    process: self.process.clone(),
                    operation: self.operation,
                },
                args: args.clone(),
                plan: Mutex::new(Some(plan)),
            }),
        })
    }
    fn invoke(&self, args: &Value, _: &Project, cancel: &CancelToken) -> Result<Value, ToolError> {
        self.execute(args, cancel)
    }
    fn error_output(&self, error: &ToolError) -> Value {
        error_output(error)
    }
    fn journal_arguments(&self, args: &Value) -> Value {
        redact(args)
    }
    fn journal_output(&self, result: &Value) -> Value {
        let mut result = redact(result);
        if let Some(object) = result.as_object_mut() {
            object.remove("text");
            object.insert("output_omitted_from_journal".into(), json!(true));
        }
        result
    }
}
impl Tool for FrozenStart {
    fn definition(&self) -> ToolDefinition {
        self.source.definition()
    }
    fn invoke(&self, args: &Value, _: &Project, cancel: &CancelToken) -> Result<Value, ToolError> {
        if args != &self.args {
            return Err(JobError::new("invalid_request").tool());
        }
        let (plan, expected) = self
            .plan
            .lock()
            .map_err(|_| JobError::new("unavailable").tool())?
            .take()
            .ok_or_else(|| JobError::new("consumed").tool())??;
        let request = validate_start(args)?;
        self.source
            .registry
            .start(
                self.source.process.clone(),
                plan,
                expected,
                crate::redact::redact_secrets(&request.label),
                Duration::from_millis(request.timeout_ms),
                cancel,
            )
            .map(|job| json!({"job":model_view::job(&job)}))
            .map_err(|e| e.tool())
    }
    fn journal_arguments(&self, args: &Value) -> Value {
        self.source.journal_arguments(args)
    }
    fn journal_output(&self, value: &Value) -> Value {
        self.source.journal_output(value)
    }
    fn error_output(&self, error: &ToolError) -> Value {
        error_output(error)
    }
}
fn validate_start(args: &Value) -> Result<JobStartRequest, ToolError> {
    let request: JobStartRequest = serde_json::from_value(args.clone())
        .map_err(|_| JobError::new("invalid_request").tool())?;
    if request.label.trim().is_empty()
        || request.label.chars().count() > 120
        || request.label.chars().any(char::is_control)
        || !(100..=86400000).contains(&request.timeout_ms)
    {
        return Err(JobError::new("invalid_request").tool());
    }
    Ok(request)
}
fn keys(args: &Value, allowed: &[&str]) -> Result<(), ToolError> {
    if args
        .as_object()
        .is_none_or(|o| o.keys().any(|k| !allowed.contains(&k.as_str())))
    {
        Err(JobError::new("invalid_request").tool())
    } else {
        Ok(())
    }
}
fn id(args: &Value) -> Result<&str, ToolError> {
    args["job_id"]
        .as_str()
        .filter(|s| uuid::Uuid::parse_str(s).is_ok())
        .ok_or_else(|| JobError::new("invalid_request").tool())
}
fn error_output(error: &ToolError) -> Value {
    let mut out = json!({"error":error.to_string()});
    if let Some(fields) = error.recovery().and_then(Value::as_object) {
        out.as_object_mut().unwrap().extend(fields.clone());
    }
    out
}
fn redact(value: &Value) -> Value {
    match value {
        Value::String(s) => json!(crate::redact::redact_secrets(s)),
        Value::Array(a) => Value::Array(a.iter().map(redact).collect()),
        Value::Object(o) => Value::Object(
            o.iter()
                .map(|(k, v)| {
                    (
                        k.clone(),
                        if crate::redact::credential_name(k) {
                            json!("[REDACTED]")
                        } else {
                            redact(v)
                        },
                    )
                })
                .collect(),
        ),
        _ => value.clone(),
    }
}
