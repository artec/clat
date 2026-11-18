use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};

#[test]
fn host_menu_quit_reaches_the_captured_desktop_and_preserves_installer_veto() {
    let (old, listener) = Endpoint::bind().unwrap();
    let (replacement, _replacement_listener) = Endpoint::bind().unwrap();
    let preferences = old.0.join("preferences.json");
    publish(&preferences, &old).unwrap();
    let target = || Target::discover(&preferences).unwrap().unwrap();
    let during_install = target();
    let captured = target();
    publish(&preferences, &replacement).unwrap();

    let (completed, received) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let calls = AtomicUsize::new(0);
        let result = receive(listener, || calls.fetch_add(1, Ordering::SeqCst) == 1);
        completed.send((result, calls.load(Ordering::SeqCst))).ok();
    });
    assert!(
        !during_install.request().unwrap(),
        "the desktop can decline while installing"
    );
    assert!(
        captured.request().unwrap(),
        "a newly published desktop is outside this quit"
    );
    let (result, calls) = received.recv_timeout(TIMEOUT).unwrap();
    result.unwrap();
    assert_eq!(calls, 2);

    fs::remove_file(record(&preferences)).unwrap();
    fs::remove_file(record(&preferences).with_extension("lock")).unwrap();
}

#[test]
fn host_menu_quit_is_a_noop_if_the_original_desktop_already_exited() {
    let (endpoint, listener) = Endpoint::bind().unwrap();
    let target = Target(endpoint.0.join(SOCKET));
    drop(listener);
    drop(endpoint);
    assert!(target.request().unwrap());
}
