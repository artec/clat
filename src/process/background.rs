//! Detached command ownership reuses the existing planner, launcher and monitor.
use super::*;
pub(crate) struct BackgroundPlan {
    pub request: ProcessStart,
    pub argv: Option<Vec<OsString>>,
    pub verification: Option<VerificationSpec>,
    pub facts: SandboxFacts,
}
impl ProcessService {
    #[cfg(test)]
    pub(crate) fn background_scratch_for_test(&self) -> std::path::PathBuf {
        self.sandbox
            .writable_roots()
            .unwrap()
            .scratch()
            .to_path_buf()
    }
    pub(crate) fn prepare_background(
        &self,
        request: ProcessStart,
        argv: Option<Vec<OsString>>,
        verification: Option<VerificationSpec>,
    ) -> Result<BackgroundPlan, ProcessError> {
        if request.tty {
            return Err("background commands are non-interactive".into());
        }
        let prepared = match &argv {
            Some(argv) => self.prepare_direct(&request, argv.clone(), verification)?,
            None => self.prepare_start(&request)?,
        };
        let project_root = self
            .project
            .root()
            .canonicalize()
            .map_err(|e| ProcessError::from(e.to_string()))?;
        if !prepared.workdir.starts_with(&project_root) {
            return Err("background working directory must belong to the project".into());
        }
        if let Some(scratch) = self
            .sandbox
            .writable_roots()
            .ok()
            .map(|r| r.scratch().to_string_lossy().into_owned())
            && (request.command.contains(&scratch)
                || argv
                    .as_ref()
                    .is_some_and(|a| a.iter().any(|s| s.to_string_lossy().contains(&scratch))))
        {
            return Err("background commands cannot borrow the current run scratch".into());
        }
        let mut request = request;
        let relative = prepared
            .workdir
            .strip_prefix(&project_root)
            .map_err(|_| ProcessError::WorkdirOutOfScope)?;
        request.workdir = Some(if relative.as_os_str().is_empty() {
            ".".into()
        } else {
            relative.to_string_lossy().into_owned()
        });
        Ok(BackgroundPlan {
            request,
            argv,
            verification,
            facts: prepared.planned.facts,
        })
    }
    pub(crate) fn start_background(
        &self,
        plan: BackgroundPlan,
        cancel: CancelToken,
        timeout: Duration,
    ) -> Result<(Arc<ProcessService>, u64), BackgroundStartError> {
        self.sandbox
            .with_level(plan.facts.mode, || {
                Ok(self.launch_background(plan, cancel, timeout))
            })
            .map_err(BackgroundStartError::rejected)?
    }
    fn launch_background(
        &self,
        plan: BackgroundPlan,
        cancel: CancelToken,
        timeout: Duration,
    ) -> Result<(Arc<ProcessService>, u64), BackgroundStartError> {
        let sandbox = Arc::new(
            self.sandbox
                .detached(plan.facts.mode)
                .map_err(BackgroundStartError::rejected)?,
        );
        let mut service = Self::with_limits(
            self.project.clone(),
            sandbox,
            ProcessLimits {
                max_lifetime: timeout,
                idle_timeout: timeout,
                stdin_write_timeout: Duration::from_secs(5),
            },
        );
        service.background = true;
        let service = Arc::new(service);
        service
            .bind_run("background-job", cancel)
            .map_err(BackgroundStartError::rejected)?;
        let result = self.launch_background_plan(&service, plan);
        match result {
            Ok(id) => Ok((service, id)),
            Err(mut error) => {
                if let Err(cleanup) = service.close() {
                    error.detail.push_str(&format!("; {cleanup}"));
                }
                Err(error)
            }
        }
    }
    fn launch_background_plan(
        &self,
        service: &ProcessService,
        plan: BackgroundPlan,
    ) -> Result<u64, BackgroundStartError> {
        let prepared = match plan.argv {
            Some(argv) => service.prepare_direct(&plan.request, argv, plan.verification),
            None => service.prepare_start(&plan.request),
        }
        .map_err(|e| BackgroundStartError::rejected(e.to_string()))?
        .with_stdin_pipe(false);
        if prepared.planned.facts.mode != plan.facts.mode
            || prepared.planned.facts.provider != plan.facts.provider
            || prepared.planned.facts.enforcement != plan.facts.enforcement
        {
            return Err(BackgroundStartError::rejected(
                "background sandbox facts changed after approval".into(),
            ));
        }
        let id = service
            .start_prepared(plan.request, prepared)
            .map_err(BackgroundStartError::unknown)?;
        #[cfg(test)]
        if self
            .fail_background_after_spawn
            .swap(false, Ordering::AcqRel)
        {
            let deadline = Instant::now() + Duration::from_secs(5);
            while Instant::now() < deadline {
                let output = service.wait_and_consume(id, Duration::from_millis(50), 4096);
                if output
                    .as_ref()
                    .is_ok_and(|o| !o.stdout.is_empty() || !o.stderr.is_empty() || !o.running)
                {
                    break;
                }
            }
            return Err(BackgroundStartError::unknown(
                "injected failure after process start".into(),
            ));
        }
        Ok(id)
    }
}
impl ProcessService {
    pub(crate) fn background_totals(&self, id: u64) -> Result<[u64; 3], ProcessError> {
        let entry = self.entry_for_current_owner(id)?;
        let state = entry
            .state
            .lock()
            .map_err(|_| ProcessError::from("process observation lock poisoned"))?;
        Ok([
            state.stdout.end_offset(),
            state.stderr.end_offset(),
            state.pty.end_offset(),
        ])
    }
}

#[derive(Debug)]
pub(crate) struct BackgroundStartError {
    pub unknown: bool,
    pub detail: String,
}
impl BackgroundStartError {
    fn rejected(detail: String) -> Self {
        Self {
            unknown: false,
            detail,
        }
    }
    fn unknown(detail: String) -> Self {
        Self {
            unknown: true,
            detail,
        }
    }
}

impl ProcessService {
    pub(crate) fn background_poll(
        &self,
        id: u64,
        wait: Duration,
    ) -> Result<ProcessOutput, ProcessError> {
        let entry = self.entry_for_current_owner(id)?;
        entry.wait_until(wait);
        // This is bounded transport into Job-owned buffers, not a model result.
        // Reserve JSON expansion without consuming and dropping escaped tails.
        Ok(entry.consume_limits_encoded(32 * 1024, 32 * 1024, 0, 6))
    }
}
pub(super) fn complete_stream_prefix(bytes: &mut Vec<u8>, cursor: &mut u64) {
    if let Err(error) = std::str::from_utf8(bytes)
        && error.error_len().is_none()
    {
        *cursor = cursor.saturating_sub((bytes.len() - error.valid_up_to()) as u64);
        bytes.truncate(error.valid_up_to());
    }
}

impl ProcessService {
    pub(crate) fn execution_project(&self) -> Project {
        self.project.clone()
    }
}
