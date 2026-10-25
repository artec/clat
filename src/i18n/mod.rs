//! Client presentation only. Runtime facts, model input and logs have no locale.
mod preferences;
mod text;
mod views;
pub use text::PresentedText;
mod template;
mod terminal;
pub use preferences::{
    load_preference, load_ui_preferences, save_preference, save_ui_preferences,
    terminal_preferences_path,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use std::sync::OnceLock;
pub use terminal::*;

pub const MANIFEST: &str = include_str!("../../locales/manifest.json");
pub const ENGLISH: &str = include_str!("../../locales/en.json");
const SIMPLIFIED: &str = include_str!("../../locales/zh-Hans.json");
const TRADITIONAL: &str = include_str!("../../locales/zh-Hant.json");

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub enum Locale {
    #[default]
    #[serde(rename = "en")]
    English,
    #[serde(rename = "zh-Hans")]
    Simplified,
    #[serde(rename = "zh-Hant")]
    Traditional,
}
impl Locale {
    pub fn id(self) -> &'static str {
        match self {
            Self::English => "en",
            Self::Simplified => "zh-Hans",
            Self::Traditional => "zh-Hant",
        }
    }
    pub fn parse(value: &str) -> Option<Self> {
        let value = value
            .split(['.', '@'])
            .next()?
            .replace('_', "-")
            .to_ascii_lowercase();
        let parts: Vec<_> = value.split('-').collect();
        if parts.first() == Some(&"zh") {
            if parts.contains(&"hant") {
                return manifest_alias("zh-hant");
            }
            if parts.contains(&"hans") {
                return manifest_alias("zh-hans");
            }
        }
        manifest_alias(&value)
            .or_else(|| {
                parts
                    .iter()
                    .skip(1)
                    .find_map(|p| manifest_alias(&format!("{}-{p}", parts[0])))
            })
            .or_else(|| manifest_alias(parts[0]))
    }
    pub fn catalog_json(self) -> &'static str {
        match self {
            Self::English => ENGLISH,
            Self::Simplified => SIMPLIFIED,
            Self::Traditional => TRADITIONAL,
        }
    }
    fn catalog(self) -> &'static BTreeMap<String, Value> {
        static CATALOGS: OnceLock<[BTreeMap<String, Value>; 3]> = OnceLock::new();
        &CATALOGS.get_or_init(|| {
            [ENGLISH, SIMPLIFIED, TRADITIONAL]
                .map(|s| serde_json::from_str(s).expect("validated language catalog"))
        })[self as usize]
    }
}
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum LanguagePreference {
    #[default]
    System,
    Fixed(Locale),
}
impl LanguagePreference {
    pub fn parse(value: &str) -> Option<Self> {
        if value == "system" {
            Some(Self::System)
        } else {
            Locale::parse(value).map(Self::Fixed)
        }
    }
    pub fn id(self) -> &'static str {
        match self {
            Self::System => "system",
            Self::Fixed(locale) => locale.id(),
        }
    }
}
#[derive(Clone, Copy, Debug, Default)]
pub struct LocaleContext {
    pub locale: Locale,
}
impl LocaleContext {
    pub fn new(locale: Locale) -> Self {
        Self { locale }
    }
    pub fn resolve(
        preference: LanguagePreference,
        systems: impl IntoIterator<Item = String>,
    ) -> Self {
        Self::new(match preference {
            LanguagePreference::Fixed(locale) => locale,
            LanguagePreference::System => systems
                .into_iter()
                .find_map(|s| Locale::parse(&s))
                .unwrap_or_default(),
        })
    }
    pub fn system(preference: LanguagePreference) -> Self {
        Self::resolve(preference, sys_locale::get_locales())
    }
    pub fn text(self, key: &str) -> &'static str {
        self.locale
            .catalog()
            .get(key)
            .and_then(Value::as_str)
            .or_else(|| Locale::English.catalog().get(key).and_then(Value::as_str))
            .unwrap_or("[missing translation]")
    }
    pub fn render(self, message: &UiMessage) -> String {
        self.render_at(message, 0)
    }
    fn render_at(self, message: &UiMessage, depth: usize) -> String {
        let entry = self
            .locale
            .catalog()
            .get(&message.key)
            .or_else(|| Locale::English.catalog().get(&message.key));
        let count = message.args.get("count").and_then(Value::as_f64);
        let text = entry
            .and_then(|value| {
                value.as_str().or_else(|| {
                    let form = if self.locale == Locale::English && count == Some(1.0) {
                        "one"
                    } else {
                        "other"
                    };
                    value.get(form).and_then(Value::as_str)
                })
            })
            .unwrap_or(&message.fallback);
        let mut args = std::borrow::Cow::Borrowed(&message.args);
        if depth < 8 && !message.arg_presentations.is_empty() {
            for (name, presentation) in &message.arg_presentations {
                args.to_mut().insert(
                    name.clone(),
                    Value::String(self.render_at(presentation, depth + 1)),
                );
            }
        }
        template::format(text, &args)
    }
}
/// Retain this descriptor in view state; do not cache a translated sentence.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct UiMessage {
    pub key: String,
    #[serde(default)]
    pub args: BTreeMap<String, Value>,
    /// Explicitly described arguments still enter the template as opaque text.
    /// Raw JSON arguments are never interpreted as presentation descriptors.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub arg_presentations: BTreeMap<String, UiMessage>,
    pub fallback: String,
}
impl UiMessage {
    pub fn new(key: &str) -> Self {
        Self {
            key: key.into(),
            args: BTreeMap::new(),
            arg_presentations: BTreeMap::new(),
            fallback: Locale::English
                .catalog()
                .get(key)
                .and_then(|v| {
                    v.as_str()
                        .or_else(|| v.get("other").and_then(Value::as_str))
                })
                .unwrap_or("[missing translation]")
                .into(),
        }
    }
    pub fn arg(mut self, name: &str, value: impl Into<Value>) -> Self {
        self.arg_presentations.remove(name);
        self.args.insert(name.into(), value.into());
        self
    }
    pub fn arg_message(mut self, name: &str, message: UiMessage) -> Self {
        self.args
            .insert(name.into(), Value::String(message.english()));
        self.arg_presentations.insert(name.into(), message);
        self
    }
    pub fn english(&self) -> String {
        LocaleContext::default().render(self)
    }
}

