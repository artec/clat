//! Process identity and approval-UI provenance for macOS.
use super::*;
use crate::control_storage::ui_consents::app_identity;
use crate::native_tools::ui_apps::{Pool, application_at_pid, boolean, string};
use std::ffi::{CStr, OsStr};
use std::mem::{MaybeUninit, size_of};
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};

#[link(name = "CoreGraphics", kind = "framework")]
unsafe extern "C" {
    fn CGSessionCopyCurrentDictionary() -> ffi::Raw;
}

pub(in crate::native_tools) fn console_ready() -> Result<()> {
    let state = ffi::Cf::owned(unsafe { CGSessionCopyCurrentDictionary() })
        .ok_or_else(|| error("FOREGROUND_REQUIRED"))?;
    if state.dict_flag(c"CGSSessionScreenIsLocked") == Some(true) {
        return Err(error("SESSION_LOCKED"));
    }
    if state.dict_flag(c"kCGSSessionOnConsoleKey") != Some(true)
        || state.dict_flag(c"kCGSessionLoginDoneKey") != Some(true)
    {
        return Err(error("FOREGROUND_REQUIRED"));
    }
    Ok(())
}

pub(super) fn read_app(pid: i32) -> Result<App> {
    let first = process(pid).ok_or_else(|| error("STALE_APP"))?;
    let (name, bundle) = metadata(pid).ok_or_else(|| error("STALE_APP"))?;
    let executable = executable(pid).ok_or_else(|| error("STALE_APP"))?;
    let last = process(pid).ok_or_else(|| error("STALE_APP"))?;
    if start(&first) != start(&last) {
        return Err(error("STALE_APP"));
    }
    let name = name
        .as_deref()
        .or(bundle.as_deref())
        .or_else(|| executable.file_name().and_then(OsStr::to_str));
    let label: String = name
        .map(display_text)
        .unwrap_or_default()
        .chars()
        .take(96)
        .collect();
    let identity = app_identity(bundle.as_deref(), &executable).map_err(|_| error("STALE_APP"))?;
    Ok(App {
        pid,
        label: if label.trim().is_empty() {
            format!("PID {pid}")
        } else {
            label
        },
        kind: classify(bundle.as_deref(), &executable),
        identity,
        bundle,
        process_identity: format!("{}:{}", last.pbi_start_tvsec, last.pbi_start_tvusec),
    })
}

fn process(pid: i32) -> Option<libc::proc_bsdinfo> {
    if pid <= 0 {
        return None;
    }
    let mut info = MaybeUninit::<libc::proc_bsdinfo>::uninit();
    let size = size_of::<libc::proc_bsdinfo>() as i32;
    let copied = unsafe {
        libc::proc_pidinfo(
            pid,
            libc::PROC_PIDTBSDINFO,
            0,
            info.as_mut_ptr().cast(),
            size,
        )
    };
    if copied != size {
        return None;
    }
    let info = unsafe { info.assume_init() };
    (info.pbi_pid == pid as u32 && info.pbi_start_tvsec != 0).then_some(info)
}

fn start(info: &libc::proc_bsdinfo) -> (u64, u64) {
    (info.pbi_start_tvsec, info.pbi_start_tvusec)
}

fn executable(pid: i32) -> Option<PathBuf> {
    let mut buffer = [0u8; libc::PROC_PIDPATHINFO_MAXSIZE as usize];
    let count = unsafe { libc::proc_pidpath(pid, buffer.as_mut_ptr().cast(), buffer.len() as u32) };
    if count <= 0 {
        return None;
    }
    let path = CStr::from_bytes_until_nul(&buffer).ok()?.to_bytes();
    let path = PathBuf::from(OsStr::from_bytes(path));
    path.is_absolute().then_some(path)
}

fn metadata(pid: i32) -> Option<(Option<String>, Option<String>)> {
    let _pool = Pool::new();
    unsafe {
        let app = application_at_pid(pid);
        if app.is_null() || boolean(app, c"isTerminated") {
            return None;
        }
        Some((
            string(app, c"localizedName"),
            string(app, c"bundleIdentifier"),
        ))
    }
}

fn classify(bundle: Option<&str>, path: &Path) -> ApplicationKind {
    if is_clat(bundle, path) || is_terminal(bundle) {
        return ApplicationKind::Protected;
    }
    // A shared Host cannot determine which browser or embedded web view
    // presents an approval. The allowlist pairs known native Apple apps with
    // their canonical executable location on the system volume.
    let native = [
        (
            "com.apple.finder",
            "/System/Library/CoreServices/Finder.app/Contents/MacOS/Finder",
        ),
        (
            "com.apple.TextEdit",
            "/System/Applications/TextEdit.app/Contents/MacOS/TextEdit",
        ),
        (
            "com.apple.Preview",
            "/System/Applications/Preview.app/Contents/MacOS/Preview",
        ),
        (
            "com.apple.calculator",
            "/System/Applications/Calculator.app/Contents/MacOS/Calculator",
        ),
        (
            "com.apple.ActivityMonitor",
            "/System/Applications/Utilities/Activity Monitor.app/Contents/MacOS/Activity Monitor",
        ),
    ];
    if native.iter().any(|(id, expected)| {
        bundle.is_some_and(|bundle| bundle.eq_ignore_ascii_case(id))
            && path
                .canonicalize()
                .is_ok_and(|real| real == Path::new(expected))
    }) {
        ApplicationKind::Other
    } else {
        ApplicationKind::BrowserOrUnknown
    }
}

fn is_clat(bundle: Option<&str>, path: &Path) -> bool {
    bundle.is_some_and(|id| id.to_ascii_lowercase().starts_with("com.artec.clat"))
        || path.components().any(|part| {
            part.as_os_str()
                .to_str()
                .is_some_and(|name| name.eq_ignore_ascii_case("CLAT.app"))
        })
        || path
            .file_name()
            .and_then(OsStr::to_str)
            .is_some_and(|name| name.eq_ignore_ascii_case("clat"))
}

fn is_terminal(bundle: Option<&str>) -> bool {
    bundle.is_some_and(|id| {
        let id = id.to_ascii_lowercase();
        matches!(
            id.as_str(),
            "com.apple.terminal"
                | "com.googlecode.iterm2"
                | "com.mitchellh.ghostty"
                | "dev.warp.warp-stable"
                | "net.kovidgoyal.kitty"
                | "org.alacritty"
                | "com.github.wez.wezterm"
                | "io.alacritty"
                | "com.raphaelamorim.rio"
        ) || id.starts_with("com.googlecode.iterm2.")
            || id.starts_with("dev.warp.")
    })
}

pub(super) fn fence(standalone: bool) -> TargetFence {
    let mut pid = std::process::id() as i32;
    let mut protected_pids = vec![pid];
    let mut verified_terminal = false;
    for _ in 0..32 {
        let Some(info) = process(pid) else { break };
        if protected_pids.last() != Some(&pid) {
            protected_pids.push(pid);
        }
        verified_terminal |=
            metadata(pid).is_some_and(|(_, bundle)| is_terminal(bundle.as_deref()));
        let parent = info.pbi_ppid as i32;
        if parent <= 1 || parent == pid {
            break;
        }
        pid = parent;
    }
    let attached_tty =
        unsafe { libc::isatty(libc::STDIN_FILENO) == 1 && libc::isatty(libc::STDOUT_FILENO) == 1 };
    TargetFence {
        protected_pids,
        protected_apps: Vec::new(),
        independent_local_tui: standalone && attached_tty && verified_terminal,
    }
}
