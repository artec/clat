use std::{
    ffi::OsStr,
    os::unix::process::CommandExt,
    path::Path,
    process::{Command, Stdio},
};
use tauri::Manager;

// Wait for the old process, including its Launch Services and single-instance
// registration, before opening the updated bundle without forcing an instance.
const AFTER_EXIT: &str = r#"
pid=$1
shift
remaining=300
while kill -0 "$pid" 2>/dev/null; do
    [ "$remaining" -gt 0 ] || exit 1
    remaining=$((remaining - 1))
    /bin/sleep 0.1
done
exec "$@"
"#;

pub(super) fn installed_app_command(app: &tauri::AppHandle) -> Option<Command> {
    let env = app.env();
    let executable = tauri::process::current_binary(&env).ok()?;
    let mut command = open_bundle_after_exit(app_bundle(&executable)?, std::process::id());
    command.args(env.args_os.iter().skip(1));
    Some(command)
}

fn open_bundle_after_exit(bundle: &Path, pid: u32) -> Command {
    let mut command = after_exit(pid);
    command.arg("/usr/bin/open").arg(bundle).arg("--args");
    command
}

fn app_bundle(executable: &Path) -> Option<&Path> {
    let macos = executable.parent()?;
    let contents = macos.parent()?;
    let bundle = contents.parent()?;
    (macos.file_name() == Some(OsStr::new("MacOS"))
        && contents.file_name() == Some(OsStr::new("Contents"))
        && bundle.extension() == Some(OsStr::new("app")))
    .then_some(bundle)
}

fn after_exit(pid: u32) -> Command {
    let mut command = Command::new("/bin/sh");
    command
        .args(["-c", AFTER_EXIT, "clat-update-relaunch"])
        .arg(pid.to_string())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0);
    command
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    #[test]
    fn macos_update_relaunch_targets_the_original_bundle() {
        let bundle = Path::new("/Applications/CLAT Preview.app");
        let executable = bundle.join("Contents/MacOS/clat-desktop");
        assert_eq!(app_bundle(&executable), Some(bundle));
        assert_eq!(app_bundle(Path::new("/tmp/clat-desktop")), None);
        let command = open_bundle_after_exit(bundle, 42);
        assert_eq!(
            command.get_args().skip(3).collect::<Vec<_>>(),
            [
                "42",
                "/usr/bin/open",
                "/Applications/CLAT Preview.app",
                "--args"
            ]
        );
    }

    #[test]
    fn macos_update_relaunch_waits_for_the_old_process() {
        let mut old = Command::new("/bin/cat")
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .spawn()
            .unwrap();
        let mut launch = after_exit(old.id()).arg("/usr/bin/true").spawn().unwrap();
        std::thread::sleep(Duration::from_millis(250));
        let early = launch.try_wait().unwrap();
        drop(old.stdin.take());
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            if old.try_wait().unwrap().is_some()
                && let Some(status) = launch.try_wait().unwrap()
            {
                assert!(status.success());
                break;
            }
            if Instant::now() >= deadline {
                let _ = old.kill();
                let _ = launch.kill();
                panic!("relaunch did not run after process exit");
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(
            early.is_none(),
            "relaunch ran while the old process was alive"
        );
    }
}
