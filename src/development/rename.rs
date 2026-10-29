//! Atomic no-clobber rename, relative to already-open directories.
use std::{ffi::OsStr, io};
#[cfg(unix)]
pub(super) fn rename_no_replace(
    from: &cap_std::fs::Dir,
    source: &OsStr,
    to: &cap_std::fs::Dir,
    target: &OsStr,
) -> Result<(), String> {
    use std::os::{fd::AsRawFd, unix::ffi::OsStrExt};
    let source = std::ffi::CString::new(source.as_bytes()).map_err(|_| "path_denied")?;
    let target = std::ffi::CString::new(target.as_bytes()).map_err(|_| "path_denied")?;
    #[cfg(target_os = "macos")]
    let result = unsafe {
        libc::renameatx_np(
            from.as_raw_fd(),
            source.as_ptr(),
            to.as_raw_fd(),
            target.as_ptr(),
            libc::RENAME_EXCL,
        )
    };
    #[cfg(not(target_os = "macos"))]
    let result = unsafe {
        libc::renameat2(
            from.as_raw_fd(),
            source.as_ptr(),
            to.as_raw_fd(),
            target.as_ptr(),
            libc::RENAME_NOREPLACE,
        )
    };
    if result == 0 {
        Ok(())
    } else {
        Err(super::io(io::Error::last_os_error()))
    }
}
#[cfg(windows)]
pub(super) fn rename_no_replace(
    from: &cap_std::fs::Dir,
    source: &OsStr,
    to: &cap_std::fs::Dir,
    target: &OsStr,
) -> Result<(), String> {
    use cap_std::fs::OpenOptionsExt;
    use std::os::windows::{ffi::OsStrExt, io::AsRawHandle};
    use windows_sys::{
        Wdk::Storage::FileSystem::{
            FILE_RENAME_INFORMATION, FileRenameInformation, NtSetInformationFile,
        },
        Win32::{Foundation::RtlNtStatusToDosError, System::IO::IO_STATUS_BLOCK},
    };
    let mut options = cap_std::fs::OpenOptions::new();
    options
        .access_mode(0x00010000 | 0x00100000)
        .custom_flags(0x02000000 | 0x00200000);
    let file = from.open_with(source, &options).map_err(super::io)?;
    if file.metadata().map_err(super::io)?.file_type().is_symlink() {
        return Err("path_denied".into());
    }
    let name: Vec<u16> = target.encode_wide().collect();
    let length = std::mem::size_of::<FILE_RENAME_INFORMATION>() + name.len() * 2;
    let mut buffer = vec![0usize; length.div_ceil(std::mem::size_of::<usize>())];
    let info = buffer.as_mut_ptr().cast::<FILE_RENAME_INFORMATION>();
    let mut status_block: IO_STATUS_BLOCK = unsafe { std::mem::zeroed() };
    unsafe {
        (*info).Anonymous.ReplaceIfExists = false;
        (*info).RootDirectory = to.as_raw_handle();
        (*info).FileNameLength = (name.len() * 2) as u32;
        std::ptr::copy_nonoverlapping(name.as_ptr(), (*info).FileName.as_mut_ptr(), name.len());
        // The Win32 FileRenameInfo wrapper rejects non-null RootDirectory.
        // The native operation preserves the already-open directory boundary
        // and atomic no-replace semantics instead of resolving ambient paths.
        let status = NtSetInformationFile(
            file.as_raw_handle(),
            &mut status_block,
            info.cast(),
            length as u32,
            FileRenameInformation,
        );
        if status < 0 {
            return Err(super::io(io::Error::from_raw_os_error(
                RtlNtStatusToDosError(status) as i32,
            )));
        }
    }
    Ok(())
}
