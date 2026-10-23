use super::{LocaleContext, UiMessage};
use std::ops::Deref;

/// English diagnostic remains available; a client owns the rendering locale.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct PresentedText {
    pub raw: String,
    pub presentation: Option<Box<UiMessage>>,
}
impl PresentedText {
    pub fn message(&self) -> UiMessage {
        self.presentation
            .as_deref()
            .cloned()
            .unwrap_or_else(|| UiMessage::new("common.diagnostic").arg("detail", self.raw.clone()))
    }
    pub fn render(&self, locale: LocaleContext) -> String {
        self.presentation
            .as_ref()
            .map(|m| locale.render(m))
            .unwrap_or_else(|| self.raw.clone())
    }
}
impl From<UiMessage> for PresentedText {
    fn from(message: UiMessage) -> Self {
        Self {
            raw: message.english(),
            presentation: Some(Box::new(message)),
        }
    }
}
impl From<String> for PresentedText {
    fn from(raw: String) -> Self {
        Self {
            raw,
            presentation: None,
        }
    }
}
impl From<&str> for PresentedText {
    fn from(raw: &str) -> Self {
        raw.to_owned().into()
    }
}
impl Deref for PresentedText {
    type Target = str;
    fn deref(&self) -> &str {
        &self.raw
    }
}
impl std::fmt::Display for PresentedText {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.raw)
    }
}

impl PartialEq<&str> for PresentedText {
    fn eq(&self, other: &&str) -> bool {
        self.raw == *other
    }
}
