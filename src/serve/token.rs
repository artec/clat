//! `clat serve` 的持久 Web token。
//!
//! 缺省 token 是 `~/.clat/web-token` 中的 0600 普通文件；写入复用
//! 共享的私有文件 tmp + fsync + rename 发布纪律。显式 `--token`
//! 是单进程覆盖，不触碰文件。

use cap_std::fs::Dir;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

pub(crate) const FILE_NAME: &str = "web-token";
const MAX_TOKEN_BYTES: u64 = 256;

#[derive(Debug)]
pub(crate) struct ResolvedToken {
    pub value: String,
    pub path: Option<PathBuf>,
}

pub(crate) fn validate(value: &str) -> Result<(), String> {
    if value.is_empty() {
        return Err("invalid token: empty".into());
    }
    if value.len() as u64 > MAX_TOKEN_BYTES {
        return Err(format!(
            "invalid token: longer than {MAX_TOKEN_BYTES} bytes"
        ));
    }
    if !value
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~'))
    {
        return Err("invalid token: use only ASCII letters, digits, '-', '.', '_' or '~'".into());
    }
    Ok(())
}

pub(crate) fn resolve(
    storage_root: &Path,
    explicit: Option<String>,
    rotate: bool,
) -> Result<ResolvedToken, String> {
    if let Some(value) = explicit {
        validate(&value)?;
        return Ok(ResolvedToken { value, path: None });
    }

    let path = storage_root.join(FILE_NAME);
    if !rotate {
        match read(&path) {
            Ok(value) => {
                return Ok(ResolvedToken {
                    value,
                    path: Some(path),
                });
            }
            Err(ReadError::Missing) => {}
            Err(ReadError::Invalid(error)) => return Err(error),
        }
    }

    let value = uuid::Uuid::new_v4().to_string();
    write(storage_root, &value)?;
    Ok(ResolvedToken {
        value,
        path: Some(path),
    })
}

enum ReadError {
    Missing,
    Invalid(String),
}

fn read(path: &Path) -> Result<String, ReadError> {
    let metadata = std::fs::symlink_metadata(path).map_err(|error| {
        if error.kind() == ErrorKind::NotFound {
            ReadError::Missing
        } else {
            ReadError::Invalid(format!("cannot inspect {}: {error}", path.display()))
        }
    })?;
    if metadata.file_type().is_symlink() {
        return Err(ReadError::Invalid(format!(
            "{} must not be a symbolic link",
            path.display()
        )));
    }
    if !metadata.is_file() {
        return Err(ReadError::Invalid(format!(
            "{} must be a regular file",
            path.display()
        )));
    }
    if metadata.len() > MAX_TOKEN_BYTES + 1 {
        return Err(ReadError::Invalid(format!(
            "{} is larger than the token limit",
            path.display()
        )));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let mode = metadata.permissions().mode() & 0o777;
        if mode & 0o077 != 0 {
            return Err(ReadError::Invalid(format!(
                "{} permissions are {mode:03o}; expected 600 (run `chmod 600 {}`)",
                path.display(),
                path.display()
            )));
        }
    }
    let raw = std::fs::read_to_string(path)
        .map_err(|error| ReadError::Invalid(format!("cannot read {}: {error}", path.display())))?;
    let value = raw.strip_suffix('\n').unwrap_or(&raw);
    validate(value)
        .map_err(|error| ReadError::Invalid(format!("{} contains an {error}", path.display())))?;
    Ok(value.to_owned())
}

fn write(storage_root: &Path, value: &str) -> Result<(), String> {
    let dir = Dir::open_ambient_dir(storage_root, cap_std::ambient_authority())
        .map_err(|error| format!("cannot open {}: {error}", storage_root.display()))?;
    crate::private_fs::write_text_atomic(&dir, storage_root, FILE_NAME, &format!("{value}\n"))
        .map_err(|error| {
            format!(
                "cannot persist {}: {error}",
                storage_root.join(FILE_NAME).display()
            )
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn root(name: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "clat-web-token-{name}-{}",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&root).unwrap();
        root
    }

    #[test]
    fn default_token_is_stable_and_rotation_replaces_it() {
        let root = root("stable");
        let first = resolve(&root, None, false).unwrap();
        let second = resolve(&root, None, false).unwrap();
        assert_eq!(first.value, second.value);
        let expected_path = root.join(FILE_NAME);
        assert_eq!(first.path.as_deref(), Some(expected_path.as_path()));

        let rotated = resolve(&root, None, true).unwrap();
        assert_ne!(rotated.value, first.value);
        assert_eq!(resolve(&root, None, false).unwrap().value, rotated.value);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            assert_eq!(
                std::fs::metadata(root.join(FILE_NAME))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
        }
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn explicit_token_never_reads_or_writes_the_file() {
        let root = root("explicit");
        std::fs::write(root.join(FILE_NAME), "persisted\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(root.join(FILE_NAME), std::fs::Permissions::from_mode(0o600))
                .unwrap();
        }
        let resolved = resolve(&root, Some("temporary-token".into()), false).unwrap();
        assert_eq!(resolved.value, "temporary-token");
        assert_eq!(resolved.path, None);
        assert_eq!(
            std::fs::read_to_string(root.join(FILE_NAME)).unwrap(),
            "persisted\n"
        );
        std::fs::remove_dir_all(root).ok();
    }

    #[cfg(unix)]
    #[test]
    fn symlink_and_group_readable_files_fail_closed() {
        use std::os::unix::fs::{PermissionsExt as _, symlink};

        let root = root("unsafe");
        let outside = root.with_extension("outside");
        std::fs::write(&outside, "outside-token\n").unwrap();
        symlink(&outside, root.join(FILE_NAME)).unwrap();
        assert!(
            resolve(&root, None, false)
                .unwrap_err()
                .contains("symbolic link")
        );

        std::fs::remove_file(root.join(FILE_NAME)).unwrap();
        std::fs::write(root.join(FILE_NAME), "readable-token\n").unwrap();
        std::fs::set_permissions(root.join(FILE_NAME), std::fs::Permissions::from_mode(0o640))
            .unwrap();
        assert!(
            resolve(&root, None, false)
                .unwrap_err()
                .contains("expected 600")
        );
        std::fs::remove_dir_all(root).ok();
        std::fs::remove_file(outside).ok();
    }
}
