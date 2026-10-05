//! Authenticated, content-addressed compiler output; never guest state.
//!
//! A plain digest is insufficient: an attacker can replace native code and
//! recompute it. Only output authenticated by this host's private key may
//! cross Wasmtime's unsafe deserialization boundary. Cache failures are misses.
mod files;
#[cfg(windows)]
mod windows;
use crate::hashing::Sha256;
use cap_std::fs::Dir;
use ring::hmac;
use std::hash::{Hash, Hasher};
use std::path::Path;
use wasmtime::{Engine, component::Component};

/// Cache lives beside the storage root so ordinary project/state grants do
/// not expose its authentication secret. The host must fence guest preopens.
pub fn cache_path(storage: &Path) -> std::io::Result<std::path::PathBuf> {
    let parent = storage
        .parent()
        .ok_or_else(|| std::io::Error::other("storage has no parent"))?
        .canonicalize()?;
    let name = storage
        .file_name()
        .ok_or_else(|| std::io::Error::other("storage has no name"))?;
    let root = parent.join(name);
    let id: String = Sha256::digest(root.as_os_str().as_encoded_bytes())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    Ok(parent.join(format!(".clat-wasm-cache-{id}")))
}

const DOMAIN: &[u8] = b"clat-component-cache-v1\0";
const MAX_CACHE_BYTES: u64 = 256 * 1024 * 1024;

/// Compile verified source bytes, optionally reusing locally authenticated code.
/// The caller must still verify the source package on every activation.
pub fn component(engine: &Engine, source: &[u8], storage: &Path) -> wasmtime::Result<Component> {
    let cache = files::open(storage).ok().and_then(|dir| {
        let secret = files::key(&dir).ok()?;
        Some((dir, hmac::Key::new(hmac::HMAC_SHA256, &secret)))
    });
    let name = address(engine, source);
    if let Some((dir, key)) = &cache
        && let Some(component) = read(engine, dir, key, &name)
    {
        return Ok(component);
    }
    #[cfg(test)]
    tests::record_compile();
    let component = Component::from_binary(engine, source)?;
    if let Some((dir, key)) = cache
        && let Ok(code) = component.serialize()
        && code.len() as u64 + 32 <= MAX_CACHE_BYTES
    {
        let tag = authenticate(&key, &name, &code);
        let mut envelope = Vec::with_capacity(32 + code.len());
        envelope.extend_from_slice(tag.as_ref());
        envelope.extend_from_slice(&code);
        let _ = files::publish(&dir, &name, &envelope, false);
    }
    Ok(component)
}

fn address(engine: &Engine, source: &[u8]) -> String {
    // Wasmtime includes its version, target ISA and compiler configuration.
    let mut compatibility = std::collections::hash_map::DefaultHasher::new();
    engine
        .precompile_compatibility_hash()
        .hash(&mut compatibility);
    let mut digest = Sha256::new();
    digest.update(DOMAIN);
    digest.update(compatibility.finish().to_le_bytes());
    digest.update(source);
    let hex: String = digest
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    format!("{hex}.cwasm")
}

fn authenticate(key: &hmac::Key, name: &str, code: &[u8]) -> hmac::Tag {
    let mut context = hmac::Context::with_key(key);
    context.update(DOMAIN);
    context.update(name.as_bytes());
    context.update(code);
    context.sign()
}

fn read(engine: &Engine, dir: &Dir, key: &hmac::Key, name: &str) -> Option<Component> {
    let envelope = files::read(dir, name, MAX_CACHE_BYTES).ok()?;
    if envelope.len() <= 32 {
        return None;
    }
    let (tag, code) = envelope.split_at(32);
    // Use ring's constant-time verification over the complete buffered bytes.
    let mut material = Vec::with_capacity(DOMAIN.len() + name.len() + code.len());
    material.extend_from_slice(DOMAIN);
    material.extend_from_slice(name.as_bytes());
    material.extend_from_slice(code);
    hmac::verify(key, &material, tag).ok()?;
    // SAFETY: bytes are authenticated compiler output from this host. They
    // are buffered, so replacement after verification cannot change input.
    // Source content and engine compatibility are bound into the MAC name.
    unsafe { Component::deserialize(engine, code).ok() }
}

#[cfg(test)]
mod tests;
