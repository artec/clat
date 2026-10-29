//! A private index binds generated messages and commits to the approved staged bytes.
use super::*;
use std::io::{Read, Write};
pub(super) struct FrozenIndex {
    path: PathBuf,
    dir: cap_std::fs::Dir,
    name: String,
}
impl Drop for FrozenIndex {
    fn drop(&mut self) {
        let _ = self.dir.remove_file(&self.name);
        let _ = self.dir.remove_file(format!("{}.lock", self.name));
    }
}
impl FrozenIndex {
    pub(super) fn path(&self) -> &Path {
        &self.path
    }
}
impl GitService {
    pub(super) fn freeze_index(&self, value: &Value) -> Result<FrozenIndex, String> {
        self.check_expected(value, true)?;
        self.check_commit_scope()?;
        let path = command::query(
            &self.project,
            &["rev-parse", "--path-format=absolute", "--git-path", "index"],
        )?;
        let path = PathBuf::from(String::from_utf8(path).map_err(|_| "git_encoding")?.trim());
        let parent = cap_std::fs::Dir::open_ambient_dir(
            path.parent().ok_or("git_index")?,
            cap_std::ambient_authority(),
        )
        .map_err(io)?;
        let mut options = cap_std::fs::OpenOptions::new();
        options.read(true);
        #[cfg(unix)]
        {
            use cap_std::fs::OpenOptionsExt;
            options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
        }
        #[cfg(windows)]
        {
            use cap_std::fs::OpenOptionsExt;
            options.custom_flags(0x00200000);
        }
        let mut file = parent
            .open_with(path.file_name().ok_or("git_index")?, &options)
            .map_err(io)?;
        if !file.metadata().map_err(io)?.is_file() {
            return Err("git_index".into());
        }
        let mut bytes = Vec::new();
        std::io::Read::by_ref(&mut file)
            .take(16 * 1024 * 1024 + 1)
            .read_to_end(&mut bytes)
            .map_err(io)?;
        if bytes.len() > 16 * 1024 * 1024 {
            return Err("git_index_large".into());
        }
        let dir = self.store.dir()?;
        let name = format!("index-{}", identity(value, "operationId")?);
        let mut options = cap_std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use cap_std::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut dest = dir.open_with(&name, &options).map_err(io)?;
        dest.write_all(&bytes).map_err(io)?;
        dest.sync_all().map_err(io)?;
        let frozen = FrozenIndex {
            path: self.store.path(&name),
            dir,
            name,
        };
        let mut cmd = command::base(&self.project, false)?;
        cmd.env("GIT_INDEX_FILE", frozen.path())
            .args(["ls-files", "--stage", "-z"]);
        let result = crate::process::capture_command(
            cmd,
            16 * 1024 * 1024,
            std::time::Duration::from_secs(10),
        )?;
        if !result.status.success()
            || digest(&result.stdout) != value["expected"]["index"].as_str().unwrap_or("")
        {
            return Err("git_changed".into());
        }
        Ok(frozen)
    }
    fn check_commit_scope(&self) -> Result<(), String> {
        let prefix = command::query(&self.project, &["rev-parse", "--show-prefix"])?;
        if !prefix.iter().all(u8::is_ascii_whitespace) {
            let prefix = String::from_utf8(prefix).map_err(|_| "git_encoding")?;
            let names = command::query(&self.project, &["diff", "--cached", "--name-only", "-z"])?;
            if names
                .split(|b| *b == 0)
                .filter(|p| !p.is_empty())
                .any(|p| !p.starts_with(prefix.trim_end().as_bytes()))
            {
                return Err("git_scope".into());
            }
        }
        Ok(())
    }
    pub(super) fn staged_diff(&self, index: &FrozenIndex) -> Result<String, String> {
        let worktree = DiffWorktree::new(&self.store)?;
        let mut command = command::base(&self.project, false)?;
        let git_dir = String::from_utf8(command::query(
            &self.project,
            &["rev-parse", "--absolute-git-dir"],
        )?)
        .map_err(|_| "git_encoding")?;
        // --cached and even --bare can reuse stat-matching live files.
        // Both cwd and worktree must be empty so Git reads staged blobs.
        command
            .current_dir(&worktree.path)
            .arg(format!("--git-dir={}", git_dir.trim()))
            .arg(format!("--work-tree={}", worktree.path.display()));
        command.env("GIT_INDEX_FILE", index.path()).args([
            "diff",
            "--cached",
            "--no-ext-diff",
            "--no-textconv",
            "--stat",
            "--patch",
        ]);
        let result = crate::process::capture_command(
            command,
            16 * 1024 * 1024,
            std::time::Duration::from_secs(10),
        )?;
        if !result.status.success() {
            return Err("git_failed".into());
        }
        String::from_utf8(result.stdout).map_err(|_| "git_encoding".into())
    }
}

struct DiffWorktree {
    dir: cap_std::fs::Dir,
    name: String,
    path: PathBuf,
}
impl DiffWorktree {
    fn new(store: &store::Store) -> Result<Self, String> {
        // Validate the private storage boundary before opening its ancestor.
        let _validated = store.dir()?;
        let parent = store.root_ancestor();
        let dir = cap_std::fs::Dir::open_ambient_dir(&parent, cap_std::ambient_authority())
            .map_err(io)?;
        // Nested repository IDs make the private index path exceed MAX_PATH.
        // Index files support long paths; CreateProcess's cwd does not. Keep
        // this empty directory outside the repositories/<id> nesting.
        let name = format!("dw-{}", uuid::Uuid::new_v4().simple());
        let builder = cap_std::fs::DirBuilder::new();
        #[cfg(unix)]
        let builder = {
            use cap_std::fs::DirBuilderExt;
            let mut builder = builder;
            builder.mode(0o700);
            builder
        };
        dir.create_dir_with(&name, &builder).map_err(io)?;
        Ok(Self {
            path: parent.join(&name),
            dir,
            name,
        })
    }
}
impl Drop for DiffWorktree {
    fn drop(&mut self) {
        let _ = self.dir.remove_dir(&self.name);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn staged_diff_cwd_is_private_empty_short_and_removed_after_nested_use() {
        let base = std::env::temp_dir().join(format!("clat-git-cwd-{}", uuid::Uuid::new_v4()));
        let store = store::Store::new(
            base.join("development")
                .join("a".repeat(64))
                .join("repositories")
                .join("b".repeat(64)),
        );
        let path = {
            let cwd = DiffWorktree::new(&store).unwrap();
            assert!(cwd.path.starts_with(&base));
            assert!(
                cwd.path
                    .as_os_str()
                    .to_string_lossy()
                    .encode_utf16()
                    .count()
                    < 248,
                "Windows process working directory must stay below MAX_PATH: {:?}",
                cwd.path
            );
            assert_eq!(std::fs::read_dir(&cwd.path).unwrap().count(), 0);
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                assert_eq!(
                    std::fs::metadata(&cwd.path).unwrap().permissions().mode() & 0o777,
                    0o700
                );
            }
            cwd.path.clone()
        };
        assert!(!path.exists());
        crate::test_support::cleanup_tree(&base);
    }
}
