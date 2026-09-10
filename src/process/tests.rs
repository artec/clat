use super::*;
use crate::sandbox::{SandboxModeSource, SandboxService};

fn fixture(tag: &str, limits: ProcessLimits) -> (PathBuf, Arc<ProcessService>) {
    let root = std::env::temp_dir().join(format!(
        "clat-process-{tag}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos()
    ));
    std::fs::create_dir_all(&root).unwrap();
    let sandbox = Arc::new(SandboxService::new(root.clone(), SandboxModeSource::Classic).unwrap());
    let service = Arc::new(ProcessService::with_limits(
        Project::new(&root),
        sandbox,
        limits,
    ));
    (root, service)
}

fn bind(service: &ProcessService) -> u64 {
    service.bind_run("session", CancelToken::new()).unwrap()
}

#[test]
fn credential_shaped_environment_keys_are_filtered() {
    for key in [
        "OPENAI_API_KEY",
        "DB_PASSWORD",
        "CLIENT_SECRET",
        "AUTH_TOKEN",
    ] {
        assert!(
            credential_shaped_env_key(std::ffi::OsStr::new(key)),
            "{key}"
        );
    }
    for key in ["PATH", "HOME", "RUST_LOG", "CLAT_MODEL"] {
        assert!(
            !credential_shaped_env_key(std::ffi::OsStr::new(key)),
            "{key}"
        );
    }
}

#[test]
#[cfg(not(target_os = "macos"))]
fn managed_stdio_required_policy_fails_before_spawn_on_unsupported_platforms() {
    let (root, service) = fixture("managed-unsupported", ProcessLimits::default());
    let error = service
        .acquire_managed_stdio(ManagedStdioStart {
            server_id: "rust".into(),
            program: OsString::from("definitely-not-a-real-clat-test-binary"),
            args: Vec::new(),
        })
        .err()
        .expect("unsupported platform must fail closed");
    assert!(error.contains("graduated provider"), "{error}");
    assert!(
        !error.contains("spawn failed"),
        "planning must precede spawn: {error}"
    );
    service.close().unwrap();
    crate::test_support::cleanup_tree(&root);
}

#[test]
#[cfg(target_os = "macos")]
fn managed_stdio_is_single_flight_project_owned_and_raw() {
    let (root, service) = fixture("managed-raw", ProcessLimits::default());
    let start = |server_id: &str| ManagedStdioStart {
        server_id: server_id.to_owned(),
        program: OsString::from("/bin/cat"),
        args: Vec::new(),
    };
    let first = service.acquire_managed_stdio(start("rust")).unwrap();
    let same = service.acquire_managed_stdio(start("rust")).unwrap();
    assert!(Arc::ptr_eq(&first.shared, &same.shared));
    let facts = first.sandbox_facts();
    assert_eq!(facts.mode.as_str(), "project-read-temp-write");
    assert_eq!(facts.provider, "seatbelt");
    assert_eq!(facts.enforcement, "full");
    assert!(facts.policy_digest.is_some());

    let generation = bind(&service);
    service.unbind_run(generation).unwrap();
    first.write_all(b"raw-ping\n").unwrap();
    assert_eq!(
        first.read_stdout(Duration::from_secs(3), 1024).unwrap(),
        b"raw-ping\n"
    );

    let second = service.acquire_managed_stdio(start("typescript")).unwrap();
    let limit = service
        .acquire_managed_stdio(start("third"))
        .err()
        .expect("managed stdio limit");
    assert!(limit.contains("limit reached"), "{limit}");
    service.close_managed_stdio(&first).unwrap();
    service.close_managed_stdio(&same).unwrap();
    service.close_managed_stdio(&second).unwrap();
    service.close().unwrap();
    crate::test_support::cleanup_tree(&root);
}

