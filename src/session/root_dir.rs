//! Capability-held session root and no-follow directory traversal.
//!
//! A startup preflight is an admission check, not a lasting filesystem
//! capability. Every later operation therefore starts from this already-open
//! root handle and opens the opaque bucket/session components with an atomic
//! no-follow operation. Replacing any checked parent with a symlink cannot
//! redirect reads, log writes, or checkpoint publication outside the root.

use crate::session::key::SessionKey;
use crate::session::path_layout;
use cap_std::ambient_authority;
use cap_std::fs::Dir;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;

pub(crate) struct SessionRootDir {
    display_path: PathBuf,
    dir: Dir,
}

impl SessionRootDir {
    pub(crate) fn open_or_create(path: &Path) -> io::Result<Arc<Self>> {
        let parent = path
            .parent()
            .ok_or_else(|| io::Error::other("session root has no parent"))?;
        let name = path
            .file_name()
            .ok_or_else(|| io::Error::other("session root has no file name"))?;
        let parent = Dir::open_ambient_dir(parent, ambient_authority())?;
        match parent.create_dir(name) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error),
        }
        let dir = open_dir_nofollow(&parent, Path::new(name))?;
        set_private_dir(&dir)?;
        sync_dir(&dir)?;
        sync_dir(&parent)?;
        Ok(Arc::new(Self {
            display_path: path.to_path_buf(),
            dir,
        }))
    }

    pub(crate) fn display_path(&self) -> &Path {
        &self.display_path
    }

    pub(crate) fn root(&self) -> io::Result<Dir> {
        self.dir.try_clone()
    }

    pub(crate) fn open_session(&self, key: &SessionKey) -> io::Result<Dir> {
        let bucket = open_dir_nofollow(&self.dir, Path::new(&key.project.bucket))?;
        open_dir_nofollow(
            &bucket,
            Path::new(&path_layout::encode_segment(key.id.as_str())),
        )
    }

    pub(crate) fn create_session(&self, key: &SessionKey) -> io::Result<Dir> {
        let bucket = open_or_create_child(&self.dir, Path::new(&key.project.bucket))?;
        let session = open_or_create_child(
            &bucket,
            Path::new(&path_layout::encode_segment(key.id.as_str())),
        )?;
        sync_dir(&self.dir)?;
        sync_dir(&bucket)?;
        sync_dir(&session)?;
        Ok(session)
    }

    pub(crate) fn open_bucket(&self, bucket: &str) -> io::Result<Dir> {
        open_dir_nofollow(&self.dir, Path::new(bucket))
    }

    pub(crate) fn open_child(parent: &Dir, name: &Path) -> io::Result<Dir> {
        open_dir_nofollow(parent, name)
    }

    pub(crate) fn open_or_create_child(parent: &Dir, name: &Path) -> io::Result<Dir> {
        open_or_create_child(parent, name)
    }
}

fn open_or_create_child(parent: &Dir, name: &Path) -> io::Result<Dir> {
    match parent.create_dir(name) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(error),
    }
    let child = open_dir_nofollow(parent, name)?;
    set_private_dir(&child)?;
    sync_dir(parent)?;
    Ok(child)
}

fn open_dir_nofollow(parent: &Dir, name: &Path) -> io::Result<Dir> {
    let parent_file = parent.try_clone()?.into_std_file();
    cap_primitives::fs::open_dir_nofollow(&parent_file, name).map(Dir::from_std_file)
}

/// fsync a capability-held directory through a real descriptor.
///
/// cap-primitives opens sandboxed directories with `O_PATH` on Linux
/// (`compute_oflags`: `dir_required && !readdir_required && !write` ⇒
/// `O_PATH`). An `O_PATH` fd is a valid `openat` dirfd but `fsync` on it
/// fails with `EBADF` — every durable-directory sync returned "Bad file
/// descriptor" on Linux CI while macOS (no `O_PATH`) stayed green.
/// Re-open `.` through the capability to obtain a regular read-only
/// directory fd and fsync that.
///
/// Windows 直接 no-op（与 `private_fs::sync_dir` 的既有决定一致）：
/// FlushFileBuffers 要求句柄带写访问，能力重开的只读目录 fd 会以
/// ERROR_ACCESS_DENIED（os error 5）失败——Windows CI 腿首跑的
/// session 侧病根；目录 fsync 的 dirent 落盘语义在 NTFS 上由元数据
/// 日志覆盖。
pub(crate) fn sync_dir(dir: &Dir) -> io::Result<()> {
    #[cfg(unix)]
    {
        dir.open(".")?.sync_all()
    }
    #[cfg(not(unix))]
    {
        let _ = dir;
        Ok(())
    }
}

fn set_private_dir(_dir: &Dir) -> io::Result<()> {
    // 0700 权限位是 Unix 概念；Windows 构建下参数不使用（下划线前缀
    // 抑制该平台的 unused 警告）。
    #[cfg(unix)]
    {
        use cap_std::fs::{Permissions, PermissionsExt as _};
        _dir.set_permissions(".", Permissions::from_mode(0o700))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sync_dir_works_through_path_only_descriptors() {
        // Linux CI 回归：O_PATH 目录句柄上 fsync 报 EBADF（macOS 无
        // O_PATH 不可复现，仅断言本平台可用性——真回归由 CI 守护）。
        let root = std::env::temp_dir().join(format!(
            "clat-syncdir-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let dir = Dir::open_ambient_dir(&root, ambient_authority()).unwrap();
        sync_dir(&dir).expect("sync_dir must work on a freshly opened directory");
        crate::test_support::cleanup_tree(&root);
    }
}
