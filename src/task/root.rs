//! A task's project identity is a directory capability, not a re-resolved name.
use super::state::digest;
#[cfg(windows)]
use cap_primitives::fs::_WindowsByHandle;
use cap_std::fs::Dir;
#[cfg(unix)]
use cap_std::fs::MetadataExt;
pub(super) struct Root {
    dir: Dir,
    path: std::path::PathBuf,
    pub(super) identity: String,
}
impl Root {
    pub(super) fn open(path: &std::path::Path) -> Result<Self, String> {
        let path = path.canonicalize().map_err(|e| e.to_string())?;
        let dir = Dir::open_ambient_dir(&path, cap_std::ambient_authority())
            .map_err(|e| e.to_string())?;
        let identity = identity(&dir)?;
        Ok(Self {
            dir,
            path,
            identity,
        })
    }
    pub(super) fn check(&self) -> Result<(), String> {
        if self.path.canonicalize().map_err(|e| e.to_string())? != self.path {
            return Err("task project root was replaced by a symbolic link".into());
        }
        let current = Dir::open_ambient_dir(&self.path, cap_std::ambient_authority())
            .map_err(|e| e.to_string())?;
        if identity(&current)? != self.identity {
            return Err("task project directory identity changed".into());
        }
        Ok(())
    }
    pub(super) fn read(&self, path: &str, limit: usize) -> std::io::Result<Option<Vec<u8>>> {
        self.check().map_err(std::io::Error::other)?;
        read_nofollow(&self.dir, path, limit)
    }
}
fn identity(dir: &Dir) -> Result<String, String> {
    let metadata = dir.metadata(".").map_err(|e| e.to_string())?;
    let birth = metadata
        .created()
        .ok()
        .and_then(|t| t.into_std().duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_nanos());
    #[cfg(unix)]
    let id = format!("unix:{}:{}:{birth}", metadata.dev(), metadata.ino());
    #[cfg(windows)]
    let id = format!(
        "windows:{}:{}:{birth}",
        metadata
            .volume_serial_number()
            .ok_or("missing volume identity")?,
        metadata.file_index().ok_or("missing file identity")?
    );
    #[cfg(not(any(unix, windows)))]
    return Err("task recovery requires a stable directory identity on this platform".into());
    #[cfg(any(unix, windows))]
    Ok(digest(id.as_bytes()))
}

fn read_nofollow(
    root: &cap_std::fs::Dir,
    path: &str,
    limit: usize,
) -> std::io::Result<Option<Vec<u8>>> {
    use cap_std::fs::{Dir, OpenOptions};
    use std::io::Read;
    let mut dir = root.try_clone()?;
    let parts: Vec<_> = std::path::Path::new(path).components().collect();
    for (index, part) in parts.iter().enumerate() {
        let last = index + 1 == parts.len();
        let mut options = OpenOptions::new();
        options.read(true);
        #[cfg(unix)]
        {
            use cap_std::fs::OpenOptionsExt;
            options.custom_flags(
                libc::O_NOFOLLOW | libc::O_NONBLOCK | if last { 0 } else { libc::O_DIRECTORY },
            );
        }
        #[cfg(windows)]
        {
            use cap_std::fs::OpenOptionsExt;
            options.custom_flags(0x0020_0000 | 0x0200_0000);
        }
        let file = match dir.open_with(part.as_os_str(), &options) {
            Ok(file) => file,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e),
        };
        let meta = file.metadata()?;
        if meta.file_type().is_symlink() || (last && !meta.is_file()) || (!last && !meta.is_dir()) {
            return Err(std::io::Error::other(
                "task reference is not a no-follow regular file",
            ));
        }
        if last {
            let mut bytes = Vec::new();
            file.take(limit as u64 + 1).read_to_end(&mut bytes)?;
            if bytes.len() > limit {
                return Err(std::io::Error::other("artifact too large"));
            }
            return Ok(Some(bytes));
        }
        dir = Dir::from_std_file(file.into_std());
    }
    Err(std::io::Error::other("empty reference"))
}
