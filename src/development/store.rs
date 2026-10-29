//! Private non-journal developer state. Atomic replacement, no-follow reads.
pub(super) use super::rename::rename_no_replace;
use super::*;
use std::io::Read;
#[derive(Clone)]
pub(super) struct Store {
    root: PathBuf,
    lane: Arc<Mutex<()>>,
}
impl Store {
    pub(super) fn new(root: PathBuf) -> Self {
        Self {
            root,
            lane: Arc::new(Mutex::new(())),
        }
    }
    pub(super) fn path(&self, name: &str) -> PathBuf {
        self.root.join(name)
    }
    pub(super) fn root_ancestor(&self) -> PathBuf {
        self.root
            .parent()
            .and_then(Path::parent)
            .unwrap_or(&self.root)
            .to_owned()
    }
    pub(super) fn dir(&self) -> Result<cap_std::fs::Dir, String> {
        let private = self.root_ancestor();
        std::fs::create_dir_all(&private).map_err(io)?;
        if std::fs::symlink_metadata(&private)
            .map_err(io)?
            .file_type()
            .is_symlink()
        {
            return Err("path_denied".into());
        }
        let mut dir = cap_std::fs::Dir::open_ambient_dir(&private, cap_std::ambient_authority())
            .map_err(io)?;
        let relative = self
            .root
            .strip_prefix(&private)
            .map_err(|_| "path_denied")?;
        for component in relative.components() {
            dir = open_private_child(&dir, component.as_os_str())?;
        }
        #[cfg(unix)]
        {
            use cap_std::fs::PermissionsExt;
            dir.set_permissions(".", cap_std::fs::Permissions::from_mode(0o700))
                .map_err(io)?;
        }
        Ok(dir)
    }

    pub(super) fn load<T: serde::de::DeserializeOwned + Default>(
        &self,
        name: &str,
    ) -> Result<T, String> {
        let dir = self.dir()?;
        match dir.symlink_metadata(name) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(T::default()),
            Ok(m) if !m.is_file() || m.file_type().is_symlink() => return Err("path_denied".into()),
            Err(e) => return Err(io(e)),
            _ => {}
        }
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
        let mut file = dir.open_with(name, &options).map_err(io)?;
        if !file.metadata().map_err(io)?.is_file() {
            return Err("path_denied".into());
        }
        let mut bytes = Vec::new();
        file.by_ref()
            .take(64 * 1024 * 1024 + 1)
            .read_to_end(&mut bytes)
            .map_err(io)?;
        if bytes.len() > 64 * 1024 * 1024 {
            return Err("state_limit".into());
        }
        serde_json::from_slice(&bytes).map_err(|_| "state_corrupt".into())
    }
    pub(super) fn save<T: Serialize>(&self, name: &str, value: &T) -> Result<(), String> {
        let _guard = self.lane.lock().map_err(|_| "state_busy")?;
        let dir = self.dir()?;
        let encoded = serde_json::to_string(value).map_err(|_| "invalid_state")?;
        if encoded.len() > 64 * 1024 * 1024 {
            return Err("state_limit".into());
        }
        crate::private_fs::write_text_atomic_in_dir(&dir, name, &encoded)
    }
}

fn open_private_child(
    parent: &cap_std::fs::Dir,
    name: &std::ffi::OsStr,
) -> Result<cap_std::fs::Dir, String> {
    match parent.create_dir(name) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(io(error)),
    }
    let mut options = cap_std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use cap_std::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_DIRECTORY);
    }
    #[cfg(windows)]
    {
        use cap_std::fs::OpenOptionsExt;
        options.custom_flags(0x02000000 | 0x00200000);
    }
    let file = parent.open_with(name, &options).map_err(io)?;
    let meta = file.metadata().map_err(io)?;
    if !meta.is_dir() || meta.file_type().is_symlink() {
        return Err("path_denied".into());
    }
    Ok(cap_std::fs::Dir::from_std_file(file.into_std()))
}
#[derive(Clone, Serialize, Deserialize)]
pub struct OperationView {
    pub id: String,
    pub phase: String,
    pub result: Option<Value>,
    pub error: Option<String>,
    pub message: Option<String>,
    #[serde(default)]
    pub presentation: Option<crate::i18n::UiMessage>,
}
impl OperationView {
    pub(super) fn pending(id: &str) -> Self {
        Self {
            id: id.into(),
            phase: "pending".into(),
            result: None,
            error: None,
            message: None,
            presentation: None,
        }
    }
}
