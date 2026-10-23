use super::*;

pub struct TerminalLanguage {
    pub context: LocaleContext,
    pub preference: LanguagePreference,
    pub warnings: Vec<UiMessage>,
}
pub fn terminal_language(explicit: Option<&str>) -> Result<TerminalLanguage, UiMessage> {
    let environment = std::env::var("CLAT_LANG").ok();
    resolve_terminal_sources(explicit, environment.as_deref(), || {
        terminal_preferences_path().and_then(|p| load_preference(&p))
    })
}
pub(super) fn resolve_terminal_sources(
    explicit: Option<&str>,
    environment: Option<&str>,
    read: impl FnOnce() -> Result<LanguagePreference, String>,
) -> Result<TerminalLanguage, UiMessage> {
    let mut warnings = Vec::new();
    let preference = if let Some(value) = explicit {
        LanguagePreference::parse(value)
            .ok_or_else(|| UiMessage::new("common.invalid_language").arg("value", value))?
    } else if let Some(value) = environment.and_then(LanguagePreference::parse) {
        value
    } else {
        if let Some(value) = environment {
            warnings.push(UiMessage::new("common.invalid_language").arg("value", value));
        }
        read().unwrap_or_else(|detail| {
            warnings.push(UiMessage::new("common.preference_error").arg("detail", detail));
            LanguagePreference::System
        })
    };
    Ok(TerminalLanguage {
        context: LocaleContext::system(preference),
        preference,
        warnings,
    })
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LanguageAction {
    Choose,
    Set(LanguagePreference),
}
/// Pure client command parsing, shared by local/attached/DSH terminals.
/// This command never reaches a model or a remote Host.
pub fn language_command(input: &str) -> Option<Result<LanguageAction, UiMessage>> {
    let mut words = input.split_whitespace();
    if words.next()? != "/language" {
        return None;
    }
    let Some(value) = words.next() else {
        return Some(Ok(LanguageAction::Choose));
    };
    let preference = LanguagePreference::parse(value).filter(|_| words.next().is_none());
    Some(
        preference
            .map(LanguageAction::Set)
            .ok_or_else(|| UiMessage::new("common.invalid_language").arg("value", value)),
    )
}
/// Only a leading global option is interpreted; prompt/command arguments are opaque.
pub fn leading_language(args: Vec<String>) -> Result<(Option<String>, Vec<String>), UiMessage> {
    let mut args = args.into_iter().peekable();
    let mut selected = None;
    while args
        .peek()
        .is_some_and(|s| s == "--lang" || s.starts_with("--lang="))
    {
        let word = args.next().expect("peeked argument");
        let value = if let Some(value) = word.strip_prefix("--lang=") {
            value.to_owned()
        } else {
            args.next().unwrap_or_default()
        };
        if LanguagePreference::parse(&value).is_none() {
            return Err(UiMessage::new("common.invalid_language").arg("value", value));
        }
        selected = Some(value);
    }
    Ok((selected, args.collect()))
}

/// Local client commands are declared here, independently of Host registries.
pub fn client_commands() -> Vec<crate::command::CommandInfo> {
    vec![crate::command::CommandInfo {
        name: "language".into(),
        aliases: vec![],
        description: UiMessage::new("command.language.description").english(),
        group: crate::command::CommandGroup::Meta,
    }]
}
