//! Process, sandbox, and model-facing exec tools.

use super::services::{
    PROCESS_SERVICE, PROCESS_SERVICE_ID, SANDBOX_SERVICE, SANDBOX_SERVICE_ID, TOOL_SERVICE,
    TOOL_SERVICE_ID,
};
use crate::permission::PermissionMode;
use crate::plugin::{
    DisposeError, Plugin, PluginContext, PluginDescriptor, PluginError, PluginId, ScopeKind,
    ServiceId,
};
use crate::process::{MAX_STDIN_WRITE_BYTES, ProcessOutput, ProcessService, ProcessStart};
use crate::sandbox::{SandboxModeSource, SandboxRequest, SandboxService};
use crate::{CancelToken, Project, Tool, ToolDefinition, ToolEffect, ToolError};
use serde_json::{Value, json};
use std::sync::{Arc, RwLock};
use std::time::Duration;
mod discrimination;
mod one_shot;
#[cfg(test)]
mod outcome_tests;
mod prepared;
#[cfg(test)]
mod sbx4_tests;
mod stdin;
mod verification;
#[cfg(all(test, feature = "runtime-tests"))]
mod verification_tests;
#[cfg(test)]
mod witness_tests;
use prepared::{NativeProcessTool, prepare_native};

const SANDBOX_ID: PluginId = PluginId::new("builtin.sandbox.policy");
const PROCESS_ID: PluginId = PluginId::new("builtin.process");
const TOOLS_ID: PluginId = PluginId::new("builtin.exec_tools");
const MAX_COMMAND_BYTES: usize = 64 * 1024;
const SANDBOX_PROVIDES: &[ServiceId] = &[SANDBOX_SERVICE_ID];
const PROCESS_PROVIDES: &[ServiceId] = &[PROCESS_SERVICE_ID];
const PROCESS_REQUIRES: &[ServiceId] = &[SANDBOX_SERVICE_ID];
const TOOLS_REQUIRES: &[ServiceId] = &[PROCESS_SERVICE_ID, TOOL_SERVICE_ID];
const SANDBOX_DESCRIPTOR: PluginDescriptor = PluginDescriptor {
    id: SANDBOX_ID,
    scope: ScopeKind::TrustedProject,
    provides: SANDBOX_PROVIDES,
    requires: &[],
    optional: &[],
};
const PROCESS_DESCRIPTOR: PluginDescriptor = PluginDescriptor {
    id: PROCESS_ID,
    scope: ScopeKind::TrustedProject,
    provides: PROCESS_PROVIDES,
    requires: PROCESS_REQUIRES,
    optional: &[],
};
const TOOLS_DESCRIPTOR: PluginDescriptor = PluginDescriptor {
    id: TOOLS_ID,
    scope: ScopeKind::TrustedProject,
    provides: &[],
    requires: TOOLS_REQUIRES,
    optional: &[],
};

pub(crate) struct SandboxPlugin {
    pub(crate) project: Project,
    pub(crate) permission_mode: Option<Arc<RwLock<PermissionMode>>>,
}

impl Plugin for SandboxPlugin {
    fn descriptor(&self) -> &'static PluginDescriptor {
        &SANDBOX_DESCRIPTOR
    }

    fn mount(&self, context: &mut PluginContext<'_>) -> Result<(), PluginError> {
        let mode = self
            .permission_mode
            .as_ref()
            .map_or(SandboxModeSource::Classic, |mode| {
                SandboxModeSource::Shared(Arc::clone(mode))
            });
        let service = Arc::new(
            SandboxService::for_project(self.project.clone(), mode).map_err(PluginError::new)?,
        );
        context
            .provide(SANDBOX_SERVICE, service)
            .map_err(|error| PluginError::new(error.to_string()))
    }
}

pub(crate) struct ProcessServicePlugin {
    pub(crate) project: Project,
}

impl Plugin for ProcessServicePlugin {
    fn descriptor(&self) -> &'static PluginDescriptor {
        &PROCESS_DESCRIPTOR
    }

    fn mount(&self, context: &mut PluginContext<'_>) -> Result<(), PluginError> {
        let sandbox = context
            .require(SANDBOX_SERVICE)
            .map_err(|error| PluginError::new(error.to_string()))?;
        let service = Arc::new(ProcessService::new(self.project.clone(), sandbox));
        let close = Arc::clone(&service);
        context.defer(move || close.close().map_err(DisposeError::new));
        context
            .provide(PROCESS_SERVICE, service)
            .map_err(|error| PluginError::new(error.to_string()))
    }
}