/// Built-in command identifiers are protocol facts; extension descriptions remain original.
pub fn command_presentation(name: &str, fallback: &str) -> Option<UiMessage> {
    let key = format!("command.{name}.description");
    Locale::English
        .catalog()
        .contains_key(&key)
        .then(|| UiMessage {
            key,
            args: BTreeMap::new(),
            arg_presentations: BTreeMap::new(),
            fallback: fallback.into(),
        })
}

pub fn preset_presentation(id: &str, fallback: &str) -> UiMessage {
    let mut message = UiMessage::new(&format!("tui.preset_description_{id}"));
    message.fallback = fallback.into();
    message
}

/// Both the Host and desktop embed these exact resources, including offline English.
pub fn browser_runtime() -> String {
    format!(
        "globalThis.__CLAT_BUILTIN_LOCALE__={{manifest:{MANIFEST},english:{ENGLISH}}};\n{}",
        include_str!("../../web/i18n.js")
    )
}

impl From<String> for UiMessage {
    fn from(detail: String) -> Self {
        Self::new("common.error").arg("detail", detail)
    }
}
impl From<&str> for UiMessage {
    fn from(detail: &str) -> Self {
        detail.to_owned().into()
    }
}
fn manifest_alias(value: &str) -> Option<Locale> {
    static ALIASES: OnceLock<BTreeMap<String, Locale>> = OnceLock::new();
    ALIASES
        .get_or_init(|| {
            let manifest: Value =
                serde_json::from_str(MANIFEST).expect("validated locale manifest");
            let mut aliases = BTreeMap::new();
            for language in manifest["languages"]
                .as_array()
                .expect("manifest languages")
            {
                let locale: Locale =
                    serde_json::from_value(language["id"].clone()).expect("supported language");
                aliases.insert(locale.id().to_ascii_lowercase(), locale);
                for alias in language["aliases"].as_array().expect("language aliases") {
                    aliases.insert(alias.as_str().expect("alias").to_ascii_lowercase(), locale);
                }
            }
            aliases
        })
        .get(value)
        .copied()
}

#[cfg(test)]
mod tests;
