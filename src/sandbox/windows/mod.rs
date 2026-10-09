//! Windows partial write confinement. All native process creation stays here.
//! DSH reference: restricted capability token + inheritable ACLs + Low label.
mod acl;
mod native;
mod process;
mod sid;
mod token;

use super::{PlannedCommand, SandboxFacts, SandboxLevel};
use std::ffi::OsString;
use std::path::{Path, PathBuf};
#[cfg(not(test))]
use windows_sys::Win32::System::Console::SetConsoleCtrlHandler;
#[cfg(not(test))]
use windows_sys::Win32::System::Environment::SetEnvironmentVariableW;

pub(crate) const INTERNAL_COMMAND: &str = "--internal-windows-sandbox";

fn runner_executable() -> Result<PathBuf, String> {
    let executable =
        std::env::current_exe().map_err(|error| format!("sandbox: current executable: {error}"))?;
    #[cfg(any(test, feature = "test-support"))]
    {
        let parent = executable
            .parent()
            .ok_or("sandbox: executable has no parent")?;
        let name = executable
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("");
        if parent.file_name().is_some_and(|name| name == "deps")
            && (name.starts_with("clat_core-") || name.starts_with("clat-"))
        {
            let binary = parent
                .parent()
                .ok_or("sandbox: missing target directory")?
                .join("clat.exe");
            if !binary.is_file() {
                return Err(
                    "sandbox: native Windows tests require cargo build --bin clat first".into(),
                );
            }
            return Ok(binary);
        }
    }
    Ok(executable)
}
const PARTIAL: &str =
    "reads unrestricted; network open; NTFS hard-link aliases can bypass path confinement";

pub(super) fn plan(
    program: OsString,
    args: Vec<OsString>,
    level: SandboxLevel,
    workspace: &Path,
    scratch: &Path,
) -> Result<PlannedCommand, String> {
    let (workspace, scratch) = roots(workspace, scratch)?;
    let mode = mode_name(level)?;
    let executable = runner_executable()?;
    probe(&executable)?;
    let digest = format!(
        "{:x}",
        sha2::Sha256::digest(format!("windows-acl-v1:{mode}:{workspace:?}:{scratch:?}"))
    );
    let mut wrapped = vec![
        INTERNAL_COMMAND.into(),
        "--workspace".into(),
        workspace.into_os_string(),
        "--temp".into(),
        scratch.into_os_string(),
        "--mode".into(),
        mode.into(),
        "--".into(),
        program,
    ];
    wrapped.extend(args);
    Ok(PlannedCommand {
        program: executable.into_os_string(),
        args: wrapped,
        facts: SandboxFacts {
            provider: "windows-acl".into(),
            mode: level,
            enforcement: "partial".into(),
            policy_digest: Some(digest),
            fallback_reason: Some(PARTIAL.into()),
        },
    })
}

use sha2::Digest;
fn mode_name(level: SandboxLevel) -> Result<&'static str, String> {
    match level {
        SandboxLevel::ReadOnly => Ok("read-only"),
        SandboxLevel::WorkspaceWrite => Ok("workspace-write"),
        SandboxLevel::ProjectReadTempWrite => Ok("project-read-temp-write"),
        SandboxLevel::FullAccess => Err("windows-acl-run: Full Access must bypass runner".into()),
    }
}

fn roots(workspace: &Path, scratch: &Path) -> Result<(PathBuf, PathBuf), String> {
    let directory = |path: &Path| -> Result<PathBuf, String> {
        let canonical = path.canonicalize().map_err(|error| {
            format!(
                "windows-acl-run: cannot resolve {}: {error}",
                path.display()
            )
        })?;
        if !canonical.is_dir() {
            return Err("windows-acl-run: roots must be existing directories".into());
        }
        canonical
            .to_str()
            .ok_or("windows-acl-run: roots must be Unicode")?;
        Ok(canonical)
    };
    let workspace = directory(workspace)?;
    let scratch = directory(scratch)?;
    if workspace.starts_with(&scratch) || scratch.starts_with(&workspace) {
        return Err("windows-acl-run: workspace and private temp must not intersect".into());
    }
    Ok((workspace, scratch))
}

struct Request {
    workspace: PathBuf,
    scratch: PathBuf,
    level: SandboxLevel,
    command: Vec<OsString>,
}

