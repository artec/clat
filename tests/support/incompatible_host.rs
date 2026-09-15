//! Real startup client process against an authenticated, identity-matched host
//! with deliberately unsupported (zero) protocol generations.
use super::*;
use std::io::{BufRead, BufReader, Read, Write};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

struct FixtureHost {
    stop: Arc<AtomicBool>,
    worker: Option<std::thread::JoinHandle<()>>,
    application: Option<clat::TrustedProjectApplication>,
}

impl FixtureHost {
    fn new(home: &Path, project: &Path, ready: &Path) -> Self {
        let root = home.join(".clat");
        let application = BootstrapApplication::open(Project::new(project), root.clone())
            .unwrap()
            .authorize_and_mount(ProjectAuthorization::grant())
            .unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let instance = "11111111-1111-1111-1111-111111111111";
        let dir = cap_std::fs::Dir::open_ambient_dir(&root, cap_std::ambient_authority()).unwrap();
        for (name, value) in [
            ("web-token", "live-incompatible-token".to_owned()),
            ("host-endpoint.json", serde_json::json!({"port":listener.local_addr().unwrap().port(),"instance_id":instance}).to_string()),
        ] {
            clat::client_ports::private_fs::write_text_atomic(&dir, &root, name, &value).unwrap();
        }
        let description = serde_json::json!({"ok":true,"value":{
            "storage_root":root,"instance_id":instance,
            "protocol_version":0,"wire_version":0,"journal_version":0,
        }})
        .to_string();
        let stop = Arc::new(AtomicBool::new(false));
        let shutdown = stop.clone();
        let ready = ready.to_path_buf();
        let worker = std::thread::spawn(move || {
            while !shutdown.load(Ordering::Acquire) {
                match listener.accept() {
                    Ok((stream, _)) => {
                        if let Err(error) = describe(stream, &description, &ready) {
                            eprintln!("fixture describe: {error}");
                        }
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(10))
                    }
                    Err(error) => {
                        eprintln!("fixture accept: {error}");
                        return;
                    }
                }
            }
        });
        Self {
            stop,
            worker: Some(worker),
            application: Some(application),
        }
    }
}

impl Drop for FixtureHost {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(worker) = self.worker.take() {
            let deadline = Instant::now() + Duration::from_secs(3);
            while !worker.is_finished() && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(10));
            }
            if worker.is_finished() {
                worker.join().unwrap();
            } else {
                eprintln!("fixture host shutdown deadline exceeded");
            }
        }
        if let Some(application) = self.application.take() {
            application.close().unwrap();
        }
    }
}

fn describe(stream: TcpStream, description: &str, ready: &Path) -> std::io::Result<()> {
    stream.set_nonblocking(false)?;
    stream.set_read_timeout(Some(Duration::from_secs(2)))?;
    stream.set_write_timeout(Some(Duration::from_secs(2)))?;
    let mut reader = BufReader::new(stream);
    let mut authorized = false;
    for _ in 0..64 {
        let mut line = String::new();
        if reader.read_line(&mut line)? == 0 {
            return Err(std::io::ErrorKind::UnexpectedEof.into());
        }
        authorized |= line
            .trim()
            .eq_ignore_ascii_case("authorization: Bearer live-incompatible-token");
        if line == "\r\n" {
            break;
        }
    }
    if !authorized {
        return Err(std::io::ErrorKind::PermissionDenied.into());
    }
    reader.read_exact(&mut [0; 2])?;
    std::fs::write(ready, "authenticated description requested")?;
    write!(
        reader.get_mut(),
        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        description.len(),
        description
    )
}

#[test]
fn live_incompatible_host_fails_fast_without_starting_a_second_writer() {
    let root = temp_root("incompatible-host");
    let home = root.join("home");
    let project = root.join("project");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&project).unwrap();
    let ready = root.join("describe-reached");
    let fixture = FixtureHost::new(&home, &project, &ready);
    let endpoint = std::fs::read(home.join(".clat/host-endpoint.json")).unwrap();
    let mut command = host_command(&home, &project);
    command.args(["host", "start"]);
    let mut process =
        BoundedProcess::spawn(&mut command, &root, "incompatible-start", WAIT).unwrap();
    let deadline = Instant::now() + WAIT;
    while !ready.is_file() {
        assert!(
            Instant::now() < deadline,
            "startup never reached authenticated host description"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    // Exclude OS executable-start latency from the compatibility decision.
    process.shorten_deadline(Duration::from_secs(2));
    let result = process.finish();
    let unchanged = std::fs::read(home.join(".clat/host-endpoint.json")).unwrap() == endpoint;
    drop(process);
    drop(fixture);
    remove_tree(&root);
    let output =
        result.expect("live incompatible host must fail fast rather than wait for startup timeout");
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("incompatible host protocol"));
    assert!(unchanged, "startup must not publish a second host endpoint");
}