pub(crate) struct ExecToolsPlugin;

impl Plugin for ExecToolsPlugin {
    fn descriptor(&self) -> &'static PluginDescriptor {
        &TOOLS_DESCRIPTOR
    }

    fn mount(&self, context: &mut PluginContext<'_>) -> Result<(), PluginError> {
        let service = context
            .require(PROCESS_SERVICE)
            .map_err(|error| PluginError::new(error.to_string()))?;
        let tools = context
            .require(TOOL_SERVICE)
            .map_err(|error| PluginError::new(error.to_string()))?;
        for tool in process_tools(service) {
            let lease = tools
                .register(context.owner(), tool)
                .map_err(|error| PluginError::new(error.to_string()))?;
            context.defer(move || {
                lease
                    .revoke()
                    .map_err(|error| DisposeError::new(error.to_string()))
            });
        }
        Ok(())
    }
}

fn process_tools(service: Arc<ProcessService>) -> Vec<Arc<dyn Tool>> {
    vec![
        Arc::new(RunCommandTool {
            service: Arc::clone(&service),
        }),
        Arc::new(ExecCommandTool {
            service: Arc::clone(&service),
        }),
        Arc::new(WriteStdinTool {
            service: Arc::clone(&service),
        }),
        Arc::new(discrimination::TestDiscriminationTool { service }),
    ]
}

#[derive(Clone)]
struct ExecCommandTool {
    service: Arc<ProcessService>,
}

impl Tool for ExecCommandTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "exec_command".into(),
            description: "Start a run-owned command session in the project. Specify exactly one of cmd (shell) or argv (direct executable and literal arguments). For test verification prefer argv with verification; its cumulative receipt survives polling and diagnostic clipping. Receipt test counts are unauthenticated output observations, not coverage. Returns immediately when it finishes within yield_time, otherwise returns a session_id for write_stdin polling/input/termination. Non-PTY stdin defaults to closed (EOF); set stdin=pipe explicitly for interactive input. tty=true keeps terminal input and creates a real PTY where supported. Project Write calls with a prepared OS sandbox run without approval; network=true still requires approval.".into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "cmd": {"type": "string", "maxLength": MAX_COMMAND_BYTES},
                    "argv": {"type":"array","minItems":1,"maxItems":128,"items":{"type":"string"},"description":"Direct executable and literal arguments; at most 65536 UTF-8 bytes total, no NUL. Mutually exclusive with cmd."},
                    "verification": {"type":"object","properties":{
                        "format":{"type":"string","enum":["rust-test-summary","playwright-summary"]},
                        "minimum_executed":{"type":"integer","minimum":1,"maximum":1000000}
                    },"required":["format","minimum_executed"],"additionalProperties":false,"description":"Optional bounded cumulative receipt; requires argv and tty=false. Only the directly launched process exit is known."},
                    "workdir": {"type": "string", "description": "Project-relative directory or absolute directory inside the active scratch root"},
                    "tty": {"type": "boolean", "default": false},
                    "stdin": {"type":"string","enum":["closed","pipe"],"description":"Non-PTY commands default to closed stdin (EOF). Use pipe explicitly for write_stdin input. PTYs keep terminal input and cannot use closed."},
                    "yield_time_ms": {"type": "integer", "minimum": 250, "maximum": 30000, "default": 10000},
                    "max_output_tokens": {"type": "integer", "minimum": 256, "maximum": 16000, "default": 10000},
                    "network": {"type": "boolean", "default": false},
                    "sandbox": {"type": "string", "enum": ["auto", "required", "off"], "default": "auto"}
                },
                "required": [],
                "additionalProperties": false
            }),
            effect: ToolEffect::Execute,
            strict: true,
        }
    }

    fn prepare_execution(&self, arguments: &Value) -> Option<crate::tool::PreparedTool> {
        Some(prepare_native(
            NativeProcessTool::ExecCommand(self.clone()),
            arguments,
        ))
    }

    fn invoke(
        &self,
        arguments: &Value,
        project: &Project,
        cancel: &CancelToken,
    ) -> Result<Value, ToolError> {
        let prepared = self
            .prepare_execution(arguments)
            .expect("native preparation");
        prepared.tool.invoke(arguments, project, cancel)
    }

    fn journal_arguments(&self, arguments: &Value) -> Value {
        verification::journal_arguments(arguments)
    }

    fn journal_output(&self, output: &Value) -> Value {
        process_journal_output(output)
    }

    fn error_output(&self, error: &ToolError) -> Value {
        process_error_output(error)
    }
}

