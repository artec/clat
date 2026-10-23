use super::*;
use serde_json::{Value, json};
use std::io::{BufRead, BufReader, Read, Write};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
    mpsc,
};
use std::time::{Duration, Instant};

struct Fixture {
    root: std::path::PathBuf,
    backend: PackagedBackend,
    busy: Arc<AtomicBool>,
    calls: Arc<Mutex<Vec<String>>>,
    stop: Arc<AtomicBool>,
    done: mpsc::Receiver<()>,
}
impl Fixture {
    fn new(description: Value, error: Option<&'static str>) -> Self {
        let (root, _) = crate::test_support::roots("host-reconcile");
        std::fs::create_dir_all(root.join("project/scoped")).unwrap();
        let root = root.canonicalize().unwrap();
        let dir = cap_std::fs::Dir::open_ambient_dir(&root, cap_std::ambient_authority()).unwrap();
        crate::private_fs::write_text_atomic(&dir, &root, "web-token", "test-token").unwrap();
        let instance = uuid::Uuid::new_v4().to_string();
        let mut description = description;
        description["instance_id"] = json!(instance);
        description["storage_root"] = json!(root);
        if description.get("protocol_version").is_none() {
            description["protocol_version"] = json!(super::super::HOST_PROTOCOL_VERSION);
        }
        description["wire_version"] = json!(crate::wire::WIRE_VERSION);
        description["journal_version"] = json!(crate::session::compat::SESSION_FORMAT_VERSION);
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        super::super::discovery::publish(&root, listener.local_addr().unwrap().port(), &instance)
            .unwrap();
        let busy = Arc::new(AtomicBool::new(true));
        let calls = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let (tx, done) = mpsc::channel();
        let (active, captured, stopping, project) = (
            busy.clone(),
            calls.clone(),
            stop.clone(),
            root.join("project"),
        );
        std::thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(30);
            while !stopping.load(Ordering::SeqCst) {
                assert!(Instant::now() < deadline, "fixture exceeded 30 seconds");
                match listener.accept() {
                    Ok((stream, _)) => {
                        reply(stream, &description, &project, &active, &captured, error)
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::yield_now()
                    }
                    Err(e) => panic!("{e}"),
                }
            }
            tx.send(()).unwrap();
        });
        let backend = PackagedBackend::fixture_for_reconcile(&root);
        Self {
            root,
            backend,
            busy,
            calls,
            stop,
            done,
        }
    }
    fn observe(
        &self,
        trusted: bool,
        activated: &std::cell::Cell<usize>,
    ) -> Result<BackendConnection, String> {
        self.backend.reconcile_at(
            &self.root,
            None,
            &|_| {
                if trusted {
                    Ok(())
                } else {
                    Err("project needs trust".into())
                }
            },
            &|path| {
                assert_eq!(path, self.root.join("project"));
                activated.set(activated.get() + 1);
                Ok(HostClient::fixture_for_frontend_tests(&self.root))
            },
        )
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        let stopped = self.done.recv_timeout(Duration::from_secs(5));
        if !std::thread::panicking() {
            stopped.expect("fixture stopped");
        }
        crate::test_support::cleanup_tree(&self.root);
    }
}
fn reply(
    stream: std::net::TcpStream,
    description: &Value,
    project: &Path,
    busy: &AtomicBool,
    calls: &Mutex<Vec<String>>,
    error: Option<&str>,
) {
    stream.set_nonblocking(false).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    stream
        .set_write_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    reader.read_line(&mut line).unwrap();
    let path = line.split_whitespace().nth(1).unwrap().to_owned();
    let method = path.split("/api/").last().unwrap().to_owned();
    let mut size = 0;
    loop {
        line.clear();
        reader.read_line(&mut line).unwrap();
        if line == "\r\n" {
            break;
        }
        if let Some(value) = line.strip_prefix("Content-Length: ") {
            size = value.trim().parse().unwrap();
        }
    }
    let mut body = vec![0; size];
    reader.read_exact(&mut body).unwrap();
    calls.lock().unwrap().push(method.clone());
    let value = match method.as_str() {
        "host.describe" => description.clone(),
        "workbench.info" => {
            json!({"project":{"root":if path.starts_with("/workspace/") { project.join("scoped") } else { project.to_path_buf() }}})
        }
        "host.runtime_status" => json!({"ready":!busy.load(Ordering::SeqCst)}),
        "host.stop" => json!({"stopping":true}),
        "host.takeover" => {
            let params: Value = serde_json::from_slice(&body).unwrap();
            assert_eq!(params["expected_instance_id"], description["instance_id"]);
            assert_eq!(
                params["replacement_product_version"],
                super::super::build_identity::product_version()
            );
            json!({"stopping":true})
        }
        _ => panic!("unexpected RPC {method}"),
    };
    let code = error
        .or_else(|| (method == "host.takeover" && busy.load(Ordering::SeqCst)).then_some("busy"));
    let result = if method == "host.takeover" && code.is_some() {
        json!({"ok":false,"error":{"code":code,"message":"arbitrary diagnostic, not a classification key"}})
    } else { json!({"ok":true,"value":value}) }.to_string();
    write!(
        reader.get_mut(),
        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{result}",
        result.len()
    )
    .unwrap();
}
#[test]
fn failed_activation_is_reported_without_retry_or_missing() {
    let fixture = Fixture::new(official("0.0.0"), None);
    fixture.busy.store(false, Ordering::SeqCst);
    let activated = std::cell::Cell::new(0);
    let result = fixture
        .backend
        .reconcile_at(&fixture.root, None, &|_| Ok(()), &|_| {
            activated.set(activated.get() + 1);
            Err("activation failed".into())
        });
    assert!(matches!(result, Err(error) if error == "activation failed"));
    assert_eq!(activated.get(), 1);
    assert_eq!(
        fixture
            .calls
            .lock()
            .unwrap()
            .iter()
            .filter(|m| *m == "host.takeover")
            .count(),
        1
    );
}

