use std::io::Read;
use std::path::Path;

pub(super) fn read_token(root: &Path) -> Result<String, String> {
    let dir = cap_std::fs::Dir::open_ambient_dir(root, cap_std::ambient_authority())
        .map_err(|_| "host storage is unavailable")?;
    let mut options = cap_std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use cap_std::fs::OpenOptionsExt as _;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    #[cfg(windows)]
    {
        use cap_std::fs::OpenOptionsExt as _;
        options.custom_flags(windows_sys::Win32::Storage::FileSystem::FILE_FLAG_OPEN_REPARSE_POINT);
    }
    let file = dir
        .open_with("web-token", &options)
        .map_err(|_| "host credential is unavailable; start clat serve first")?;
    let meta = file
        .metadata()
        .map_err(|_| "cannot inspect host credential")?;
    if !meta.is_file() || meta.file_type().is_symlink() || meta.len() > 257 {
        return Err("host credential is not a bounded regular file".into());
    }
    #[cfg(unix)]
    {
        use cap_std::fs::PermissionsExt as _;
        if meta.permissions().mode() & 0o077 != 0 {
            return Err("host credential must be private (mode 600)".into());
        }
    }
    let mut token = String::new();
    file.take(258)
        .read_to_string(&mut token)
        .map_err(|_| "cannot read host credential")?;
    if token.len() > 257 {
        return Err("host credential exceeds the limit".into());
    }
    Ok(token.strip_suffix('\n').unwrap_or(&token).into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_token_reader_accepts_private_regular_file_and_rejects_oversized_input() {
        let (root, _) = crate::test_support::roots("host-token-bounds");
        std::fs::create_dir_all(&root).unwrap();
        let dir = cap_std::fs::Dir::open_ambient_dir(&root, cap_std::ambient_authority()).unwrap();
        crate::private_fs::write_text_atomic(&dir, &root, "web-token", "private-token\n").unwrap();
        assert_eq!(read_token(&root).unwrap(), "private-token");
        crate::private_fs::write_text_atomic(&dir, &root, "web-token", &"x".repeat(258)).unwrap();
        assert!(read_token(&root).is_err());
        drop(dir);
        crate::test_support::cleanup_tree(&root);
    }

    #[cfg(unix)]
    #[test]
    fn host_token_reader_rejects_symlinks_and_group_readable_credentials() {
        use std::os::unix::fs::{PermissionsExt, symlink};
        let (root, _) = crate::test_support::roots("host-token-nofollow");
        std::fs::create_dir_all(&root).unwrap();
        let private = root.join("private");
        let dir = cap_std::fs::Dir::open_ambient_dir(&root, cap_std::ambient_authority()).unwrap();
        crate::private_fs::write_text_atomic(&dir, &root, "private", "private-token").unwrap();
        symlink(&private, root.join("web-token")).unwrap();
        assert!(
            read_token(&root).is_err(),
            "credential symlink must never be followed"
        );
        std::fs::remove_file(root.join("web-token")).unwrap();
        std::fs::rename(&private, root.join("web-token")).unwrap();
        std::fs::set_permissions(
            root.join("web-token"),
            std::fs::Permissions::from_mode(0o640),
        )
        .unwrap();
        assert!(
            read_token(&root).is_err(),
            "shared credential must be rejected"
        );
        drop(dir);
        crate::test_support::cleanup_tree(&root);
    }
}
