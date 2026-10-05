//! macOS process-boundary handoff probe; the OS command is a recording fixture,
//! so the full gate never opens windows. GUI operation is checked separately.
use super::*;
use std::os::unix::fs::PermissionsExt;

fn executable(path: &Path, script: &str) {
    std::fs::write(path, script).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

#[test]
fn host_first_start_hands_off_clean_url_once_and_attach_does_not_open() {
    let _port = FIXED_BACKGROUND_PORT.lock().unwrap();
    let root = temp_root("browser-handoff");
    let home = root.join("home");
    let project = root.join("project");
    let bin = root.join("bin");
    for path in [&home, &project, &bin] {
        std::fs::create_dir_all(path).unwrap();
    }
    let marker = root.join("browser-url");
    executable(&bin.join("launchctl"), "#!/bin/sh\nprintf 'Aqua\\n'\n");
    executable(
        &bin.join("open"),
        "#!/bin/sh\nprintf '%s\\n' \"$1\" >> \"$CLAT_BROWSER_MARKER\"\n",
    );
    let mut cleanup = HostCleanup {
        root: &root,
        home: &home,
        project: &project,
        armed: true,
    };
    let command = || {
        let mut command = host_command(&home, &project);
        for key in ["CI", "SSH_CONNECTION", "SSH_TTY", "SSH_CLIENT"] {
            command.env_remove(key);
        }
        command
            .env(
                "PATH",
                format!(
                    "{}:{}",
                    bin.display(),
                    std::env::var("PATH").unwrap_or_default()
                ),
            )
            .env("CLAT_BROWSER_MARKER", &marker);
        command
    };
    let quiet = command_output(
        command().args(["host", "start", "--trust", "--no-open"]),
        &root,
        "browser-disabled",
    );
    assert!(quiet.status.success(), "{:?}", quiet);
    assert!(
        !marker.exists(),
        "--no-open suppresses both parent and child"
    );
    assert!(
        command_output(command().args(["host", "stop"]), &root, "quiet-stop")
            .status
            .success()
    );
    wait_for_host_stop(&home, &project);
    let started = command_output(
        command().args(["host", "start", "--trust"]),
        &root,
        "browser-start",
    );
    assert!(started.status.success(), "{:?}", started);
    let deadline = Instant::now() + WAIT;
    while !marker.is_file() {
        assert!(
            Instant::now() < deadline,
            "host start must launch before exiting"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    let urls = std::fs::read_to_string(&marker).unwrap();
    assert_eq!(
        urls.lines().count(),
        1,
        "child serve must not open a second browser"
    );
    assert!(urls.starts_with("http://127.0.0.1:2691/"));
    assert!(!urls.contains("token") && !urls.contains("?t="));
    let attached = command_output(command().args(["host", "start"]), &root, "browser-attach");
    assert!(attached.status.success());
    assert_eq!(std::fs::read_to_string(&marker).unwrap(), urls);
    let stopped = command_output(command().args(["host", "stop"]), &root, "browser-stop");
    assert!(stopped.status.success());
    wait_for_host_stop(&home, &project);
    cleanup.armed = false;
    remove_tree(&root);
}
