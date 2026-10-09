use super::native::{Local, error, invoke, invoke_code, wide};
use super::sid::Sid;
use sha2::{Digest, Sha256};
use std::fs::OpenOptions;
use std::mem::{size_of, zeroed};
use std::os::windows::{
    fs::{MetadataExt, OpenOptionsExt},
    io::IntoRawHandle,
};
use std::path::Path;
use std::ptr::null_mut;
use windows_sys::Win32::Foundation::GetLastError;
use windows_sys::Win32::Security::Authorization::*;
use windows_sys::Win32::Security::*;
use windows_sys::Win32::Storage::FileSystem::{LOCKFILE_EXCLUSIVE_LOCK, LockFileEx, UnlockFileEx};
use windows_sys::Win32::System::IO::OVERLAPPED;

pub(super) const GRANT_MASK: u32 = (0x120116 | 0x10000 | 0x40) & !0x20000;
const INHERIT: u32 = 3;
// winnt.h mandatory label policy; avoids enabling unrelated SystemServices.
const SYSTEM_MANDATORY_LABEL_NO_WRITE_UP: u32 = 1;

pub(super) fn entry(
    sid: PSID,
    mode: ACCESS_MODE,
    mask: u32,
    inheritance: u32,
) -> EXPLICIT_ACCESS_W {
    EXPLICIT_ACCESS_W {
        grfAccessPermissions: mask,
        grfAccessMode: mode,
        grfInheritance: inheritance,
        Trustee: TRUSTEE_W {
            pMultipleTrustee: null_mut(),
            MultipleTrusteeOperation: NO_MULTIPLE_TRUSTEE,
            TrusteeForm: TRUSTEE_IS_SID,
            TrusteeType: TRUSTEE_IS_UNKNOWN,
            ptstrName: sid.cast(),
        },
    }
}

fn with_lock(path: &Path, action: impl FnOnce() -> Result<(), String>) -> Result<(), String> {
    let directory = std::env::temp_dir().join("clat-acl-locks");
    std::fs::create_dir_all(&directory)
        .map_err(|error| format!("windows-acl-run: lock directory: {error}"))?;
    let digest = format!(
        "{:x}",
        Sha256::digest(path.to_string_lossy().to_lowercase().as_bytes())
    );
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .share_mode(3)
        .open(directory.join(format!("{}.lock", &digest[..16])))
        .map_err(|error| format!("windows-acl-run: CreateFileW(lock): {error}"))?;
    let file = super::native::Handle::new("CreateFileW(lock)", file.into_raw_handle())?;
    let mut overlapped: OVERLAPPED = unsafe { zeroed() };
    invoke("LockFileEx", || unsafe {
        LockFileEx(file.0, LOCKFILE_EXCLUSIVE_LOCK, 0, 1, 0, &mut overlapped)
    })?;
    let result = action();
    let unlock = invoke("UnlockFileEx", || unsafe {
        UnlockFileEx(file.0, 0, 1, 0, &mut overlapped)
    });
    let close = file.close();
    result.and(unlock).and(close)
}

struct Security {
    allocation: Local,
    dacl: *mut ACL,
    label: *mut ACL,
}

fn read(path: &[u16]) -> Result<Security, String> {
    let mut descriptor = null_mut();
    let mut dacl = null_mut();
    let mut label = null_mut();
    invoke_code("GetNamedSecurityInfoW", || unsafe {
        GetNamedSecurityInfoW(
            path.as_ptr(),
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION | LABEL_SECURITY_INFORMATION,
            null_mut(),
            null_mut(),
            &mut dacl,
            &mut label,
            &mut descriptor,
        )
    })?;
    Ok(Security {
        allocation: Local(descriptor),
        dacl,
        label,
    })
}

fn exact(acl: *mut ACL, kind: u8, flags: u8, mask: u32, sid: &Sid) -> Result<bool, String> {
    if acl.is_null() {
        return Ok(false);
    }
    invoke("IsValidAcl", || unsafe { IsValidAcl(acl) })?;
    for index in 0..unsafe { (*acl).AceCount } {
        let mut ace = null_mut();
        invoke("GetAce", || unsafe { GetAce(acl, index as u32, &mut ace) })?;
        let header = unsafe { &*ace.cast::<ACE_HEADER>() };
        if header.AceType != kind || header.AceFlags != flags {
            continue;
        }
        if (header.AceSize as usize) < size_of::<ACCESS_ALLOWED_ACE>() {
            continue;
        }
        let allowed = unsafe { &*ace.cast::<ACCESS_ALLOWED_ACE>() };
        let ace_sid = (&allowed.SidStart as *const u32).cast_mut().cast();
        invoke("IsValidSid(ACE)", || unsafe { IsValidSid(ace_sid) })?;
        if allowed.Mask == mask && unsafe { EqualSid(ace_sid, sid.ptr()) } != 0 {
            return Ok(true);
        }
    }
    Ok(false)
}

