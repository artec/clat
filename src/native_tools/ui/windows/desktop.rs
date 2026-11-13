//! Win32 desktop, process and top-level window checks.
use super::*;
use ::windows::Win32::{
    Foundation::{CloseHandle, FILETIME, HANDLE, HWND, LPARAM},
    Graphics::Dwm::{DWMWA_CLOAKED, DwmGetWindowAttribute},
    Security::{
        EqualSid, GetSidSubAuthority, GetSidSubAuthorityCount, GetTokenInformation,
        TOKEN_ELEVATION, TOKEN_INFORMATION_CLASS, TOKEN_MANDATORY_LABEL, TOKEN_QUERY, TOKEN_USER,
        TokenElevation, TokenIntegrityLevel, TokenUser,
    },
    Storage::Packaging::Appx::GetApplicationUserModelId,
    System::{
        Console::{
            CONSOLE_MODE, GetConsoleMode, GetConsoleWindow, GetStdHandle, STD_INPUT_HANDLE,
            STD_OUTPUT_HANDLE,
        },
        RemoteDesktop::{
            ProcessIdToSessionId, WTS_SESSIONSTATE_UNLOCK, WTSActive, WTSFreeMemory, WTSINFOEXW,
            WTSQuerySessionInformationW, WTSSessionInfoEx,
        },
        StationsAndDesktops::*,
        SystemInformation::GetSystemWindowsDirectoryW,
        Threading::*,
    },
    UI::WindowsAndMessaging::*,
};
use ::windows::core::{BOOL, PWSTR};
use std::{ffi::OsString, os::windows::ffi::OsStringExt, path::PathBuf};

struct OwnedHandle(HANDLE);

impl Drop for OwnedHandle {
    fn drop(&mut self) {
        let _ = unsafe { CloseHandle(self.0) };
    }
}

pub(super) fn interactive() -> Result<()> {
    let unavailable = || error("WINDOWS_DESKTOP_UNAVAILABLE");
    let session = session(std::process::id())
        .filter(|id| *id != 0)
        .ok_or_else(unavailable)?;
    if !session_unlocked(session) {
        return Err(unavailable());
    }
    let input = unsafe { OpenInputDesktop(DESKTOP_CONTROL_FLAGS(0), false, DESKTOP_READOBJECTS) }
        .map_err(|_| unavailable())?;
    let current = unsafe { GetThreadDesktop(GetCurrentThreadId()) }.ok();
    let input_name = desktop_name(input);
    let _ = unsafe { CloseDesktop(input) };
    if !input_name
        .as_deref()
        .is_some_and(|name| name.eq_ignore_ascii_case("Default"))
        || current.and_then(desktop_name) != input_name
        || !current.is_some_and(receives_input)
    {
        return Err(unavailable());
    }
    Ok(())
}

fn receives_input(desktop: HDESK) -> bool {
    let mut active = BOOL(0);
    let checked = unsafe {
        GetUserObjectInformationW(
            HANDLE(desktop.0),
            UOI_IO,
            Some((&mut active as *mut BOOL).cast()),
            std::mem::size_of::<BOOL>() as u32,
            None,
        )
    };
    checked.is_ok() && active.as_bool()
}

fn session(pid: u32) -> Option<u32> {
    let mut id = 0;
    unsafe { ProcessIdToSessionId(pid, &mut id) }.ok()?;
    Some(id)
}

fn session_unlocked(id: u32) -> bool {
    let mut data = PWSTR::null();
    let mut bytes = 0;
    if unsafe { WTSQuerySessionInformationW(None, id, WTSSessionInfoEx, &mut data, &mut bytes) }
        .is_err()
    {
        return false;
    }
    // The input desktop can still appear to be Default while Winlogon has
    // locked this session. Require an explicit unlocked session as well.
    let unlocked = !data.is_null()
        && bytes as usize >= std::mem::size_of::<WTSINFOEXW>()
        && unsafe { session_info_unlocked(&data.0.cast::<WTSINFOEXW>().read_unaligned(), id) };
    unsafe { WTSFreeMemory(data.0.cast()) };
    unlocked
}

fn session_info_unlocked(info: &WTSINFOEXW, id: u32) -> bool {
    if info.Level != 1 {
        return false;
    }
    let level = unsafe { info.Data.WTSInfoExLevel1 };
    level.SessionId == id
        && level.SessionState == WTSActive
        && level.SessionFlags == WTS_SESSIONSTATE_UNLOCK as i32
}