#[derive(Clone)]
struct WriteStdinTool {
    service: Arc<ProcessService>,
}

impl Tool for WriteStdinTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "write_stdin".into(),
            description: "Write up to 256 KiB of characters to, poll, close stdin for, or terminate a run-owned exec_command session. Empty chars polls. Unknown IDs return bounded current-run session metadata for recovery; no input is redirected or job restarted. Raw chars are redacted from the durable tool/call journal; sensitive user secrets still must not be routed through a model tool.".into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "session_id": {"type": "integer", "minimum": 1},
                    "chars": {"type": "string", "maxLength": MAX_STDIN_WRITE_BYTES, "default": ""},
                    "close_stdin": {"type": "boolean", "default": false},
                    "terminate": {"type": "boolean", "default": false},
                    "sensitive": {"type": "boolean", "const": false, "default": false},
                    "yield_time_ms": {"type": "integer", "minimum": 250, "maximum": 30000, "default": 250},
                    "max_output_tokens": {"type": "integer", "minimum": 256, "maximum": 16000, "default": 10000}
                },
                "required": ["session_id"],
                "additionalProperties": false
            }),
            effect: ToolEffect::Execute,
            strict: true,
        }
    }

    fn journal_arguments(&self, arguments: &Value) -> Value {
        let chars_bytes = arguments
            .get("chars")
            .and_then(Value::as_str)
            .map_or(0, str::len);
        json!({
            "session_id": arguments.get("session_id").cloned().unwrap_or(Value::Null),
            "chars_bytes": chars_bytes,
            "chars_redacted": true,
            "close_stdin": arguments.get("close_stdin").and_then(Value::as_bool).unwrap_or(false),
            "terminate": arguments.get("terminate").and_then(Value::as_bool).unwrap_or(false),
            "yield_time_ms": arguments.get("yield_time_ms").cloned().unwrap_or(json!(250)),
            "max_output_tokens": arguments.get("max_output_tokens").cloned().unwrap_or(json!(10000))
        })
    }

    fn journal_output(&self, output: &Value) -> Value {
        process_journal_output(output)
    }

    fn error_output(&self, error: &ToolError) -> Value {
        let mut output = process_error_output(error);
        if let Some(recovery) = self.service.recovery_for_error(&error.to_string()) {
            output["process_recovery"] = recovery;
        }
        output
    }

    fn prepare_execution(&self, arguments: &Value) -> Option<crate::tool::PreparedTool> {
        Some(prepare_native(
            NativeProcessTool::WriteStdin(self.clone()),
            arguments,
        ))
    }

    fn invoke(
        &self,
        arguments: &Value,
        project: &Project,
        cancel: &CancelToken,
    ) -> Result<Value, ToolError> {
        let prepared = self
            .prepare_execution(arguments)
            .expect("native preparation");
        prepared.tool.invoke(arguments, project, cancel)
    }
}

#[derive(Clone)]
struct RunCommandTool {
    service: Arc<ProcessService>,
}

