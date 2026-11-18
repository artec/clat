use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};

struct Sandbox(PathBuf);

impl Sandbox {
    fn new() -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let root = std::env::temp_dir().join(format!(
            "clat-host-menu-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed),
        ));
        std::fs::create_dir(&root).unwrap();
        Self(root)
    }
}

impl Drop for Sandbox {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn host_menu_new_build_takes_over_and_same_build_reopen_keeps_the_owner() {
    let dir = Sandbox::new();
    let prefs = dir.0.join("preferences.json");
    let executable = dir.0.join("companion");
    std::fs::write(&executable, "old").unwrap();
    let old = Instance::claim(&prefs, &executable, || panic!("unexpected legacy owner"))
        .unwrap()
        .unwrap();
    assert!(
        Instance::claim(&prefs, &executable, || panic!("unexpected legacy owner"))
            .unwrap()
            .is_none()
    );
    assert!(!old.replaced());

    let replacement = dir.0.join("replacement");
    std::fs::write(&replacement, "new").unwrap();
    std::fs::rename(replacement, &executable).unwrap();
    let (send, outcome) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        send.send(Instance::claim(&prefs, &executable, || {
            panic!("unexpected legacy owner")
        }))
        .unwrap()
    });
    let deadline = Instant::now() + Duration::from_secs(3);
    while !old.replaced() {
        assert!(
            Instant::now() < deadline,
            "the new build did not request handoff"
        );
        std::thread::sleep(POLL);
    }
    assert!(
        outcome.try_recv().is_err(),
        "both builds must never own the icon"
    );
    drop(old);
    let new = outcome
        .recv_timeout(Duration::from_secs(3))
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(!new.replaced());
}

#[test]
fn host_menu_migrates_an_existing_owner_without_a_handoff_record() {
    let dir = Sandbox::new();
    let prefs = dir.0.join("preferences.json");
    let executable = dir.0.join("companion");
    std::fs::write(&executable, "new").unwrap();
    let legacy = lock_file(&dir.0.join("host-menu.lock")).unwrap();
    assert!(try_lock(&legacy).unwrap());
    let current = Instance::claim(&prefs, &executable, || drop(legacy)).unwrap();
    assert!(current.is_some());
}
