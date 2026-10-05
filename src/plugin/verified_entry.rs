//! Activation-local evidence from a complete tree scan, never a durable cache.
use super::PluginPackageManifest;
#[cfg(windows)]
use cap_primitives::fs::_WindowsByHandle;
use cap_std::fs::{Metadata, MetadataExt};
use std::path::{Path, PathBuf};

#[derive(Clone, Debug)]
pub(crate) struct VerifiedEntry {
    root: PathBuf,
    relative: PathBuf,
    metadata: Metadata,
    #[cfg(windows)]
    change_time: i64,
    #[cfg(windows)]
    expected: String,
}

impl VerifiedEntry {
    pub(super) fn capture(manifest: &PluginPackageManifest, path: &Path) -> Result<Self, String> {
        let root = path
            .parent()
            .ok_or("manifest has no parent")?
            .canonicalize()
            .map_err(|error| format!("resolve package root: {error}"))?;
        let relative = PathBuf::from(&manifest.runtime.entry);
        let file = super::store::open_package_file(&root, &relative)?;
        let metadata = file
            .metadata()
            .map_err(|error| format!("stat entry: {error}"))?;
        if metadata.len() > super::package::MAX_MANIFEST_ENTRY_BYTES {
            return Err("package entry exceeds its size cap".into());
        }
        Ok(Self {
            root,
            relative,
            metadata,
            #[cfg(windows)]
            change_time: change_time(&file)?,
            #[cfg(windows)]
            expected: manifest
                .runtime
                .sha256
                .trim()
                .trim_start_matches("sha256:")
                .to_owned(),
        })
    }

    /// Windows permits explicit timestamp rewriting, so retain its final full
    /// digest gate. Unix ctime and inode evidence can consume the tree proof.
    pub(crate) fn activation_path(&self) -> Result<PathBuf, String> {
        let path = self.checked_path()?;
        #[cfg(windows)]
        {
            let mut file = super::store::open_package_file(&self.root, &self.relative)?;
            #[cfg(test)]
            super::store::tests::record_hash(&path);
            let actual = super::store::hash_reader_bounded(&mut file, &path, self.metadata.len())?;
            if !actual.eq_ignore_ascii_case(&self.expected) {
                return Err("package entry changed before activation".into());
            }
        }
        Ok(path)
    }

    /// Recheck the actual no-follow file identity after hashing and before use.
    /// This evidence is scoped to this activation, not reused across restarts.
    pub(crate) fn checked_path(&self) -> Result<PathBuf, String> {
        let file = super::store::open_package_file(&self.root, &self.relative)?;
        let current = file
            .metadata()
            .map_err(|error| format!("reinspect entry: {error}"))?;
        #[cfg(windows)]
        if self.change_time != change_time(&file)? {
            return Err("package entry changed after full verification".into());
        }
        if !same_file(&self.metadata, &current) {
            return Err("package entry changed after full verification".into());
        }
        Ok(self.root.join(&self.relative))
    }
}

fn same_file(before: &Metadata, after: &Metadata) -> bool {
    if !after.is_file()
        || before.len() != after.len()
        || before.modified().ok() != after.modified().ok()
        || before.created().ok() != after.created().ok()
        || before.permissions() != after.permissions()
    {
        return false;
    }
    #[cfg(unix)]
    return before.dev() == after.dev()
        && before.ino() == after.ino()
        && before.ctime() == after.ctime()
        && before.ctime_nsec() == after.ctime_nsec();
    #[cfg(windows)]
    return before.file_index().is_some()
        && before.volume_serial_number().is_some()
        && before.file_index() == after.file_index()
        && before.volume_serial_number() == after.volume_serial_number()
        && before.last_write_time() == after.last_write_time();
    #[cfg(not(any(unix, windows)))]
    false
}

#[cfg(windows)]
fn change_time(file: &cap_std::fs::File) -> Result<i64, String> {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_BASIC_INFO, FileBasicInfo, GetFileInformationByHandleEx,
    };
    let mut info: FILE_BASIC_INFO = unsafe { std::mem::zeroed() };
    // SAFETY: the live file owns this handle and info is an initialized,
    // correctly sized FILE_BASIC_INFO writable for the duration of the call.
    let read = unsafe {
        GetFileInformationByHandleEx(
            file.as_raw_handle(),
            FileBasicInfo,
            (&mut info as *mut FILE_BASIC_INFO).cast(),
            std::mem::size_of::<FILE_BASIC_INFO>() as u32,
        )
    };
    if read == 0 {
        return Err(format!(
            "inspect entry change time: {}",
            std::io::Error::last_os_error()
        ));
    }
    Ok(info.ChangeTime)
}
