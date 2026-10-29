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
        let mut command = command::base(&self.project, false)?;
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
