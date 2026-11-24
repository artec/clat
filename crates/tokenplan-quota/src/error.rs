//! Typed errors. Every failure names the layer it failed at so callers can
//! distinguish "device login expired" from "gateway rejected the request".

use std::fmt;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QuotaError {
    /// The HTTP transport itself failed (DNS, connect, timeout, TLS).
    Transport(String),
    /// Non-success HTTP status with the response body snippet.
    Http(u16, String),
    /// The response was not the JSON shape the protocol promised.
    Shape(String),
    /// The outer envelope reported failure (`code != "200"`), e.g. `ConsoleNeedLogin`.
    Envelope { code: String, message: String },
    /// The inner business layer reported failure (`ret` not `SUCCESS::`).
    Business { code: String, message: String },
    /// The integrator cancelled the operation before it finished.
    Cancelled,
    /// The local console login wait expired before a credential arrived.
    CallbackExpired,
}

impl QuotaError {
    pub(crate) fn shape(message: impl Into<String>) -> Self {
        Self::Shape(message.into())
    }
    pub(crate) fn business(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self::Business {
            code: code.into(),
            message: message.into(),
        }
    }
    pub(crate) fn envelope(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self::Envelope {
            code: code.into(),
            message: message.into(),
        }
    }
}

impl fmt::Display for QuotaError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Transport(message) => write!(f, "transport error: {message}"),
            Self::Http(status, body) => write!(f, "HTTP {status}: {body}"),
            Self::Shape(message) => write!(f, "unexpected response shape: {message}"),
            Self::Envelope { code, message } => {
                write!(f, "gateway envelope error {code}: {message}")
            }
            Self::Business { code, message } => write!(f, "business error {code}: {message}"),
            Self::Cancelled => write!(f, "cancelled"),
            Self::CallbackExpired => write!(
                f,
                "transport error: console login timed out: no credentials received"
            ),
        }
    }
}

impl std::error::Error for QuotaError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_names_the_layer() {
        assert_eq!(
            QuotaError::envelope("ConsoleNeedLogin", "You need to log in.").to_string(),
            "gateway envelope error ConsoleNeedLogin: You need to log in."
        );
        assert_eq!(
            QuotaError::business("SUCCESS::", "").to_string(),
            "business error SUCCESS::: "
        );
        assert!(matches!(QuotaError::shape("bad"), QuotaError::Shape(message) if message == "bad"));
    }
}
