//! Signed author manifest v2. Verification never derives trust from the package.
use crate::capabilities::Policy;
use serde::Deserialize;
use serde_json::value::RawValue;
use sha2::{Digest, Sha256};
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Manifest {
    manifest_version: u32,
    id: String,
    version: String,
    runtime: Runtime,
    capabilities: Box<RawValue>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Runtime {
    kind: String,
    entry: String,
    sha256: String,
}
pub(crate) struct VerifiedManifest {
    manifest: Manifest,
    bytes: Vec<u8>,
}
impl VerifiedManifest {
    pub(crate) fn verify(bytes: &[u8], signature: &str, trusted_key: &str) -> Result<Self, String> {
        if bytes.len() > 256 * 1024 || signature.len() > 16 * 1024 {
            return Err("signed manifest exceeds byte limits".into());
        }
        let key = minisign_verify::PublicKey::from_base64(trusted_key.trim())
            .map_err(|_| "invalid trusted publisher key")?;
        let signature = minisign_verify::Signature::decode(signature)
            .map_err(|_| "invalid manifest signature")?;
        key.verify(bytes, &signature, false)
            .map_err(|_| "manifest signature mismatch")?;
        Self::parse_verified(bytes)
    }
    fn parse_verified(bytes: &[u8]) -> Result<Self, String> {
        let manifest: Manifest =
            serde_json::from_slice(bytes).map_err(|_| "invalid v2 manifest")?;
        if manifest.manifest_version != 2
            || manifest.runtime.kind != "wasm-component"
            || !identifier(&manifest.id)
            || !manifest.id.ends_with("-wasm")
            || manifest.version.is_empty()
            || manifest.version.len() > 64
            || manifest
                .version
                .chars()
                .any(|c| c.is_whitespace() || c.is_control())
        {
            return Err("invalid v2 flavor identity/runtime".into());
        }
        entry(&manifest.runtime.entry)?;
        if manifest.runtime.sha256.len() != 64
            || !manifest
                .runtime
                .sha256
                .bytes()
                .all(|b| b.is_ascii_hexdigit())
        {
            return Err("invalid entry digest".into());
        }
        let value = Self {
            manifest,
            bytes: bytes.to_vec(),
        };
        value.policy(None)?;
        Ok(value)
    }
    pub(crate) fn policy(&self, config: Option<&[u8]>) -> Result<Policy, String> {
        // Preserve signed capability tokens: Value would erase duplicate fields.
        let bytes = format!(
            r#"{{"manifestVersion":{},"capabilities":{}}}"#,
            self.manifest.manifest_version,
            self.manifest.capabilities.get()
        );
        Policy::parse(bytes.as_bytes(), config)
            .map_err(|_| "invalid or expanded network capabilities".into())
    }
    pub(crate) fn verify_component(&self, bytes: &[u8]) -> Result<(), String> {
        if bytes.len() > 256 * 1024 * 1024 {
            return Err("component exceeds byte limit".into());
        }
        let digest = format!("{:x}", Sha256::digest(bytes));
        if !digest.eq_ignore_ascii_case(&self.manifest.runtime.sha256) {
            return Err("component digest mismatch".into());
        }
        Ok(())
    }
    pub(crate) fn identity(&self) -> (&str, &str) {
        (&self.manifest.id, &self.manifest.version)
    }
    pub(crate) fn signed_bytes(&self) -> &[u8] {
        &self.bytes
    }
}
fn identifier(value: &str) -> bool {
    (1..=128).contains(&value.len())
        && value
            .bytes()
            .next()
            .is_some_and(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
        && value
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"._-".contains(&b))
}
fn entry(value: &str) -> Result<(), String> {
    if value.is_empty()
        || value.len() > 4096
        || value.contains(['\\', ':', '\0'])
        || value
            .split('/')
            .any(|part| part.is_empty() || matches!(part, "." | ".."))
    {
        return Err("unsafe v2 entry path".into());
    }
    Ok(())
}
#[cfg(test)]
mod tests;
