//! Real auxiliary producer and real summary, with a deterministic commit ordering.
use super::*;
use crate::plugins::services::{CompactionOutcome, CompactionRequest, HistoryCompactor};
struct HeldSummary {
    inner: Arc<dyn HistoryCompactor>,
    entered: Arc<AtomicBool>,
    released: Arc<AtomicBool>,
}
impl HistoryCompactor for HeldSummary {
    fn compact(&self, request: CompactionRequest<'_>) -> CompactionOutcome {
        self.entered.store(true, Ordering::SeqCst);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
        while !self.released.load(Ordering::SeqCst) && !request.cancel.is_cancelled() {
            assert!(
                std::time::Instant::now() < deadline,
                "title race witness timeout"
            );
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        self.inner.compact(request)
    }
}
#[test]
#[ignore = "paid GLM title/summary race; explicitly arm CLAT_PHASE9_LIVE"]
fn live_glm_title_race_rejects_stale_summary() {
    if std::env::var("CLAT_PHASE9_LIVE").as_deref() != Ok("1") {
        eprintln!("phase9 title race NOT_ARMED");
        return;
    }
    let out = PathBuf::from(std::env::var_os("CLAT_PHASE9_OUTPUT").unwrap());
    std::fs::create_dir(&out).unwrap();
    let root = out.join("project");
    std::fs::create_dir(&root).unwrap();
    let project = Project::new(&root);
    let storage = out.join("state");
    std::fs::create_dir(&storage).unwrap();
    exercise::policy(&storage);
    let case = corpus::roster().remove(0);
    let mut seed = super::super::tests::mount(
        &project,
        &storage,
        crate::test_support::TestBehavior::Success,
    );
    crate::test_support::configure_test_model_with_budget(&seed, u32::MAX);
    for prompt in [case.initial(), case.correction(), case.evidence()] {
        super::super::tests::run(&mut seed, &prompt).unwrap();
    }
    let id = seed.current_session_id().unwrap();
    seed.close().unwrap();
    let config_path = storage.join("builtin-plugins.json");
    let mut config: Value = serde_json::from_slice(&std::fs::read(&config_path).unwrap()).unwrap();
    config["disabled"]
        .as_array_mut()
        .unwrap()
        .retain(|id| id != "builtin.utility_model");
    std::fs::write(config_path, serde_json::to_vec(&config).unwrap()).unwrap();
    let capture = Arc::new(model::Capture::new(out.clone()));
    capture.stage("title-race");
    let state = Arc::new(tools::State::default());
    let mut app = crate::test_support::reopen_after_close(|| {
        exercise::mount(&project, &storage, &capture, &state)
    });
    exercise::configure(&app, 24000);
    app.switch_session(id.clone()).unwrap();
    let surface = app.sessions.surface_nodes().unwrap();
    let before = app.committed_seq();
    let entered = Arc::new(AtomicBool::new(false));
    let released = Arc::new(AtomicBool::new(false));
    app.compactor = Some(Arc::new(HeldSummary {
        inner: app.compactor.take().unwrap(),
        entered: entered.clone(),
        released: released.clone(),
    }));
    let (tx, rx) = std::sync::mpsc::channel();
    app.subscribe(tx);
    let handle = app.compact_session().unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    while !entered.load(Ordering::SeqCst) && std::time::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    let (config, credentials) = app.model_state().unwrap();
    app.title_worker
        .as_ref()
        .unwrap()
        .sender
        .try_send(super::super::title::AutotitleJob {
            session_id: id,
            config,
            credentials,
        })
        .unwrap();
    let mut title_committed = false;
    while !title_committed && std::time::Instant::now() < deadline {
        if let Ok(ApplicationEvent::TitleUpdated { .. }) =
            rx.recv_timeout(std::time::Duration::from_millis(100))
        {
            title_committed = true;
        }
    }
    released.store(true, Ordering::SeqCst);
    let result = handle.join_report().unwrap();
    let unchanged = surface == app.sessions.surface_nodes().unwrap();
    let reason = result.err().map(|e| e.raw).unwrap_or_default();
    let result = json!({"title_committed":title_committed,"revision_advanced":app.committed_seq()>before,"surface_unchanged":unchanged,"stale_rejected":reason.contains("session changed"),"reason":reason,"model_requests":capture.next.load(Ordering::SeqCst)});
    write_new(&out.join("result.json"), &result);
    app.close().unwrap();
    assert!(
        title_committed && unchanged && result["stale_rejected"] == true,
        "{result}"
    );
}
