//! Repository identities are resolved by the core, never supplied as caller paths.
use super::*;
use std::time::{Duration, Instant};
impl GitService {
    pub(super) fn repositories(&self) -> Result<Value, String> {
        let root = self.project.root().canonicalize().map_err(io)?;
        let mut rows = Vec::new();
        if let Ok(row) = describe(&root, &root, "project", "main") {
            rows.push(row);
        }
        let mut partial = add_registered(&root, &mut rows)?;
        let deadline = Instant::now() + Duration::from_secs(3);
        let mut walker = ignore::WalkBuilder::new(&root);
        walker
            .hidden(false)
            .follow_links(false)
            .git_global(false)
            .require_git(false);
        walker.filter_entry(|entry| {
            entry.depth() == 0
                || ![".git", "node_modules", "target", ".clat"]
                    .iter()
                    .any(|name| entry.file_name() == *name)
        });
        let mut visited = 0;
        for entry in walker.build() {
            if Instant::now() >= deadline {
                partial = true;
                break;
            }
            let entry = match entry {
                Ok(entry) => entry,
                Err(_) => {
                    partial = true;
                    continue;
                }
            };
            if entry.depth() == 0
                || !entry
                    .file_type()
                    .is_some_and(|t| t.is_dir() && !t.is_symlink())
            {
                continue;
            }
            visited += 1;
            if visited > 5000 {
                partial = true;
                break;
            }
            if std::fs::symlink_metadata(entry.path().join(".git"))
                .is_ok_and(|m| !m.file_type().is_symlink())
            {
                if rows.len() >= 128 {
                    partial = true;
                    break;
                }
                add_available(&root, entry.path(), "nested", &mut rows);
            }
        }
        rows.sort_by_key(|row| {
            (
                row["id"] != "project",
                row["path"].as_str().unwrap_or("").to_owned(),
            )
        });
        Ok(json!({"repositories":rows,"truncated":partial}))
    }
    pub(super) fn repository(self: &Arc<Self>, id: &str) -> Result<Arc<Self>, String> {
        if id == "project" {
            return Ok(self.clone());
        }
        if self.closed.load(Ordering::SeqCst) {
            return Err("host_closed".into());
        }
        let root = self.project.root().canonicalize().map_err(io)?;
        let cached = self
            .repositories
            .lock()
            .map_err(|_| "git_busy")?
            .get(id)
            .cloned();
        if let Some(service) = cached {
            validate_repository(&root, service.project.root())?;
            return Ok(service);
        }
        let directory = self.repositories()?;
        let row = directory["repositories"]
            .as_array()
            .and_then(|rows| {
                rows.iter()
                    .find(|r| r["id"] == id && r["available"] == true)
            })
            .ok_or("git_repository_missing")?;
        let path = root.join(text(row, "path")?);
        validate_repository(&root, &path)?;
        let mut services = self.repositories.lock().map_err(|_| "git_busy")?;
        Ok(services
            .entry(id.into())
            .or_insert_with(|| {
                Arc::new(Self::new(
                    Project::new(&path),
                    store::Store::new(self.store.path("repositories").join(id)),
                ))
            })
            .clone())
    }
}
fn describe(root: &Path, path: &Path, id: &str, kind: &str) -> Result<Value, String> {
    let project = Project::new(path);
    let top = command::query(&project, &["rev-parse", "--show-toplevel"])?;
    if id != "project"
        && Path::new(String::from_utf8_lossy(&top).trim())
            .canonicalize()
            .map_err(io)?
            != path
    {
        return Err("git_repository_missing".into());
    }
    let branch = command::query(&project, &["symbolic-ref", "--quiet", "--short", "HEAD"])
        .unwrap_or_default();
    let status = command::query(
        &project,
        &[
            "status",
            "--porcelain=v1",
            "-z",
            "--untracked-files=normal",
            "--",
            ".",
        ],
    )?;
    let submodule = command::query(&project, &["rev-parse", "--show-superproject-working-tree"])
        .unwrap_or_default();
    Ok(
        json!({"id":id,"path":path.strip_prefix(root).map_err(|_|"path_denied")?.to_string_lossy(),"kind":if submodule.iter().any(|b| !b.is_ascii_whitespace()) {"submodule"}else{kind},"available":true,"branch":String::from_utf8_lossy(&branch).trim(),"dirty":!status.is_empty()}),
    )
}
// Registered repositories remain discoverable even when their paths are ignored.
fn add_registered(root: &Path, rows: &mut Vec<Value>) -> Result<bool, String> {
    let project = Project::new(root);
    let worktrees =
        command::query(&project, &["worktree", "list", "--porcelain", "-z"]).unwrap_or_default();
    for field in worktrees.split(|b| *b == 0) {
        let Some(path) = field
            .strip_prefix(b"worktree ")
            .and_then(|p| std::str::from_utf8(p).ok())
        else {
            continue;
        };
        if rows.len() >= 128 {
            return Ok(true);
        }
        if let Some(target) = rooted_worktree(root, Path::new(path)) {
            add_available(root, &target, "worktree", rows);
        }
    }
    let index = command::query(&project, &["ls-files", "--stage", "-z"]).unwrap_or_default();
    for entry in index
        .split(|b| *b == 0)
        .filter(|b| b.starts_with(b"160000 "))
    {
        let Some(path) = entry
            .splitn(2, |b| *b == b'\t')
            .nth(1)
            .and_then(|p| std::str::from_utf8(p).ok())
        else {
            continue;
        };
        if relative(path).is_err() || rows.iter().any(|r| r["path"] == path) {
            continue;
        }
        if rows.len() >= 128 {
            return Ok(true);
        }
        let target = root.join(path);
        if !add_available(root, &target, "submodule", rows) {
            rows.push(json!({"id":digest(target.to_string_lossy().as_bytes()),"path":path,"kind":"submodule","available":false,"branch":"","dirty":false}));
        }
    }
    Ok(false)
}
fn add_available(root: &Path, path: &Path, kind: &str, rows: &mut Vec<Value>) -> bool {
    let Ok(relative) = path.strip_prefix(root) else {
        return false;
    };
    if rows
        .iter()
        .any(|row| row["path"] == relative.to_string_lossy().as_ref())
    {
        return true;
    }
    if validate_repository(root, path).is_err() {
        return false;
    }
    let id = digest(path.to_string_lossy().as_bytes());
    if let Ok(row) = describe(root, path, &id, kind) {
        rows.push(row);
        return true;
    }
    false
}

