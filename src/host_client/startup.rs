//! Startup only. Reconnection deliberately has no path back into this module.
use super::HostClient;
use crate::{BootstrapApplication, Project};
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

impl HostClient {
    pub fn start_command(args: impl Iterator<Item = String>) -> Result<String, String> {
        Self::start_command_presented(args)
            .map(|m| m.english())
            .map_err(|e| e.to_string())
    }
    pub fn start_command_presented(
        args: impl Iterator<Item = String>,
    ) -> Result<crate::i18n::UiMessage, crate::i18n::PresentedText> {
        let args: Vec<_> = args.collect();
        let mut trust = false;
        let mut no_open = false;
        for arg in &args {
            match arg.as_str() {
                "--trust" if !trust => trust = true,
                "--no-open" if !no_open => no_open = true,
                _ => return Err(crate::i18n::UiMessage::new("cli.host_start_usage").into()),
            }
        }
        let project = Project::current().map_err(|error| error.to_string())?;
        let client = Self::start_or_attach(&project, trust, !no_open)?;
        Ok(crate::i18n::UiMessage::new("cli.host_started")
            .arg("instance", client.instance.clone())
            .arg("url", client.browser_url()))
    }

    pub fn browser_url(&self) -> String {
        let workspace = self
            .prefix
            .strip_prefix("/workspace/")
            .map(|id| format!("?workspace={id}"))
            .unwrap_or_default();
        format!("http://127.0.0.1:{}/{workspace}", self.port)
    }

    pub fn project_needs_trust(project: &Project) -> Result<bool, String> {
        BootstrapApplication::open_default(project.clone())
            .and_then(|boot| boot.is_trusted())
            .map(|trusted| !trusted)
            .map_err(|error| error.to_string())
    }

    pub fn spawn_or_attach(project: &Project, trust: bool) -> Result<Self, String> {
        Self::start_or_attach(project, trust, false)
    }

    fn start_or_attach(project: &Project, trust: bool, open: bool) -> Result<Self, String> {
        let executable = std::env::current_exe().map_err(|error| error.to_string())?;
        Self::start_with_backend(
            project,
            trust,
            open,
            &executable,
            super::BuildMatchPolicy::RequireIfAdvertised,
        )
    }

