//! Process-boundary lifecycle checks for `clat serve`.
//!
//! Unit tests exercise the injected shutdown flag. This integration leg owns
//! a real child process so deleting the Unix termination-signal registration
//! makes the test observe a signal exit instead of a clean Application close.

use clat::{BootstrapApplication, Project, ProjectAuthorization};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const WAIT: Duration = Duration::from_secs(30);

fn temp_root(tag: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!(
        "clat-serve-lifecycle-{tag}-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock after epoch")
            .as_nanos()
    ));
    std::fs::create_dir_all(&root).expect("create test root");
    root
}

fn reserve_port() -> u16 {
    TcpListener::bind(("127.0.0.1", 0))
        .expect("reserve loopback port")
        .local_addr()
        .expect("reserved local address")
        .port()
}

fn wait_until_listening(child: &mut Child, port: u16) {
    let deadline = Instant::now() + WAIT;
    loop {
        if TcpStream::connect(("127.0.0.1", port)).is_ok() {
            return;
        }
        if let Some(status) = child.try_wait().expect("poll serve child") {
            panic!("serve exited before listening: {status}");
        }
        assert!(
            Instant::now() < deadline,
            "serve did not listen within {WAIT:?}"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn wait_until_exit(child: &mut Child) -> std::process::ExitStatus {
    let deadline = Instant::now() + WAIT;
    loop {
        if let Some(status) = child.try_wait().expect("poll serve child") {
            return status;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            panic!("serve did not terminate within {WAIT:?}");
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn remove_tree(path: &Path) {
    for _ in 0..100 {
        match std::fs::remove_dir_all(path) {
            Ok(()) => return,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return,
            Err(_) => std::thread::sleep(Duration::from_millis(10)),
        }
    }
    std::fs::remove_dir_all(path).expect("remove test root");
}

#[cfg(unix)]
#[test]
fn sigterm_uses_the_graceful_serve_shutdown_path() {
    let root = temp_root("sigterm");
    let home = root.join("home");
    let project_root = root.join("project");
    std::fs::create_dir_all(&home).expect("create test home");
    std::fs::create_dir_all(&project_root).expect("create test project");

    let project = Project::new(&project_root);
    let application = BootstrapApplication::open(project, home.join(".clat"))
        .expect("open bootstrap")
        .authorize_and_mount(ProjectAuthorization::grant())
        .expect("trust project");
    application.close().expect("close trust bootstrap");

    let port = reserve_port();
    let mut child = Command::new(env!("CARGO_BIN_EXE_clat"))
        .args([
            "serve",
            "--port",
            &port.to_string(),
            "--token",
            "serve-lifecycle-test-token",
        ])
        .current_dir(&project_root)
        .env("HOME", &home)
        .env("USERPROFILE", &home)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn clat serve");

    wait_until_listening(&mut child, port);
    // SAFETY: `child.id()` names the live child observed above; SIGTERM does
    // not access memory and the return value is checked.
    let killed = unsafe { libc::kill(child.id() as libc::pid_t, libc::SIGTERM) };
    assert_eq!(
        killed,
        0,
        "send SIGTERM: {}",
        std::io::Error::last_os_error()
    );
    let status = wait_until_exit(&mut child);

    let stderr = child
        .stderr
        .take()
        .map(|mut stderr| {
            use std::io::Read as _;
            let mut text = String::new();
            stderr.read_to_string(&mut text).expect("read serve stderr");
            text
        })
        .unwrap_or_default();
    assert!(
        status.success(),
        "SIGTERM must complete the normal shutdown path, got {status}; stderr={stderr}"
    );
    remove_tree(&root);
}

#[test]
fn a_second_serve_process_fails_even_on_a_different_port() {
    let root = temp_root("single-instance");
    let home = root.join("home");
    let project_root = root.join("project");
    std::fs::create_dir_all(&home).expect("create test home");
    std::fs::create_dir_all(&project_root).expect("create test project");

    let project = Project::new(&project_root);
    let application = BootstrapApplication::open(project, home.join(".clat"))
        .expect("open bootstrap")
        .authorize_and_mount(ProjectAuthorization::grant())
        .expect("trust project");
    application.close().expect("close trust bootstrap");

    let first_port = reserve_port();
    let second_port = reserve_port();
    assert_ne!(first_port, second_port, "the test requires distinct ports");
    let spawn = |port: u16| {
        Command::new(env!("CARGO_BIN_EXE_clat"))
            .args([
                "serve",
                "--port",
                &port.to_string(),
                "--token",
                "serve-lifecycle-test-token",
            ])
            .current_dir(&project_root)
            .env("HOME", &home)
            .env("USERPROFILE", &home)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn clat serve")
    };

    let mut first = spawn(first_port);
    wait_until_listening(&mut first, first_port);
    let mut second = spawn(second_port);
    let second_status = wait_until_exit(&mut second);
    let second_stderr = second
        .stderr
        .take()
        .map(|mut stderr| {
            use std::io::Read as _;
            let mut text = String::new();
            stderr
                .read_to_string(&mut text)
                .expect("read second serve stderr");
            text
        })
        .unwrap_or_default();
    assert!(!second_status.success(), "second serve must fail");
    assert!(
        second_stderr.contains("another CLAT process holds this storage root; close it first"),
        "second serve must identify the live owner; status={second_status}; stderr={second_stderr}"
    );

    first.kill().expect("stop first serve");
    first.wait().expect("reap first serve");
    remove_tree(&root);
}
