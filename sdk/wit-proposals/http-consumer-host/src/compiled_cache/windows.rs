//! Only the current user and OS administrators may access compiler secrets.
use std::io;
use std::os::windows::io::AsRawHandle;
use std::ptr::{addr_of_mut, null_mut};
use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, LocalFree};
use windows_sys::Win32::Security::Authorization::{GetSecurityInfo, SE_FILE_OBJECT};
use windows_sys::Win32::Security::*;
use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

struct Descriptor(PSECURITY_DESCRIPTOR);
impl Drop for Descriptor {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: GetSecurityInfo allocated this descriptor with LocalAlloc.
            unsafe {
                LocalFree(self.0);
            }
        }
    }
}
struct Token(HANDLE);
impl Drop for Token {
    fn drop(&mut self) {
        // SAFETY: OpenProcessToken returned this owned live handle.
        unsafe {
            CloseHandle(self.0);
        }
    }
}

pub(super) fn private(file: &impl AsRawHandle) -> io::Result<()> {
    let user = user_token()?;
    // SAFETY: GetTokenInformation initialized TOKEN_USER and its SID in user.
    let current = unsafe { (*user.as_ptr().cast::<TOKEN_USER>()).User.Sid };
    let mut descriptor = Descriptor(null_mut());
    let mut owner = null_mut();
    let mut dacl = null_mut();
    // SAFETY: live file handle; all output pointers refer to writable locals.
    let status = unsafe {
        GetSecurityInfo(
            file.as_raw_handle(),
            SE_FILE_OBJECT,
            OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION,
            &mut owner,
            null_mut(),
            &mut dacl,
            null_mut(),
            &mut descriptor.0,
        )
    };
    if status != 0 {
        return Err(io::Error::from_raw_os_error(status as i32));
    }
    // SAFETY: owner and ACL are valid pointers into the live descriptor.
    if unsafe { !trusted_sid(owner, current) } || dacl.is_null() {
        return Err(io::Error::other("cache owner or DACL is not private"));
    }
    // SAFETY: GetSecurityInfo returned a valid, live ACL.
    let count = unsafe { (*dacl).AceCount };
    for index in 0..count {
        // SAFETY: the ACL is live throughout validation.
        unsafe {
            private_ace(dacl, index as u32, current)?;
        }
    }
    Ok(())
}

unsafe fn private_ace(dacl: *const ACL, index: u32, current: PSID) -> io::Result<()> {
    let mut ace = null_mut();
    // SAFETY: caller supplies the live OS ACL and an in-range index.
    if unsafe { GetAce(dacl, index, &mut ace) } == 0 || ace.is_null() {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: GetAce returned an ACE within that valid OS descriptor.
    let header = unsafe { &*ace.cast::<ACE_HEADER>() };
    if header.AceType == 1 {
        return Ok(());
    } // ACCESS_DENIED_ACE
    if header.AceType != 0 || (header.AceSize as usize) < std::mem::size_of::<ACCESS_ALLOWED_ACE>()
    {
        return Err(io::Error::other("unsupported cache ACL"));
    }
    let allowed = ace.cast::<ACCESS_ALLOWED_ACE>();
    // SAFETY: the allow ACE contains its complete SID following SidStart.
    let sid = unsafe { addr_of_mut!((*allowed).SidStart).cast() };
    if unsafe { !trusted_sid(sid, current) } {
        return Err(io::Error::other(
            "cache ACL exposes host secrets to another principal",
        ));
    }
    Ok(())
}

unsafe fn trusted_sid(sid: PSID, current: PSID) -> bool {
    !sid.is_null()
        && unsafe {
            EqualSid(sid, current) != 0
                || IsWellKnownSid(sid, WinLocalSystemSid) != 0
                || IsWellKnownSid(sid, WinBuiltinAdministratorsSid) != 0
        }
}

fn user_token() -> io::Result<Vec<usize>> {
    let mut token = Token(null_mut());
    // SAFETY: current process handle is valid and token output is writable.
    if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token.0) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let mut size = 0;
    // SAFETY: sizing query with a null buffer; size is a writable output.
    unsafe {
        GetTokenInformation(token.0, TokenUser, null_mut(), 0, &mut size);
    }
    if size < std::mem::size_of::<TOKEN_USER>() as u32 || size > 64 * 1024 {
        return Err(io::Error::other("invalid token user size"));
    }
    let mut buffer = vec![0usize; (size as usize).div_ceil(std::mem::size_of::<usize>())];
    // SAFETY: aligned buffer holds at least size bytes and remains owned here.
    if unsafe {
        GetTokenInformation(
            token.0,
            TokenUser,
            buffer.as_mut_ptr().cast(),
            size,
            &mut size,
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    Ok(buffer)
}