fn low_label(low: &Sid) -> Result<Vec<u32>, String> {
    let sid_length = unsafe { GetLengthSid(low.ptr()) };
    if sid_length == 0 {
        return Err(error("GetLengthSid", unsafe { GetLastError() }));
    }
    let length = size_of::<ACL>() as u32 + 8 + sid_length;
    let mut storage = vec![0u32; (length as usize).div_ceil(4)];
    let acl = storage.as_mut_ptr().cast();
    invoke("InitializeAcl", || unsafe {
        InitializeAcl(acl, length, ACL_REVISION)
    })?;
    invoke("AddMandatoryAce", || unsafe {
        AddMandatoryAce(
            acl,
            ACL_REVISION,
            INHERIT,
            SYSTEM_MANDATORY_LABEL_NO_WRITE_UP,
            low.ptr(),
        )
    })?;
    Ok(storage)
}

fn apply(
    path: &[u16],
    security: Security,
    entries: &[EXPLICIT_ACCESS_W],
    label: Option<*mut ACL>,
) -> Result<(), String> {
    // NULL DACL conversion would turn an unrestricted descriptor into an empty
    // ACL on revoke. Reject that lossy round-trip instead of manufacturing it.
    if security.dacl.is_null() {
        return Err("windows-acl-run: refusing to replace NULL DACL".into());
    }
    let mut merged = null_mut();
    invoke_code("SetEntriesInAclW", || unsafe {
        SetEntriesInAclW(
            entries.len() as u32,
            entries.as_ptr(),
            security.dacl,
            &mut merged,
        )
    })?;
    let allocation = Local(merged.cast());
    if merged.is_null() {
        return Err("windows-acl-run: null merged ACL".into());
    }
    security.allocation.free()?;
    let flags = DACL_SECURITY_INFORMATION
        | if label.is_some() {
            LABEL_SECURITY_INFORMATION
        } else {
            0
        };
    invoke_code("SetNamedSecurityInfoW", || unsafe {
        SetNamedSecurityInfoW(
            path.as_ptr(),
            SE_FILE_OBJECT,
            flags,
            null_mut(),
            null_mut(),
            merged,
            label.unwrap_or(null_mut()),
        )
    })?;
    allocation.free()
}

pub(super) fn grant(path: &Path, capability: &Sid) -> Result<(), String> {
    with_lock(path, || {
        let native_path = path;
        let path = wide(path.as_os_str())?;
        let security = read(&path)?;
        let world = Sid::parse("S-1-1-0")?;
        let low = Sid::parse("S-1-16-4096")?;
        if exact(security.dacl, 0, 3, GRANT_MASK, capability)?
            && exact(security.dacl, 1, 2, 0x40, &world)?
            && exact(
                security.label,
                17,
                3,
                SYSTEM_MANDATORY_LABEL_NO_WRITE_UP,
                &low,
            )?
        {
            return security.allocation.free();
        }
        reject_reparse_tree(native_path)?;
        let mut label = low_label(&low)?;
        let entries = [
            entry(world.ptr(), DENY_ACCESS, 0x40, 2),
            entry(capability.ptr(), GRANT_ACCESS, GRANT_MASK, INHERIT),
        ];
        apply(&path, security, &entries, Some(label.as_mut_ptr().cast()))
    })
}

// SetNamedSecurityInfoW eagerly propagates inherited entries. Refuse an
// existing reparse object before that traversal could touch an alias target.
// Protected child DACLs keep their protection; they may deny writes even within
// the workspace rather than having their security silently replaced.
fn reject_reparse_tree(root: &Path) -> Result<(), String> {
    let mut directories = vec![root.to_path_buf()];
    while let Some(directory) = directories.pop() {
        let entries = std::fs::read_dir(&directory).map_err(|error| {
            format!(
                "windows-acl-run: grant tree scan {}: {error}",
                directory.display()
            )
        })?;
        for entry in entries {
            let path = entry
                .map_err(|error| format!("windows-acl-run: grant tree entry: {error}"))?
                .path();
            let metadata = std::fs::symlink_metadata(&path).map_err(|error| {
                format!(
                    "windows-acl-run: grant tree metadata {}: {error}",
                    path.display()
                )
            })?;
            if metadata.file_attributes() & 0x400 != 0 {
                return Err(format!(
                    "windows-acl-run: refusing ACL propagation through reparse object {}",
                    path.display()
                ));
            }
            if metadata.is_dir() {
                directories.push(path);
            }
        }
    }
    Ok(())
}

pub(super) fn revoke(path: &Path, capability: &Sid) -> Result<(), String> {
    with_lock(path, || {
        let path = wide(path.as_os_str())?;
        let security = read(&path)?;
        // Private scratch has exactly one capability identity. Keep its Low
        // label / ambient-delete deny until its owning directory is removed.
        apply(
            &path,
            security,
            &[entry(capability.ptr(), REVOKE_ACCESS, 0, INHERIT)],
            None,
        )
    })
}
