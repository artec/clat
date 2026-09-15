//! A private endpoint hint is not authority: authenticate and fence its instance.
use super::{BuildMatchPolicy, HostClient, credentials};
use serde::{Deserialize, Serialize};
use std::path::Path;

const FILE_NAME: &str = "host-endpoint.json";

pub(super) enum DiscoveryFailure {
    Unavailable(String),
    Incompatible(String),
}

impl From<String> for DiscoveryFailure {
    fn from(message: String) -> Self {
        Self::Unavailable(message)
    }
}

impl From<&str> for DiscoveryFailure {
    fn from(message: &str) -> Self {
        Self::Unavailable(message.into())
    }
}

impl DiscoveryFailure {
    pub(super) fn message(self) -> String {
        match self {
            Self::Unavailable(message) | Self::Incompatible(message) => message,
        }
    }
}

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

    pub(super) fn discover_local_for_management() -> Result<Self, String> {
        let root = crate::control_storage::sentinel::default_storage_root()?;
        Self::discover_for_management(&root)
    }

    /// Read-only; an absent, stale or incompatible hint never starts a writer.
    pub fn discover(root: &Path) -> Result<Self, String> {
        Self::discover_checked(root, BuildMatchPolicy::RequireIfAdvertised)
            .map_err(DiscoveryFailure::message)
    }

    pub(super) fn discover_for_startup(root: &Path) -> Result<Option<Self>, String> {
        match Self::discover_checked(root, BuildMatchPolicy::RequireIfAdvertised) {
            Ok(client) => Ok(Some(client)),
            Err(DiscoveryFailure::Unavailable(_)) => Ok(None),
            Err(DiscoveryFailure::Incompatible(message)) => Err(message),
        }
    }

    fn discover_for_management(root: &Path) -> Result<Self, String> {
        Self::discover_checked(root, BuildMatchPolicy::Ignore).map_err(DiscoveryFailure::message)
    }

    fn discover_checked(
        root: &Path,
        build_policy: BuildMatchPolicy,
    ) -> Result<Self, DiscoveryFailure> {
        let text = credentials::read_private_file(root, FILE_NAME, 1024)?;
        let endpoint: Endpoint = serde_json::from_str(&text)
            .map_err(|_| "invalid host endpoint; start a compatible host explicitly")?;
        uuid::Uuid::parse_str(&endpoint.instance_id).map_err(|_| "invalid host instance")?;
        Self::connect_checked(
            endpoint.port,
            credentials::read_token(root)?,
            root,
            Some(&endpoint.instance_id),
            build_policy,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn startup_distinguishes_live_incompatible_from_stale_identity_and_unavailable() {
        use std::io::{BufRead, Read, Write};
        let (root, _) = crate::test_support::roots("startup-discovery-kinds");
        std::fs::create_dir_all(&root).unwrap();
        let dir = cap_std::fs::Dir::open_ambient_dir(&root, cap_std::ambient_authority()).unwrap();
        crate::private_fs::write_text_atomic(&dir, &root, "web-token", "fixture-token").unwrap();
        let instance = uuid::Uuid::new_v4().to_string();
        for identity in [
            "matched",
            "stale-instance",
            "different-root",
            "different-build",
        ] {
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            listener.set_nonblocking(true).unwrap();
            publish(&root, listener.local_addr().unwrap().port(), &instance).unwrap();
            let description = serde_json::json!({"ok":true,"value":{
                "instance_id":if identity == "stale-instance" {uuid::Uuid::new_v4().to_string()} else {instance.clone()},
                "storage_root":if identity == "different-root" {root.join("missing")} else {root.clone()},
                "protocol_version":if identity == "different-build" {super::super::HOST_PROTOCOL_VERSION} else {0},
                "wire_version":if identity == "different-build" {crate::wire::WIRE_VERSION} else {0},
                "journal_version":if identity == "different-build" {crate::session::compat::SESSION_FORMAT_VERSION} else {0},
                "build_fingerprint":if identity == "different-build" {"build-v1-sha256:0000000000000000000000000000000000000000000000000000000000000000"} else {"legacy-fixture"}
            }}).to_string();
            let request_count = if identity == "different-build" { 3 } else { 1 };
            let server = std::thread::spawn(move || {
                for request in 0..request_count {
                    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
                    let stream = loop {
                        match listener.accept() {
                            Ok((stream, _)) => break stream,
                            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                                assert!(
                                    std::time::Instant::now() < deadline,
                                    "fixture accept deadline"
                                );
                                std::thread::sleep(std::time::Duration::from_millis(10));
                            }
                            Err(error) => panic!("{error}"),
                        }
                    };
                    stream.set_nonblocking(false).unwrap();
                    stream
                        .set_read_timeout(Some(std::time::Duration::from_secs(2)))
                        .unwrap();
                    stream
                        .set_write_timeout(Some(std::time::Duration::from_secs(2)))
                        .unwrap();
                    let mut reader = std::io::BufReader::new(stream);
                    for _ in 0..64 {
                        let mut line = String::new();
                        assert!(reader.read_line(&mut line).unwrap() > 0);
                        if line == "\r\n" {
                            break;
                        }
                    }
                    reader.read_exact(&mut [0; 2]).unwrap();
                    let response = if request == 2 {
                        r#"{"ok":true,"value":{"stopping":true}}"#
                    } else {
                        &description
                    };
                    write!(
                        reader.get_mut(),
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                        response.len(),
                        response
                    )
                    .unwrap();
                }
            });
            let result = HostClient::discover_for_startup(&root);
            let management = (identity == "different-build").then(|| {
                HostClient::discover_for_management(&root)
                    .and_then(|client| client.call("host.stop", &serde_json::json!({})))
            });
            server.join().unwrap();
            if identity == "matched" {
                assert!(
                    result
                        .err()
                        .is_some_and(|message| message.contains("incompatible host protocol"))
                );
            } else if identity == "different-build" {
                assert!(
                    result
                        .err()
                        .is_some_and(|message| message.contains("different CLAT build")),
                    "a new client must not silently attach to an old same-protocol build"
                );
                assert!(
                    management.unwrap().is_ok(),
                    "explicit status/stop management must still reach and stop the old build"
                );
            } else {
                assert!(
                    matches!(result, Ok(None)),
                    "unmatched identity is only a stale hint"
                );
            }
        }
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        publish(&root, listener.local_addr().unwrap().port(), &instance).unwrap();
        drop(listener);
        assert!(matches!(HostClient::discover_for_startup(&root), Ok(None)));
        drop(dir);
        crate::test_support::cleanup_tree(&root);
    }

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
