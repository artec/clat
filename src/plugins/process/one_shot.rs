use super::*;

impl RunCommandTool {
    pub(super) fn invoke_planned(
        &self,
        arguments: &Value,
        _project: &Project,
        _cancel: &CancelToken,
        prepared: Option<crate::process::PreparedProcess>,
    ) -> Result<Value, ToolError> {
        ensure_object_keys(
            arguments,
            &[
                "command",
                "argv",
                "verification",
                "workdir",
                "timeout_seconds",
                "network",
                "sandbox",
            ],
            "run_command",
        )?;
        let invocation = verification::Invocation::compat(arguments)?;
        let timeout = bounded_u64(arguments, "timeout_seconds", 120, 1, 600, "run_command")?;
        let request = ProcessStart {
            command: invocation.command,
            workdir: optional_string(arguments, "workdir", "run_command")?.map(str::to_owned),
            tty: false,
            network: optional_bool(arguments, "network", "run_command")?.unwrap_or(false),
            sandbox: SandboxRequest::parse(optional_string(arguments, "sandbox", "run_command")?)
                .map_err(ToolError::new)?,
        };
        let prepared = prepared.ok_or_else(|| ToolError::new("missing prepared command"))?;
        let cwd = prepared.working_directory().to_string_lossy().into_owned();
        let output = self
            .service
            .run_compat_prepared(request, Duration::from_secs(timeout), prepared)
            .map_err(ToolError::new)?;
        let mut value = if output.direct {
            process_output_json(output)
        } else {
            legacy_json(output)
        };
        value["cwd"] = json!(cwd);
        if let Some(receipt) = value.get("verification_receipt").cloned() {
            value = verification::bounded_view(value, receipt);
        }
        Ok(verification::with_guidance(value))
    }
}

fn legacy_json(output: ProcessOutput) -> Value {
    let signal = output
        .signal
        .as_deref()
        .and_then(|signal| signal.parse::<i64>().ok());
    let sandbox = output
        .sandbox
        .json(output.sandbox_denied, output.sandbox_unavailable);
    json!({
        "execution":"shell", "exit_scope":"shell",
        "verification_witness": output.verification_witness,
        "command_outcome": output.command_outcome.json(),
        "command": output.command,
        "exit_code": output.exit_code, "signal": signal, "timed_out": output.timed_out,
        "stdout": output.stdout, "stderr": output.stderr,
        "stdout_bytes": output.stdout_bytes, "stderr_bytes": output.stderr_bytes,
        "stdout_truncated": output.stdout_truncated, "stderr_truncated": output.stderr_truncated,
        "sandbox": sandbox
    })
}
