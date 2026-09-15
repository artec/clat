use super::*;
use crate::plugins::services::{UtilityModel, UtilityOutput, UtilityTask};
use serde_json::json;
use std::sync::{Condvar, mpsc};

struct BlockedUtility {
    reached: mpsc::Sender<()>,
    release: Arc<(Mutex<bool>, Condvar)>,
}

impl UtilityModel for BlockedUtility {
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
    ) -> Option<UtilityOutput> {
        self.reached.send(()).unwrap();
        let (lock, wake) = &*self.release;
        let (released, _) = wake
            .wait_timeout_while(lock.lock().unwrap(), Duration::from_secs(10), |released| {
                !*released
            })
            .unwrap();
        (*released).then(|| UtilityOutput {
            text: "next question".into(),
            provider: "fixture".into(),
            model: "fixture".into(),
        })
    }
}

#[test]
fn suggestion_provider_does_not_lock_other_clients_and_freezes_selection() {
    let (storage, project_root, project) = setup("suggestion-concurrency");
    let application = BootstrapApplication::open(project, storage.clone())
        .unwrap()
        .authorize_and_mount_with_provider(Arc::new(TestProviderPlugin {
            behavior: TestBehavior::Success,
        }))
        .unwrap();
    crate::test_support::configure_test_model(&application);
    let app = Arc::new(Mutex::new(application));
    let shared = Arc::new(ServeShared::new(app.clone(), "unit".into(), 0));
    protocol::dispatch("prompt.send", &json!({"text":"initial context"}), &shared).unwrap();
    let deadline = Instant::now() + WAIT;
    while !shared.active_run_info().is_null() {
        assert!(Instant::now() < deadline, "fixture run did not settle");
        std::thread::sleep(Duration::from_millis(10));
    }
    let (reached, observed) = mpsc::channel();
    let release = Arc::new((Mutex::new(false), Condvar::new()));
    app.lock()
        .unwrap()
        .set_utility_for_test(Arc::new(BlockedUtility {
            reached,
            release: release.clone(),
        }));
    let generation = shared.selection_generation();
    let request = shared.clone();
    let suggestion = std::thread::spawn(move || {
        protocol::dispatch(
            "prompt.suggest",
            &json!({"expected_selection_generation":generation}),
            &request,
        )
    });
    observed
        .recv_timeout(Duration::from_secs(5))
        .expect("provider reached");
    let (completed, result) = mpsc::channel();
    let request = shared.clone();
    let other = std::thread::spawn(move || {
        completed
            .send(protocol::dispatch("session.new", &json!({}), &request))
            .unwrap();
    });
    let early = result.recv_timeout(Duration::from_millis(500));
    *release.0.lock().unwrap() = true;
    release.1.notify_all();
    let suggested = suggestion.join().unwrap().unwrap();
    other.join().unwrap();
    shared.mark_shutting_down();
    shared.drain_workers();
    drop(shared);
    Arc::try_unwrap(app)
        .ok()
        .unwrap()
        .into_inner()
        .unwrap()
        .close()
        .unwrap();
    crate::test_support::cleanup_tree(&storage);
    crate::test_support::cleanup_tree(&project_root);
    assert!(
        early.is_ok(),
        "another client's session.new must finish while utility I/O is blocked"
    );
    early.unwrap().unwrap();
    assert_eq!(
        suggested["selection_generation"], generation,
        "reply must retain the prepared selection"
    );
}
