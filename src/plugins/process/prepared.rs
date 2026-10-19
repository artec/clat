use super::*;

pub(super) enum NativeProcessTool {
    ExecCommand(ExecCommandTool),
    RunCommand(RunCommandTool),
    WriteStdin(WriteStdinTool),
}
struct FrozenProcessTool {
    source: NativeProcessTool,
    arguments: Value,
    stdin: Option<crate::process::PreparedStdin>,
    plan: std::sync::Mutex<Option<Result<Option<crate::process::PreparedProcess>, ToolError>>>,
}
impl NativeProcessTool {
    fn tool(&self) -> &dyn Tool {
        match self {
            Self::ExecCommand(t) => t,
            Self::RunCommand(t) => t,
            Self::WriteStdin(t) => t,
        }
    }
    fn service(&self) -> &Arc<ProcessService> {
        match self {
            Self::ExecCommand(t) => &t.service,
            Self::RunCommand(t) => &t.service,
            Self::WriteStdin(t) => &t.service,
        }
    }
}
pub(super) fn prepare_native(
    source: NativeProcessTool,
    arguments: &Value,
) -> crate::tool::PreparedTool {
    let plan = prepare_native_plan(&source, arguments);
    let stdin = if matches!(&source, NativeProcessTool::WriteStdin(_)) {
        arguments
            .get("session_id")
            .and_then(Value::as_u64)
            .and_then(|id| source.service().prepare_stdin(id).ok())
    } else {
        None
    };
    let workspace_enforced = stdin
        .as_ref()
        .is_some_and(|s| s.eligible && enforced_workspace(&s.facts))
        || plan
            .as_ref()
            .ok()
            .and_then(|p| p.as_ref())
            .is_some_and(|p| {
                enforced_workspace(&p.planned.facts)
                    && arguments.get("network").and_then(Value::as_bool) != Some(true)
            });
    let status = match &plan {
        Ok(Some(p)) => format!(
            "provider={}, enforcement={}, mode={}, network={}",
            p.planned.facts.provider,
            p.planned.facts.enforcement,
            p.planned.facts.mode.as_str(),
            arguments
                .get("network")
                .and_then(Value::as_bool)
                .unwrap_or(false)
        ),
        Ok(None) => stdin.as_ref().map_or_else(
            || "stdin session not found in active run".into(),
            |s| {
                format!(
                    "stdin provider={}, enforcement={}, mode={}, network_eligible={}",
                    s.facts.provider,
                    s.facts.enforcement,
                    s.facts.mode.as_str(),
                    s.eligible
                )
            },
        ),
        Err(e) => format!("enforcement unavailable: {e}"),
    };
    crate::tool::PreparedTool {
        workspace_enforced,
        status,
        tool: Arc::new(FrozenProcessTool {
            source,
            stdin,
            arguments: arguments.clone(),
            plan: std::sync::Mutex::new(Some(plan)),
        }),
    }
}
fn prepare_native_plan(
    source: &NativeProcessTool,
    arguments: &Value,
) -> Result<Option<crate::process::PreparedProcess>, ToolError> {
    if matches!(source, NativeProcessTool::WriteStdin(_)) {
        return Ok(None);
    }
    if matches!(source, NativeProcessTool::RunCommand(_)) {
        evidence::request(arguments)?;
    }
    let invocation = if matches!(source, NativeProcessTool::ExecCommand(_)) {
        verification::Invocation::parse(arguments)
    } else {
        verification::Invocation::compat(arguments)
    }?;
    let request = ProcessStart {
        command: invocation.command,
        workdir: arguments
            .get("workdir")
            .and_then(Value::as_str)
            .map(str::to_owned),
        tty: arguments
            .get("tty")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        network: arguments
            .get("network")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        sandbox: SandboxRequest::parse(arguments.get("sandbox").and_then(Value::as_str))
            .map_err(ToolError::new)?,
    };
    let stdin_pipe = if matches!(source, NativeProcessTool::ExecCommand(_)) {
        super::stdin::pipe(arguments)?
    } else {
        false
    };
    match invocation.argv {
        Some(argv) => source
            .service()
            .prepare_direct(&request, argv, invocation.verification),
        None => source.service().prepare_start(&request),
    }
    .map_err(process_error)
    .and_then(|plan| {
        if arguments.get("evidence").is_some()
            && plan.planned.facts.mode == crate::sandbox::SandboxLevel::ReadOnly
        {
            return Err(ToolError::new(
                "evidence recording requires a writable project mode",
            ));
        }
        Ok(Some(plan.with_stdin_pipe(stdin_pipe)))
    })
}
impl Tool for FrozenProcessTool {
    fn definition(&self) -> ToolDefinition {
        self.source.tool().definition()
    }
    fn journal_arguments(&self, a: &Value) -> Value {
        self.source.tool().journal_arguments(a)
    }
    fn journal_output(&self, a: &Value) -> Value {
        self.source.tool().journal_output(a)
    }
    fn error_output(&self, error: &ToolError) -> Value {
        self.source.tool().error_output(error)
    }
    fn invoke(&self, a: &Value, p: &Project, c: &CancelToken) -> Result<Value, ToolError> {
        if a != &self.arguments {
            return Err(ToolError::new("prepared arguments changed"));
        }
        let plan = self
            .plan
            .lock()
            .expect("prepared plan")
            .take()
            .ok_or_else(|| ToolError::new("prepared invocation already consumed"))??;
        match &self.source {
            NativeProcessTool::ExecCommand(t) => t.invoke_planned(a, p, c, plan),
            NativeProcessTool::RunCommand(t) => t.invoke_planned(a, p, c, plan),
            NativeProcessTool::WriteStdin(t) => t.invoke_planned(a, p, c, self.stdin.as_ref()),
        }
    }
}

pub(super) fn enforced_workspace(f: &crate::sandbox::SandboxFacts) -> bool {
    f.mode == crate::sandbox::SandboxLevel::WorkspaceWrite
        && ((f.provider == "seatbelt" && f.enforcement == "full")
            || (f.provider == "windows-acl" && f.enforcement == "partial"))
}

// Only adapters translate domain identity into tool diagnostics.
pub(super) fn process_error(error: crate::process::ProcessError) -> ToolError {
    use crate::process::ProcessError;
    let recovery = match &error {
        ProcessError::WorkdirOutOfScope => Some(json!({"code":"workdir_out_of_scope"})),
        ProcessError::SessionUnavailable(_) => Some(json!({"code":"session_unavailable"})),
        ProcessError::Other(_) => None,
    };
    let result = ToolError::new(error.to_string());
    match recovery {
        Some(recovery) => result.with_recovery(recovery),
        None => result,
    }
}
