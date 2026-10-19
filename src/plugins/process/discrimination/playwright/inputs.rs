use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::io::Read;
use std::path::{Component, Path};

pub(super) type Hashes = BTreeMap<String, String>;
pub(super) fn relative(path: &str) -> Result<(), String> {
    if path.is_empty()
        || path.len() > 160
        || !Path::new(path)
            .components()
            .all(|c| matches!(c, Component::Normal(_)))
    {
        return Err(
            "experiment paths must be bounded relative files without dot/parent components".into(),
        );
    }
    Ok(())
}
pub(super) fn hash(root: &Path, files: &[String], artifacts: &[String]) -> Result<Hashes, String> {
    let directory = cap_std::fs::Dir::open_ambient_dir(root, cap_std::ambient_authority())
        .map_err(|e| e.to_string())?;
    let mut hashes = Hashes::new();
    let mut remaining = 512 * 1024 * 1024u64;
    for path in files {
        relative(path)?;
        let mut file = open_regular(&directory, path)?;
        let limit = if artifacts.contains(path) {
            256 * 1024 * 1024
        } else {
            1024 * 1024
        };
        let mut digest = Sha256::new();
        let mut size = 0u64;
        let mut chunk = [0u8; 8192];
        loop {
            let n = file.read(&mut chunk).map_err(|e| e.to_string())?;
            if n == 0 {
                break;
            }
            size += n as u64;
            if size > limit || n as u64 > remaining {
                return Err("experiment input exceeds file/phase byte bound".into());
            }
            remaining -= n as u64;
            digest.update(&chunk[..n]);
        }
        hashes.insert(path.clone(), format!("{:x}", digest.finalize()));
    }
    Ok(hashes)
}
pub(super) fn identity(a: &Hashes, b: &Hashes, artifacts: &[String]) -> Value {
    let fingerprint = |h: &Hashes| format!("{:x}", Sha256::digest(serde_json::to_vec(h).unwrap()));
    let bindings = |h: &Hashes| {
        artifacts
            .iter()
            .map(|p| (p.clone(), json!(h[p])))
            .collect::<serde_json::Map<_, _>>()
    };
    json!({"candidate_sha256":fingerprint(a),"counterexample_sha256":fingerprint(b),"declared_files":a.len(),
        "candidate_artifacts":bindings(a),"counterexample_artifacts":bindings(b)})
}

fn open_regular(directory: &cap_std::fs::Dir, path: &str) -> Result<cap_std::fs::File, String> {
    let mut options = cap_std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use cap_std::fs::OpenOptionsExt as _;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    #[cfg(windows)]
    {
        use cap_std::fs::OpenOptionsExt as _;
        options.custom_flags(0x0020_0000);
    }
    let file = directory
        .open_with(path, &options)
        .map_err(|e| e.to_string())?;
    let metadata = file.metadata().map_err(|e| e.to_string())?;
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Err("experiment inputs must be regular non-symlink files".into());
    }
    Ok(file)
}
