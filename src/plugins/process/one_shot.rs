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
        Ok(output::one_shot(output, cwd, evidence))
    }
}