fn parse(args: Vec<OsString>) -> Result<Request, String> {
    let mut workspace = None;
    let mut scratch = None;
    let mut level = None;
    let mut args = args.into_iter();
    loop {
        let flag = args.next().ok_or("windows-acl-run: missing -- command")?;
        if flag == "--" {
            break;
        }
        let value = args.next().ok_or("windows-acl-run: missing option value")?;
        match flag.to_str() {
            Some("--workspace") if workspace.is_none() => workspace = Some(PathBuf::from(value)),
            Some("--temp") if scratch.is_none() => scratch = Some(PathBuf::from(value)),
            Some("--mode") if level.is_none() => {
                level = Some(match value.to_str() {
                    Some("read-only") => SandboxLevel::ReadOnly,
                    Some("workspace-write") => SandboxLevel::WorkspaceWrite,
                    Some("project-read-temp-write") => SandboxLevel::ProjectReadTempWrite,
                    _ => return Err("windows-acl-run: invalid mode".into()),
                })
            }
            _ => return Err("windows-acl-run: unknown or duplicate option".into()),
        }
    }
    let workspace = workspace.ok_or("windows-acl-run: missing workspace")?;
    let scratch = scratch.ok_or("windows-acl-run: missing private temp")?;
    let (workspace, scratch) = roots(&workspace, &scratch)?;
    let level = level.ok_or("windows-acl-run: missing mode")?;
    let command: Vec<_> = args.collect();
    if command.is_empty() {
        return Err("windows-acl-run: missing command".into());
    }
    Ok(Request {
        workspace,
        scratch,
        level,
        command,
    })
}

fn probe(executable: &Path) -> Result<(), String> {
    static PROBE: std::sync::OnceLock<Result<(), String>> = std::sync::OnceLock::new();
    PROBE
        .get_or_init(|| {
            let status = crate::process::probe_command(
                executable,
                &[INTERNAL_COMMAND, "--probe"],
                std::time::Duration::from_secs(5),
            )?;
            if status.success() {
                Ok(())
            } else {
                Err(format!(
                    "sandbox: Windows runner probe failed ({status}); refusing unconfined execution"
                ))
            }
        })
        .clone()
}

pub(crate) fn run_internal(args: Vec<OsString>) -> u32 {
    let result = if args == [OsString::from("--probe")] {
        token::restricted(&[])
            .and_then(native::Handle::close)
            .map(|()| 0)
    } else {
        parse(args).and_then(execute)
    };
    match result {
        Ok(exit) => exit,
        Err(error) => {
            eprintln!("windows-acl-run: {error}");
            127
        }
    }
}

fn execute(request: Request) -> Result<u32, String> {
    native::invoke("SetConsoleCtrlHandler", || {
        #[cfg(test)]
        {
            1
        }
        #[cfg(not(test))]
        unsafe {
            SetConsoleCtrlHandler(None, 1)
        }
    })?;
    let workspace = sid::Sid::parse(&sid::identity(&request.workspace, false)?)?;
    let scratch = sid::Sid::parse(&sid::identity(&request.scratch, true)?)?;
    let writes = prepare(&request, &workspace, &scratch)?;
    let token = token::restricted(&writes)?;
    let result = process::run(&token, &request.command);
    let closed = token.close();
    result.and_then(|exit| closed.map(|()| exit))
}

fn prepare<'a>(
    request: &Request,
    workspace: &'a sid::Sid,
    scratch: &'a sid::Sid,
) -> Result<Vec<&'a sid::Sid>, String> {
    let mut writes = Vec::new();
    if request.level == SandboxLevel::WorkspaceWrite {
        acl::grant(&request.workspace, workspace)?;
        writes.push(workspace);
    }
    if request.level != SandboxLevel::ReadOnly {
        acl::grant(&request.scratch, scratch)?;
        writes.push(scratch);
        let value = native::wide(request.scratch.as_os_str())?;
        for name in ["TMP", "TEMP"] {
            let name = native::wide(name.as_ref())?;
            native::invoke("SetEnvironmentVariableW(TMP/TEMP)", || {
                #[cfg(test)]
                {
                    let _ = (&name, &value);
                    1
                }
                #[cfg(not(test))]
                unsafe {
                    SetEnvironmentVariableW(name.as_ptr(), value.as_ptr())
                }
            })?;
        }
    }
    Ok(writes)
}

