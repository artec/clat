use super::registry::Entry;
use super::*;
use crate::{process::BackgroundPlan, session::catalog::JobPhase};
use std::sync::atomic::Ordering;
impl JobRegistry {
    pub(crate) fn start(
        self: &Arc<Self>,
        source: Arc<crate::process::ProcessService>,
        plan: BackgroundPlan,
        expected: SessionId,
        label: String,
        timeout: Duration,
        cancel: &CancelToken,
    ) -> Result<JobView, JobError> {
        let _admission = self
            .admission
            .lock()
            .map_err(|_| JobError::new("unavailable"))?;
        if cancel.is_cancelled() {
            return Err(JobError::new("cancelled"));
        }
        let entry = self.prepare_entry(label, &expected)?;
        self.persist(&entry, JobPhase::Registered)?;
        self.insert(entry.clone());
        if cancel.is_cancelled() {
            self.settle_without_process(&entry, "cancelled before spawn");
            return Ok(entry.view().clone());
        }
        match source.start_background(plan, entry.cancel.clone(), timeout) {
            Ok(kernel) => *entry.kernel.lock().unwrap() = Some(kernel),
            Err(error) => {
                if error.unknown {
                    let mut view = entry.view();
                    view.state = "interrupted".into();
                    view.observation = Some(
                        json!({"execution_outcome":"unknown","diagnostic":crate::redact::redact_secrets(&error.detail)}),
                    );
                    drop(view);
                    self.finish(&entry);
                } else {
                    self.settle_without_process(&entry, &error.detail);
                }
                return Ok(entry.view().clone());
            }
        }
        entry.view().state = "running".into();
        self.publish(&entry);
        let registry = self.clone();
        let running = entry.clone();
        match std::thread::Builder::new()
            .name("clat-job".into())
            .spawn(move || registry.run_monitor(running))
        {
            Ok(join) => *entry.join.lock().unwrap() = Some(join),
            Err(error) => {
                entry.cancel.cancel();
                if let Some((kernel, _)) = entry.kernel.lock().unwrap().as_ref()
                    && let Err(cleanup) = kernel.close()
                {
                    entry.view().cleanup_error = Some(crate::redact::redact_secrets(&cleanup));
                }
                {
                    let mut view = entry.view();
                    view.state = "interrupted".into();
                    view.observation =
                        Some(json!({"execution_outcome":"unknown","diagnostic":error.to_string()}));
                }
                self.finish(&entry);
            }
        }
        let view = entry.view().clone();
        Ok(view)
    }
    fn settle_without_process(&self, entry: &Entry, detail: &str) {
        let mut view = entry.view();
        view.state = "failed".into();
        view.observation = Some(
            json!({"execution_outcome":"not_started","diagnostic":crate::redact::redact_secrets(detail)}),
        );
        drop(view);
        self.finish(entry);
    }
    pub(super) fn persist(&self, entry: &Entry, phase: JobPhase) -> Result<(), JobError> {
        let view = entry.view().clone();
        self.sessions
            .commit_background(
                &key(&self.project, &view.session),
                &[record::event(&view, phase)],
            )
            .map_err(|e| JobError::new("persistence_failed").detail(e))
    }
    fn run_monitor(&self, entry: Arc<Entry>) {
        let observed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            #[cfg(test)]
            if self.panic_next_monitor.swap(false, Ordering::AcqRel) {
                panic!("job monitor fault probe");
            }
            self.monitor(entry.clone());
        }));
        if observed.is_err() && !entry.finished.load(Ordering::Acquire) {
            entry.cancel.cancel();
            let kernel = entry
                .kernel
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .clone();
            if let Some((kernel, _)) = kernel
                && let Err(error) = kernel.close()
            {
                entry.view().cleanup_error = Some(crate::redact::redact_secrets(&error));
            }
            let mut view = entry.view();
            if view.observation.is_none() {
                view.state = "interrupted".into();
                view.observation =
                    Some(json!({"execution_outcome":"unknown","reason":"monitor_panic"}));
            }
            drop(view);
            self.finish(&entry);
        }
    }
    fn monitor(&self, entry: Arc<Entry>) {
        let kernel = entry
            .kernel
            .lock()
            .unwrap()
            .clone()
            .expect("started kernel");
        let mut streams = [
            output::RedactedLines::default(),
            output::RedactedLines::default(),
        ];
        loop {
            match kernel
                .0
                .background_poll(kernel.1, Duration::from_millis(100))
            {
                Ok(output) => {
                    if output.stdout_lossy
                        || output.stderr_lossy
                        || output.stdout_bytes > output.stdout.len()
                        || output.stderr_bytes > output.stderr.len()
                    {
                        entry.lost.store(true, Ordering::Release);
                    }
                    Self::capture_streams(
                        &entry,
                        &mut streams,
                        [&output.stdout, &output.stderr],
                        false,
                    );
                    if !output.running {
                        if output.output_truncated
                            && (output.stdout_bytes > 0 || output.stderr_bytes > 0)
                        {
                            continue;
                        }
                        Self::capture_streams(&entry, &mut streams, ["", ""], true);
                        Self::observe_exit(&entry, &kernel, output);
                        break;
                    }
                }
                Err(error) => {
                    let mut view = entry.view();
                    view.state = "interrupted".into();
                    view.observation = Some(
                        json!({"execution_outcome":"unknown","diagnostic":crate::redact::redact_secrets(&error.to_string())}),
                    );
                    drop(view);
                    break;
                }
            }
        }
        if let Err(error) = kernel.0.close() {
            entry.view().cleanup_error = Some(crate::redact::redact_secrets(&error));
        }
        self.finish(&entry);
    }
    fn observe_exit(
        entry: &Entry,
        kernel: &(Arc<crate::process::ProcessService>, u64),
        mut output: crate::process::ProcessOutput,
    ) {
        if let Ok(totals) = kernel.0.background_totals(kernel.1) {
            output.stdout_bytes = totals[0].try_into().unwrap_or(usize::MAX);
            output.stderr_bytes = totals[1].try_into().unwrap_or(usize::MAX);
            output.pty_bytes = totals[2].try_into().unwrap_or(usize::MAX);
        }
        output.output_truncated |= entry.lost.load(Ordering::Acquire)
            || entry.output.lock().unwrap().end_offset() > MAX_OUTPUT as u64;
        let mut view = entry.view();
        view.state = if output.cancelled || output.terminated {
            "killed"
        } else if output.exit_code == Some(0) && !output.timed_out {
            "completed"
        } else {
            "failed"
        }
        .into();
        view.observation = Some(crate::plugins::process::background_observation(output));
        drop(view);
    }
    fn capture_streams(
        entry: &Entry,
        streams: &mut [output::RedactedLines; 2],
        chunks: [&str; 2],
        eof: bool,
    ) {
        let mut text = String::new();
        for (stream, chunk) in streams.iter_mut().zip(chunks) {
            let (safe, lost) = stream.push(chunk, eof);
            text.push_str(&safe);
            if lost {
                entry.lost.store(true, Ordering::Release);
            }
        }
        entry.output.lock().unwrap().append(text.as_bytes());
    }
    fn finish(&self, entry: &Entry) {
        {
            let mut view = entry.view();
            view.finished_at = Some(now());
            view.reference.last_observation = view.state.clone();
            view.notification_id = Some(format!("job:{}:settled", view.id));
        }
        if let Err(error) = self.persist(entry, JobPhase::Settled) {
            entry.view().persistence_error = Some(error.detail);
        }
        entry.permit.settle();
        entry.finished.store(true, Ordering::Release);
        self.publish(entry);
    }
}
