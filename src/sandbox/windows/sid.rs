use super::native::{Local, invoke};
use sha2::{Digest, Sha256};
use std::path::Path;
use std::ptr::null_mut;
use windows_sys::Win32::Security::Authorization::ConvertStringSidToSidW;
use windows_sys::Win32::Security::{IsValidSid, PSID};

pub(super) fn identity(path: &Path, temporary: bool) -> Result<String, String> {
    let path = path
        .to_str()
        .ok_or("windows-acl-run: capability path must be Unicode")?;
    let mut hash = Sha256::new();
    if temporary {
        hash.update(b"temp\0");
    }
    // GetFinalPathNameByHandle-backed canonicalization converges ordinary case
    // aliases. Never lowercase here: NTFS case-sensitive directories can hold
    // distinct Foo/foo workspaces and must mint distinct capabilities.
    hash.update(path.as_bytes());
    let digest = hash.finalize();
    let first = u32::from_le_bytes(digest[0..4].try_into().unwrap()) % ((1 << 30) - 1) + 1;
    let second = u32::from_le_bytes(digest[4..8].try_into().unwrap()) % ((1 << 30) - 1) + 1;
    Ok(format!(
        "S-1-4-{first}-{second}{}",
        if temporary { "-1" } else { "" }
    ))
}

pub(super) struct Sid(Local);
impl Sid {
    pub(super) fn parse(text: &str) -> Result<Self, String> {
        let text = super::native::wide(text.as_ref())?;
        let mut pointer = null_mut();
        invoke("ConvertStringSidToSidW", || unsafe {
            ConvertStringSidToSidW(text.as_ptr(), &mut pointer)
        })?;
        let sid = Self(Local(pointer));
        invoke("IsValidSid", || unsafe { IsValidSid(sid.ptr()) })?;
        Ok(sid)
    }
    pub(super) fn ptr(&self) -> PSID {
        self.0.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn capability_domains_and_sessions_are_distinct() {
        let path = Path::new(r"C:\canonical\workspace");
        assert_eq!(
            identity(path, false).unwrap(),
            identity(path, false).unwrap()
        );
        assert_ne!(
            identity(path, false).unwrap(),
            identity(path, true).unwrap()
        );
        assert_ne!(
            identity(path, true).unwrap(),
            identity(Path::new(r"C:\other"), true).unwrap()
        );
        assert_eq!(identity(path, false).unwrap().split('-').count(), 5);
        assert_eq!(identity(path, true).unwrap().split('-').count(), 6);
    }
}
