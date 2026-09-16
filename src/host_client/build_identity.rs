//! Process build identity used only to prevent stale native-host attachment.
//! The host snapshots it before accepting clients; later executable replacement
//! therefore cannot make an old process impersonate the newly built client.

use sha2::{Digest as _, Sha256};
use std::sync::OnceLock;

static CURRENT: OnceLock<Result<String, String>> = OnceLock::new();

pub(crate) fn product_version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

pub(crate) fn compare_product_versions(
    candidate: &str,
    host: &str,
) -> Result<std::cmp::Ordering, semver::Error> {
    Ok(semver::Version::parse(candidate)?.cmp(&semver::Version::parse(host)?))
}

pub(crate) fn current() -> Result<&'static str, String> {
    match CURRENT.get_or_init(compute) {
        Ok(identity) => Ok(identity.as_str()),
        Err(error) => Err(error.clone()),
    }
}

pub(super) fn verify_advertised(
    description: &serde_json::Value,
) -> Result<(), super::discovery::DiscoveryFailure> {
    let Some(advertised) = description.get("build_fingerprint") else {
        // Additive rollout: hosts predating build fingerprints remain attachable.
        return Ok(());
    };
    let advertised = advertised.as_str().ok_or_else(|| {
        super::discovery::DiscoveryFailure::Incompatible(
            "host build fingerprint is invalid; stop that host explicitly".into(),
        )
    })?;
    let current = current().map_err(super::discovery::DiscoveryFailure::Incompatible)?;
    if advertised == current {
        return Ok(());
    }

    let host_version = description["product_version"].as_str().ok_or_else(|| {
        super::discovery::DiscoveryFailure::Incompatible(
            "a different CLAT build is hosting this storage root but did not advertise a product version; run `clat host stop` explicitly, then retry".into(),
        )
    })?;
    let order = compare_product_versions(product_version(), host_version).map_err(|_| {
        super::discovery::DiscoveryFailure::Incompatible(
            "host product version is invalid; automatic host takeover is disabled".into(),
        )
    })?;
    match order {
        std::cmp::Ordering::Greater => Err(super::discovery::DiscoveryFailure::Upgradeable {
            host_version: host_version.to_owned(),
        }),
        std::cmp::Ordering::Equal => Err(super::discovery::DiscoveryFailure::Incompatible(
            "a different build of the same CLAT version is already hosting this storage root; run `clat host stop` explicitly, then retry".into(),
        )),
        std::cmp::Ordering::Less => Err(super::discovery::DiscoveryFailure::Incompatible(format!(
            "a newer CLAT {host_version} host is already running; upgrade this client instead of replacing that host"
        ))),
    }
}

fn compute() -> Result<String, String> {
    let executable = std::env::current_exe()
        .map_err(|error| format!("cannot locate this CLAT build: {error}"))?;
    let metadata = std::fs::metadata(&executable)
        .map_err(|error| format!("cannot inspect this CLAT build: {error}"))?;
    let modified = metadata
        .modified()
        .and_then(|time| {
            time.duration_since(std::time::UNIX_EPOCH)
                .map_err(std::io::Error::other)
        })
        .map_err(|error| format!("cannot inspect this CLAT build: {error}"))?;
    // Integers only: no path spelling, inode/dev identity, case folding or
    // Windows 8.3 aliases enter the cross-platform wire value. A rebuild or
    // replacement changes this coordinate even when the product version does
    // not; hashing keeps local filesystem timestamps out of the response.
    let material = format!(
        "v1\0{}\0{}\0{}\0{}\0{}",
        env!("CARGO_PKG_VERSION"),
        std::env::consts::OS,
        std::env::consts::ARCH,
        metadata.len(),
        modified.as_nanos()
    );
    Ok(format!(
        "build-v1-sha256:{:x}",
        Sha256::digest(material.as_bytes())
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn process_identity_is_cached_and_path_independent() {
        let first = current().unwrap();
        let second = current().unwrap();
        assert!(
            std::ptr::eq(first, second),
            "one process caches one identity"
        );
        assert!(
            first.strip_prefix("build-v1-sha256:").is_some_and(
                |digest| digest.len() == 64 && digest.bytes().all(|b| b.is_ascii_hexdigit())
            ),
            "identity is a hashed build coordinate, not a platform path: {first}"
        );
    }

    #[test]
    fn advertised_identity_is_additive_and_same_build_is_accepted() {
        assert!(verify_advertised(&serde_json::json!({})).is_ok());
        assert!(
            verify_advertised(&serde_json::json!({"build_fingerprint":current().unwrap()})).is_ok(),
            "the same build attaches without stopping or replacing its host"
        );
    }

    #[test]
    fn different_builds_are_upgradeable_only_when_the_client_version_is_newer() {
        let different =
            "build-v1-sha256:0000000000000000000000000000000000000000000000000000000000000000";
        let current_version = semver::Version::parse(product_version()).unwrap();
        assert!(current_version > semver::Version::new(0, 0, 0));

        assert!(matches!(
            verify_advertised(&serde_json::json!({
                "build_fingerprint":different,
                "product_version":"0.0.0"
            })),
            Err(super::super::discovery::DiscoveryFailure::Upgradeable { host_version })
                if host_version == "0.0.0"
        ));
        assert!(matches!(
            verify_advertised(&serde_json::json!({
                "build_fingerprint":different,
                "product_version":product_version()
            })),
            Err(super::super::discovery::DiscoveryFailure::Incompatible(message))
                if message.contains("same CLAT version")
        ));
        assert!(matches!(
            verify_advertised(&serde_json::json!({
                "build_fingerprint":different,
                "product_version":"999.0.0"
            })),
            Err(super::super::discovery::DiscoveryFailure::Incompatible(message))
                if message.contains("newer CLAT 999.0.0 host")
        ));
    }
}
