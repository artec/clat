//! Host scheduling pump. Model execution remains in RunExecutionEngine.
use super::state::{ServeShared, SseFrame};
use crate::application::{ApplicationRunResult, AutomaticRequest};
use crate::scheduler::Claim;
use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
    mpsc,
};
use std::time::{Duration, Instant};
struct Running {
    claim: Claim,
    handle: crate::RunHandle,
    completion: mpsc::Receiver<ApplicationRunResult>,
    meter: Arc<AtomicU64>,
    web: Arc<crate::web_tools::PaidCallBudget>,
    started: Instant,
}
impl ServeShared {
    pub(super) fn spawn_automatic(self: &Arc<Self>) {
        let weak = Arc::downgrade(self);
        let worker = std::thread::Builder::new()
            .name("clat-host-scheduler".into())
            .spawn(move || {
                let mut running = None;
                let mut last_error = String::new();
                loop {
                    let Some(shared) = weak.upgrade() else {
                        return;
                    };
                    if shared.is_shutting_down() {
                        return;
                    }
                    let result = tick(&shared, &mut running);
                    if let Err(error) = result {
                        if last_error != error.to_string() {
                            shared.automatic_notice("error", None, Some(error.presentation()));
                            last_error = error.to_string();
                        }
                    } else {
                        last_error.clear();
                    }
                    drop(shared);
                    std::thread::park_timeout(Duration::from_millis(500));
                }
            })
            .expect("spawn Host scheduler");
        self.register_worker(worker);
    }
    pub(super) fn is_automatic_run(&self) -> bool {
        self.inner
            .lock()
            .expect("serve inner lock")
            .active_run
            .as_ref()
            .is_some_and(|r| r.automatic)
    }
    pub(super) fn scope_interaction(&self, mut ctl: serde_json::Value) -> serde_json::Value {
        if let Some(run) = self
            .inner
            .lock()
            .expect("serve inner lock")
            .active_run
            .as_ref()
            .filter(|r| r.automatic)
        {
            ctl["automatic_session_id"] = serde_json::json!(run.session_id);
        }
        ctl
    }
    pub(super) fn automatic_notice(
        &self,
        phase: &str,
        session: Option<&str>,
        message: Option<crate::i18n::UiMessage>,
    ) {
        self.broadcast(SseFrame { event: "notice", data: super::shapes::ctl_data(&serde_json::json!({
            "kind":"background_run", "payload":{"phase":phase,"session_id":session,"presentation":message}
        })) });
    }
}
fn tick(
    shared: &Arc<ServeShared>,
    running: &mut Option<Running>,
) -> Result<(), crate::ApplicationError> {
    if let Some(active) = running {
        match active.completion.try_recv() {
            Ok(result) => {
                let active = running.take().unwrap();
                return finish(shared, active, result);
            }
            Err(mpsc::TryRecvError::Disconnected) => {
                let active = running.take().unwrap();
                return finish(
                    shared,
                    active,
                    Err(crate::ApplicationRunFailure {
                        receipt: None,
                        error: "automatic Run worker exited without a result".into(),
                        turns: 0,
                        usage: Default::default(),
                    }),
                );
            }
            Err(mpsc::TryRecvError::Empty) => {}
        }
        let reason = if !shared.pending.lock().expect("approvals").is_empty() {
            Some("approval_pending")
        } else if !shared.questions.0.lock().expect("questions").is_empty() {
            Some("question_pending")
        } else {
            None
        };
        let app = shared.app.lock().expect("application lock");
        if let Some(reason) = reason {
            app.block_automatic(&active.claim.rule.id, reason)?;
        }
        return Ok(());
    }
    let Ok(_mutation) = shared.rpc_mutations.try_lock() else {
        return Ok(());
    };
    let rpc = format!("auto-{}", uuid::Uuid::new_v4());
    if !shared.try_claim_run(&rpc, super::state::now_ms()) {
        return Ok(());
    }
    let result = start(shared);
    match result {
        Ok(Some(active)) => {
            *running = Some(active);
            Ok(())
        }
        Ok(None) => {
            shared.release_run_claim();
            Ok(())
        }
        Err(error) => {
            shared.release_run_claim();
            Err(error)
        }
    }
}
fn start(shared: &Arc<ServeShared>) -> Result<Option<Running>, crate::ApplicationError> {
    let mut app = shared.app.lock().expect("application lock");
    app.recover_automatic()?;
    let Some(claim) = app.automatic_candidate(super::state::now_ms())? else {
        return Ok(None);
    };
    {
        let mut inner = shared.inner.lock().expect("serve inner lock");
        if let Some(run) = &mut inner.active_run {
            run.automatic = true;
            run.session_id = Some(claim.rule.scope.session.clone());
        }
    }
    let (completion, receive) = mpsc::channel();
    let meter = Arc::new(AtomicU64::new(u64::MAX));
    let web = Arc::new(crate::web_tools::PaidCallBudget::new(
        claim.rule.spec.budget.run_paid_web_calls,
    ));
    let started = Instant::now();
    let result = app.start_automatic(
        &claim,
        AutomaticRequest {
            approver: Arc::new(super::approver::ServeApprover::new(shared.clone())),
            asker: Some(Arc::new(super::questions::ServeAsker::new(shared))),
            events: Box::new(ScopedSink {
                shared: shared.clone(),
                session: claim.rule.scope.session.clone(),
            }),
            completion,
            meter: meter.clone(),
            web_budget: web.clone(),
        },
    );
    match result {
        Ok(handle) => {
            shared.automatic_notice("started", Some(&claim.rule.scope.session), None);
            Ok(Some(Running {
                claim,
                handle,
                completion: receive,
                meter,
                web,
                started,
            }))
        }
        Err(error) => {
            let failure = Err(crate::ApplicationRunFailure {
                receipt: error.admission_receipt().cloned().map(Box::new),
                error: error.to_string(),
                turns: 0,
                usage: Default::default(),
            });
            app.settle_automatic(&claim, &failure, u64::MAX, 0, 0)?;
            Err(error)
        }
    }
}
fn finish(
    shared: &Arc<ServeShared>,
    active: Running,
    result: ApplicationRunResult,
) -> Result<(), crate::ApplicationError> {
    let settlement = shared
        .app
        .lock()
        .expect("application lock")
        .settle_automatic(
            &active.claim,
            &result,
            active.meter.load(Ordering::Acquire),
            active.web.used(),
            active.started.elapsed().as_secs().saturating_add(1),
        );
    let _ = active.handle.join();
    // Terminal control commit precedes notice and slot release, even on failure.
    shared.finish_automatic_notice(super::shapes::ctl_data(&serde_json::json!({
        "kind":"background_run","payload":{"phase":"settled","session_id":active.claim.rule.scope.session,
        "presentation":settlement.as_ref().err().map(crate::ApplicationError::presentation)}
    })));
    if !shared.is_shutting_down() {
        super::next_turn::dispatch_next(shared);
    }
    settlement
}
struct ScopedSink {
    shared: Arc<ServeShared>,
    session: String,
}
impl crate::EventSink for ScopedSink {
    fn emit(&mut self, event: crate::RunEvent) {
        let wire: serde_json::Value =
            serde_json::from_str(&super::shapes::realtime_data(&event)).expect("Run wire JSON");
        self.shared.broadcast(SseFrame { event: "notice", data: super::shapes::ctl_data(&serde_json::json!({
            "kind":"background_run", "payload":{"phase":"event","session_id":self.session,"event":wire["event"]}
        })) });
        if matches!(
            event,
            crate::RunEvent::RunCompleted { .. } | crate::RunEvent::RunFailed { .. }
        ) {
            self.shared.publish_notification("completed", None);
        }
    }
}
