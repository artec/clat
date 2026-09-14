//! A private endpoint hint is not authority: authenticate and fence its instance.
use super::{HostClient, credentials};
use serde::{Deserialize, Serialize};
use std::path::Path;

const FILE_NAME: &str = "host-endpoint.json";

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Endpoint {
    port: u16,
    instance_id: String,
}

pub(super) fn validate_existing(root: &Path) -> Result<(), String> {
    match std::fs::symlink_metadata(root.join(FILE_NAME)) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(format!("cannot inspect endpoint: {error}")),
        Ok(_) => {}
    }
    let text = credentials::read_private_file(root, FILE_NAME, 1024)?;
    let endpoint: Endpoint = serde_json::from_str(&text).map_err(|_| "invalid host endpoint")?;
    uuid::Uuid::parse_str(&endpoint.instance_id).map_err(|_| "invalid host instance")?;
    if endpoint.port == 0 {
        return Err("invalid host port".into());
    }
    Ok(())
}

/// Called only by the host while it owns the storage-root lease. Stale hints
/// intentionally survive shutdown; no late cleanup can delete a successor.
pub(crate) fn publish(root: &Path, port: u16, instance: &str) -> Result<(), String> {
    let endpoint = Endpoint {
        port,
        instance_id: instance.to_owned(),
    };
    let dir = cap_std::fs::Dir::open_ambient_dir(root, cap_std::ambient_authority())
        .map_err(|_| "host storage is unavailable")?;
    let text = serde_json::to_string(&endpoint).map_err(|_| "cannot encode host endpoint")?;
    crate::private_fs::write_text_atomic(&dir, root, FILE_NAME, &text)
}

impl HostClient {
    pub fn discover_local() -> Result<Self, String> {
        let root = crate::control_storage::sentinel::default_storage_root()?;
        Self::discover(&root)
    }

    /// Read-only; an absent, stale or incompatible hint never starts a writer.
    pub fn discover(root: &Path) -> Result<Self, String> {
        let text = credentials::read_private_file(root, FILE_NAME, 1024)?;
        let endpoint: Endpoint = serde_json::from_str(&text)
            .map_err(|_| "invalid host endpoint; start a compatible host explicitly")?;
        uuid::Uuid::parse_str(&endpoint.instance_id).map_err(|_| "invalid host instance")?;
        let client = Self::connect(endpoint.port, credentials::read_token(root)?, root)?;
        if client.instance != endpoint.instance_id {
            return Err("host endpoint instance changed; reconnect explicitly".into());
        }
        Ok(client)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_discovery_rejects_invalid_or_unbounded_hints_without_writes() {
        let (root, _) = crate::test_support::roots("discovery-invalid");
        std::fs::create_dir_all(&root).unwrap();
        assert!(HostClient::discover(&root).is_err());
        assert_eq!(std::fs::read_dir(&root).unwrap().count(), 0);
        let dir = cap_std::fs::Dir::open_ambient_dir(&root, cap_std::ambient_authority()).unwrap();
        for value in [
            "{}".to_owned(),
            "x".repeat(1025),
            r#"{"port":2691,"instance_id":"invalid"}"#.into(),
        ] {
            crate::private_fs::write_text_atomic(&dir, &root, FILE_NAME, &value).unwrap();
            assert!(HostClient::discover(&root).is_err());
            assert_eq!(
                std::fs::read_to_string(root.join(FILE_NAME)).unwrap(),
                value
            );
            assert_eq!(std::fs::read_dir(&root).unwrap().count(), 1);
        }
        drop(dir);
        crate::test_support::cleanup_tree(&root);
    }

    #[cfg(unix)]
    #[test]
    fn host_discovery_rejects_links_shared_files_and_fifo() {
        use std::os::unix::fs::{PermissionsExt, symlink};
        let (root, _) = crate::test_support::roots("discovery-file-fence");
        std::fs::create_dir_all(&root).unwrap();
        let id = uuid::Uuid::new_v4().to_string();
        publish(&root, 2691, &id).unwrap();
        std::fs::rename(root.join(FILE_NAME), root.join("saved")).unwrap();
        symlink(root.join("saved"), root.join(FILE_NAME)).unwrap();
        assert!(credentials::read_private_file(&root, FILE_NAME, 1024).is_err());
        assert!(publish(&root, 2691, &id).is_err());
        std::fs::remove_file(root.join(FILE_NAME)).unwrap();
        std::fs::rename(root.join("saved"), root.join(FILE_NAME)).unwrap();
        std::fs::set_permissions(root.join(FILE_NAME), std::fs::Permissions::from_mode(0o644))
            .unwrap();
        assert!(credentials::read_private_file(&root, FILE_NAME, 1024).is_err());
        std::fs::remove_file(root.join(FILE_NAME)).unwrap();
        let name =
            std::ffi::CString::new(root.join(FILE_NAME).as_os_str().as_encoded_bytes()).unwrap();
        // A hostile FIFO must fail without blocking on open.
        assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
        assert!(credentials::read_private_file(&root, FILE_NAME, 1024).is_err());
        crate::test_support::cleanup_tree(&root);
    }
}
