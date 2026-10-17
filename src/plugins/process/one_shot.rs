use super::*;

impl RunCommandTool {
    pub(super) fn invoke_planned(
        &self,
        arguments: &Value,
        project: &Project,
        cancel: &CancelToken,
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
                "evidence",
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
        self.service
            .validate_prepared_owner(&prepared)
            .map_err(ToolError::new)?;
        if cancel.is_cancelled() {
            return Err(ToolError::new("run_command: cancelled"));
        }
        let capture = evidence::Capture::prepare(arguments, project, &cwd, cancel)?;
        let output = self
            .service
            .run_compat_prepared(request, Duration::from_secs(timeout), prepared)
            .map_err(ToolError::new)?;
        let evidence = capture.map(|c| c.finish(project, &output, cancel));
        let mut value = if output.direct {
            process_output_json(output)
        } else {
            legacy_json(output)
        };
        value["cwd"] = json!(cwd);
        if let Some(evidence) = evidence {
            value["evidence"] = evidence;
        }
        if let Some(receipt) = value.get("verification_receipt").cloned() {
            value = verification::bounded_view(value, receipt);
        }
        Ok(verification::with_guidance(value))
    }
}

fn legacy_json(output: ProcessOutput) -> Value {
    let failure_context = output.failure_context.clone();
    let signal = output
        .signal
        .as_deref()
        .and_then(|signal| signal.parse::<i64>().ok());
    let sandbox = output
        .sandbox
        .json(output.sandbox_denied, output.sandbox_unavailable);
    let mut value = json!({
        "execution":"shell", "exit_scope":"shell",
        "verification_witness": output.verification_witness,
        "command_outcome": output.command_outcome.json(),
        "command": output.command,
        "exit_code": output.exit_code, "signal": signal, "timed_out": output.timed_out,
        "stdout": output.stdout, "stderr": output.stderr,
        "stdout_bytes": output.stdout_bytes, "stderr_bytes": output.stderr_bytes,
        "stdout_truncated": output.stdout_truncated, "stderr_truncated": output.stderr_truncated,
        "sandbox": sandbox
    });
    if let Some(context) = failure_context {
        value["failure_context"] = context;
    }
    value
}
