//! Validated installation resources. Cache slots never contain business state.
use super::{BuildMatchPolicy, HostClient};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Manifest {
    version: String,
    target: String,
    sha256: String,
    size: u64,
    official: bool,
}

/// The executable is installation-owned, never supplied by web content.
pub struct PackagedBackend {
    executable: PathBuf,
    policy: BuildMatchPolicy,
    _lease: File,
}

fn executable_name() -> &'static str {
    if cfg!(windows) {
        "clat-backend.exe"
    } else {
        "clat-backend"
    }
}

impl PackagedBackend {
    pub fn prepare(resources: &Path, cache: &Path) -> Result<Self, String> {
        require_directory(resources)?;
        let text = read_regular(&resources.join("backend.json"), 16384)?;
        let manifest: Manifest =
            serde_json::from_slice(&text).map_err(|_| "invalid backend manifest")?;
        validate_manifest(&manifest)?;
        let source = resources.join(executable_name());
        verify_binary(&source, &manifest)?;
        private_directory(cache)?;
        // Serialize prepare/GC so dropping a slot lock before unlinking it
        // cannot race a new preparer opening that same lock file.
        let cache_gate = open_lease(cache)?;
        cache_gate.lock().map_err(|e| e.to_string())?;
        let slot = cache.join(format!("{}-{}", manifest.version, manifest.sha256));
        private_directory(&slot)?;
        let lease = open_lease(&slot)?;
        lease.lock_shared().map_err(|e| e.to_string())?;
        let executable = slot.join(executable_name());
        if verify_binary(&executable, &manifest).is_err() {
            lease.unlock().map_err(|e| e.to_string())?;
            lease
                .try_lock()
                .map_err(|_| "runtime cache is busy; no running executable was replaced")?;
            stage_binary(&source, &executable, &manifest)?;
            lease.unlock().map_err(|e| e.to_string())?;
            lease.lock_shared().map_err(|e| e.to_string())?;
        }
        let fingerprint = super::build_identity::fingerprint_for(&executable)?;
        prune(cache, &slot);
        Ok(Self {
            executable,
            policy: BuildMatchPolicy::Backend {
                fingerprint,
                official: manifest.official,
            },
            _lease: lease,
        })
    }

    pub fn discover(&self) -> Result<Option<HostClient>, String> {
        let root = crate::control_storage::sentinel::default_storage_root()?;
        match HostClient::discover_startup_with(&root, self.policy.clone())? {
            super::discovery::StartupDiscovery::Compatible(client) => Ok(Some(client)),
            super::discovery::StartupDiscovery::Missing
            | super::discovery::StartupDiscovery::Upgradeable { .. } => Ok(None),
        }
    }

    /// An explicitly launched newer desktop may activate its packaged runtime
    /// once idle. Mere reconnection never creates a missing host.
    pub fn activate_idle_upgrade(&self, client: &HostClient) -> Result<Option<HostClient>, String> {
        if !matches!(
            self.policy,
            BuildMatchPolicy::Backend { official: true, .. }
        ) {
            return Ok(None);
        }
        let description = client.description()?;
        let older = description["product_version"]
            .as_str()
            .is_some_and(|version| {
                super::build_identity::compare_product_versions(
                    super::build_identity::product_version(),
                    version,
                )
                .ok()
                    == Some(std::cmp::Ordering::Greater)
            });
        if !older || client.runtime_status()?["ready"] != true {
            return Ok(None);
        }
        let info = client.call("workbench.info", &serde_json::json!({}))?;
        let root = info["project"]["root"]
            .as_str()
            .ok_or("host omitted project root")?;
        self.open_project(Path::new(root), false).map(Some)
    }

    pub fn open_project(&self, path: &Path, trust: bool) -> Result<HostClient, String> {
        if !path.is_dir() {
            return Err("project directory is unavailable".into());
        }
        let project = crate::Project::new(path.canonicalize().map_err(|e| e.to_string())?);
        HostClient::start_with_backend(
            &project,
            trust,
            false,
            &self.executable,
            self.policy.clone(),
        )
    }

