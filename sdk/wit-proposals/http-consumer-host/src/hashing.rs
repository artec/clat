//! Add ARM SHA instructions without replacing any established software fallback.
#[cfg(target_arch = "aarch64")]
use fast_sha2::Digest as _;
use sha2::Digest as _;

pub struct Sha256(Context);
enum Context {
    Established(sha2::Sha256),
    #[cfg(target_arch = "aarch64")]
    Hardware(fast_sha2::Sha256),
}
impl Sha256 {
    pub fn new() -> Self {
        #[cfg(target_arch = "aarch64")]
        if std::arch::is_aarch64_feature_detected!("sha2") {
            return Self(Context::Hardware(fast_sha2::Sha256::new()));
        }
        Self(Context::Established(sha2::Sha256::new()))
    }
    pub fn update(&mut self, bytes: impl AsRef<[u8]>) {
        match &mut self.0 {
            Context::Established(context) => context.update(bytes.as_ref()),
            #[cfg(target_arch = "aarch64")]
            Context::Hardware(context) => context.update(bytes.as_ref()),
        }
    }
    pub fn finalize(self) -> [u8; 32] {
        let mut bytes = [0; 32];
        match self.0 {
            Context::Established(context) => bytes.copy_from_slice(&context.finalize()),
            #[cfg(target_arch = "aarch64")]
            Context::Hardware(context) => bytes.copy_from_slice(&context.finalize()),
        }
        bytes
    }
    pub fn digest(bytes: impl AsRef<[u8]>) -> [u8; 32] {
        let mut digest = Self::new();
        digest.update(bytes);
        digest.finalize()
    }
}
impl Default for Sha256 {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn established_fallback_has_the_same_digest_and_streaming_contract() {
        let mut fallback = Sha256(Context::Established(sha2::Sha256::new()));
        fallback.update(b"a");
        fallback.update(b"bc");
        let expected = sha2::Sha256::digest(b"abc");
        assert_eq!(&fallback.finalize()[..], &expected[..]);
        assert_eq!(Sha256::digest(b"abc"), Sha256::digest([97, 98, 99]));
        assert_eq!(&Sha256::digest(b"abc")[..], &expected[..]);
    }
}
