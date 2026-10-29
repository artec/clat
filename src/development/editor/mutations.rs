//! Capability-bound human file operations; trash remains outside the repository.
use super::*;
impl DevelopmentService {
    pub(super) fn mutate_document(
        &self,
        method: &str,
        value: &Value,
        id: &str,
    ) -> Result<Value, String> {
        let path = text(value, "path")?;
        relative(path)?;
        self.check_private(path)?;
        let root =
            cap_std::fs::Dir::open_ambient_dir(self.project.root(), cap_std::ambient_authority())
                .map_err(io)?;
        if method == "editor.create" {
            if value["directory"] == true {
                root.create_dir(path).map_err(io)?;
            } else {
                Project::new(self.project.root())
                    .writable_target(path, false, crate::WriteScope::ProjectRoot)
                    .map_err(io)?
                    .atomic_create("")
                    .map_err(io)?;
            }
            return Ok(json!({"path":path}));
        }
        if method == "editor.restore" {
            let dir = self.store.dir()?;
            let name = format!("trash-{}", identity(value, "backupId")?);
            let meta: Value = self.store.load(&format!("{name}.json"))?;
            if meta["path"] != path {
                return Err("path_denied".into());
            }
            rename_new(&dir, Path::new(&name), &root, Path::new(path))?;
            return Ok(json!({"path":path}));
        }
        let actual = inspect(&root, path)?;
        if value["version"] != actual["version"] {
            return Err("file_conflict".into());
        }
        if method == "editor.rename" {
            let target = text(value, "target")?;
            relative(target)?;
            self.check_private(target)?;
            rename_new(&root, Path::new(path), &root, Path::new(target))?;
            Ok(json!({"path":target}))
        } else {
            if value["confirmed"] != true {
                return Err("confirmation_required".into());
            }
            let name = format!("trash-{id}");
            let dir = self.store.dir()?;
            self.store.save(
                &format!("{name}.json"),
                &json!({"path":path,"snapshot":actual}),
            )?;
            rename_new(&root, Path::new(path), &dir, Path::new(&name))?;
            Ok(json!({"path":path,"backupId":id}))
        }
    }
    pub(super) fn inspect_document(&self, path: &str) -> Result<Value, String> {
        relative(path)?;
        self.check_private(path)?;
        let root =
            cap_std::fs::Dir::open_ambient_dir(self.project.root(), cap_std::ambient_authority())
                .map_err(io)?;
        inspect(&root, path)
    }
}
fn inspect(root: &cap_std::fs::Dir, path: &str) -> Result<Value, String> {
    let mut entries = Vec::new();
    walk(root, Path::new(path), &mut entries)?;
    Ok(
        json!({"path":path,"version":digest(serde_json::to_string(&entries).map_err(|_|"invalid_state")?.as_bytes()),"entries":entries}),
    )
}
fn walk(root: &cap_std::fs::Dir, path: &Path, entries: &mut Vec<Value>) -> Result<(), String> {
    if entries.len() >= 10000 {
        return Err("directory_large".into());
    }
    let meta = root.symlink_metadata(path).map_err(io)?;
    if meta.file_type().is_symlink() || !meta.is_file() && !meta.is_dir() {
        return Err("path_denied".into());
    }
    let modified = meta
        .modified()
        .map_err(io)?
        .into_std()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|_| "invalid_time")?
        .as_nanos()
        .to_string();
    let hash = if meta.is_file() {
        Some(file_hash(root, path)?)
    } else {
        None
    };
    entries.push(json!({"path":path.to_string_lossy(),"directory":meta.is_dir(),"bytes":meta.len(),"modified":modified,"sha256":hash}));
    if meta.is_dir() {
        let mut children = root
            .read_dir(path)
            .map_err(io)?
            .map(|e| e.map(|e| e.file_name()))
            .collect::<Result<Vec<_>, _>>()
            .map_err(io)?;
        children.sort();
        for child in children {
            walk(root, &path.join(child), entries)?;
        }
    }
    Ok(())
}
/// No destination replacement, including a destination created after the check.
fn rename_new(
    from: &cap_std::fs::Dir,
    source: &Path,
    to: &cap_std::fs::Dir,
    target: &Path,
) -> Result<(), String> {
    let source_parent = source
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let target_parent = target
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let source_dir = from.open_dir(source_parent).map_err(io)?;
    let target_dir = to.open_dir(target_parent).map_err(io)?;
    let source_name = source.file_name().ok_or("path_denied")?;
    let target_name = target.file_name().ok_or("path_denied")?;
    super::super::store::rename_no_replace(&source_dir, source_name, &target_dir, target_name)
}

fn file_hash(root: &cap_std::fs::Dir, path: &Path) -> Result<String, String> {
    use std::io::Read;
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
    let mut file = root.open_with(path, &options).map_err(io)?;
    let before = file.metadata().map_err(io)?;
    if !before.is_file() || before.file_type().is_symlink() {
        return Err("path_denied".into());
    }
    if before.len() > 64 * 1024 * 1024 {
        return Err("file_large".into());
    }
    let mut hasher = Sha256::new();
    let mut buffer = [0u8; 65536];
    let mut count = 0u64;
    loop {
        let n = file.read(&mut buffer).map_err(io)?;
        if n == 0 {
            break;
        }
        count += n as u64;
        if count > 64 * 1024 * 1024 {
            return Err("file_large".into());
        }
        hasher.update(&buffer[..n]);
    }
    let after = file.metadata().map_err(io)?;
    if before.len() != after.len()
        || before.modified().map_err(io)? != after.modified().map_err(io)?
    {
        return Err("file_conflict".into());
    }
    Ok(format!("{:x}", hasher.finalize()))
}
