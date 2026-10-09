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
    plan: std::sync::Mutex<Option<Result<Option<crate::process::PreparedProcess>, String>>>,
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
) -> Result<Option<crate::process::PreparedProcess>, String> {
    if matches!(source, NativeProcessTool::WriteStdin(_)) {
        return Ok(None);
    }
    let key = if matches!(source, NativeProcessTool::RunCommand(_)) {
        "command"
    } else {
        "cmd"
    };
    let command = arguments
        .get(key)
        .and_then(Value::as_str)
        .ok_or("missing command")?;
    let request = ProcessStart {
        command: command.into(),
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
        sandbox: SandboxRequest::parse(arguments.get("sandbox").and_then(Value::as_str))?,
    };
    source.service().prepare_start(&request).map(Some)
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
    fn invoke(&self, a: &Value, p: &Project, c: &CancelToken) -> Result<Value, ToolError> {
        if a != &self.arguments {
            return Err(ToolError::new("prepared arguments changed"));
        }
        let plan = self
            .plan
            .lock()
            .expect("prepared plan")
            .take()
            .ok_or_else(|| ToolError::new("prepared invocation already consumed"))?
            .map_err(ToolError::new)?;
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
