use super::*;

impl ProcessService {
    pub(crate) fn prepare_direct(
        &self,
        request: &ProcessStart,
        argv: Vec<OsString>,
        verification: Option<receipt::VerificationSpec>,
    ) -> Result<PreparedProcess, String> {
        let owner = self.current_owner()?;
        let workdir = self.resolve_workdir(request.workdir.as_deref())?;
        let verification = Some(receipt::Context::new(&argv, &workdir, verification));
        let mut argv = argv.into_iter();
        let program = argv.next().ok_or("argv needs an executable")?;
        // Windows batch launch is shell execution even when passed to Command.
        #[cfg(windows)]
        if Path::new(&program).extension().is_some_and(|ext| {
            matches!(
                ext.to_string_lossy().to_ascii_lowercase().as_str(),
                "cmd" | "bat"
            )
        }) {
            return Err(
                "argv does not launch batch files; use cmd explicitly for shell execution".into(),
            );
        }
        let planned =
            self.sandbox
                .plan(program, argv.collect(), request.sandbox, request.network)?;
        Ok(PreparedProcess {
            stdin_pipe: true,
            owner,
            workdir,
            planned,
            verification,
        })
    }
}
