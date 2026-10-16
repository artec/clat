use super::*;
use serde::Serialize;
use std::collections::BTreeMap;
use std::io::Read;

#[derive(Debug, PartialEq)]
pub(super) struct Snapshot {
    pub head: String,
    pub files: Vec<File>,
}
#[derive(Debug, PartialEq, Serialize)]
pub(super) struct File {
    path: String,
    untracked: bool,
    sha256: Option<String>,
    bytes: usize,
    #[serde(skip)]
    stamp: String,
}
impl Snapshot {
    pub(super) fn capture(project: &Project, cancel: &CancelToken) -> Result<Self, String> {
        let prefix = git(project, &["rev-parse", "--show-prefix"], 4096, cancel)?.0;
        if prefix != b"\n" {
            return Err("export_patch: run from the repository root".into());
        }
        let head =
            String::from_utf8(git(project, &["rev-parse", "--verify", "HEAD"], 4096, cancel)?.0)
                .map_err(|_| "Invalid Git HEAD")?
                .trim()
                .to_owned();
        if !matches!(head.len(), 40 | 64) || !head.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err("Invalid Git HEAD".into());
        }
        if !git(
            project,
            &["ls-files", "--unmerged", "-z"],
            PATCH_BYTES,
            cancel,
        )?
        .0
        .is_empty()
        {
            return Err("export_patch: unresolved merge; no partial export".into());
        }
        let paths = changed_paths(project, cancel)?;
        if paths.is_empty() || paths.len() > 128 {
            return Err("export_patch: requires 1..=128 changed files; no partial export".into());
        }
        let mut total = 0;
        let mut files = Vec::new();
        for (path, untracked) in paths {
            check_cancel(cancel)?;
            let file = File::capture(project, path, untracked, cancel)?;
            total += file.bytes;
            if total > 8 * PATCH_BYTES {
                return Err("export_patch: source bytes exceed 8 MiB".into());
            }
            files.push(file);
        }
        Ok(Self { head, files })
    }

    pub(super) fn patch(&self, project: &Project, cancel: &CancelToken) -> Result<Vec<u8>, String> {
        let flags = [
            "diff",
            "--ignore-submodules=none",
            "--binary",
            "--full-index",
            "--no-ext-diff",
            "--no-textconv",
            "--no-color",
            "--no-renames",
            "--src-prefix=a/",
            "--dst-prefix=b/",
        ];
        let mut tracked = flags.to_vec();
        tracked.extend(["HEAD", "--"]);
        let mut patch = git(project, &tracked, PATCH_BYTES, cancel)?.0;
        for file in self.files.iter().filter(|f| f.untracked) {
            let mut args = flags.to_vec();
            args.extend(["--no-index", "--", "/dev/null", &file.path]);
            patch.extend(git(project, &args, PATCH_BYTES, cancel)?.0);
            if patch.len() > PATCH_BYTES {
                return Err(
                    "export_patch: patch exceeds 1 MiB; no truncation or publication".into(),
                );
            }
        }
        Ok(patch)
    }
}
impl File {
    fn capture(
        project: &Project,
        path: String,
        untracked: bool,
        cancel: &CancelToken,
    ) -> Result<Self, String> {
        let deleted = |path| Self {
            path,
            untracked,
            sha256: None,
            bytes: 0,
            stamp: "deleted".into(),
        };
        let target = match project.writable_target(&path, false, WriteScope::ProjectRoot) {
            Ok(target) => target,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(deleted(path)),
            Err(e) => return Err(e.to_string()),
        };
        let mut file = match target.open_regular_nofollow() {
            Ok(file) => file,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(deleted(path)),
            Err(_) => return Err("export_patch: source must be a regular non-symlink file".into()),
        };
        let metadata = file.metadata().map_err(|e| e.to_string())?;
        let mut bytes = Vec::new();
        file.by_ref()
            .take((PATCH_BYTES + 1) as u64)
            .read_to_end(&mut bytes)
            .map_err(|e| e.to_string())?;
        if bytes.len() > PATCH_BYTES {
            return Err("export_patch: source file exceeds 1 MiB".into());
        }
        let raw = git(
            project,
            &["hash-object", "--no-filters", "--", &path],
            4096,
            cancel,
        )?
        .0;
        let normalized = git(
            project,
            &["hash-object", &format!("--path={path}"), "--", &path],
            4096,
            cancel,
        )?
        .0;
        if raw != normalized {
            return Err("export_patch: Git encoding/EOL conversion changes source bytes; normalize before freezing".into());
        }
        let stamp = format!("{:?}:{:?}", metadata.modified(), metadata.permissions());
        Ok(Self {
            path,
            untracked,
            sha256: Some(digest(&bytes)),
            bytes: bytes.len(),
            stamp,
        })
    }
}
fn path_string(bytes: &[u8]) -> Result<String, String> {
    let path = std::str::from_utf8(bytes).map_err(|_| "export_patch: non-UTF-8 Git path")?;
    if path.len() > 4096 {
        return Err("export_patch: Git path exceeds 4096 bytes".into());
    }
    super::super::validate_relative_path(std::path::Path::new(path)).map_err(|e| e.to_string())?;
    Ok(path.to_owned())
}

fn changed_paths(
    project: &Project,
    cancel: &CancelToken,
) -> Result<BTreeMap<String, bool>, String> {
    let raw = git(
        project,
        &[
            "diff",
            "--raw",
            "--ignore-submodules=none",
            "-z",
            "--no-abbrev",
            "--no-renames",
            "HEAD",
            "--",
        ],
        PATCH_BYTES,
        cancel,
    )?
    .0;
    let mut paths = BTreeMap::new();
    let mut entries = raw.split(|b| *b == 0).filter(|part| !part.is_empty());
    while let Some(header) = entries.next() {
        let header = std::str::from_utf8(header).map_err(|_| "Invalid Git raw header")?;
        let mut modes = header.trim_start_matches(':').split_whitespace();
        for _ in 0..2 {
            if !matches!(modes.next(), Some("000000" | "100644" | "100755")) {
                return Err(
                    "export_patch: symlink/submodule/type change unsupported; no partial export"
                        .into(),
                );
            }
        }
        let path = entries.next().ok_or("Invalid Git raw path")?;
        paths.insert(path_string(path)?, false);
    }
    for path in git(
        project,
        &["ls-files", "--others", "--exclude-standard", "-z", "--"],
        PATCH_BYTES,
        cancel,
    )?
    .0
    .split(|b| *b == 0)
    .filter(|p| !p.is_empty())
    {
        paths.insert(path_string(path)?, true);
    }

    Ok(paths)
}
