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

#[test]
fn host_spawn_or_attach_requires_trust_and_converges_two_launchers() {
    let root = temp_root("spawn-concurrent");
    let home = root.join("home");
    let project = root.join("project");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&project).unwrap();
    let command = || {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_clat"));
        cmd.current_dir(&project)
            .env("HOME", &home)
            .env("USERPROFILE", &home)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        cmd
    };
    let refused = command().args(["host", "start"]).output().unwrap();
    assert!(!refused.status.success());
    assert!(!home.join(".clat").exists(), "no trust means no writes");
    let first = command()
        .args(["host", "start", "--trust"])
        .spawn()
        .unwrap();
    let second = command()
        .args(["host", "start", "--trust"])
        .spawn()
        .unwrap();
    let first = first.wait_with_output().unwrap();
    let second = second.wait_with_output().unwrap();
    // Stop before assertions so a behavioral failure cannot leave our host running.
    let status = command().args(["host", "status"]).output().unwrap();
    let stop = command().args(["host", "stop"]).output().unwrap();
    assert!(
        first.status.success(),
        "{}",
        String::from_utf8_lossy(&first.stderr)
    );
    assert!(
        second.status.success(),
        "{}",
        String::from_utf8_lossy(&second.stderr)
    );
    assert_eq!(
        first.stdout, second.stdout,
        "both launchers must attach to one instance"
    );
    assert!(status.status.success(), "launcher exit must not close host");
    assert!(stop.status.success());
    let deadline = Instant::now() + WAIT;
    loop {
        let boot = BootstrapApplication::open(Project::new(&project), home.join(".clat")).unwrap();
        if let Ok(app) = boot.into_trusted() {
            app.close().unwrap();
            break;
        }
        assert!(
            Instant::now() < deadline,
            "host stop must release the lease"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    remove_tree(&root);
}

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
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("serve did not listen within {WAIT:?}");
        }
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

#[test]
fn host_cli_status_preserves_host_and_stop_closes_it() {
    let root = temp_root("host-cli");
    let home = root.join("home");
    let project_root = root.join("project");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&project_root).unwrap();
    BootstrapApplication::open(Project::new(&project_root), home.join(".clat"))
        .unwrap()
        .authorize_and_mount(ProjectAuthorization::grant())
        .unwrap()
        .close()
        .unwrap();
    let command = || {
        let mut command = Command::new(env!("CARGO_BIN_EXE_clat"));
        command
            .current_dir(&project_root)
            .env("HOME", &home)
            .env("USERPROFILE", &home)
            .stdin(Stdio::null());
        command
    };
    let port = reserve_port().to_string();
    let mut child = command()
        .args(["serve", "--port", &port])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    wait_until_listening(&mut child, port.parse().unwrap());
    // Binding precedes token publication; a TCP listener alone is not readiness.
    let deadline = Instant::now() + WAIT;
    while !home.join(".clat/host-endpoint.json").is_file() {
        if child.try_wait().unwrap().is_some() || Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("host did not publish discovery endpoint");
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let status = command().args(["host", "status"]).output().unwrap();
    // Always clean up the child, including against a pre-feature binary.
    if !status.status.success() {
        let _ = child.kill();
        let _ = child.wait();
        panic!(
            "host status failed: {}",
            String::from_utf8_lossy(&status.stderr)
        );
    }
    assert!(
        child.try_wait().unwrap().is_none(),
        "status must not stop host"
    );
    let output = String::from_utf8(status.stdout).unwrap();
    assert!(output.contains("online at 127.0.0.1:"));
    let token = std::fs::read_to_string(home.join(".clat/web-token")).unwrap();
    let endpoint = std::fs::read_to_string(home.join(".clat/host-endpoint.json")).unwrap();
    assert!(
        !endpoint.contains(token.trim()),
        "discovery must not expose token"
    );
    assert!(
        !output.contains(token.trim()),
        "status must not expose token"
    );
    let stop = command().args(["host", "stop"]).output().unwrap();
    if !stop.status.success() {
        let _ = child.kill();
        let _ = child.wait();
        panic!(
            "host stop failed: {}",
            String::from_utf8_lossy(&stop.stderr)
        );
    }
    assert!(String::from_utf8_lossy(&stop.stdout).contains("accepted shutdown"));
    assert!(wait_until_exit(&mut child).success());
    let stale = command().args(["host", "status"]).output().unwrap();
    assert!(
        !stale.status.success(),
        "stale discovery must not start another host"
    );
    assert_eq!(
        std::fs::read_to_string(home.join(".clat/host-endpoint.json")).unwrap(),
        endpoint
    );
    // The normal close path must release the writer lease.
    BootstrapApplication::open(Project::new(&project_root), home.join(".clat"))
        .unwrap()
        .into_trusted()
        .unwrap()
        .close()
        .unwrap();
    remove_tree(&root);
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