fn desktop_name(desktop: HDESK) -> Option<String> {
    let mut name = [0u16; 128];
    unsafe {
        GetUserObjectInformationW(
            HANDLE(desktop.0),
            UOI_NAME,
            Some(name.as_mut_ptr().cast()),
            std::mem::size_of_val(&name) as u32,
            None,
        )
    }
    .ok()?;
    String::from_utf16(&name[..name.iter().position(|unit| *unit == 0)?]).ok()
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(super) struct Window {
    pub(super) raw: isize,
    pub(super) pid: i32,
    thread: u32,
}

impl Window {
    pub(super) fn hwnd(self) -> HWND {
        HWND(self.raw as *mut _)
    }

    pub(super) fn from_hwnd(hwnd: HWND) -> Option<Self> {
        let mut pid = 0;
        let thread = unsafe { GetWindowThreadProcessId(hwnd, Some(&mut pid)) };
        (thread != 0 && pid > 0 && pid <= i32::MAX as u32).then_some(Self {
            raw: hwnd.0 as isize,
            pid: pid as i32,
            thread,
        })
    }

    pub(super) fn check(self) -> Result<()> {
        if Self::from_hwnd(self.hwnd()) != Some(self)
            || unsafe { GetAncestor(self.hwnd(), GA_ROOT) } != self.hwnd()
        {
            return Err(error("STALE_SNAPSHOT"));
        }
        Ok(())
    }
}

pub(super) fn visible_windows(call: &CancelToken) -> Result<Vec<Window>> {
    active(call)?;
    let mut windows = Vec::new();
    unsafe {
        EnumWindows(
            Some(collect_window),
            LPARAM((&mut windows as *mut Vec<Window>) as isize),
        )
    }
    .map_err(|_| error("NATIVE_ERROR"))?;
    active(call)?;
    Ok(windows)
}

unsafe extern "system" fn collect_window(hwnd: HWND, data: LPARAM) -> BOOL {
    let windows = unsafe { &mut *(data.0 as *mut Vec<Window>) };
    let mut cloaked = 0u32;
    let visible = unsafe {
        IsWindowVisible(hwnd).as_bool()
            && hwnd != GetShellWindow()
            && DwmGetWindowAttribute(hwnd, DWMWA_CLOAKED, (&mut cloaked as *mut u32).cast(), 4)
                .is_ok()
            && cloaked == 0
    };
    if visible && let Some(window) = Window::from_hwnd(hwnd) {
        windows.push(window);
    }
    BOOL(1)
}

pub(super) fn foreground_pid() -> Option<i32> {
    Window::from_hwnd(unsafe { GetForegroundWindow() }).map(|window| window.pid)
}

fn open_process(pid: i32) -> Result<OwnedHandle> {
    if pid <= 0 || session(pid as u32).is_none_or(|id| Some(id) != session(std::process::id())) {
        return Err(error("WINDOWS_TARGET_UNAVAILABLE"));
    }
    unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid as u32) }
        .map(OwnedHandle)
        .map_err(|_| error("WINDOWS_TARGET_UNAVAILABLE"))
}

fn creation_time(process: HANDLE) -> Result<String> {
    let (mut start, mut exit, mut kernel, mut user) = (
        FILETIME::default(),
        FILETIME::default(),
        FILETIME::default(),
        FILETIME::default(),
    );
    unsafe { GetProcessTimes(process, &mut start, &mut exit, &mut kernel, &mut user) }
        .map_err(|_| error("STALE_APP"))?;
    if exit.dwHighDateTime != 0 || exit.dwLowDateTime != 0 {
        return Err(error("STALE_APP"));
    }
    Ok(format!(
        "{:08x}{:08x}",
        start.dwHighDateTime, start.dwLowDateTime
    ))
}

pub(super) fn app(pid: i32) -> Result<App> {
    let process = open_process(pid)?;
    check_token(process.0)?;
    let start = creation_time(process.0)?;
    let path = executable(process.0)?;
    let package = package_id(process.0);
    let name = path
        .file_stem()
        .map(|name| name.to_string_lossy())
        .unwrap_or_default();
    let mut hash = Sha256::new();
    if let Some(package) = &package {
        hash.update(b"windows:aumid:");
        hash.update(package.as_bytes());
    } else {
        hash.update(b"windows:executable-path:");
        hash.update(path.as_os_str().as_encoded_bytes());
    }
    Ok(App {
        pid,
        identity: format!("{:x}", hash.finalize()),
        process_identity: start,
        label: display_text(&name).chars().take(96).collect(),
        kind: classify(&path, package.as_deref()),
        bundle: package,
    })
}

