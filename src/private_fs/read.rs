//! Read bounded private control text without repair, link following or defaults.
use std::io::Read;
pub(crate) fn read_private_text(
    dir: &cap_std::fs::Dir,
    name: &str,
    limit: usize,
) -> Result<Option<String>, String> {
    match dir.symlink_metadata(name) {
        Ok(meta) if !meta.is_file() || meta.file_type().is_symlink() => {
            return Err(format!("invalid private control file: {name}"));
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.to_string()),
        _ => {}
    }
    let mut options = cap_std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use cap_std::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW);
    }
    #[cfg(windows)]
    {
        use cap_std::fs::OpenOptionsExt;
        options.custom_flags(windows_sys::Win32::Storage::FileSystem::FILE_FLAG_OPEN_REPARSE_POINT);
    }
    let file = dir.open_with(name, &options).map_err(|e| e.to_string())?;
    if !file.metadata().map_err(|e| e.to_string())?.is_file() {
        return Err(format!("invalid private control file: {name}"));
    }
    let mut text = String::new();
    file.take(limit.saturating_add(1) as u64)
        .read_to_string(&mut text)
        .map_err(|e| e.to_string())?;
    if text.len() > limit {
        return Err(format!("private control file exceeds limit: {name}"));
    }
    Ok(Some(text))
}
