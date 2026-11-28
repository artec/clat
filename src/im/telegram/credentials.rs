//! Bot token for one Telegram bot.
//!
//! The token is never printed. This slice accepts only `https://api.telegram.org`.

use std::fmt;

pub(crate) const DOMAIN: &str = "https://api.telegram.org";

#[derive(Clone, Eq, PartialEq)]
pub(crate) struct Credentials {
    token: String,
    domain: String,
}

impl fmt::Debug for Credentials {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Credentials")
            .field("token", &"<redacted>")
            .field("domain", &self.domain)
            .finish()
    }
}

impl Credentials {
    pub(crate) fn new(token: String) -> Result<Self, String> {
        Self::from_parts(token, DOMAIN.to_owned())
    }

    pub(crate) fn from_parts(token: String, domain: String) -> Result<Self, String> {
        if !acceptable_token(&token) {
            return Err("invalid Telegram bot token".into());
        }
        if domain != DOMAIN {
            return Err("unsupported Telegram domain".into());
        }
        Ok(Self { token, domain })
    }

    pub(crate) fn token(&self) -> &str {
        &self.token
    }

    pub(crate) fn domain(&self) -> &str {
        &self.domain
    }
}

/// Same bound as a Feishu app secret: a non-empty string up to 8192 bytes
/// with no control characters.
pub(crate) fn acceptable_token(value: &str) -> bool {
    !value.is_empty() && value.len() <= 8192 && !value.chars().any(char::is_control)
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct BindingStatus {
    pub(crate) bound: bool,
    pub(crate) bound_at: Option<String>,
    pub(crate) paired_users: usize,
    pub(crate) mapped_chats: usize,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_bound_matches_a_feishu_secret_and_the_domain_stays_pinned() {
        assert!(acceptable_token("123456:secret"));
        assert!(!acceptable_token(""));
        assert!(!acceptable_token(&"a".repeat(8193)));
        assert!(!acceptable_token("bad\ntoken"));
        assert!(Credentials::new("123456:secret".into()).is_ok());
        assert!(
            Credentials::from_parts("123456:secret".into(), "https://example.test".into()).is_err()
        );
        let credentials = Credentials::new("123456:secret".into()).unwrap();
        assert!(!format!("{credentials:?}").contains("secret"));
    }
}
