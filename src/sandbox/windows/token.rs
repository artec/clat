use super::acl::entry;
use super::native::{Handle, Local, error, invoke, invoke_code};
use super::sid::Sid;
use std::mem::size_of;
use std::ptr::{null, null_mut};
use windows_sys::Win32::Foundation::{ERROR_INSUFFICIENT_BUFFER, GetLastError, HANDLE};
use windows_sys::Win32::Security::Authorization::{GRANT_ACCESS, SetEntriesInAclW};
use windows_sys::Win32::Security::*;
// winnt.h group attribute constants (windows-sys exposes them under SystemServices).
const SE_GROUP_LOGON_ID: u32 = 0xc0000000;
const SE_GROUP_INTEGRITY: u32 = 0x20;
use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

pub(super) fn restricted(write_sids: &[&Sid]) -> Result<Handle, String> {
    let mut current = null_mut();
    invoke("OpenProcessToken", || unsafe {
        OpenProcessToken(
            GetCurrentProcess(),
            TOKEN_QUERY | TOKEN_DUPLICATE | TOKEN_ADJUST_DEFAULT | TOKEN_ASSIGN_PRIMARY,
            &mut current,
        )
    })?;
    let current = Handle::new("OpenProcessToken", current)?;
    let groups = information(current.0, TokenGroups)?;
    let logon = logon_sid(&groups)?;
    let world = Sid::parse("S-1-1-0")?;
    let low = Sid::parse("S-1-16-4096")?;
    let mut sids = vec![
        SID_AND_ATTRIBUTES {
            Sid: logon,
            Attributes: 0,
        },
        SID_AND_ATTRIBUTES {
            Sid: world.ptr(),
            Attributes: 0,
        },
    ];
    sids.extend(write_sids.iter().map(|sid| SID_AND_ATTRIBUTES {
        Sid: sid.ptr(),
        Attributes: 0,
    }));
    let mut token = null_mut();
    invoke("CreateRestrictedToken", || unsafe {
        CreateRestrictedToken(
            current.0,
            DISABLE_MAX_PRIVILEGE | LUA_TOKEN | WRITE_RESTRICTED,
            0,
            null(),
            0,
            null(),
            sids.len() as u32,
            sids.as_ptr(),
            &mut token,
        )
    })?;
    let token = Handle::new("CreateRestrictedToken", token)?;
    // Prefer the private run capability for new kernel objects. Using the
    // workspace capability would share their default authority across runs;
    // Everyone/logon full-access grants would share it across confined modes.
    // Files with inheritable parent ACLs still inherit the workspace grant.
    default_dacl(
        token.0,
        write_sids.last().map_or(world.ptr(), |sid| sid.ptr()),
    )?;
    integrity(token.0, &low)?;
    current.close()?;
    Ok(token)
}

// Use pointer-aligned storage because TOKEN_GROUPS / DEFAULT_DACL contain pointers.
fn information(token: HANDLE, class: TOKEN_INFORMATION_CLASS) -> Result<Vec<usize>, String> {
    let mut length = 0;
    super::native::before("GetTokenInformation(size)")?;
    let first = unsafe { GetTokenInformation(token, class, null_mut(), 0, &mut length) };
    let last = unsafe { GetLastError() };
    if first != 0 || last != ERROR_INSUFFICIENT_BUFFER || length == 0 {
        return Err(error("GetTokenInformation(size)", last));
    }
    let mut buffer = vec![0usize; (length as usize).div_ceil(size_of::<usize>())];
    invoke("GetTokenInformation", || unsafe {
        GetTokenInformation(
            token,
            class,
            buffer.as_mut_ptr().cast(),
            length,
            &mut length,
        )
    })?;
    Ok(buffer)
}

fn logon_sid(groups: &[usize]) -> Result<PSID, String> {
    if std::mem::size_of_val(groups) < size_of::<TOKEN_GROUPS>() {
        return Err("windows-acl-run: truncated TokenGroups".into());
    }
    let groups_pointer = groups.as_ptr().cast::<TOKEN_GROUPS>();
    let count = unsafe { (*groups_pointer).GroupCount } as usize;
    let offset = std::mem::offset_of!(TOKEN_GROUPS, Groups);
    let capacity = (std::mem::size_of_val(groups) - offset) / size_of::<SID_AND_ATTRIBUTES>();
    if count > capacity {
        return Err("windows-acl-run: invalid TokenGroups count".into());
    }
    let entries = unsafe { std::slice::from_raw_parts((*groups_pointer).Groups.as_ptr(), count) };
    for group in entries {
        if group.Attributes & SE_GROUP_LOGON_ID == SE_GROUP_LOGON_ID {
            invoke("IsValidSid(logon)", || unsafe { IsValidSid(group.Sid) })?;
            return Ok(group.Sid);
        }
    }
    Err("windows-acl-run: token has no logon SID".into())
}