    pub(super) fn start_with_backend(
        project: &Project,
        trust: bool,
        open: bool,
        executable: &Path,
        policy: super::BuildMatchPolicy,
    ) -> Result<Self, String> {
        if Self::project_needs_trust(project)? && !trust {
            return Err("project is not trusted; explicit consent is required".into());
        }
        let root = crate::control_storage::sentinel::default_storage_root()?;
        super::discovery::validate_existing(&root)?;
        let deadline = Instant::now() + Duration::from_secs(30);
        let mut child = None;
        let mut attempted = false;
        let mut takeover_from = None;
        loop {
            let host_missing = match Self::discover_startup_with(&root, policy.clone()) {
                Ok(super::discovery::StartupDiscovery::Compatible(client)) => {
                    if !attempted && takeover_from.is_none() && upgrade_idle_compatible(&client)? {
                        takeover_from = Some("compatible host".into());
                        continue;
                    }
                    reap_host_child(child.take());
                    let client = client.open_project(project.root(), trust)?;
                    if attempted && open {
                        super::browser::open(&client.browser_url(), false);
                    }
                    return Ok(client);
                }
                Ok(super::discovery::StartupDiscovery::Upgradeable { host_version }) => {
                    if takeover_from.is_none() {
                        request_upgrade_takeover(&root, &host_version)?;
                        takeover_from = Some(host_version);
                    }
                    false
                }
                Ok(super::discovery::StartupDiscovery::Missing) => true,
                Err(error) => {
                    reap_host_child(child.take());
                    return Err(error);
                }
            };
            if host_missing && !attempted {
                let lease = crate::session::root_lease::try_acquire(&root)
                    .map_err(|error| format!("cannot inspect host lease: {error}"))?;
                if let Some(lease) = lease {
                    ensure_background_port_available()?;
                    drop(lease);
                    child = Some(spawn_host(executable, project.root(), trust)?);
                    attempted = true;
                }
            }
            if let Some(status) = child_exit_status(&mut child)?
                && !status.success()
            {
                let lease = crate::session::root_lease::try_acquire(&root)
                    .map_err(|error| format!("cannot inspect host lease: {error}"))?;
                if let Some(lease) = lease {
                    let port = ensure_background_port_available();
                    drop(lease);
                    port?;
                }
            }
            if Instant::now() >= deadline {
                reap_host_child(child);
                if let Some(version) = takeover_from {
                    return Err(format!(
                        "CLAT {version} host accepted upgrade takeover but did not release the storage root; inspect `clat host status` before retrying"
                    ));
                }
                if !attempted {
                    return Err("storage root is busy with another writer; stop that writer explicitly before starting a host".into());
                }
                return Err("host startup did not become ready; no operation retried. Check `clat host status` or run `clat serve` explicitly".into());
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }
}

fn upgrade_idle_compatible(client: &HostClient) -> Result<bool, String> {
    let description = client.description()?;
    if description["build_kind"] != "official" {
        return Ok(false);
    }
    let version = description["product_version"]
        .as_str()
        .ok_or("host version missing")?;
    if super::build_identity::compare_product_versions(
        super::build_identity::product_version(),
        version,
    )
    .ok()
        != Some(std::cmp::Ordering::Greater)
    {
        return Ok(false);
    }
    if client.runtime_status()?["ready"] != true {
        return Ok(false);
    }
    // The readiness read is advisory. The host rechecks atomically; losing a
    // race leaves the current host running and the caller attaches normally.
    try_upgrade_takeover(client).map_err(|e| e.to_string())
}

fn request_upgrade_takeover(root: &Path, host_version: &str) -> Result<(), String> {
    let client = HostClient::discover_for_management(root)?;
    if !try_upgrade_takeover(&client).map_err(|e| e.to_string())? {
        return Err(format!(
            "CLAT {host_version} host has active work; wait until idle before upgrading"
        ));
    }
    eprintln!(
        "clat: upgrading idle background host CLAT {host_version} → CLAT {}; PWA remains at http://127.0.0.1:{}",
        super::build_identity::product_version(),
        super::DEFAULT_HOST_PORT
    );
    Ok(())
}

/// The server's atomic idle check is authoritative, not a preceding readiness read.
pub(super) fn try_upgrade_takeover(client: &HostClient) -> Result<bool, super::HostCallError> {
    match client.call_with_receipt(
        "host.takeover",
        &serde_json::json!({
            "expected_instance_id": client.instance_id(),
            "replacement_product_version": super::build_identity::product_version()
        }),
    ) {
        Ok(value) if value["stopping"] == true => Ok(true),
        Ok(_) => Err("old host did not acknowledge upgrade takeover; no stop was retried".into()),
        Err(error) if error.code.as_deref() == Some("busy") => Ok(false),
        Err(error) => Err(error),
    }
}

fn child_exit_status(
    child: &mut Option<std::process::Child>,
) -> Result<Option<std::process::ExitStatus>, String> {
    let Some(process) = child.as_mut() else {
        return Ok(None);
    };
    let status = process
        .try_wait()
        .map_err(|error| format!("cannot inspect background host: {error}"))?;
    if status.is_some() {
        child.take();
    }
    Ok(status)
}

fn ensure_background_port_available() -> Result<(), String> {
    let port = super::DEFAULT_HOST_PORT;
    match std::net::TcpListener::bind(("127.0.0.1", port)) {
        Ok(listener) => {
            drop(listener);
            Ok(())
        }
        Err(error) => Err(format!(
            "background host requires 127.0.0.1:{port}, but that port cannot be bound: {error}. \
             Free port {port}, or run `clat serve --port <n>` explicitly for a custom port"
        )),
    }
}

fn reap_host_child(child: Option<std::process::Child>) {
    if let Some(mut child) = child {
        // Reap a losing child or the long-lived host when it eventually exits.
        std::thread::spawn(move || {
            let _ = child.wait();
        });
    }
}

fn spawn_host(
    executable: &Path,
    project: &Path,
    trust: bool,
) -> Result<std::process::Child, String> {
    let mut command = Command::new(executable);
    command
        .arg("serve")
        .arg("--no-open")
        .arg("--port")
        .arg(super::DEFAULT_HOST_PORT.to_string())
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
