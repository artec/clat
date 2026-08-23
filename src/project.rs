//! `Project`: the trusted project root with capability-relative
//! filesystem handles (path resolution and traversal defense).

use cap_std::ambient_authority;
use cap_std::fs::{Dir, OpenOptions};
use std::env;
use std::ffi::OsString;
use std::io;
use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

static TEMP_FILE_COUNTER: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Project {
    root: PathBuf,
}

impl Project {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    pub fn current() -> io::Result<Self> {
        Ok(Self::new(env::current_dir()?))
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn resolve_existing(&self, relative: impl AsRef<Path>) -> io::Result<PathBuf> {
        let requested = relative.as_ref();
        // SR1（读自由，对齐 DSH「every mode permits reading」）：绝对路径
        // 是显式的越项目读口——canonicalize 解析 symlink/`..`，存在即
        // 可读，全档位一致。保护水平与 DSH 的进程内读相同（检查后直读，
        // 接受同等的目录项竞态窗口）；项目相对路径仍走下方句柄纪律。
        if requested.is_absolute() {
            return requested.canonicalize();
        }

        validate_relative_path(requested)?;

        let root = self.root.canonicalize()?;
        let candidate = root.join(requested).canonicalize()?;

        if candidate.starts_with(&root) {
            Ok(candidate)
        } else {
            Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "path resolves outside the project root",
            ))
        }
    }

    /// 打开一个受写入围栏约束的写入目标。
    ///
    /// `scope == ProjectRoot`（一切档位的默认；exec 恒为）：目标必须是
    /// 项目根相对路径，`cap_std::fs::Dir` 在 Unix 使用 *at/目录句柄语义，
    /// 在 Windows 使用等价的 capability 路径解析。父目录创建、读取、
    /// 临时文件和最终 rename 都相对同一个已打开句柄：仓库中的符号链接
    /// 或并行目录替换不能把后续 I/O 重新解释到项目根之外。
    ///
    /// `scope == Unrestricted`（Full Access，SR2）：接受任意绝对路径。
    /// W-INV1 原子纪律泛化——canonicalize 目标父目录后打开 ambient 句
    /// 柄，temp+rename 仍绑定到该父目录的同一句柄；相对路径语义不变
    /// （仍按项目根解析）。RO/PW 下的绝对路径在这里被拒——错误明确
    /// 指向 "only writable under Full Access mode"。
    pub(crate) fn writable_target(
        &self,
        relative: impl AsRef<Path>,
        create_parents: bool,
        scope: crate::permission::WriteScope,
    ) -> io::Result<WritableTarget> {
        let requested = relative.as_ref();
        if requested.is_absolute() {
            if !matches!(scope, crate::permission::WriteScope::Unrestricted) {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "absolute paths are only writable under Full Access mode",
                ));
            }
            let file_name = requested.file_name().ok_or_else(|| {
                io::Error::new(io::ErrorKind::PermissionDenied, "path has no file name")
            })?;
            let parent = requested.parent().unwrap_or_else(|| Path::new("/"));
            // 已存在/创建失败都不在此报错——真正不可用由下一行的
            // canonicalize 以原始错误暴露。
            if create_parents {
                let _ = std::fs::create_dir_all(parent);
            }
            let parent = parent.canonicalize()?;
            let parent_dir = Dir::open_ambient_dir(&parent, ambient_authority())?;
            return Ok(WritableTarget {
                parent: parent_dir,
                file_name: file_name.to_os_string(),
            });
        }

        validate_relative_path(requested)?;

        let root = self.root.canonicalize()?;
        let file_name = requested.file_name().ok_or_else(|| {
            io::Error::new(io::ErrorKind::PermissionDenied, "path has no file name")
        })?;
        let root_dir = Dir::open_ambient_dir(&root, ambient_authority())?;
        let parent = requested.parent().unwrap_or_else(|| Path::new(""));
        if create_parents && !parent.as_os_str().is_empty() {
            root_dir.create_dir_all(parent)?;
        }
        let parent_dir = if parent.as_os_str().is_empty() {
            root_dir
        } else {
            root_dir.open_dir(parent)?
        };

        Ok(WritableTarget {
            parent: parent_dir,
            file_name: file_name.to_os_string(),
        })
    }

    /// 相对已打开的项目根 capability 读取普通文件。不存在返回 None；
    /// symlink（含 broken link）和非普通文件显式失败。最终 open 仍由
    /// cap-std 的目录句柄解析，检查后目录项替换不能把读取重定向到根外。
    pub(crate) fn read_file_limited(
        &self,
        relative: impl AsRef<Path>,
        max_bytes: usize,
    ) -> io::Result<Option<Vec<u8>>> {
        let relative = relative.as_ref();
        validate_relative_path(relative)?;
        let root = self.root.canonicalize()?;
        let root_dir = Dir::open_ambient_dir(root, ambient_authority())?;
        let metadata = match root_dir.symlink_metadata(relative) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error),
        };
        if metadata.file_type().is_symlink() {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "read target must not be a symbolic link",
            ));
        }
        if !metadata.is_file() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "read target is not a regular file",
            ));
        }
        let mut bytes = Vec::new();
        root_dir
            .open(relative)?
            .take(max_bytes as u64)
            .read_to_end(&mut bytes)?;
        Ok(Some(bytes))
    }

    pub fn relative_path(&self, path: &Path) -> io::Result<PathBuf> {
        let root = self.root.canonicalize()?;
        let path = path.canonicalize()?;
        path.strip_prefix(root)
            .map(Path::to_path_buf)
            .map_err(|_| io::Error::new(io::ErrorKind::PermissionDenied, "path is outside project"))
    }
}