fn validate_repository(root: &Path, path: &Path) -> Result<(), String> {
    let relative = path.strip_prefix(root).map_err(|_| "path_denied")?;
    let mut current = root.to_path_buf();
    for part in relative.components() {
        current.push(part);
        let meta = std::fs::symlink_metadata(&current).map_err(io)?;
        if !meta.is_dir() || meta.file_type().is_symlink() {
            return Err("path_denied".into());
        }
    }
    if std::fs::symlink_metadata(path.join(".git"))
        .map_err(io)?
        .file_type()
        .is_symlink()
    {
        return Err("path_denied".into());
    }
    let top = command::query(&Project::new(path), &["rev-parse", "--show-toplevel"])?;
    if Path::new(String::from_utf8_lossy(&top).trim())
        .canonicalize()
        .map_err(io)?
        != path
    {
        return Err("git_repository_missing".into());
    }
    Ok(())
}

// Git may print non-verbatim Windows paths or a filesystem alias for the project
// root. Preserve lexical components beneath that root so validation still rejects
// interior symlinks rather than silently canonicalizing them into another repo.
fn rooted_worktree(root: &Path, path: &Path) -> Option<PathBuf> {
    for ancestor in path.ancestors() {
        if ancestor.canonicalize().ok().as_deref() == Some(root) {
            return Some(root.join(path.strip_prefix(ancestor).ok()?));
        }
    }
    None
}