pub(super) fn verify_process(app: &App) -> Result<()> {
    let process = open_process(app.pid)?;
    check_token(process.0)?;
    if creation_time(process.0)? != app.process_identity {
        return Err(error("STALE_APP"));
    }
    Ok(())
}

fn executable(process: HANDLE) -> Result<PathBuf> {
    let mut image = vec![0u16; 32768];
    let mut len = image.len() as u32;
    unsafe {
        QueryFullProcessImageNameW(
            process,
            PROCESS_NAME_WIN32,
            PWSTR(image.as_mut_ptr()),
            &mut len,
        )
    }
    .map_err(|_| error("WINDOWS_TARGET_UNAVAILABLE"))?;
    PathBuf::from(OsString::from_wide(&image[..len as usize]))
        .canonicalize()
        .map_err(|_| error("WINDOWS_TARGET_UNAVAILABLE"))
}

fn package_id(process: HANDLE) -> Option<String> {
    let mut id = [0u16; 256];
    let mut len = id.len() as u32;
    let result =
        unsafe { GetApplicationUserModelId(process, &mut len, Some(PWSTR(id.as_mut_ptr()))) };
    (result.0 == 0 && len > 1 && len as usize <= id.len())
        .then(|| String::from_utf16_lossy(&id[..len as usize - 1]))
}

fn classify(path: &std::path::Path, package: Option<&str>) -> ApplicationKind {
    let name = path
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .to_ascii_lowercase();
    if name.starts_with("clat")
        || [
            "windowsterminal.exe",
            "windowsterminalpreview.exe",
            "cmd.exe",
            "conhost.exe",
            "openconsole.exe",
            "powershell.exe",
            "pwsh.exe",
            "wsl.exe",
            "mintty.exe",
            "wezterm-gui.exe",
            "alacritty.exe",
            "tabby.exe",
            "hyper.exe",
            "winlogon.exe",
            "logonui.exe",
            "consent.exe",
            "credentialuibroker.exe",
            "lockapp.exe",
            "securityhealthsystray.exe",
            "taskmgr.exe",
        ]
        .contains(&name.as_str())
    {
        return ApplicationKind::Protected;
    }
    // The process API supplies the package AUMID, including the publisher
    // identity. Modern Windows ships Notepad as this system app.
    if name == "notepad.exe"
        && package
            .is_some_and(|id| id.eq_ignore_ascii_case("Microsoft.WindowsNotepad_8wekyb3d8bbwe!App"))
    {
        return ApplicationKind::Other;
    }
    // Shared clients can present approval in a browser or WebView. Only known
    // native executables in the OS directory are eligible on that surface.
    let mut directory = [0u16; 260];
    let len = unsafe { GetSystemWindowsDirectoryW(Some(&mut directory)) } as usize;
    if (1..directory.len()).contains(&len) {
        let root = PathBuf::from(OsString::from_wide(&directory[..len]));
        if ["explorer.exe", "System32/notepad.exe", "notepad.exe"]
            .iter()
            .any(|relative| {
                root.join(relative)
                    .canonicalize()
                    .is_ok_and(|native| native == path)
            })
        {
            return ApplicationKind::Other;
        }
    }
    ApplicationKind::BrowserOrUnknown
}

fn token(process: HANDLE) -> Result<OwnedHandle> {
    let mut token = HANDLE::default();
    unsafe { OpenProcessToken(process, TOKEN_QUERY, &mut token) }
        .map_err(|_| error("WINDOWS_TARGET_UNAVAILABLE"))?;
    Ok(OwnedHandle(token))
}

fn token_info(token: &OwnedHandle, class: TOKEN_INFORMATION_CLASS) -> Result<Vec<usize>> {
    let mut size = 0;
    let _ = unsafe { GetTokenInformation(token.0, class, None, 0, &mut size) };
    if !(4..=65536).contains(&size) {
        return Err(error("WINDOWS_TARGET_UNAVAILABLE"));
    }
    let mut buffer = vec![0usize; (size as usize).div_ceil(std::mem::size_of::<usize>())];
    unsafe {
        GetTokenInformation(
            token.0,
            class,
            Some(buffer.as_mut_ptr().cast()),
            size,
            &mut size,
        )
    }
    .map_err(|_| error("WINDOWS_TARGET_UNAVAILABLE"))?;
    Ok(buffer)
}