impl Tool for RunCommandTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "run_command".into(),
            description: "One-shot command: specify command (shell) OR argv (literal executable/arguments). Prefer argv with verification for tests: child exit and cumulative receipt survive output clipping and journaling; shell pipelines only report the shell exit. workdir is project-relative or active scratch. Waits to completion/timeout with stdin EOF. Use exec_command for interactive or long-running jobs.".into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "command": {"type": "string", "maxLength": MAX_COMMAND_BYTES},
                    "argv": {"type":"array","minItems":1,"maxItems":128,"items":{"type":"string"}},
                    "verification": {"type":"object","properties":{
                        "format":{"type":"string","enum":["rust-test-summary","playwright-summary"]},
                        "minimum_executed":{"type":"integer","minimum":1,"maximum":1000000}
                    },"required":["format","minimum_executed"],"additionalProperties":false},
                    "workdir": {"type":"string"},
                    "timeout_seconds": {"type": "integer", "minimum": 1, "maximum": 600, "default": 120},
                    "network": {"type": "boolean", "default": false},
                    "sandbox": {"type": "string", "enum": ["auto", "required", "off"], "default": "auto"}
                },
                "required": [],
                "additionalProperties": false
            }),
            effect: ToolEffect::Execute,
            strict: true,
        }
    }

    fn prepare_execution(&self, arguments: &Value) -> Option<crate::tool::PreparedTool> {
        Some(prepare_native(
            NativeProcessTool::RunCommand(self.clone()),
            arguments,
        ))
    }

    fn invoke(
        &self,
        arguments: &Value,
        project: &Project,
        cancel: &CancelToken,
    ) -> Result<Value, ToolError> {
        let prepared = self
            .prepare_execution(arguments)
            .expect("native preparation");
        prepared.tool.invoke(arguments, project, cancel)
    }

    fn journal_arguments(&self, arguments: &Value) -> Value {
        bounded_command_journal(&verification::journal_arguments(arguments), "command")
    }

    fn journal_output(&self, output: &Value) -> Value {
        process_journal_output(output)
    }

    fn error_output(&self, error: &ToolError) -> Value {
        process_error_output(error)
    }
}

fn process_error_output(error: &ToolError) -> Value {
    let message = error.to_string();
    json!({
        "command_outcome": crate::process::classify_tool_error(&message).json(),
        "verification_witness": crate::process::verification::unknown(),
        "error": message,
    })
}

fn process_journal_output(output: &Value) -> Value {
    // Tool errors are scalar strings produced by Run, not raw process output.
    // Preserve them so durable replay can explain the same failure the model
    // saw; only successful object results carry transient streams to omit.
    if !output.is_object() || output.get("error").is_some() {
        return output.clone();
    }
    let bytes = |field: &str, count_field: &str| {
        output
            .get(count_field)
            .and_then(Value::as_u64)
            .unwrap_or_else(|| {
                output
                    .get(field)
                    .and_then(Value::as_str)
                    .map_or(0, |text| text.len() as u64)
            })
    };
    let output_truncated = output
        .get("output_truncated")
        .and_then(Value::as_bool)
        .unwrap_or_else(|| {
            ["stdout_truncated", "stderr_truncated", "pty_truncated"]
                .iter()
                .any(|field| output.get(field).and_then(Value::as_bool).unwrap_or(false))
        });
    let mut journal = json!({
        "session_id": output.get("session_id").cloned().unwrap_or(Value::Null),
        "running": output.get("running").cloned().unwrap_or(json!(false)),
        "exit_code": output.get("exit_code").cloned().unwrap_or(Value::Null),
        "signal": output.get("signal").cloned().unwrap_or(Value::Null),
        "timed_out": output.get("timed_out").cloned().unwrap_or(json!(false)),
        "cancelled": output.get("cancelled").cloned().unwrap_or(json!(false)),
        "terminated": output.get("terminated").cloned().unwrap_or(json!(false)),
        "stdout_bytes": bytes("stdout", "stdout_bytes"),
        "stderr_bytes": bytes("stderr", "stderr_bytes"),
        "pty_bytes": bytes("pty", "pty_bytes"),
        "output_truncated": output_truncated,
        "sandbox": output.get("sandbox").cloned().unwrap_or(Value::Null),
        "output_omitted_from_journal": true
    });
    if let Some(outcome) = output.get("command_outcome") {
        journal["command_outcome"] = outcome.clone();
    }
    if let Some(witness) = output.get("verification_witness") {
        journal["verification_witness"] = witness.clone();
    }
    for key in ["execution", "exit_scope"] {
        if let Some(v) = output.get(key) {
            journal[key] = v.clone();
        }
    }
    if let Some(receipt) = output.get("verification_receipt") {
        journal["verification_receipt"] = receipt.clone();
    }
    journal
}