    pub fn project_needs_trust(path: &Path) -> Result<bool, String> {
        if !path.is_dir() {
            return Err("project directory is unavailable".into());
        }
        HostClient::project_needs_trust(&crate::Project::new(
            path.canonicalize().map_err(|e| e.to_string())?,
        ))
    }
}

fn validate_manifest(m: &Manifest) -> Result<(), String> {
    if m.version != super::build_identity::product_version()
        || m.target != crate::upgrade::target_triple()
        || m.sha256.len() != 64
        || !m.sha256.bytes().all(|b| b.is_ascii_hexdigit())
        || m.size == 0
        || m.size > 512 * 1024 * 1024
        || m.official != super::build_identity::official_release()
    {
        return Err(
            "backend version, target or release identity does not match this desktop".into(),
        );
    }
    Ok(())
}

fn read_regular(path: &Path, cap: u64) -> Result<Vec<u8>, String> {
    let mut file = open_regular(path)?;
    if file.metadata().map_err(|e| e.to_string())?.len() > cap {
        return Err("backend resource too large".into());
    }
    let mut bytes = Vec::new();
    Read::by_ref(&mut file)
        .take(cap + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() as u64 > cap {
        return Err("backend resource too large".into());
    }
    Ok(bytes)
}

fn open_regular(path: &Path) -> Result<File, String> {
    let metadata = fs::symlink_metadata(path).map_err(|e| e.to_string())?;
    if is_link(&metadata) || !metadata.is_file() {
        return Err("backend resources must be regular files".into());
    }
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        options.custom_flags(0x00200000); // FILE_FLAG_OPEN_REPARSE_POINT
    }
    let file = options.open(path).map_err(|e| e.to_string())?;
    let metadata = file.metadata().map_err(|e| e.to_string())?;
    if is_link(&metadata) || !metadata.is_file() {
        return Err("backend resource is not a file".into());
    }
    Ok(file)
}

fn is_link(metadata: &fs::Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        metadata.file_attributes() & 0x400 != 0 // FILE_ATTRIBUTE_REPARSE_POINT
    }
    #[cfg(not(windows))]
    metadata.file_type().is_symlink()
}

fn require_directory(path: &Path) -> Result<(), String> {
    let metadata = fs::symlink_metadata(path).map_err(|e| e.to_string())?;
    if is_link(&metadata) || !metadata.is_dir() {
        return Err("runtime directory cannot be a link".into());
    }
    Ok(())
}

fn verify_binary(path: &Path, m: &Manifest) -> Result<(), String> {
    let mut file = open_regular(path)?;
    if file.metadata().map_err(|e| e.to_string())?.len() != m.size {
        return Err("backend size mismatch".into());
    }
    let mut digest = Sha256::new();
    let mut seen = 0u64;
    let mut chunk = [0; 65536];
    loop {
        let count = file.read(&mut chunk).map_err(|e| e.to_string())?;
        if count == 0 {
            break;
        }
        seen += count as u64;
        if seen > m.size {
            return Err("backend grew during verification".into());
        }
        digest.update(&chunk[..count]);
    }
    if format!("{:x}", digest.finalize()) != m.sha256 {
        return Err("backend checksum mismatch".into());
    }
    Ok(())
}

fn private_directory(path: &Path) -> Result<(), String> {
    if !path.exists() {
        if let Some(parent) = path.parent()
            && !parent.exists()
        {
            private_directory(parent)?;
        }
        match fs::create_dir(path) {
            Ok(()) => {
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    fs::set_permissions(path, fs::Permissions::from_mode(0o700))
                        .map_err(|e| e.to_string())?;
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(e.to_string()),
        }
    }
    require_directory(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700)).map_err(|e| e.to_string())?;
    }
    Ok(())
}

fn open_lease(slot: &Path) -> Result<File, String> {
    require_directory(slot)?;
    let path = slot.join("lease");
    if let Ok(metadata) = fs::symlink_metadata(&path)
        && (is_link(&metadata) || !metadata.is_file())
    {
        return Err("runtime lease must be a regular file".into());
    }
    let mut options = OpenOptions::new();
    options.read(true).write(true).create(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        options.custom_flags(0x00200000); // FILE_FLAG_OPEN_REPARSE_POINT
    }
    let lease = options.open(path).map_err(|e| e.to_string())?;
    let metadata = lease.metadata().map_err(|e| e.to_string())?;
    if is_link(&metadata) || !metadata.is_file() {
        return Err("runtime lease must be a regular file".into());
    }
    Ok(lease)
}

