//! System browser handoff. Arguments are generated locally; no shell, URL
//! credentials, new dependency, or frontend-owned subprocess is involved.
use std::process::{Command, Stdio};

fn suppressed(env: impl Fn(&str) -> Option<String>) -> bool {
    ["SSH_CONNECTION", "SSH_CLIENT", "SSH_TTY", "CI"]
        .iter()
        .any(|key| env(key).is_some_and(|value| !value.is_empty()))
        || env("SESSIONNAME").is_some_and(|value| value.eq_ignore_ascii_case("services"))
}

fn graphical_session() -> bool {
    if suppressed(|key| std::env::var(key).ok()) {
        return false;
    }
    #[cfg(target_os = "macos")]
    {
        Command::new("launchctl")
            .arg("managername")
            .stderr(Stdio::null())
            .output()
            .is_ok_and(|out| out.status.success() && out.stdout.starts_with(b"Aqua"))
    }
    #[cfg(target_os = "windows")]
    {
        true
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        ["DISPLAY", "WAYLAND_DISPLAY"]
            .iter()
            .any(|key| std::env::var(key).is_ok_and(|s| !s.is_empty()))
    }
}

pub(crate) fn open(url: &str, no_open: bool) {
    if no_open || !graphical_session() {
        return;
    }
    #[cfg(target_os = "macos")]
    let mut command = Command::new("open");
    #[cfg(target_os = "windows")]
    let mut command = {
        let mut command = Command::new("rundll32.exe");
        command.arg("url.dll,FileProtocolHandler");
        command
    };
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    let mut command = Command::new("xdg-open");
    command
        .arg(url)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    // Reap outside the startup path; an unavailable browser is a URL-only fallback.
    // Spawn before returning: host start may exit before a new thread runs.
    if let Ok(mut child) = command.spawn() {
        let _ = std::thread::Builder::new()
            .name("browser-handoff".into())
            .spawn(move || {
                let _ = child.wait();
            });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn browser_handoff_suppresses_ssh_ci_and_services() {
        assert!(!suppressed(|_| None));
        for key in [
            "SSH_CONNECTION",
            "SSH_CLIENT",
            "SSH_TTY",
            "CI",
            "SESSIONNAME",
        ] {
            assert!(suppressed(|candidate| (candidate == key).then(|| {
                if key == "SESSIONNAME" {
                    "Services"
                } else {
                    "1"
                }
                .into()
            })));
        }
    }
}