/// The run-roots owner calls this before deleting its private directory. It
/// must never revoke per command: concurrent commands can share one run root.
pub(crate) fn revoke_scratch(path: &Path) -> Result<(), String> {
    let path = path
        .canonicalize()
        .map_err(|error| format!("windows-acl-run: scratch canonicalize: {error}"))?;
    acl::revoke(&path, &sid::Sid::parse(&sid::identity(&path, true)?)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn parser_rejects_forged_capability_and_duplicate_flags() {
        for arguments in [
            vec!["--write-sid", "S-1-4-1-2"],
            vec!["--mode", "read-only", "--mode", "workspace-write"],
            vec!["--mode", "full-access"],
            vec!["--temp"],
        ] {
            assert!(parse(arguments.into_iter().map(OsString::from).collect()).is_err());
        }
    }
    #[test]
    fn win32_errors_never_become_success() {
        assert!(
            native::code("InjectedAclFailure", 5)
                .unwrap_err()
                .contains("Win32 5")
        );
        assert!(
            native::check("InjectedTokenFailure", 0)
                .unwrap_err()
                .contains("InjectedTokenFailure")
        );
        assert!(native::Handle::new("InjectedRunnerFailure", std::ptr::null_mut()).is_err());
    }
}

#[cfg(test)]
mod failure_tests {
    use super::*;
    fn fixture() -> (PathBuf, Request, PathBuf) {
        let root = std::env::temp_dir().join(format!("clat-acl-failure-{}", uuid::Uuid::new_v4()));
        let workspace = root.join("workspace");
        let scratch = root.join("scratch");
        std::fs::create_dir_all(&workspace).unwrap();
        std::fs::create_dir_all(&scratch).unwrap();
        let marker = scratch.join("child-ran");
        let command = vec![
            "cmd.exe".into(),
            "/d".into(),
            "/s".into(),
            "/c".into(),
            format!("echo escaped > \"{}\"", marker.display()).into(),
        ];
        let (workspace, scratch) = roots(&workspace, &scratch).unwrap();
        (
            root,
            Request {
                workspace,
                scratch,
                level: SandboxLevel::WorkspaceWrite,
                command,
            },
            marker,
        )
    }

    #[test]
    fn injected_acl_token_job_and_spawn_failures_never_execute_unconfined_child() {
        let apis = [
            "SetConsoleCtrlHandler",
            "LockFileEx",
            "GetNamedSecurityInfoW",
            "InitializeAcl",
            "AddMandatoryAce",
            "SetEntriesInAclW",
            "SetNamedSecurityInfoW",
            "SetEnvironmentVariableW(TMP/TEMP)",
            "OpenProcessToken",
            "GetTokenInformation(size)",
            "GetTokenInformation",
            "CreateRestrictedToken",
            "SetEntriesInAclW(default)",
            "SetTokenInformation(default)",
            "SetTokenInformation(Low)",
            "CreateJobObjectW",
            "SetInformationJobObject",
            "CreateProcessAsUserW",
            "AssignProcessToJobObject",
            "ResumeThread",
        ];
        for api in apis {
            let (root, request, marker) = fixture();
            let guard = native::injection::Guard::fail(api);
            let error = execute(request).expect_err(api);
            assert!(error.contains(api), "{api}: {error}");
            assert!(error.contains("Win32 5"), "{api}: {error}");
            let calls = guard.calls();
            assert!(calls.contains(&api), "failure hook not reached: {api}");
            if ![
                "CreateProcessAsUserW",
                "AssignProcessToJobObject",
                "ResumeThread",
            ]
            .contains(&api)
            {
                assert!(
                    !calls.contains(&"CreateProcessAsUserW"),
                    "spawn after {api} failure"
                );
            }
            assert!(!marker.exists(), "child ran after {api} failure");
            drop(guard);
            std::fs::remove_dir_all(root).unwrap();
        }
    }

    #[test]
    fn injected_temp_revoke_error_is_reported_to_lifecycle_owner() {
        let (root, request, _) = fixture();
        acl::grant(
            &request.scratch,
            &sid::Sid::parse(&sid::identity(&request.scratch, true).unwrap()).unwrap(),
        )
        .unwrap();
        let guard = native::injection::Guard::fail("SetEntriesInAclW");
        assert!(
            revoke_scratch(&request.scratch)
                .unwrap_err()
                .contains("SetEntriesInAclW")
        );
        drop(guard);
        revoke_scratch(&request.scratch).unwrap();
        std::fs::remove_dir_all(root).unwrap();
    }
}
