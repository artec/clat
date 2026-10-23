//! Shared release envelope: signatures stay in latest.json, never sidecars.
use super::*;

pub(super) const MAX_INDEX_BYTES: usize = 1024 * 1024;

pub(super) fn verify_index_checksum(
    bytes: &[u8],
    checksum: &[u8],
    actual: &str,
    asset: &str,
    tag: &str,
) -> Result<(), String> {
    verify_with_key(bytes, checksum, actual, asset, tag, &release_public_key()?)
}

fn verify_with_key(
    bytes: &[u8],
    checksum: &[u8],
    actual: &str,
    asset: &str,
    tag: &str,
    key: &minisign_verify::PublicKey,
) -> Result<(), String> {
    let envelope: Value = serde_json::from_slice(bytes).map_err(|_| "invalid release index")?;
    if envelope["schema_version"] != 1 {
        return Err("unsupported release index schema".into());
    }
    let payload = envelope["payload"]
        .as_str()
        .ok_or("release payload missing")?;
    let signature = envelope["signatures"]["cli"]
        .as_str()
        .ok_or("release CLI signature missing")?;
    let signature = minisign_verify::Signature::decode(signature)
        .map_err(|_| "invalid release index signature")?;
    key.verify(payload.as_bytes(), &signature, false)
        .map_err(|_| "release index signature verification failed")?;
    if signature.trusted_comment() != format!("CLAT {tag} release index") {
        return Err("release index signature tag mismatch (possible replay)".into());
    }
    let value: Value =
        serde_json::from_str(payload).map_err(|_| "invalid signed release payload")?;
    if value["version"].as_str() != tag.strip_prefix('v') {
        return Err("release index version mismatch".into());
    }
    let checksum_name = format!("{asset}.sha256");
    if value["artifacts"][asset] != actual
        || value["artifacts"][&checksum_name] != sha256_hex(checksum)
    {
        return Err("release index asset checksum mismatch".into());
    }
    let text = std::str::from_utf8(checksum).map_err(|_| "invalid checksum encoding")?;
    if parse_checksum_file(text, &checksum_name, asset)? != actual {
        return Err("checksum mismatch".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests;