fn stage_binary(source: &Path, destination: &Path, m: &Manifest) -> Result<(), String> {
    let stage = destination.with_extension(uuid::Uuid::new_v4().to_string());
    let result = (|| {
        let mut input = open_regular(source)?.take(m.size + 1);
        let mut output = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&stage)
            .map_err(|e| e.to_string())?;
        std::io::copy(&mut input, &mut output).map_err(|e| e.to_string())?;
        output
            .flush()
            .and_then(|_| output.sync_all())
            .map_err(|e| e.to_string())?;
        verify_binary(&stage, m)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&stage, fs::Permissions::from_mode(0o500))
                .map_err(|e| e.to_string())?;
        }
        fs::rename(&stage, destination).map_err(|e| e.to_string())
    })();
    if result.is_err() {
        let _ = fs::remove_file(stage);
    }
    result
}

pub(crate) fn hold_current_runtime() -> Result<Option<File>, String> {
    let executable = std::env::current_exe().map_err(|e| e.to_string())?;
    if executable.file_name().and_then(|s| s.to_str()) != Some(executable_name()) {
        return Ok(None);
    }
    let slot = executable.parent().ok_or("runtime directory missing")?;
    let managed = slot
        .parent()
        .and_then(Path::file_name)
        .and_then(|n| n.to_str())
        == Some("runtimes")
        && slot
            .file_name()
            .and_then(|n| n.to_str())
            .and_then(|n| n.rsplit_once('-'))
            .is_some_and(|(v, h)| {
                v == super::build_identity::product_version()
                    && h.len() == 64
                    && h.bytes().all(|b| b.is_ascii_hexdigit())
            });
    if !managed {
        return Ok(None);
    }
    let lease = open_lease(slot)?;
    lease.lock_shared().map_err(|e| e.to_string())?;
    Ok(Some(lease))
}

fn managed_slot_name(name: &str) -> bool {
    name.rsplit_once('-').is_some_and(|(v, h)| {
        semver::Version::parse(v).is_ok()
            && h.len() == 64
            && h.bytes().all(|b| b.is_ascii_hexdigit())
    })
}

fn prune(cache: &Path, current: &Path) {
    let Ok(entries) = fs::read_dir(cache) else {
        return;
    };
    let mut slots = entries
        .flatten()
        .filter(|e| {
            e.path() != current
                && managed_slot_name(&e.file_name().to_string_lossy())
                && e.file_type().is_ok_and(|kind| kind.is_dir())
        })
        .collect::<Vec<_>>();
    slots.sort_by_key(|e| std::cmp::Reverse(e.metadata().and_then(|m| m.modified()).ok()));
    let previous = slots
        .iter()
        .find(|entry| {
            require_directory(&entry.path()).is_ok()
                && open_regular(&entry.path().join(executable_name())).is_ok()
        })
        .map(|entry| entry.path());
    for entry in slots {
        if previous.as_ref() == Some(&entry.path()) {
            continue;
        }
        let name = entry.file_name();
        let Some((version, hash)) = name.to_str().and_then(|n| n.rsplit_once('-')) else {
            continue;
        };
        if semver::Version::parse(version).is_err()
            || hash.len() != 64
            || !hash.bytes().all(|b| b.is_ascii_hexdigit())
        {
            continue;
        }
        if !entry.file_type().is_ok_and(|kind| kind.is_dir()) {
            continue;
        }
        let Ok(lease) = open_lease(&entry.path()) else {
            continue;
        };
        if lease.try_lock().is_err() {
            continue;
        }
        // A Windows handle may prevent deleting lease itself: remove payload,
        // then release the handle and remove only this empty, verified slot.
        let _ = fs::remove_file(entry.path().join(executable_name()));
        drop(lease);
        let _ = fs::remove_file(entry.path().join("lease"));
        let _ = fs::remove_dir(entry.path());
    }
}

#[cfg(test)]
mod tests;