/// 已绑定到项目内父目录句柄的单个文件目标。所有方法都相对
/// `parent` 操作，调用方拿不到可在检查后被重新解释的绝对路径。
#[derive(Debug)]
pub(crate) struct WritableTarget {
    parent: Dir,
    file_name: OsString,
}

impl WritableTarget {
    pub(crate) fn is_file(&self) -> io::Result<bool> {
        match self.parent.symlink_metadata(&self.file_name) {
            Ok(metadata) if metadata.file_type().is_symlink() => Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "write target must not be a symbolic link",
            )),
            Ok(metadata) => Ok(metadata.is_file()),
            Err(error) => Err(error),
        }
    }

    /// 从最终文件的 no-follow 句柄有界读取 UTF-8。元数据长度仅可作为
    /// 快速提示，不能作为内存边界：文件可在 stat 与 read 之间增长。
    /// 真正的闸门是这里最多取 cap+1 字节并立即拒绝超帽。
    pub(crate) fn read_to_string_limited(&self, max_bytes: usize) -> io::Result<String> {
        let file = self.open_regular_nofollow()?;
        read_utf8_limited(file, max_bytes)
    }

    /// 原子写入并返回目标在提交前是否存在。
    ///
    /// 父目录锁序列化所有遵守该协议的 CLAT 写入者；快照复查和
    /// rename 均在锁内。非合作进程无法被 advisory lock 强制约束，
    /// 但目录句柄仍保证它们不能借路径替换把本次写入引到项目外。
    pub(crate) fn atomic_write(
        &self,
        content: &str,
        expected_previous: Option<&str>,
    ) -> io::Result<bool> {
        let _guard = WriteGuard::acquire(&self.parent)?;
        self.reject_final_symlink()?;

        let metadata = match self.parent.symlink_metadata(&self.file_name) {
            Ok(metadata) => Some(metadata),
            Err(error) if error.kind() == io::ErrorKind::NotFound => None,
            Err(error) => return Err(error),
        };
        let existed = metadata.is_some();
        if let Some(expected) = expected_previous {
            // 快照复查只需知道「完全相等」；最多读 expected.len()+1。
            // 即使非合作进程在初读后把文件扩成巨物，提交区也不会整体
            // 物化它，且任何多/少/不同字节都按并发冲突处理。
            let current = self.open_regular_nofollow()?;
            if !reader_matches_expected(current, expected.as_bytes())? {
                return Err(io::Error::new(
                    io::ErrorKind::ResourceBusy,
                    "file changed while editing — re-read and retry",
                ));
            }
        }

        let temp_name = self.create_temp_file_name();
        let result = (|| -> io::Result<()> {
            let mut options = OpenOptions::new();
            options.write(true).create_new(true);
            let mut temp = self.parent.open_with(&temp_name, &options)?;
            temp.write_all(content.as_bytes())?;
            if let Some(metadata) = metadata
                && metadata.is_file()
            {
                temp.set_permissions(metadata.permissions())?;
            }
            temp.sync_all()?;
            drop(temp);

            // 在 rename 的最后时刻再次拒绝最终符号链接。即使非合作
            // 进程随后插入链接，rename 也只会替换目录项本身，不会
            // 跟随链接写入其目标。
            self.reject_final_symlink()?;
            self.parent
                .rename(&temp_name, &self.parent, &self.file_name)
        })();
        if let Err(error) = result {
            let _ = self.parent.remove_file(&temp_name);
            return Err(error);
        }
        Ok(existed)
    }

    fn reject_final_symlink(&self) -> io::Result<()> {
        match self.parent.symlink_metadata(&self.file_name) {
            Ok(metadata) if metadata.file_type().is_symlink() => Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "write target must not be a symbolic link",
            )),
            Ok(_) => Ok(()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error),
        }
    }

    /// 相对已经固定的父目录句柄，以 OS no-follow 标志打开最终文件；
    /// 打开后的 metadata 再确认普通文件，消除 check-then-open 竞态。
    fn open_regular_nofollow(&self) -> io::Result<cap_std::fs::File> {
        let mut options = OpenOptions::new();
        options.read(true);
        #[cfg(unix)]
        {
            use cap_std::fs::OpenOptionsExt as _;
            options.custom_flags(libc::O_NOFOLLOW);
        }
        #[cfg(windows)]
        {
            use cap_std::fs::OpenOptionsExt as _;
            const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
            options.custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
        }
        let file = self.parent.open_with(&self.file_name, &options)?;
        let metadata = file.metadata()?;
        if metadata.file_type().is_symlink() {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "write target must not be a symbolic link",
            ));
        }
        if !metadata.is_file() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "write target is not a regular file",
            ));
        }
        Ok(file)
    }

    fn create_temp_file_name(&self) -> OsString {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_nanos())
            .unwrap_or(0);
        let counter = TEMP_FILE_COUNTER.fetch_add(1, Ordering::Relaxed);
        OsString::from(format!(
            ".clat-tmp-{}-{unique}-{counter}.tmp",
            std::process::id()
        ))
    }
}