fn integrity(token: &OwnedHandle) -> Result<u32> {
    let info = token_info(token, TokenIntegrityLevel)?;
    let sid = unsafe { (*info.as_ptr().cast::<TOKEN_MANDATORY_LABEL>()).Label.Sid };
    let count = unsafe { *GetSidSubAuthorityCount(sid) };
    if count == 0 {
        return Err(error("WINDOWS_TARGET_UNAVAILABLE"));
    }
    Ok(unsafe { *GetSidSubAuthority(sid, u32::from(count - 1)) })
}

fn check_token(process: HANDLE) -> Result<()> {
    let target = token(process)?;
    let current = token(unsafe { GetCurrentProcess() })?;
    let user = token_info(&target, TokenUser)?;
    let own_user = token_info(&current, TokenUser)?;
    let equal = unsafe {
        EqualSid(
            (*user.as_ptr().cast::<TOKEN_USER>()).User.Sid,
            (*own_user.as_ptr().cast::<TOKEN_USER>()).User.Sid,
        )
    }
    .is_ok();
    let elevation = token_info(&target, TokenElevation)?;
    let elevated = unsafe { (*elevation.as_ptr().cast::<TOKEN_ELEVATION>()).TokenIsElevated != 0 };
    if !target_allowed(equal, elevated, integrity(&target)?, integrity(&current)?) {
        return Err(error("WINDOWS_TARGET_UNAVAILABLE"));
    }
    Ok(())
}

pub(super) fn fence(standalone: bool) -> TargetFence {
    let pid = std::process::id() as i32;
    let console = unsafe { GetConsoleWindow() };
    let owner = Window::from_hwnd(console).map(|window| window.pid);
    let mut protected_pids = vec![pid];
    protected_pids.extend(owner);
    // A pseudoconsole's window is hidden and does not identify the GUI
    // terminal presenting the prompt. Only a visible native console proves
    // that a standalone TUI's approval is isolated from the target.
    let verified = owner.is_some_and(|owner| {
        (unsafe { IsWindowVisible(console).as_bool() })
            && app(owner).is_ok_and(|app| app.kind == ApplicationKind::Protected)
            && [STD_INPUT_HANDLE, STD_OUTPUT_HANDLE].iter().all(|&which| {
                unsafe { GetStdHandle(which) }.is_ok_and(|handle| {
                    unsafe { GetConsoleMode(handle, &mut CONSOLE_MODE::default()) }.is_ok()
                })
            })
    });
    TargetFence {
        protected_pids,
        protected_apps: vec![],
        independent_local_tui: standalone && verified,
    }
}

fn target_allowed(
    same_user: bool,
    elevated: bool,
    target_integrity: u32,
    host_integrity: u32,
) -> bool {
    same_user && !elevated && target_integrity <= host_integrity.min(0x2fff)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cua_windows_active_session_must_also_report_unlocked() {
        use ::windows::Win32::System::RemoteDesktop::{
            WTSDisconnected, WTSINFOEX_LEVEL_W, WTSINFOEX_LEVEL1_W,
        };
        let mut level = WTSINFOEX_LEVEL1_W {
            SessionId: 42,
            SessionState: WTSActive,
            SessionFlags: WTS_SESSIONSTATE_UNLOCK as i32,
            ..Default::default()
        };
        let info = |level| WTSINFOEXW {
            Level: 1,
            Data: WTSINFOEX_LEVEL_W {
                WTSInfoExLevel1: level,
            },
        };
        assert!(session_info_unlocked(&info(level), 42));
        assert!(!session_info_unlocked(&info(level), 43));
        for flags in [0, -1] {
            level.SessionFlags = flags;
            assert!(!session_info_unlocked(&info(level), 42));
        }
        level.SessionFlags = WTS_SESSIONSTATE_UNLOCK as i32;
        level.SessionState = WTSDisconnected;
        assert!(!session_info_unlocked(&info(level), 42));
        assert!(!session_info_unlocked(&WTSINFOEXW::default(), 42));
    }

    #[test]
    fn cua_windows_target_requires_same_user_and_non_elevated_integrity() {
        assert!(target_allowed(true, false, 0x2000, 0x2000));
        assert!(!target_allowed(false, false, 0x2000, 0x2000));
        assert!(!target_allowed(true, false, 0x2000, 0x1000));
        assert!(!target_allowed(true, true, 0x2000, 0x3000));
        assert!(!target_allowed(true, false, 0x3000, 0x3000));
    }
}