fn bounded_command_journal(arguments: &Value, field: &str) -> Value {
    let Some(command) = arguments.get(field).and_then(Value::as_str) else {
        return arguments.clone();
    };
    if command.len() <= MAX_COMMAND_BYTES {
        return arguments.clone();
    }
    let mut bounded = arguments.clone();
    if let Some(object) = bounded.as_object_mut() {
        object.remove(field);
        object.insert("command_bytes".into(), json!(command.len()));
        object.insert("command_omitted_from_journal".into(), json!(true));
    }
    bounded
}

fn process_output_json(output: ProcessOutput) -> Value {
    let receipt = output.verification_receipt.clone();
    let mut value = json!({
        "verification_witness": output.verification_witness,
        "command_outcome": output.command_outcome.json(),
        "session_id": output.session_id,
        "tty": output.tty,
        "running": output.running,
        "stdout": output.stdout,
        "stderr": output.stderr,
        "pty": output.pty,
        "stdout_bytes": output.stdout_bytes,
        "stderr_bytes": output.stderr_bytes,
        "pty_bytes": output.pty_bytes,
        "stdout_lossy": output.stdout_lossy,
        "stderr_lossy": output.stderr_lossy,
        "pty_lossy": output.pty_lossy,
        "stdout_truncated": output.stdout_truncated,
        "stderr_truncated": output.stderr_truncated,
        "pty_truncated": output.pty_truncated,
        "output_truncated": output.output_truncated,
        "exit_code": output.exit_code,
        "signal": output.signal,
        "timed_out": output.timed_out,
        "cancelled": output.cancelled,
        "terminated": output.terminated,
        "sandbox": output.sandbox.json(output.sandbox_denied, output.sandbox_unavailable)
    });
    value["execution"] = json!(if output.direct { "argv" } else { "shell" });
    value["exit_scope"] = json!(if output.direct {
        "direct_child"
    } else {
        "shell"
    });
    match receipt {
        Some(receipt) => verification::bounded_view(value, receipt),
        None => value,
    }
}

fn output_bytes(arguments: &Value, tool: &str) -> Result<usize, ToolError> {
    let tokens = bounded_u64(arguments, "max_output_tokens", 10_000, 256, 16_000, tool)?;
    Ok((tokens as usize).saturating_mul(4).clamp(1024, 64 * 1024))
}

fn ensure_command_bound(command: &str, tool: &str) -> Result<(), ToolError> {
    if command.len() > MAX_COMMAND_BYTES {
        return Err(ToolError::new(format!(
            "{tool}: command exceeds {MAX_COMMAND_BYTES} UTF-8 bytes; write a temporary script in the supplied scratch directory instead"
        )));
    }
    Ok(())
}

fn ensure_object_keys(arguments: &Value, allowed: &[&str], tool: &str) -> Result<(), ToolError> {
    let object = arguments
        .as_object()
        .ok_or_else(|| ToolError::new(format!("{tool}: arguments must be an object")))?;
    if let Some(key) = object.keys().find(|key| !allowed.contains(&key.as_str())) {
        return Err(ToolError::new(format!("{tool}: unknown argument `{key}`")));
    }
    Ok(())
}

fn optional_string<'a>(
    arguments: &'a Value,
    name: &str,
    tool: &str,
) -> Result<Option<&'a str>, ToolError> {
    match arguments.get(name) {
        None => Ok(None),
        Some(value) => value
            .as_str()
            .map(Some)
            .ok_or_else(|| ToolError::new(format!("{tool}: `{name}` must be a string"))),
    }
}

fn optional_bool(arguments: &Value, name: &str, tool: &str) -> Result<Option<bool>, ToolError> {
    match arguments.get(name) {
        None => Ok(None),
        Some(value) => value
            .as_bool()
            .map(Some)
            .ok_or_else(|| ToolError::new(format!("{tool}: `{name}` must be a boolean"))),
    }
}

fn required_u64(arguments: &Value, name: &str, tool: &str) -> Result<u64, ToolError> {
    arguments
        .get(name)
        .and_then(Value::as_u64)
        .filter(|value| *value > 0)
        .ok_or_else(|| ToolError::new(format!("{tool}: `{name}` must be a positive integer")))
}