fn official(version: &str) -> Value {
    json!({"build_kind":"official", "product_version":version, "native_client_compatibility":1,
        "capabilities":["desktop-workbench-v1","host.runtime_status"],"build_fingerprint":"different"})
}
#[test]
fn compatible_same_or_newer_official_host_connects_without_takeover() {
    for version in [super::super::build_identity::product_version(), "999.0.0"] {
        let fixture = Fixture::new(official(version), None);
        let activated = std::cell::Cell::new(0);
        fixture.busy.store(false, Ordering::SeqCst);
        assert!(matches!(
            fixture.observe(true, &activated).unwrap(),
            BackendConnection::Connected(_)
        ));
        assert_eq!(activated.get(), 0);
        assert!(
            !fixture
                .calls
                .lock()
                .unwrap()
                .iter()
                .any(|m| m == "host.takeover")
        );
    }
}
#[test]
fn compatible_busy_host_remains_usable_and_idle_host_upgrades() {
    let fixture = Fixture::new(official("0.0.0"), None);
    let activated = std::cell::Cell::new(0);
    assert!(matches!(
        fixture.observe(true, &activated).unwrap(),
        BackendConnection::Connected(_)
    ));
    assert_eq!(activated.get(), 0);
    assert!(
        !fixture
            .calls
            .lock()
            .unwrap()
            .iter()
            .any(|m| m == "host.takeover")
    );
    fixture.busy.store(false, Ordering::SeqCst);
    assert!(matches!(
        fixture.observe(true, &activated).unwrap(),
        BackendConnection::Connected(_)
    ));
    assert_eq!(activated.get(), 1);
}
#[test]
fn atomic_busy_after_ready_observation_keeps_compatible_connection() {
    let fixture = Fixture::new(official("0.0.0"), Some("busy"));
    fixture.busy.store(false, Ordering::SeqCst);
    let activated = std::cell::Cell::new(0);
    assert!(matches!(
        fixture.observe(true, &activated).unwrap(),
        BackendConnection::Connected(_)
    ));
    assert_eq!(activated.get(), 0);
    assert!(
        fixture
            .calls
            .lock()
            .unwrap()
            .iter()
            .any(|m| m == "host.takeover")
    );
}
#[test]
fn untrusted_project_and_non_busy_failure_never_activate_a_replacement() {
    for (trusted, code) in [(false, None), (true, Some("internal"))] {
        let fixture = Fixture::new(official("0.0.0"), code);
        fixture.busy.store(false, Ordering::SeqCst);
        let activated = std::cell::Cell::new(0);
        assert!(fixture.observe(trusted, &activated).is_err());
        assert_eq!(activated.get(), 0);
        if !trusted {
            assert!(
                !fixture
                    .calls
                    .lock()
                    .unwrap()
                    .iter()
                    .any(|m| m == "host.takeover")
            );
        }
    }
}
#[test]
fn missing_or_wrong_protocol_never_starts_or_takes_over_a_host() {
    let fixture = Fixture::new(json!({"protocol_version":0}), None);
    assert!(fixture.observe(true, &std::cell::Cell::new(0)).is_err());
    assert_eq!(*fixture.calls.lock().unwrap(), ["host.describe"]);
    let (root, _) = crate::test_support::roots("reconcile-missing");
    std::fs::create_dir_all(&root).unwrap();
    let backend = PackagedBackend::fixture_for_reconcile(&root);
    let result = backend.reconcile_at(
        &root,
        None,
        &|_| panic!("must not check a project"),
        &|_| panic!("must not restart after disconnect"),
    );
    assert!(matches!(result.unwrap(), BackendConnection::Missing));
    drop(backend);
    crate::test_support::cleanup_tree(&root);
}
#[test]
fn development_candidate_rejects_a_different_official_build() {
    let mut fixture = Fixture::new(
        official(super::super::build_identity::product_version()),
        None,
    );
    fixture.backend.policy = super::super::BuildMatchPolicy::Backend {
        fingerprint: "development".into(),
        official: false,
    };
    assert!(fixture.observe(true, &std::cell::Cell::new(0)).is_err());
    assert_eq!(*fixture.calls.lock().unwrap(), ["host.describe"]);
}
#[test]
fn incompatible_native_generation_is_reported_without_takeover() {
    let mut description = official(super::super::build_identity::product_version());
    description["native_client_compatibility"] = json!(2);
    let fixture = Fixture::new(description, None);
    assert!(fixture.observe(true, &std::cell::Cell::new(0)).is_err());
    assert_eq!(*fixture.calls.lock().unwrap(), ["host.describe"]);
}

