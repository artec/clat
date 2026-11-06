use super::*;
use crate::plugins::services::{UtilityFailure, UtilityModel, UtilityOutput, UtilityTask};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Mutex, mpsc};

struct BlockingUtility {
    entered: mpsc::SyncSender<()>,
    release: Mutex<mpsc::Receiver<()>>,
    calls: Arc<AtomicUsize>,
}
impl UtilityModel for BlockingUtility {
    fn enabled(&self, _: UtilityTask) -> bool {
        true
    }
    fn generate(
        &self,
        _: UtilityTask,
        _: &crate::ModelConfig,
        _: &crate::ProviderCredentials,
        _: &str,
        _: &crate::CancelToken,
    ) -> Result<UtilityOutput, UtilityFailure> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.entered.send(()).unwrap();
        self.release
            .lock()
            .unwrap()
            .recv_timeout(crate::test_support::HANG)
            .unwrap();
        // Deliberately ignore cancellation and return a diagnostic-producing failure.
        Err(UtilityFailure::RouteUnavailable)
    }
}
fn setup() -> (
    std::path::PathBuf,
    Project,
    HostApplication,
    Arc<Mutex<TrustedProjectApplication>>,
) {
    let (root, _) = crate::test_support::roots("phase5-utility-owner");
    let project = Project::new(root.join("project"));
    std::fs::create_dir_all(project.root()).unwrap();
    let mut host = mount(&project, &root.join("state"));
    let attached = host.attach(project.clone()).unwrap();
    {
        let mut app = attached.lock().unwrap();
        crate::test_support::configure_test_model(&app);
        run(&mut app);
    }
    (root, project, host, attached)
}
fn excluded(root: std::path::PathBuf) -> bool {
    std::thread::spawn(move || {
        crate::session::root_lease::try_acquire(&root)
            .unwrap()
            .is_none()
    })
    .join()
    .unwrap()
}
#[test]
fn phase5_prepared_idle_handles_cannot_pin_lease_or_execute_after_reopen() {
    let (root, project, mut host, attached) = setup();
    let (entered, _rx) = mpsc::sync_channel(1);
    let (_tx, release) = mpsc::channel();
    let calls = Arc::new(AtomicUsize::new(0));
    let prepared = {
        let mut app = attached.lock().unwrap();
        app.set_utility_for_test(Arc::new(BlockingUtility {
            entered,
            release: Mutex::new(release),
            calls: calls.clone(),
        }));
        app.prepare_prompt_suggestion().unwrap()
    };
    drop(attached);
    host.close().unwrap();
    drop(host);
    assert!(
        !excluded(root.join("state")),
        "idle prepared handle must release real root lease at close"
    );
    let mut host = mount(&project, &root.join("state"));
    assert!(prepared.generate(&crate::CancelToken::new()).is_err());
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    assert!(!root.join("state/utility-log.jsonl").exists());
    host.close().unwrap();
    drop(host);
    crate::test_support::cleanup_tree(&root);
}
#[test]
fn phase5_inflight_suggestion_retains_real_lease_and_cannot_write_after_close() {
    let (root, _project, mut host, attached) = setup();
    let (entered, rx) = mpsc::sync_channel(1);
    let (tx, release) = mpsc::channel();
    let prepared = {
        let mut app = attached.lock().unwrap();
        app.set_utility_for_test(Arc::new(BlockingUtility {
            entered,
            release: Mutex::new(release),
            calls: Arc::new(AtomicUsize::new(0)),
        }));
        app.prepare_prompt_suggestion().unwrap()
    };
    let worker = std::thread::spawn(move || prepared.generate(&crate::CancelToken::new()));
    rx.recv_timeout(crate::test_support::HANG).unwrap();
    drop(attached);
    assert!(host.close().is_err(), "actual producer is still running");
    drop(host);
    assert!(excluded(root.join("state")));
    tx.send(()).unwrap();
    assert!(worker.join().unwrap().is_err());
    assert!(!excluded(root.join("state")));
    assert!(!root.join("state/utility-log.jsonl").exists());
    crate::test_support::cleanup_tree(&root);
}
