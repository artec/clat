use clat::client_ports::presentation::{
    LanguagePreference, LocaleContext, UiMessage, save_preference, terminal_preferences_path,
};
use std::process::ExitCode;

pub(super) fn run(
    args: impl Iterator<Item = String>,
    locale: LocaleContext,
    preference: LanguagePreference,
) -> ExitCode {
    let values: Vec<_> = args.collect();
    if values.len() > 1 {
        eprintln!(
            "{}",
            locale
                .render(&UiMessage::new("common.invalid_language").arg("value", values.join(" ")))
        );
        return ExitCode::from(2);
    }
    let Some(value) = values.first() else {
        println!(
            "{}",
            locale.render(
                &UiMessage::new("common.language_current")
                    .arg("language", locale.locale.id())
                    .arg("preference", preference.id())
            )
        );
        return ExitCode::SUCCESS;
    };
    let Some(preference) = LanguagePreference::parse(value) else {
        eprintln!(
            "{}",
            locale.render(&UiMessage::new("common.invalid_language").arg("value", value.clone()))
        );
        return ExitCode::from(2);
    };
    let result = terminal_preferences_path().and_then(|p| save_preference(&p, preference));
    match result {
        Ok(()) => {
            println!(
                "{}",
                LocaleContext::system(preference).render(
                    &UiMessage::new("common.language_saved").arg("language", preference.id())
                )
            );
            ExitCode::SUCCESS
        }
        Err(detail) => {
            eprintln!(
                "{}",
                locale.render(&UiMessage::new("common.error").arg("detail", detail))
            );
            ExitCode::FAILURE
        }
    }
}
