use std::ffi::{OsStr, c_void};
use std::os::windows::ffi::OsStrExt;
use std::ptr::null_mut;
use windows_sys::Win32::Foundation::{CloseHandle, GetLastError, HANDLE, LocalFree};

pub(super) fn error(api: &str, code: u32) -> String {
    format!(
        "windows-acl-run: {api} failed (Win32 {code}): {}",
        std::io::Error::from_raw_os_error(code as i32)
    )
}

pub(super) fn check(api: &str, ok: i32) -> Result<(), String> {
    if ok == 0 {
        Err(error(api, unsafe { GetLastError() }))
    } else {
        Ok(())
    }
}

pub(super) fn code(api: &str, result: u32) -> Result<(), String> {
    if result == 0 {
        Ok(())
    } else {
        Err(error(api, result))
    }
}

pub(super) fn wide(value: &OsStr) -> Result<Vec<u16>, String> {
    let mut value: Vec<u16> = value.encode_wide().collect();
    if value.contains(&0) {
        return Err("windows-acl-run: embedded NUL".into());
    }
    value.push(0);
    Ok(value)
}

pub(super) struct Handle(pub HANDLE);
impl Handle {
    pub(super) fn new(api: &str, handle: HANDLE) -> Result<Self, String> {
        if handle.is_null() || handle == windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE {
            Err(error(api, unsafe { GetLastError() }))
        } else {
            Ok(Self(handle))
        }
    }
    pub(super) fn close(mut self) -> Result<(), String> {
        let handle = std::mem::replace(&mut self.0, null_mut());
        check("CloseHandle", unsafe { CloseHandle(handle) })
    }
}
impl Drop for Handle {
    fn drop(&mut self) {
        if !self.0.is_null()
            && let Err(error) = check("CloseHandle", unsafe { CloseHandle(self.0) })
        {
            eprintln!("{error}");
        }
    }
}

pub(super) struct Local(pub *mut c_void);
impl Local {
    pub(super) fn free(mut self) -> Result<(), String> {
        let pointer = std::mem::replace(&mut self.0, null_mut());
        if !pointer.is_null() && !unsafe { LocalFree(pointer) }.is_null() {
            return Err(error("LocalFree", unsafe { GetLastError() }));
        }
        Ok(())
    }
}
impl Drop for Local {
    fn drop(&mut self) {
        if !self.0.is_null() && !unsafe { LocalFree(self.0) }.is_null() {
            eprintln!("{}", error("LocalFree", unsafe { GetLastError() }));
        }
    }
}

pub(super) fn invoke(api: &'static str, action: impl FnOnce() -> i32) -> Result<(), String> {
    before(api)?;
    check(api, action())
}

pub(super) fn invoke_code(api: &'static str, action: impl FnOnce() -> u32) -> Result<(), String> {
    before(api)?;
    code(api, action())
}

pub(super) fn before(api: &'static str) -> Result<(), String> {
    #[cfg(test)]
    {
        injection::before(api)
    }
    #[cfg(not(test))]
    {
        let _ = api;
        Ok(())
    }
}

#[cfg(test)]
pub(super) mod injection {
    use std::cell::RefCell;
    thread_local! { static STATE: RefCell<(Option<&'static str>, Vec<&'static str>)> = const { RefCell::new((None, Vec::new())) }; }
    pub(crate) struct Guard;
    impl Guard {
        pub(crate) fn fail(api: &'static str) -> Self {
            STATE.with(|state| *state.borrow_mut() = (Some(api), Vec::new()));
            Self
        }
        pub(crate) fn calls(&self) -> Vec<&'static str> {
            STATE.with(|state| state.borrow().1.clone())
        }
    }
    impl Drop for Guard {
        fn drop(&mut self) {
            STATE.with(|state| *state.borrow_mut() = (None, Vec::new()));
        }
    }
    pub(super) fn before(api: &'static str) -> Result<(), String> {
        STATE.with(|state| {
            let mut state = state.borrow_mut();
            state.1.push(api);
            if state.0 == Some(api) {
                Err(super::error(api, 5))
            } else {
                Ok(())
            }
        })
    }
}
