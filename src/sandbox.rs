//! Platform sandbox policy planning for ProcessService.

use crate::permission::PermissionMode;
use serde_json::{Value, json};
#[cfg(target_os = "macos")]
use sha2::{Digest, Sha256};
use std::ffi::OsString;
#[cfg(target_os = "macos")]
use std::path::Path;
use std::path::PathBuf;
#[cfg(target_os = "macos")]
use std::sync::OnceLock;
use std::sync::{Arc, RwLock};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum SandboxRequest {
    Auto,
    Required,
    Off,
}

impl SandboxRequest {
    pub(crate) fn parse(value: Option<&str>) -> Result<Self, String> {
        match value.unwrap_or("auto") {
            "auto" => Ok(Self::Auto),
            "required" => Ok(Self::Required),
            "off" => Ok(Self::Off),
            other => Err(format!("unknown sandbox mode `{other}`")),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum SandboxLevel {
    ReadOnly,
    WorkspaceWrite,
    FullAccess,
}

impl SandboxLevel {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::ReadOnly => "read-only",
            Self::WorkspaceWrite => "workspace-write",
            Self::FullAccess => "full-access",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct SandboxFacts {
    pub provider: String,
    pub mode: SandboxLevel,
    pub enforcement: String,
    pub policy_digest: Option<String>,
    pub fallback_reason: Option<String>,
}

impl SandboxFacts {
    pub(crate) fn json(&self, denied: bool, unavailable: bool) -> Value {
        json!({
            "provider": self.provider,
            "mode": self.mode.as_str(),
            "enforcement": if unavailable { "unusable" } else { self.enforcement.as_str() },
            "policy_digest": self.policy_digest,
            "fallback_reason": self.fallback_reason,
            "denied": denied,
            "unavailable": unavailable,
        })
    }

    pub(crate) fn denied(&self, stderr: &str) -> bool {
        self.provider == "seatbelt"
            && stderr
                .to_ascii_lowercase()
                .contains("operation not permitted")
    }

    pub(crate) fn unavailable(&self, output: &str) -> bool {
        self.provider == "seatbelt" && output.to_ascii_lowercase().contains("sandbox-exec:")
    }
}

#[derive(Clone, Debug)]
pub(crate) struct PlannedCommand {
    pub program: OsString,
    pub args: Vec<OsString>,
    pub facts: SandboxFacts,
}

#[derive(Clone)]
pub(crate) enum SandboxModeSource {
    Classic,
    Shared(Arc<RwLock<PermissionMode>>),
}

impl SandboxModeSource {
    fn level(&self) -> SandboxLevel {
        match self {
            Self::Classic => SandboxLevel::WorkspaceWrite,
            Self::Shared(mode) => match *mode.read().expect("permission mode lock") {
                PermissionMode::ReadOnly => SandboxLevel::ReadOnly,
                PermissionMode::ProjectWrite => SandboxLevel::WorkspaceWrite,
                PermissionMode::FullAccess => SandboxLevel::FullAccess,
            },
        }
    }
}

pub(crate) struct SandboxService {
    /// seatbelt（macOS）读取它生成路径字面量；bwrap/AppContainer 等
    /// provider 落地后同样读取。在尚无 provider 的平台上仅构造写入。
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    project_root: PathBuf,
    mode: SandboxModeSource,
}

impl SandboxService {
    pub(crate) fn new(project_root: PathBuf, mode: SandboxModeSource) -> Result<Self, String> {
        let project_root = project_root
            .canonicalize()
            .map_err(|error| format!("sandbox: cannot resolve project root: {error}"))?;
        Ok(Self { project_root, mode })
    }

    pub(crate) fn plan(
        &self,
        program: OsString,
        args: Vec<OsString>,
        request: SandboxRequest,
        network: bool,
    ) -> Result<PlannedCommand, String> {
        let level = self.mode.level();
        if request == SandboxRequest::Off {
            if level != SandboxLevel::FullAccess {
                return Err("sandbox=off requires interactive Full Access mode".into());
            }
            return Ok(raw_plan(
                program,
                args,
                level,
                Some("explicit Full Access bypass"),
            ));
        }
        if level == SandboxLevel::FullAccess {
            if request == SandboxRequest::Required {
                return Err(
                    "sandbox=required conflicts with Full Access; switch to Project Write".into(),
                );
            }
            return Ok(raw_plan(
                program,
                args,
                level,
                Some("Full Access is unconfined"),
            ));
        }

        #[cfg(target_os = "macos")]
        {
            let executable = Path::new("/usr/bin/sandbox-exec");
            probe_seatbelt(executable)?;
            let profile = seatbelt_profile(level, &self.project_root, network)?;
            let digest = format!("{:x}", Sha256::digest(profile.as_bytes()));
            let mut wrapped = vec![
                OsString::from("-p"),
                OsString::from(profile),
                OsString::from("--"),
            ];
            wrapped.push(program);
            wrapped.extend(args);
            Ok(PlannedCommand {
                program: executable.as_os_str().to_owned(),
                args: wrapped,
                facts: SandboxFacts {
                    provider: "seatbelt".into(),
                    mode: level,
                    enforcement: "full".into(),
                    policy_digest: Some(digest),
                    fallback_reason: None,
                },
            })
        }

        #[cfg(not(target_os = "macos"))]
        {
            let _ = network;
            if request == SandboxRequest::Required {
                return Err(format!(
                    "sandbox: required but no graduated provider exists for {}",
                    std::env::consts::OS
                ));
            }
            Ok(raw_plan(
                program,
                args,
                level,
                Some("no graduated sandbox provider on this platform"),
            ))
        }
    }
}

#[cfg(target_os = "macos")]
fn probe_seatbelt(executable: &Path) -> Result<(), String> {
    static PROBE: OnceLock<Result<(), String>> = OnceLock::new();
    PROBE
        .get_or_init(|| {
            if !executable.is_file() {
                return Err(
                    "sandbox: /usr/bin/sandbox-exec is unavailable; refusing unconfined execution"
                        .into(),
                );
            }
            let profile = "(version 1) (allow default) (deny file-write*) \
                           (allow file-write* (literal \"/dev/null\")) (deny network*)";
            let status = crate::process::probe_command(
                executable,
                &["-p", profile, "--", "/usr/bin/true"],
                std::time::Duration::from_secs(5),
            )?;
            if status.success() {
                Ok(())
            } else {
                Err(format!(
                    "sandbox: sandbox-exec rejected the functional probe ({status}); refusing unconfined execution"
                ))
            }
        })
        .clone()
}

fn raw_plan(
    program: OsString,
    args: Vec<OsString>,
    level: SandboxLevel,
    reason: Option<&str>,
) -> PlannedCommand {
    PlannedCommand {
        program,
        args,
        facts: SandboxFacts {
            provider: "none".into(),
            mode: level,
            enforcement: "none".into(),
            policy_digest: None,
            fallback_reason: reason.map(str::to_owned),
        },
    }
}

#[cfg(target_os = "macos")]
fn seatbelt_profile(
    level: SandboxLevel,
    project_root: &Path,
    network: bool,
) -> Result<String, String> {
    let mut forms = vec![
        "(version 1)".to_owned(),
        "(allow default)".to_owned(),
        "(deny file-write*)".to_owned(),
        "(allow file-write* (literal \"/dev/null\"))".to_owned(),
    ];
    if !network {
        forms.push("(deny network*)".into());
    }
    if level == SandboxLevel::WorkspaceWrite {
        let mut roots = vec![project_root.to_path_buf(), PathBuf::from("/tmp")];
        roots.push(std::env::temp_dir());
        let mut canonical = Vec::new();
        for root in roots {
            let root = root.canonicalize().unwrap_or(root);
            if !canonical.contains(&root) {
                canonical.push(root);
            }
        }
        let clauses = canonical
            .iter()
            .map(|root| format!("(subpath {})", sbpl_string(root)))
            .collect::<Vec<_>>()
            .join(" ");
        forms.push(format!("(allow file-write* {clauses})"));
    }
    Ok(forms.join(" "))
}

#[cfg(target_os = "macos")]
fn sbpl_string(path: &Path) -> String {
    let text = path.to_string_lossy();
    format!("\"{}\"", text.replace('\\', "\\\\").replace('"', "\\\""))
}

#[cfg(test)]
mod tests {
    use super::*;
    // 这三个导入只被 macOS 门控的 seatbelt 测试使用。
    #[cfg(target_os = "macos")]
    use crate::{CancelToken, Project};
    #[cfg(target_os = "macos")]
    use std::time::Duration;

    #[test]
    fn off_requires_full_access_and_fallback_is_explicit() {
        let root = std::env::current_dir().unwrap();
        let service = SandboxService::new(root, SandboxModeSource::Classic).unwrap();
        assert!(
            service
                .plan("sh".into(), vec![], SandboxRequest::Off, false)
                .unwrap_err()
                .contains("Full Access")
        );
        #[cfg(not(target_os = "macos"))]
        {
            let auto = service
                .plan("sh".into(), vec![], SandboxRequest::Auto, false)
                .unwrap();
            assert_eq!(auto.facts.enforcement, "none");
            assert!(auto.facts.fallback_reason.is_some());
            assert!(
                service
                    .plan("sh".into(), vec![], SandboxRequest::Required, false)
                    .is_err()
            );
        }
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn seatbelt_profile_is_path_escaped_and_network_explicit() {
        let profile =
            seatbelt_profile(SandboxLevel::WorkspaceWrite, Path::new("/tmp/a\"b"), false).unwrap();
        assert!(profile.contains("(deny network*)"));
        assert!(profile.contains("a\\\"b"));
        assert!(profile.contains("(deny file-write*)"));
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn real_seatbelt_denies_world_effects_and_allows_workspace_write() {
        let base = std::env::current_dir()
            .unwrap()
            .join("target")
            .join(format!(
                "clat-seatbelt-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
        let project_root = base.join("project");
        let outside = base.join("outside");
        std::fs::create_dir_all(&project_root).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        let mode = Arc::new(RwLock::new(PermissionMode::ProjectWrite));
        let sandbox = Arc::new(
            SandboxService::new(
                project_root.clone(),
                SandboxModeSource::Shared(Arc::clone(&mode)),
            )
            .unwrap(),
        );
        let service = crate::process::ProcessService::new(Project::new(&project_root), sandbox);
        let generation = service.bind_run("seatbelt", CancelToken::new()).unwrap();

        let inside = service
            .run_compat(
                "printf allowed > allowed.txt",
                Duration::from_secs(3),
                false,
                SandboxRequest::Required,
            )
            .unwrap();
        assert_eq!(inside.exit_code, Some(0), "{}", inside.stderr);
        assert_eq!(
            std::fs::read_to_string(project_root.join("allowed.txt")).unwrap(),
            "allowed"
        );

        let readable_path = outside.join("readable.txt");
        std::fs::write(&readable_path, "account-readable").unwrap();
        let outside_read = service
            .run_compat(
                &format!("cat {}", shell_quote(&readable_path)),
                Duration::from_secs(3),
                false,
                SandboxRequest::Required,
            )
            .unwrap();
        assert_eq!(outside_read.exit_code, Some(0), "{}", outside_read.stderr);
        assert_eq!(outside_read.stdout, "account-readable");

        let temp_write_path = std::env::temp_dir().join(format!(
            "clat-seatbelt-temp-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let temp_write = service
            .run_compat(
                &format!("printf temp > {}", shell_quote(&temp_write_path)),
                Duration::from_secs(3),
                false,
                SandboxRequest::Required,
            )
            .unwrap();
        assert_eq!(temp_write.exit_code, Some(0), "{}", temp_write.stderr);
        assert_eq!(std::fs::read_to_string(&temp_write_path).unwrap(), "temp");
        std::fs::remove_file(&temp_write_path).unwrap();

        let inherited_environment = service
            .run_compat(
                "test -n \"$PATH\"",
                Duration::from_secs(3),
                false,
                SandboxRequest::Required,
            )
            .unwrap();
        assert_eq!(
            inherited_environment.exit_code,
            Some(0),
            "Seatbelt is not an inherited-environment filter"
        );

        let denied_path = outside.join("denied.txt");
        let outside_write = service
            .run_compat(
                &format!("printf denied > {}", shell_quote(&denied_path)),
                Duration::from_secs(3),
                false,
                SandboxRequest::Required,
            )
            .unwrap();
        assert_ne!(outside_write.exit_code, Some(0));
        assert!(outside_write.sandbox_denied, "{}", outside_write.stderr);
        assert!(!denied_path.exists());

        let budget_denied_path = outside.join("budget-denied.txt");
        let budget_denied = service
            .start(crate::process::ProcessStart {
                command: format!(
                    "head -c 100000 /dev/zero; printf denied > {}",
                    shell_quote(&budget_denied_path)
                ),
                workdir: None,
                tty: false,
                network: false,
                sandbox: SandboxRequest::Required,
            })
            .unwrap();
        let budget_denied = service
            .wait_and_consume(budget_denied, Duration::from_secs(3), 1024)
            .unwrap();
        assert_eq!(budget_denied.stdout_bytes, 1024);
        assert!(budget_denied.stderr.is_empty());
        assert!(
            budget_denied.sandbox_denied,
            "sandbox denial must not depend on the result output budget"
        );
        assert!(!budget_denied_path.exists());

        std::os::unix::fs::symlink(&outside, project_root.join("escape")).unwrap();
        let symlink_escape = service
            .run_compat(
                "printf denied > escape/denied.txt",
                Duration::from_secs(3),
                false,
                SandboxRequest::Required,
            )
            .unwrap();
        assert!(symlink_escape.sandbox_denied, "{}", symlink_escape.stderr);
        assert!(!outside.join("denied.txt").exists());

        *mode.write().unwrap() = PermissionMode::ReadOnly;
        let read_only = service
            .run_compat(
                "printf denied > read-only.txt",
                Duration::from_secs(3),
                false,
                SandboxRequest::Required,
            )
            .unwrap();
        assert!(read_only.sandbox_denied, "{}", read_only.stderr);
        assert!(!project_root.join("read-only.txt").exists());

        *mode.write().unwrap() = PermissionMode::ProjectWrite;
        if Path::new("/usr/bin/nc").is_file() {
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            let port = listener.local_addr().unwrap().port();
            let (accepted_tx, accepted_rx) = std::sync::mpsc::channel();
            let accept = std::thread::spawn(move || {
                let result = listener.accept().map(|_| ());
                let _ = accepted_tx.send(result);
            });
            let denied_network = service
                .run_compat(
                    &format!("/usr/bin/nc -z 127.0.0.1 {port}"),
                    Duration::from_secs(3),
                    false,
                    SandboxRequest::Required,
                )
                .unwrap();
            assert_ne!(denied_network.exit_code, Some(0), "{denied_network:?}");
            assert!(
                accepted_rx.try_recv().is_err(),
                "denied call must not connect"
            );

            let allowed_network = service
                .run_compat(
                    &format!("/usr/bin/nc -z 127.0.0.1 {port}"),
                    Duration::from_secs(3),
                    true,
                    SandboxRequest::Required,
                )
                .unwrap();
            assert_eq!(allowed_network.exit_code, Some(0), "{allowed_network:?}");
            accepted_rx
                .recv_timeout(Duration::from_secs(3))
                .expect("network=true reaches listener")
                .expect("accept");
            accept.join().unwrap();
        }

        service.unbind_run(generation).unwrap();
        service.close().unwrap();
        crate::test_support::cleanup_tree(&base);
    }

    #[cfg(target_os = "macos")]
    fn shell_quote(path: &Path) -> String {
        format!("'{}'", path.to_string_lossy().replace('\'', "'\\''"))
    }
}