fn read_utf8_limited(mut reader: impl Read, max_bytes: usize) -> io::Result<String> {
    let mut bytes = Vec::new();
    reader
        .by_ref()
        .take(max_bytes.saturating_add(1) as u64)
        .read_to_end(&mut bytes)?;
    if bytes.len() > max_bytes {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("file exceeds the {max_bytes}-byte file cap"),
        ));
    }
    String::from_utf8(bytes)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error.utf8_error()))
}

fn reader_matches_expected(mut reader: impl Read, expected: &[u8]) -> io::Result<bool> {
    let mut current = Vec::new();
    reader
        .by_ref()
        .take(expected.len().saturating_add(1) as u64)
        .read_to_end(&mut current)?;
    Ok(current == expected)
}

#[cfg(unix)]
struct WriteGuard {
    _directory: std::fs::File,
}

#[cfg(unix)]
impl WriteGuard {
    fn acquire(parent: &Dir) -> io::Result<Self> {
        // Linux 上 `Dir` 本身可能是 O_PATH 描述符，不能直接 flock。
        // 通过 capability 再打开 `.` 得到可锁的只读目录描述符；路径
        // 解析仍受同一个父目录句柄约束。
        let directory = parent.open(".")?.into_std();
        directory.lock()?;
        Ok(Self {
            _directory: directory,
        })
    }
}

