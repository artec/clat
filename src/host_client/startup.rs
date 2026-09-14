//! Startup only. Reconnection deliberately has no path back into this module.
use super::HostClient;
use crate::{BootstrapApplication, Project};
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

impl HostClient {
    pub fn start_command(args: impl Iterator<Item = String>) -> Result<String, String> {
        let args: Vec<_> = args.collect();
        let trust = match args.as_slice() {
            [] => false,
            [arg] if arg == "--trust" => true,
            _ => return Err("use clat host start [--trust]".into()),
        };
        let project = Project::current().map_err(|error| error.to_string())?;
        let client = Self::spawn_or_attach(&project, trust)?;
        Ok(format!(
            "Host {} online at 127.0.0.1:{}",
            client.instance, client.port
        ))
    }
    pub fn project_needs_trust(project: &Project) -> Result<bool, String> {
        BootstrapApplication::open_default(project.clone())
            .and_then(|boot| boot.is_trusted())
            .map(|trusted| !trusted)
            .map_err(|error| error.to_string())
    }

    pub fn spawn_or_attach(project: &Project, trust: bool) -> Result<Self, String> {
        if Self::project_needs_trust(project)? && !trust {
            return Err("project is not trusted; explicit consent is required".into());
        }
        let root = crate::control_storage::sentinel::default_storage_root()?;
        super::discovery::validate_existing(&root)?;
        if let Ok(client) = Self::discover(&root) {
            return client.open_project(project.root(), trust);
        }
        let executable = std::env::current_exe().map_err(|error| error.to_string())?;
        let deadline = Instant::now() + Duration::from_secs(30);
        let mut child = None;
        let mut attempted = false;
        loop {
            if let Ok(client) = Self::discover(&root) {
                if let Some(mut child) = child.take() {
                    // Reap a losing child or the long-lived host when it eventually exits.
                    std::thread::spawn(move || {
                        let _ = std::process::Child::wait(&mut child);
                    });
                }
                return client.open_project(project.root(), trust);
            }
            if !attempted {
                let lease = crate::session::root_lease::try_acquire(&root)
                    .map_err(|error| format!("cannot inspect host lease: {error}"))?;
                if let Some(lease) = lease {
                    drop(lease);
                    child = Some(spawn_host(&executable, project.root(), trust)?);
                    attempted = true;
                }
            }
            if Instant::now() >= deadline {
                if let Some(mut child) = child {
                    std::thread::spawn(move || {
                        let _ = child.wait();
                    });
                }
                return Err("host startup did not become ready; no operation retried. Check `clat host status` or run `clat serve` explicitly".into());
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }
}

fn spawn_host(
    executable: &Path,
    project: &Path,
    trust: bool,
) -> Result<std::process::Child, String> {
    let mut command = Command::new(executable);
    command
        .args(["serve", "--port", "0"])
        .current_dir(project)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    if trust {
        command.arg("--trust");
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        // Only async-signal-safe setsid runs between fork and exec.
        unsafe {
            command.pre_exec(|| {
                if libc::setsid() == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP; no inherited console.
        command.creation_flags(0x00000008 | 0x00000200);
    }
    command
        .spawn()
        .map_err(|error| format!("cannot start host: {error}"))
}