#[test]
#[cfg(target_os = "macos")]
fn managed_stdio_project_read_temp_write_policy_has_real_world_effects() {
    let (root, service) = fixture("managed-world", ProcessLimits::default());
    std::fs::write(root.join("readable.txt"), "project-readable").unwrap();
    let outside = std::env::current_dir()
        .unwrap()
        .join("target")
        .join(format!(
            "clat-managed-outside-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
    std::fs::create_dir_all(&outside).unwrap();
    let shell_quote = |path: &Path| format!("'{}'", path.to_string_lossy().replace('\'', "'\\''"));
    let run = |server_id: &str, command: String| {
        let lease = service
            .acquire_managed_stdio(ManagedStdioStart {
                server_id: server_id.to_owned(),
                program: OsString::from("/bin/sh"),
                args: vec![OsString::from("-c"), OsString::from(command)],
            })
            .unwrap();
        let facts = lease.sandbox_facts();
        let mut stdout = Vec::new();
        for _ in 0..100 {
            let chunk = lease
                .read_stdout(Duration::from_millis(50), 64 * 1024)
                .unwrap();
            let empty = chunk.is_empty();
            stdout.extend(chunk);
            if lease.is_terminal() && empty {
                break;
            }
        }
        assert!(lease.is_terminal(), "managed command did not terminate");
        let stderr = lease.stderr_tail();
        service.close_managed_stdio(&lease).unwrap();
        (stdout, stderr, facts)
    };

    let (read, _, managed_facts) = run("read", "cat readable.txt".into());
    assert_eq!(read, b"project-readable");
    assert_eq!(managed_facts.mode.as_str(), "project-read-temp-write");
    let normal = service
        .sandbox
        .plan(
            OsString::from("/usr/bin/true"),
            Vec::new(),
            SandboxRequest::Required,
            false,
        )
        .unwrap();
    assert_ne!(managed_facts.policy_digest, normal.facts.policy_digest);

    let (_, project_write_stderr, _) = run("project-write", "printf denied > denied.txt".into());
    assert!(!root.join("denied.txt").exists());
    assert!(!project_write_stderr.is_empty());

    let outside_file = outside.join("denied.txt");
    let (_, _, _) = run(
        "outside-write",
        format!("printf denied > {}", shell_quote(&outside_file)),
    );
    assert!(!outside_file.exists());

    std::os::unix::fs::symlink(&outside, root.join("escape")).unwrap();
    let (_, _, _) = run("symlink-write", "printf denied > escape/symlink.txt".into());
    assert!(!outside.join("symlink.txt").exists());

    let temp_file = std::env::temp_dir().join(format!(
        "clat-managed-temp-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let (_, temp_stderr, _) = run(
        "temp-write",
        format!("printf temp-ok > {}", shell_quote(&temp_file)),
    );
    assert!(
        temp_stderr.is_empty(),
        "{}",
        String::from_utf8_lossy(&temp_stderr)
    );
    assert_eq!(std::fs::read_to_string(&temp_file).unwrap(), "temp-ok");
    std::fs::remove_file(&temp_file).unwrap();

    if Path::new("/usr/bin/nc").is_file() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let port = listener.local_addr().unwrap().port();
        let (_, _, _) = run("network", format!("/usr/bin/nc -z 127.0.0.1 {port}"));
        assert!(
            listener.accept().is_err(),
            "managed policy must deny network"
        );
    }

    service.close().unwrap();
    crate::test_support::cleanup_tree(&outside);
    crate::test_support::cleanup_tree(&root);
}

#[test]
#[cfg(unix)]
fn stdin_round_trip_and_cross_run_owner_fence() {
    let (root, service) = fixture("stdin", ProcessLimits::default());
    let (notice_tx, notice_rx) = std::sync::mpsc::channel();
    service.set_notice_sink(Arc::new(move |notice| {
        let _ = notice_tx.send(notice);
    }));
    let generation = bind(&service);
    let id = service
        .start(ProcessStart {
            command: "read line; printf 'got:%s' \"$line\"".into(),
            workdir: None,
            tty: false,
            network: false,
            sandbox: SandboxRequest::Auto,
        })
        .unwrap();
    service.write_stdin(id, b"hello\n", false, false).unwrap();
    let output = service
        .wait_and_consume(id, Duration::from_secs(3), 4096)
        .unwrap();
    assert!(!output.running);
    assert_eq!(output.stdout, "got:hello");
    let notice = notice_rx
        .recv_timeout(Duration::from_secs(1))
        .expect("completion notice");
    assert_eq!(notice.session_id, id);
    assert_eq!(notice.exit_code, Some(0));
    service.unbind_run(generation).unwrap();
    let next = bind(&service);
    assert!(service.wait_and_consume(id, Duration::ZERO, 10).is_err());
    service.unbind_run(next).unwrap();
    service.close().unwrap();
    crate::test_support::cleanup_tree(&root);
}

#[test]
#[cfg(unix)]
fn terminate_and_timeout_kill_descendants_without_markers() {
    let limits = ProcessLimits {
        max_lifetime: Duration::from_secs(30),
        idle_timeout: Duration::from_secs(30),
        stdin_write_timeout: Duration::from_secs(5),
    };
    let (root, service) = fixture("tree", limits);
    let generation = bind(&service);
    let id = service
        .start(ProcessStart {
            command: "(sleep 2; printf orphan > orphan-marker) & wait".into(),
            workdir: None,
            tty: false,
            network: false,
            sandbox: SandboxRequest::Auto,
        })
        .unwrap();
    service.write_stdin(id, b"", false, true).unwrap();
    let output = service
        .wait_and_consume(id, Duration::from_secs(3), 4096)
        .unwrap();
    assert!(output.terminated);
    std::thread::sleep(Duration::from_millis(2300));
    assert!(!root.join("orphan-marker").exists());
    service.unbind_run(generation).unwrap();
    service.close().unwrap();
    crate::test_support::cleanup_tree(&root);
}

#[test]
#[cfg(unix)]
fn pty_and_ring_output_are_bounded() {
    let (root, service) = fixture("pty", ProcessLimits::default());
    let generation = bind(&service);
    let id = service
        .start(ProcessStart {
            command: "read line; printf 'pty:%s' \"$line\"".into(),
            workdir: None,
            tty: true,
            network: false,
            sandbox: SandboxRequest::Auto,
        })
        .unwrap();
    service.write_stdin(id, b"hello\n", false, false).unwrap();
    let output = service
        .wait_and_consume(id, Duration::from_secs(3), 4096)
        .unwrap();
    assert!(output.pty.contains("pty:hello"), "{}", output.pty);

    let big = service
        .start(ProcessStart {
            command: "head -c 400000 /dev/zero | tr '\\0' x".into(),
            workdir: None,
            tty: false,
            network: false,
            sandbox: SandboxRequest::Auto,
        })
        .unwrap();
    let first = service
        .wait_and_consume(big, Duration::from_secs(5), 1024)
        .unwrap();
    let tail = service.wait_and_consume(big, Duration::ZERO, 1024).unwrap();
    assert!(first.output_truncated || first.stdout_lossy);
    assert!(!tail.stdout.is_empty());

    let combined = service
        .start(ProcessStart {
            command: "head -c 2000 /dev/zero | tr '\\0' o; head -c 2000 /dev/zero | tr '\\0' e >&2"
                .into(),
            workdir: None,
            tty: false,
            network: false,
            sandbox: SandboxRequest::Auto,
        })
        .unwrap();
    let combined = service
        .wait_and_consume(combined, Duration::from_secs(3), 1024)
        .unwrap();
    assert!(combined.stdout.len() + combined.stderr.len() <= 1024);
    assert!(combined.output_truncated);

    let invalid_utf8 = service
        .start(ProcessStart {
            command: "printf '\\377'".into(),
            workdir: None,
            tty: false,
            network: false,
            sandbox: SandboxRequest::Auto,
        })
        .unwrap();
    let invalid_utf8 = service
        .wait_and_consume(invalid_utf8, Duration::from_secs(3), 1024)
        .unwrap();
    assert!(invalid_utf8.stdout_lossy);
    assert_eq!(invalid_utf8.stdout_bytes, 1);
    assert_ne!(invalid_utf8.stdout.len(), invalid_utf8.stdout_bytes);
    let (expanded, lossy, display_truncated) = decode_output(&vec![0xff; 1024], 1024);
    assert!(lossy);
    assert!(display_truncated);
    assert!(expanded.len() <= 1024);
    let (escaped, lossy, display_truncated) = decode_output(&vec![0; 1024], 1024);
    assert!(!lossy);
    assert!(display_truncated);
    assert!(serde_json::to_string(&escaped).unwrap().len() <= 1026);

    let marker = root.join("pty-orphan-marker");
    let tree = service
        .start(ProcessStart {
            command: "(trap '' TERM; sleep 2; printf orphan > pty-orphan-marker) & wait".into(),
            workdir: None,
            tty: true,
            network: false,
            sandbox: SandboxRequest::Auto,
        })
        .unwrap();
    service.write_stdin(tree, b"", false, true).unwrap();
    let stopped = service
        .wait_and_consume(tree, Duration::from_secs(3), 1024)
        .unwrap();
    assert!(stopped.terminated);
    std::thread::sleep(Duration::from_millis(2300));
    assert!(
        !marker.exists(),
        "PTY descendants must not outlive terminate"
    );
    service.unbind_run(generation).unwrap();
    service.close().unwrap();
    crate::test_support::cleanup_tree(&root);
}

#[test]
#[cfg(unix)]
fn lifetime_and_active_job_limits_fail_closed() {
    let limits = ProcessLimits {
        max_lifetime: Duration::from_secs(30),
        idle_timeout: Duration::from_secs(30),
        stdin_write_timeout: Duration::from_secs(5),
    };
    let (root, service) = fixture("limits", limits);
    let generation = bind(&service);
    let timed = service
        .run_compat(
            "sleep 30",
            Duration::from_millis(200),
            false,
            SandboxRequest::Auto,
        )
        .unwrap();
    assert!(timed.timed_out);

    let mut jobs = Vec::new();
    for _ in 0..MAX_ACTIVE_PROCESSES {
        jobs.push(
            service
                .start(ProcessStart {
                    command: "sleep 30".into(),
                    workdir: None,
                    tty: false,
                    network: false,
                    sandbox: SandboxRequest::Auto,
                })
                .unwrap(),
        );
    }
    assert!(
        service
            .start(ProcessStart {
                command: "sleep 30".into(),
                workdir: None,
                tty: false,
                network: false,
                sandbox: SandboxRequest::Auto,
            })
            .unwrap_err()
            .contains("active limit")
    );
    for id in jobs {
        service.write_stdin(id, b"", false, true).unwrap();
    }
    service.unbind_run(generation).unwrap();
    service.close().unwrap();
    crate::test_support::cleanup_tree(&root);
}

#[test]
#[cfg(unix)]
fn concurrent_starts_never_oversubscribe_the_active_limit() {
    let limits = ProcessLimits {
        max_lifetime: Duration::from_secs(30),
        idle_timeout: Duration::from_secs(30),
        stdin_write_timeout: Duration::from_secs(5),
    };
    let (root, service) = fixture("concurrent-limit", limits);
    let generation = bind(&service);
    let worker_count = MAX_ACTIVE_PROCESSES * 4;
    let barrier = Arc::new(std::sync::Barrier::new(worker_count));
    let mut workers = Vec::new();
    for _ in 0..worker_count {
        let service = Arc::clone(&service);
        let barrier = Arc::clone(&barrier);
        workers.push(std::thread::spawn(move || {
            barrier.wait();
            service.start(ProcessStart {
                command: "sleep 30".into(),
                workdir: None,
                tty: false,
                network: false,
                sandbox: SandboxRequest::Auto,
            })
        }));
    }
    let jobs = workers
        .into_iter()
        .filter_map(|worker| worker.join().expect("start worker").ok())
        .collect::<Vec<_>>();
    assert!(
        jobs.len() <= MAX_ACTIVE_PROCESSES,
        "concurrent start admitted {} active jobs past limit {}",
        jobs.len(),
        MAX_ACTIVE_PROCESSES
    );
    for id in jobs {
        service.write_stdin(id, b"", false, true).unwrap();
    }
    service.unbind_run(generation).unwrap();
    service.close().unwrap();
    crate::test_support::cleanup_tree(&root);
}

#[test]
#[cfg(unix)]
fn blocked_stdin_write_cannot_starve_timeout_or_teardown() {
    let limits = ProcessLimits {
        max_lifetime: Duration::from_secs(30),
        idle_timeout: Duration::from_secs(30),
        stdin_write_timeout: Duration::from_millis(200),
    };
    let (root, service) = fixture("stdin-backpressure", limits);
    let generation = bind(&service);
    let id = service
        .start(ProcessStart {
            command: "sleep 30".into(),
            workdir: None,
            tty: false,
            network: false,
            sandbox: SandboxRequest::Auto,
        })
        .unwrap();
    assert!(
        service
            .write_stdin(id, &vec![b'x'; MAX_STDIN_WRITE_BYTES + 1], false, false)
            .unwrap_err()
            .contains("exceeds")
    );
    let writer_service = Arc::clone(&service);
    let (done_tx, done_rx) = std::sync::mpsc::channel();
    let writer = std::thread::spawn(move || {
        let result =
            writer_service.write_stdin(id, &vec![b'x'; MAX_STDIN_WRITE_BYTES], false, false);
        let _ = done_tx.send(result);
    });
    let write = done_rx
        .recv_timeout(Duration::from_secs(3))
        .expect("stdin backpressure must be interrupted by the monitor");
    assert!(write.is_err(), "blocked write unexpectedly completed");
    let output = service
        .wait_and_consume(id, Duration::from_secs(1), 1024)
        .unwrap();
    assert!(output.timed_out, "{output:?}");
    writer.join().unwrap();
    service.unbind_run(generation).unwrap();
    service.close().unwrap();
    crate::test_support::cleanup_tree(&root);
}

#[test]
#[cfg(unix)]
fn one_shot_compatibility_discards_unaddressable_output_remainders() {
    let (root, service) = fixture("compat-remainders", ProcessLimits::default());
    let generation = bind(&service);
    for _ in 0..=MAX_COMPLETED_PROCESSES {
        let output = service
            .run_compat(
                "head -c 40000 /dev/zero",
                Duration::from_secs(3),
                false,
                SandboxRequest::Auto,
            )
            .unwrap();
        assert!(output.stdout_truncated);
        assert_eq!(output.exit_code, Some(0));
    }
    service.unbind_run(generation).unwrap();
    service.close().unwrap();
    crate::test_support::cleanup_tree(&root);
}

#[test]
#[cfg(unix)]
fn lifetime_and_idle_ttls_each_terminate_the_tree() {
    for (tag, limits) in [
        (
            "lifetime-ttl",
            ProcessLimits {
                max_lifetime: Duration::from_millis(200),
                idle_timeout: Duration::from_secs(30),
                stdin_write_timeout: Duration::from_secs(5),
            },
        ),
        (
            "idle-ttl",
            ProcessLimits {
                max_lifetime: Duration::from_secs(30),
                idle_timeout: Duration::from_millis(200),
                stdin_write_timeout: Duration::from_secs(5),
            },
        ),
    ] {
        let (root, service) = fixture(tag, limits);
        let generation = bind(&service);
        let id = service
            .start(ProcessStart {
                command: "(sleep 1; printf orphan > ttl-marker) & wait".into(),
                workdir: None,
                tty: false,
                network: false,
                sandbox: SandboxRequest::Auto,
            })
            .unwrap();
        let output = service
            .wait_and_consume(id, Duration::from_secs(3), 1024)
            .unwrap();
        assert!(output.timed_out, "{tag}: {output:?}");
        std::thread::sleep(Duration::from_millis(1100));
        assert!(!root.join("ttl-marker").exists(), "{tag} left a descendant");
        service.unbind_run(generation).unwrap();
        service.close().unwrap();
        crate::test_support::cleanup_tree(&root);
    }
}

#[test]
fn malformed_workdir_and_unbound_calls_fail_closed() {
    let (root, service) = fixture("closed", ProcessLimits::default());
    assert!(
        service
            .start(ProcessStart {
                command: "echo x".into(),
                workdir: None,
                tty: false,
                network: false,
                sandbox: SandboxRequest::Auto,
            })
            .unwrap_err()
            .contains("active run")
    );
    let generation = bind(&service);
    assert!(
        service
            .start(ProcessStart {
                command: "echo x".into(),
                workdir: Some("../outside".into()),
                tty: false,
                network: false,
                sandbox: SandboxRequest::Auto,
            })
            .is_err()
    );
    service.unbind_run(generation).unwrap();
    let cancelled = CancelToken::new();
    let cancelled_generation = service.bind_run("cancelled", cancelled.clone()).unwrap();
    cancelled.cancel();
    assert!(
        service
            .start(ProcessStart {
                command: "echo x".into(),
                workdir: None,
                tty: false,
                network: false,
                sandbox: SandboxRequest::Auto,
            })
            .unwrap_err()
            .contains("cancelled")
    );
    service.unbind_run(cancelled_generation).unwrap();
    service.close().unwrap();
    crate::test_support::cleanup_tree(&root);
}