#[test]
fn explicit_stop_suspends_discovery_even_if_the_same_endpoint_is_still_live() {
    let fixture = Fixture::new(
        official(super::super::build_identity::product_version()),
        None,
    );
    fixture.backend.stop_at(&fixture.root).unwrap();
    assert_eq!(
        *fixture.calls.lock().unwrap(),
        ["host.describe", "host.stop"]
    );
    let activated = std::cell::Cell::new(0);
    assert!(matches!(
        fixture.observe(true, &activated).unwrap(),
        BackendConnection::Missing
    ));
    assert_eq!(activated.get(), 0);
    assert_eq!(
        *fixture.calls.lock().unwrap(),
        ["host.describe", "host.stop"]
    );
}

#[test]
fn connected_frontend_keeps_its_project_without_hot_replacing_an_idle_host() {
    let fixture = Fixture::new(official("0.0.0"), None);
    let BackendConnection::Connected(mut current) =
        fixture.observe(true, &std::cell::Cell::new(0)).unwrap()
    else {
        panic!("compatible connection");
    };
    current.prefix = "/workspace/11111111-1111-1111-1111-111111111111".into();
    fixture.busy.store(false, Ordering::SeqCst);
    let result = fixture
        .backend
        .reconcile_at(
            &fixture.root,
            Some(&current),
            &|_| panic!("do not interrupt an attached frontend"),
            &|_| panic!("do not reload a page-local draft"),
        )
        .unwrap();
    let BackendConnection::Connected(client) = result else {
        panic!("keep connected");
    };
    assert_eq!(client.prefix, current.prefix);
    assert!(
        !fixture
            .calls
            .lock()
            .unwrap()
            .iter()
            .any(|m| m == "host.takeover")
    );
}
