//! App credentials for one enterprise self-built Feishu app.
//!
//! The secret is never printed. The first slice accepts only the China open
//! platform origin.

use std::fmt;

pub(crate) const DOMAIN: &str = "https://open.feishu.cn";

#[derive(Clone, Eq, PartialEq)]
pub(crate) struct Credentials {
    app_id: String,
    app_secret: String,
    domain: String,
}

impl fmt::Debug for Credentials {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Credentials")
            .field("app_id", &self.app_id)
            .field("app_secret", &"<redacted>")
            .field("domain", &self.domain)
            .finish()
    }
}

impl Credentials {
    pub(crate) fn new(app_id: String, app_secret: String) -> Result<Self, String> {
        Self::from_parts(app_id, app_secret, DOMAIN.to_owned())
    }

    pub(crate) fn from_parts(
        app_id: String,
        app_secret: String,
        domain: String,
    ) -> Result<Self, String> {
        if !acceptable_credential(&app_id) || !acceptable_credential(&app_secret) {
            return Err("invalid Feishu app credential".into());
        }
        if domain != DOMAIN {
            return Err("unsupported Feishu domain".into());
        }
        Ok(Self {
            app_id,
            app_secret,
            domain,
        })
    }

    pub(crate) fn app_id(&self) -> &str {
        &self.app_id
    }

    pub(crate) fn app_secret(&self) -> &str {
        &self.app_secret
    }

    pub(crate) fn domain(&self) -> &str {
        &self.domain
    }
}

/// Same bound as a remembered model API key: a non-empty string up to 8192
/// bytes with no control characters.
pub(crate) fn acceptable_credential(value: &str) -> bool {
    !value.is_empty() && value.len() <= 8192 && !value.chars().any(char::is_control)
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct BindingStatus {
    pub(crate) bound: bool,
    pub(crate) bound_at: Option<String>,
    pub(crate) paired_users: usize,
    pub(crate) mapped_chats: usize,
}