fn default_dacl(token: HANDLE, sid: PSID) -> Result<(), String> {
    let buffer = information(token, TokenDefaultDacl)?;
    let old = unsafe { (*(buffer.as_ptr().cast::<TOKEN_DEFAULT_DACL>())).DefaultDacl };
    if old.is_null() {
        return Err("windows-acl-run: token has no default DACL".into());
    }
    let entry = entry(sid, GRANT_ACCESS, 0x1f01ff, 0);
    let mut new = null_mut();
    invoke_code("SetEntriesInAclW(default)", || unsafe {
        SetEntriesInAclW(1, &entry, old, &mut new)
    })?;
    let allocation = Local(new.cast());
    if new.is_null() {
        return Err("windows-acl-run: null merged default DACL".into());
    }
    let info = TOKEN_DEFAULT_DACL { DefaultDacl: new };
    invoke("SetTokenInformation(default)", || unsafe {
        SetTokenInformation(
            token,
            TokenDefaultDacl,
            (&info as *const TOKEN_DEFAULT_DACL).cast(),
            size_of::<TOKEN_DEFAULT_DACL>() as u32,
        )
    })?;
    allocation.free()
}

fn integrity(token: HANDLE, low: &Sid) -> Result<(), String> {
    let length = unsafe { GetLengthSid(low.ptr()) };
    if length == 0 {
        return Err(error("GetLengthSid", unsafe { GetLastError() }));
    }
    let size = size_of::<TOKEN_MANDATORY_LABEL>() + length as usize;
    let mut buffer = vec![0usize; size.div_ceil(size_of::<usize>())];
    let info = buffer.as_mut_ptr().cast::<TOKEN_MANDATORY_LABEL>();
    unsafe {
        (*info).Label = SID_AND_ATTRIBUTES {
            Sid: low.ptr(),
            Attributes: SE_GROUP_INTEGRITY,
        };
    }
    invoke("SetTokenInformation(Low)", || unsafe {
        SetTokenInformation(token, TokenIntegrityLevel, info.cast(), size as u32)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    fn restricting_sids(token: HANDLE) -> Vec<Sid> {
        let groups = information(token, TokenRestrictedSids).unwrap();
        let groups_pointer = groups.as_ptr().cast::<TOKEN_GROUPS>();
        let count = unsafe { (*groups_pointer).GroupCount } as usize;
        let entries =
            unsafe { std::slice::from_raw_parts((*groups_pointer).Groups.as_ptr(), count) };
        entries
            .iter()
            .map(|entry| {
                let mut text = null_mut();
                invoke("ConvertSidToStringSidW", || unsafe {
                    windows_sys::Win32::Security::Authorization::ConvertSidToStringSidW(
                        entry.Sid, &mut text,
                    )
                })
                .unwrap();
                let owned = Local(text.cast());
                let mut length = 0;
                while unsafe { *text.add(length) } != 0 {
                    length += 1;
                }
                let string =
                    String::from_utf16(unsafe { std::slice::from_raw_parts(text, length) })
                        .unwrap();
                let sid = Sid::parse(&string).unwrap();
                owned.free().unwrap();
                sid
            })
            .collect()
    }

    fn has_full_default_ace(token: HANDLE, sid: &Sid) -> bool {
        let buffer = information(token, TokenDefaultDacl).unwrap();
        let acl = unsafe { (*(buffer.as_ptr().cast::<TOKEN_DEFAULT_DACL>())).DefaultDacl };
        for index in 0..unsafe { (*acl).AceCount } {
            let mut ace = null_mut();
            invoke("GetAce", || unsafe { GetAce(acl, index as u32, &mut ace) }).unwrap();
            let ace = unsafe { &*ace.cast::<ACCESS_ALLOWED_ACE>() };
            if ace.Header.AceType != 0 {
                continue;
            }
            let pointer = (&ace.SidStart as *const u32).cast_mut().cast();
            if ace.Mask == 0x1f01ff && unsafe { EqualSid(pointer, sid.ptr()) } != 0 {
                return true;
            }
        }
        false
    }

    #[test]
    fn restricted_token_lists_low_label_and_private_default_dacl_are_kernel_observed() {
        let workspace = Sid::parse("S-1-4-123-456").unwrap();
        let scratch = Sid::parse("S-1-4-123-456-1").unwrap();
        let world = Sid::parse("S-1-1-0").unwrap();
        let write_sids = [&workspace, &scratch];
        for writable in [false, true] {
            let token = restricted(if writable { &write_sids } else { &[] }).unwrap();
            let list = restricting_sids(token.0);
            assert_eq!(list.len(), if writable { 4 } else { 2 });
            for capability in [&workspace, &scratch] {
                let present = list
                    .iter()
                    .any(|entry| unsafe { EqualSid(entry.ptr(), capability.ptr()) } != 0);
                assert_eq!(present, writable);
            }
            let label = information(token.0, TokenIntegrityLevel).unwrap();
            let label = unsafe { &*label.as_ptr().cast::<TOKEN_MANDATORY_LABEL>() };
            let low = Sid::parse("S-1-16-4096").unwrap();
            assert_ne!(unsafe { EqualSid(label.Label.Sid, low.ptr()) }, 0);
            assert!(has_full_default_ace(
                token.0,
                if writable { &scratch } else { &world }
            ));
            if writable {
                assert!(!has_full_default_ace(token.0, &workspace));
                assert!(!has_full_default_ace(token.0, &world));
            }
            token.close().unwrap();
        }
    }
}