fn bounded_u64(
    arguments: &Value,
    name: &str,
    default: u64,
    min: u64,
    max: u64,
    tool: &str,
) -> Result<u64, ToolError> {
    let value = arguments.get(name).map_or(Ok(default), |value| {
        value
            .as_u64()
            .ok_or_else(|| ToolError::new(format!("{tool}: `{name}` must be an integer")))
    })?;
    if !(min..=max).contains(&value) {
        return Err(ToolError::new(format!(
            "{tool}: `{name}` must be {min}..={max}"
        )));
    }
    Ok(value)
}

impl ExecCommandTool {
    fn invoke_planned(
        &self,
        arguments: &Value,
        _project: &Project,
        _cancel: &CancelToken,
        prepared: Option<crate::process::PreparedProcess>,
    ) -> Result<Value, ToolError> {
        ensure_object_keys(
            arguments,
            &[
                "cmd",
                "argv",
                "verification",
                "stdin",
                "workdir",
                "tty",
                "yield_time_ms",
                "max_output_tokens",
                "network",
                "sandbox",
            ],
            "exec_command",
        )?;
        let invocation = verification::Invocation::parse(arguments)?;
        let workdir = optional_string(arguments, "workdir", "exec_command")?.map(str::to_owned);
        let tty = optional_bool(arguments, "tty", "exec_command")?.unwrap_or(false);
        let network = optional_bool(arguments, "network", "exec_command")?.unwrap_or(false);
        let sandbox = SandboxRequest::parse(optional_string(arguments, "sandbox", "exec_command")?)
            .map_err(ToolError::new)?;
        let yield_ms = bounded_u64(
            arguments,
            "yield_time_ms",
            10_000,
            250,
            30_000,
            "exec_command",
        )?;
        let output_bytes = output_bytes(arguments, "exec_command")?;
        let id = self
            .service
            .start_prepared(
                ProcessStart {
                    command: invocation.command,
                    workdir,
                    tty,
                    network,
                    sandbox,
                },
                prepared.ok_or_else(|| ToolError::new("missing prepared command"))?,
            )
            .map_err(ToolError::new)?;
        let output = self
            .service
            .wait_and_consume(id, Duration::from_millis(yield_ms), output_bytes)
            .map_err(ToolError::new)?;
        Ok(process_output_json(output))
    }
}

