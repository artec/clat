//! PKCE S256 pair for the device flow. The Qianwen auth service rejects
//! `POST /cli/device/code` without `code_challenge` + `code_challenge_method`
//! (live-probed 2026-11-22), so a verifier/challenge pair is mandatory here.

use base64::Engine;
use sha2::{Digest, Sha256};

/// Random URL-safe verifier (43 chars) + its S256 challenge (base64url, no padding).
/// The 32 verifier bytes come from two v4 UUIDs (16 OS-random bytes each), so
/// the crate carries no RNG dependency beyond `uuid`.
pub fn generate_pkce_pair() -> (String, String) {
    let mut bytes = [0u8; 32];
    let (first, second) = bytes.split_at_mut(16);
    first.copy_from_slice(uuid::Uuid::new_v4().as_bytes());
    second.copy_from_slice(uuid::Uuid::new_v4().as_bytes());
    let verifier = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes);
    let challenge = s256_challenge(&verifier);
    (verifier, challenge)
}

/// challenge = base64url(sha256(verifier)) without padding.
pub fn s256_challenge(verifier: &str) -> String {
    let digest = Sha256::digest(verifier.as_bytes());
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(digest)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn challenge_matches_rfc7636_example_vector() {
        // RFC 7636 Appendix B test vector.
        let verifier = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
        assert_eq!(
            s256_challenge(verifier),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
    }

    #[test]
    fn generated_verifier_is_url_safe_and_43_chars() {
        let (verifier, challenge) = generate_pkce_pair();
        assert_eq!(verifier.len(), 43);
        assert!(
            verifier
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'),
            "verifier must be base64url: {verifier}"
        );
        assert_eq!(s256_challenge(&verifier), challenge);
        let (verifier_again, _) = generate_pkce_pair();
        assert_ne!(verifier, verifier_again, "verifiers must be random");
    }
}
