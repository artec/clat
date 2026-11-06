#[derive(Clone, Eq, PartialEq)]
pub(crate) struct PairingChallenge {
    pub(crate) code: String,
    pub(crate) expires_at_ms: i64,
}

impl std::fmt::Debug for PairingChallenge {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PairingChallenge")
            .field("code", &"<redacted>")
            .field("expires_at_ms", &self.expires_at_ms)
            .finish()
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum PairingAttempt {
    Paired,
    AlreadyPaired,
    Invalid { remaining_attempts: u8 },
    Expired,
    Unavailable,
    RateLimited { retry_after_ms: u64 },
}