#[cfg(windows)]
struct WriteGuard {
    mutex: windows_sys::Win32::Foundation::HANDLE,
}

/// `Send`：句柄可跨线程使用；单持所有权随 guard 移动，Drop 恰好一次
///（与 `session::root_lease::StorageRootLease` 同款论证）。
#[cfg(windows)]
unsafe impl Send for WriteGuard {}

#[cfg(windows)]
impl WriteGuard {
    fn acquire(parent: &Dir) -> io::Result<Self> {
        // LockFileEx 不接受目录句柄（ERROR_INVALID_PARAMETER，F-W1-a
        // 首跑病根三）。以目录句柄的内核身份（卷序列号 + 文件索引，
        // GetFileInformationByHandle）命名一个互斥体——同目录的并发
        // atomic_write 在进程内与跨进程都被串行化（与 Unix flock 目录
        // 锁同语义）。known-divergence：命名互斥体线程可重入，同线程
        // 嵌套写同目录不会自锁——CLAT 无此调用形态。
        use sha2::Digest as _;
        use std::os::windows::io::AsRawHandle as _;
        use windows_sys::Win32::Foundation::{CloseHandle, WAIT_ABANDONED, WAIT_OBJECT_0};
        use windows_sys::Win32::Storage::FileSystem::{
            BY_HANDLE_FILE_INFORMATION, GetFileInformationByHandle,
        };
        use windows_sys::Win32::System::Threading::{CreateMutexW, INFINITE, WaitForSingleObject};

        let directory = parent.try_clone()?.into_std_file();
        let mut information = unsafe { std::mem::zeroed::<BY_HANDLE_FILE_INFORMATION>() };
        // SAFETY: directory 持有有效的目录句柄（FILE_FLAG_BACKUP_
        // SEMANTICS 打开），information 是未初始化目标的合法指针。
        if unsafe { GetFileInformationByHandle(directory.as_raw_handle(), &mut information) } == 0 {
            return Err(io::Error::last_os_error());
        }
        let identity = format!(
            "clat-write-guard-{:08x}-{:08x}{:08x}",
            information.dwVolumeSerialNumber, information.nFileIndexHigh, information.nFileIndexLow
        );
        let name = format!(
            "Local\\CLAT-{:x}",
            sha2::Sha256::digest(identity.as_bytes())
        );
        let mut wide: Vec<u16> = name.encode_utf16().collect();
        wide.push(0);
        // SAFETY: null 安全属性 + NUL 结尾的 UTF-16 名；所有权由
        // WaitForSingleObject 的获取语义建立。
        let mutex = unsafe { CreateMutexW(std::ptr::null(), 0, wide.as_ptr()) };
        if mutex.is_null() {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: mutex 是 CreateMutexW 的活句柄。
        let waited = unsafe { WaitForSingleObject(mutex, INFINITE) };
        match waited {
            WAIT_OBJECT_0 | WAIT_ABANDONED => Ok(Self { mutex }),
            _ => {
                unsafe { CloseHandle(mutex) };
                Err(io::Error::last_os_error())
            }
        }
    }
}

#[cfg(windows)]
impl Drop for WriteGuard {
    fn drop(&mut self) {
        use windows_sys::Win32::Foundation::CloseHandle;
        use windows_sys::Win32::System::Threading::ReleaseMutex;
        // SAFETY: mutex 是本 guard 持有的活句柄；ReleaseMutex 失败仅
        // 意味着跨线程 Drop（ERROR_NOT_OWNER），句柄关闭即释放最后引用。
        unsafe {
            ReleaseMutex(self.mutex);
            CloseHandle(self.mutex);
        }
    }
}

fn validate_relative_path(path: &Path) -> io::Result<()> {
    if path.is_absolute() {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "absolute paths are not allowed",
        ));
    }

    for component in path.components() {
        if matches!(
            component,
            Component::ParentDir | Component::RootDir | Component::Prefix(_)
        ) {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "parent traversal is not allowed",
            ));
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RA-04 判别腿：生产读取与提交复查的底层 helper 对一个永不 EOF
    /// 的来源也只请求 cap+1 / expected+1 字节。删掉 `take` 会继续读到
    /// 人工 EOF 并越过计数断言。
    #[test]
    fn bounded_read_helpers_stop_at_cap_plus_one() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicUsize, Ordering};

        struct CountedBytes {
            reads: Arc<AtomicUsize>,
            remaining: usize,
        }
        impl Read for CountedBytes {
            fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
                if self.remaining == 0 {
                    return Ok(0);
                }
                let count = buffer.len().min(self.remaining);
                buffer[..count].fill(b'x');
                self.remaining -= count;
                self.reads.fetch_add(count, Ordering::SeqCst);
                Ok(count)
            }
        }

        let reads = Arc::new(AtomicUsize::new(0));
        let error = read_utf8_limited(
            CountedBytes {
                reads: Arc::clone(&reads),
                remaining: 1024 * 1024,
            },
            16,
        )
        .expect_err("the source exceeds the cap");
        assert_eq!(reads.load(Ordering::SeqCst), 17);
        assert!(error.to_string().contains("16-byte file cap"));

        reads.store(0, Ordering::SeqCst);
        assert!(
            !reader_matches_expected(
                CountedBytes {
                    reads: Arc::clone(&reads),
                    remaining: 1024 * 1024,
                },
                b"short",
            )
            .unwrap()
        );
        assert_eq!(
            reads.load(Ordering::SeqCst),
            6,
            "snapshot comparison reads only expected.len()+1"
        );
    }

    /// RA-04：最终文件替换成 symlink 后，实际读取 open 必须 no-follow；
    /// 检查时刻与打开时刻之间的目录项变化不能把读取导向 victim。
    #[cfg(unix)]
    #[test]
    fn writable_target_reads_never_follow_the_final_symlink() {
        let root = temp_dir();
        let victim = root.join("victim.txt");
        fs::write(&victim, "secret").unwrap();
        let path = root.join("note.txt");
        fs::write(&path, "safe").unwrap();
        let target = Project::new(&root)
            .writable_target(
                "note.txt",
                false,
                crate::permission::WriteScope::ProjectRoot,
            )
            .unwrap();
        fs::remove_file(&path).unwrap();
        std::os::unix::fs::symlink(&victim, &path).unwrap();
        let error = target
            .read_to_string_limited(1024)
            .expect_err("final symlink is refused by open");
        // O_NOFOLLOW 打开 symlink 的错误码是平台方言：Linux 报 ELOOP，
        // macOS 报 EPERM（→ PermissionDenied）。两者都是「拒绝跟随」的
        // 证明；判别腿是读取失败且 victim 内容原封不动。stable Rust 不能
        // match 未稳定的 ErrorKind::FilesystemLoop（io_error_more），所以
        // 用原始 errno 判别。
        assert!(
            error.raw_os_error() == Some(libc::ELOOP)
                || error.raw_os_error() == Some(libc::EPERM)
                || error.kind() == io::ErrorKind::PermissionDenied,
            "unexpected error for no-follow refusal: {error}"
        );
        assert_eq!(fs::read_to_string(&victim).unwrap(), "secret");
        crate::test_support::cleanup_tree(&root);
    }

    /// RA-04 提交阶段判别腿：初读后文件膨胀成大对象时，快照比较只读
    /// 旧快照长度+1 并返回冲突；不得为判断不等而物化当前整文件。
    #[test]
    fn atomic_snapshot_check_rejects_a_grown_file() {
        let root = temp_dir();
        let path = root.join("note.txt");
        fs::write(&path, "small").unwrap();
        let target = Project::new(&root)
            .writable_target(
                "note.txt",
                false,
                crate::permission::WriteScope::ProjectRoot,
            )
            .unwrap();
        fs::write(&path, vec![b'x'; 2 * 1024 * 1024]).unwrap();
        let error = target
            .atomic_write("mine", Some("small"))
            .expect_err("the changed snapshot is rejected");
        assert_eq!(error.kind(), io::ErrorKind::ResourceBusy);
        assert_eq!(fs::metadata(&path).unwrap().len(), 2 * 1024 * 1024);
        crate::test_support::cleanup_tree(&root);
    }
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_dir() -> PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time")
            .as_nanos();
        let dir = env::temp_dir().join(format!("clat-project-test-{unique}"));
        fs::create_dir_all(&dir).expect("temp dir");
        dir
    }

    #[test]
    fn resolves_paths_inside_project() {
        let root = temp_dir();
        fs::write(root.join("README.md"), "hello").expect("file");
        let project = Project::new(&root);

        let resolved = project.resolve_existing("README.md").expect("resolve");
        assert!(resolved.starts_with(root.canonicalize().expect("root")));

        crate::test_support::cleanup_tree(&root);
    }

    #[test]
    fn rejects_parent_traversal() {
        let root = temp_dir();
        let project = Project::new(&root);

        let error = project
            .resolve_existing("../secret")
            .expect_err("must reject");
        assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);

        crate::test_support::cleanup_tree(&root);
    }

    /// W-INV1：写入解析绝不指向项目根之外。
    ///（F-W1：`std::os::unix::fs::symlink` 只在 Unix 编译——Windows
    /// CI 腿因此曾会编译失败。）
    #[cfg(unix)]
    #[test]
    fn writable_paths_stay_inside_the_project() {
        let root = temp_dir();
        let project = Project::new(&root);

        // 新建路径（文件不存在）：合法，父目录和文件都只在根内物化。
        let target = project
            .writable_target(
                "src/new/module.rs",
                true,
                crate::permission::WriteScope::ProjectRoot,
            )
            .expect("open write target");
        assert!(!target.atomic_write("hello", None).expect("write file"));
        assert_eq!(
            fs::read_to_string(root.join("src/new/module.rs")).expect("read file"),
            "hello"
        );

        // `..` 穿越：拒绝。
        let error = project
            .writable_target(
                "../escape.txt",
                true,
                crate::permission::WriteScope::ProjectRoot,
            )
            .expect_err("must reject traversal");
        assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);

        // 符号链接父目录指向项目外（unix）：canonicalize 展开后落在
        // 根外 → 拒绝。两种形态都要 fail closed：
        // - 指向已存在目录 → PermissionDenied（逐组件先查再建）；
        // - 悬空链接 → 同样拒绝（错误类型不是安全语义的一部分）。
        #[cfg(unix)]
        {
            let outside = root.parent().expect("parent").join(format!(
                "clat-escape-{}",
                root.file_name().unwrap().to_string_lossy()
            ));
            fs::create_dir_all(&outside).expect("outside dir");
            std::os::unix::fs::symlink(&outside, root.join("link")).expect("symlink");
            assert!(
                project
                    .writable_target(
                        "link/pwned.txt",
                        true,
                        crate::permission::WriteScope::ProjectRoot
                    )
                    .is_err(),
                "must reject symlinked parent"
            );
            // 不变量核查：项目外没有任何新路径被物化。
            assert!(!outside.join("pwned.txt").exists());

            // NWE-01 回归：链接后跟**多级不存在目录**——旧实现
            // create_dir_all 会先在根外物化嵌套目录再拒绝。修复后
            // 第一个组件（link）就被拦截，嵌套目录零物化。
            assert!(
                project
                    .writable_target(
                        "link/created/nested/deep.txt",
                        true,
                        crate::permission::WriteScope::ProjectRoot
                    )
                    .is_err(),
                "must reject before creating anything"
            );
            assert!(
                !outside.join("created").exists(),
                "no directory outside the project may be materialized"
            );
            let _ = std::fs::remove_file(root.join("link"));

            // 悬空链接：同样必须失败（错误类型不限），不得跟随。
            let dangling = root.join("dangling");
            std::os::unix::fs::symlink("/nonexistent-clat-escape", &dangling).expect("symlink");
            assert!(
                project
                    .writable_target(
                        "dangling/x.txt",
                        true,
                        crate::permission::WriteScope::ProjectRoot
                    )
                    .is_err()
            );
            let _ = std::fs::remove_file(&dangling);
            let _ = fs::remove_dir(&outside);
        }

        // 符号链接文件名指向项目外：写入即写目标 → 拒绝。
        #[cfg(unix)]
        {
            let outside_file = root
                .parent()
                .expect("parent")
                .join("clat-escape-target.txt");
            fs::write(&outside_file, "before").expect("target");
            std::os::unix::fs::symlink(&outside_file, root.join("alias.txt")).expect("symlink");
            let target = project
                .writable_target(
                    "alias.txt",
                    true,
                    crate::permission::WriteScope::ProjectRoot,
                )
                .expect("open parent capability");
            let error = target
                .atomic_write("after", None)
                .expect_err("must reject symlinked file");
            assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
            assert_eq!(fs::read_to_string(&outside_file).unwrap(), "before");
            let _ = fs::remove_file(root.join("alias.txt"));
            let _ = fs::remove_file(outside_file);
        }

        crate::test_support::cleanup_tree(&root);
    }

    /// NWE-05 回归：打开父目录句柄后，即使路径名被替换为指向项目外
    /// 的链接，提交仍落在原父目录句柄，不会重新解析越界路径。
    #[cfg(unix)]
    #[test]
    fn parent_replacement_cannot_redirect_an_open_write_target() {
        let root = temp_dir();
        let outside = root
            .parent()
            .expect("parent")
            .join(format!("clat-race-outside-{}", std::process::id()));
        fs::create_dir_all(root.join("work")).expect("work dir");
        fs::create_dir_all(&outside).expect("outside dir");
        let project = Project::new(&root);
        let target = project
            .writable_target(
                "work/result.txt",
                false,
                crate::permission::WriteScope::ProjectRoot,
            )
            .expect("open target");

        fs::rename(root.join("work"), root.join("detached-work")).expect("replace parent");
        std::os::unix::fs::symlink(&outside, root.join("work")).expect("redirecting symlink");

        target
            .atomic_write("safe", None)
            .expect("write through handle");
        assert_eq!(
            fs::read_to_string(root.join("detached-work/result.txt")).expect("safe file"),
            "safe"
        );
        assert!(!outside.join("result.txt").exists());

        fs::remove_file(root.join("work")).expect("remove symlink");
        crate::test_support::cleanup_tree(&root);
        crate::test_support::cleanup_tree(&outside);
    }

    /// NWE-06 回归：两个 CLAT 写入者持有相同旧快照时，只允许一个
    /// 在父目录锁内完成快照复查与提交，另一个必须报告冲突。
    #[test]
    fn concurrent_snapshot_commits_do_not_silently_overwrite() {
        use std::sync::{Arc, Barrier};

        let root = temp_dir();
        fs::write(root.join("note.txt"), "original").expect("seed file");
        let project = Project::new(&root);
        let first = project
            .writable_target(
                "note.txt",
                false,
                crate::permission::WriteScope::ProjectRoot,
            )
            .expect("first target");
        let second = project
            .writable_target(
                "note.txt",
                false,
                crate::permission::WriteScope::ProjectRoot,
            )
            .expect("second target");
        let barrier = Arc::new(Barrier::new(3));

        let spawn_writer = |target: WritableTarget, content: &'static str| {
            let barrier = Arc::clone(&barrier);
            std::thread::spawn(move || {
                barrier.wait();
                target.atomic_write(content, Some("original"))
            })
        };
        let first = spawn_writer(first, "first");
        let second = spawn_writer(second, "second");
        barrier.wait();

        let results = [
            first.join().expect("first join"),
            second.join().expect("second join"),
        ];
        assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
        let conflict = results
            .iter()
            .find_map(|result| result.as_ref().err())
            .expect("one conflict");
        assert_eq!(conflict.kind(), io::ErrorKind::ResourceBusy);
        let final_content = fs::read_to_string(root.join("note.txt")).expect("final content");
        assert!(matches!(final_content.as_str(), "first" | "second"));

        crate::test_support::cleanup_tree(&root);
    }
}