impl WriteStdinTool {
    fn invoke_planned(
        &self,
        arguments: &Value,
        _project: &Project,
        _cancel: &CancelToken,
        prepared: Option<&crate::process::PreparedStdin>,
    ) -> Result<Value, ToolError> {
        ensure_object_keys(
            arguments,
            &[
                "session_id",
                "chars",
                "close_stdin",
                "terminate",
                "sensitive",
                "yield_time_ms",
                "max_output_tokens",
            ],
            "write_stdin",
        )?;
        if optional_bool(arguments, "sensitive", "write_stdin")?.unwrap_or(false) {
            return Err(ToolError::new(
                "write_stdin: sensitive input is unavailable through the model tool",
            ));
        }
        let session_id = required_u64(arguments, "session_id", "write_stdin")?;
        let chars = optional_string(arguments, "chars", "write_stdin")?.unwrap_or("");
        let close_stdin = optional_bool(arguments, "close_stdin", "write_stdin")?.unwrap_or(false);
        let terminate = optional_bool(arguments, "terminate", "write_stdin")?.unwrap_or(false);
        let yield_ms = bounded_u64(arguments, "yield_time_ms", 250, 250, 30_000, "write_stdin")?;
        let output_bytes = output_bytes(arguments, "write_stdin")?;
        match prepared {
            Some(prepared) => self.service.write_prepared_stdin(
                prepared,
                session_id,
                chars.as_bytes(),
                close_stdin,
                terminate,
            ),
            None => self
                .service
                .write_stdin(session_id, chars.as_bytes(), close_stdin, terminate),
        }
        .map_err(ToolError::new)?;
        let output = self
            .service
            .wait_and_consume(session_id, Duration::from_millis(yield_ms), output_bytes)
            .map_err(ToolError::new)?;
        Ok(process_output_json(output))
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::plugin::PluginManager;
    use crate::plugins::ToolRegistryPlugin;
    use crate::plugins::services::{PROCESS_SERVICE, TOOL_SERVICE};

    use super::sbx4_tests::root;

    #[test]
    #[cfg(unix)]
    fn catalog_registers_three_tools_and_redacts_stdin_journal_arguments() {
        let root = root();
        let mut manager = PluginManager::root(ScopeKind::TrustedProject);
        manager
            .mount_all(vec![
                Arc::new(ToolRegistryPlugin),
                Arc::new(SandboxPlugin {
                    project: Project::new(root.clone()),
                    permission_mode: None,
                }),
                Arc::new(ProcessServicePlugin {
                    project: Project::new(&root),
                }),
                Arc::new(ExecToolsPlugin),
            ])
            .unwrap();
        let tools = manager.require(TOOL_SERVICE).unwrap();
        assert_eq!(
            tools
                .definitions()
                .into_iter()
                .map(|tool| tool.name)
                .collect::<Vec<_>>(),
            [
                "run_command",
                "exec_command",
                "write_stdin",
                "test_discrimination"
            ]
        );
        assert!(
            tools
                .definitions()
                .iter()
                .all(|definition| definition.effect == ToolEffect::Execute)
        );
        let write = tools.get("write_stdin").unwrap();
        let journal = write.journal_arguments(&json!({
            "session_id": 7,
            "chars": "super-secret\n",
            "terminate": false
        }));
        let text = serde_json::to_string(&journal).unwrap();
        assert!(!text.contains("super-secret"));
        assert_eq!(journal["chars_bytes"], 13);
        let journal_result = write.journal_output(&json!({
            "session_id": 7,
            "running": false,
            "exit_code": 0,
            "stdout": "super-secret",
            "stderr": "",
            "pty": "super-secret echoed",
            "stdout_bytes": 1,
            "stderr_bytes": 0,
            "pty_bytes": 2,
            "stdout_truncated": true,
            "sandbox": {"provider": "seatbelt"}
        }));
        let result_text = serde_json::to_string(&journal_result).unwrap();
        assert!(!result_text.contains("super-secret"));
        assert_eq!(journal_result["stdout_bytes"], 1);
        assert_eq!(journal_result["pty_bytes"], 2);
        assert_eq!(journal_result["output_truncated"], true);
        assert_eq!(journal_result["output_omitted_from_journal"], true);
        assert_eq!(
            write.journal_output(&json!("stdin write failed")),
            json!("stdin write failed")
        );
        assert_eq!(
            write.journal_output(&json!({"error": "stdin write failed"})),
            json!({"error": "stdin write failed"})
        );

        let service = manager.require(PROCESS_SERVICE).unwrap();
        let generation = service.bind_run("s", CancelToken::new()).unwrap();
        let exec = tools.get("exec_command").unwrap();
        let output = exec
            .invoke(
                &json!({"cmd": "printf ok", "yield_time_ms": 1000}),
                &Project::new(&root),
                &CancelToken::new(),
            )
            .unwrap();
        assert_eq!(output["stdout"], "ok");
        let oversized = json!({"cmd": "x".repeat(MAX_COMMAND_BYTES + 1)});
        let oversized_journal = exec.journal_arguments(&oversized);
        assert!(oversized_journal.get("cmd").is_none());
        assert_eq!(oversized_journal["command_bytes"], MAX_COMMAND_BYTES + 1);
        assert_eq!(oversized_journal["command_omitted_from_journal"], true);
        assert!(
            exec.invoke(&oversized, &Project::new(&root), &CancelToken::new())
                .unwrap_err()
                .to_string()
                .contains("command exceeds")
        );
        let compat = tools
            .get("run_command")
            .unwrap()
            .invoke(
                &json!({"command": "printf compat"}),
                &Project::new(&root),
                &CancelToken::new(),
            )
            .unwrap();
        assert_eq!(compat["stdout"], "compat");
        assert!(compat["cwd"].as_str().is_some());
        assert!(compat["stdout_truncated"].is_boolean());
        assert!(compat["stderr_truncated"].is_boolean());
        service.unbind_run(generation).unwrap();
        manager.close().unwrap();
        crate::test_support::cleanup_tree(&root);
    }
}
