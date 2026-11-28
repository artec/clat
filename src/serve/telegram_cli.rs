//! Terminal control surface for a Telegram bot token and pairing.
//!
//! The token is read from a prompt. It is never accepted as a process argument.

use super::*;
use crate::i18n::{LocaleContext, UiMessage};
use crate::im::telegram::acceptable_token;

pub fn run_telegram_command_with_locale<I>(project: Project, args: I, locale: LocaleContext) -> u8
where
    I: IntoIterator<Item = String>,
{
    let (words, action) = match parse_action(args, locale) {
        Ok(value) => value,
        Err(code) => return code,
    };
    let bootstrap = match BootstrapApplication::open_default(project) {
        Ok(bootstrap) => bootstrap.with_permission_modes(),
        Err(error) => {
            eprintln!("clat: {error}");
            return 1;
        }
    };
    let application = match bootstrap.into_trusted() {
        Ok(application) => application,
        Err(error) => {
            eprintln!("clat: {error}");
            return 1;
        }
    };
    let action_ok = match execute_action(&application, &words, &action, locale) {
        Ok(()) => true,
        Err(error) => {
            eprintln!("clat: {error}");
            false
        }
    };
    let close_ok = match application.close() {
        Ok(()) => true,
        Err(error) => {
            eprintln!(
                "{}",
                locale
                    .render(&UiMessage::new("cli.close_failure").arg("detail", error.to_string()))
            );
            false
        }
    };
    if action_ok && close_ok { 0 } else { 1 }
}

fn parse_action<I>(args: I, locale: LocaleContext) -> Result<(Vec<String>, String), u8>
where
    I: IntoIterator<Item = String>,
{
    let words: Vec<String> = args.into_iter().collect();
    let Some(action) = words.first().cloned() else {
        eprintln!("{}", locale.text("cli.telegram_usage"));
        return Err(2);
    };
    let valid = match action.as_str() {
        "bind" => words.len() == 1 || words.as_slice() == ["bind", "--replace"],
        "status" | "pair" => words.len() == 1,
        "unbind" => words.as_slice() == ["unbind", "--confirm"],
        _ => {
            eprintln!(
                "{}",
                locale.render(&UiMessage::new("cli.telegram_unknown").arg("action", action))
            );
            return Err(2);
        }
    };
    if !valid {
        eprintln!(
            "{}",
            locale.render(&UiMessage::new("cli.telegram_args").arg("action", action))
        );
        return Err(2);
    }
    Ok((words, action))
}

fn execute_action(
    application: &TrustedProjectApplication,
    words: &[String],
    action: &str,
    locale: LocaleContext,
) -> Result<(), crate::ApplicationError> {
    match action {
        "status" => print_status(application, locale),
        "pair" => print_pair(application, locale),
        "unbind" => application.revoke_telegram_binding().map(|()| {
            println!("{}", locale.text("cli.telegram_removed"));
        }),
        "bind" => bind(
            application,
            words.get(1).is_some_and(|word| word == "--replace"),
            locale,
        ),
        _ => unreachable!(),
    }
}

fn print_status(
    application: &TrustedProjectApplication,
    locale: LocaleContext,
) -> Result<(), crate::ApplicationError> {
    let status = application.telegram_binding()?.status;
    println!(
        "{}",
        locale.render(
            &UiMessage::new("cli.telegram_status")
                .arg(
                    "state",
                    locale.text(if status.bound {
                        "cli.telegram_bound"
                    } else {
                        "cli.telegram_unbound"
                    })
                )
                .arg("users", status.paired_users)
                .arg("chats", status.mapped_chats)
        )
    );
    Ok(())
}

fn print_pair(
    application: &TrustedProjectApplication,
    locale: LocaleContext,
) -> Result<(), crate::ApplicationError> {
    if !application.telegram_binding()?.status.bound {
        return Err(crate::ApplicationError::from_message(
            locale.text("cli.telegram_unconfigured"),
        ));
    }
    let challenge = application.issue_telegram_pairing_code()?;
    println!(
        "{}",
        locale.render(&UiMessage::new("cli.telegram_code").arg("code", challenge.code))
    );
    println!("{}", locale.text("cli.telegram_expiry"));
    println!("{}", locale.text("cli.telegram_pair_hint"));
    Ok(())
}

fn bind(
    application: &TrustedProjectApplication,
    replace: bool,
    locale: LocaleContext,
) -> Result<(), crate::ApplicationError> {
    if application.telegram_binding()?.status.bound && !replace {
        return Err(crate::ApplicationError::from_message(
            locale.text("cli.telegram_existing"),
        ));
    }
    let token = read_token(locale)?;
    let credentials = crate::im::telegram::Credentials::new(token)
        .map_err(crate::ApplicationError::from_message)?;
    application.replace_telegram_binding(&credentials)?;
    println!("{}", locale.text("cli.telegram_confirmed"));
    println!("{}", locale.text("cli.telegram_not_authorized"));
    println!("{}", locale.text("cli.telegram_next"));
    Ok(())
}

fn read_token(locale: LocaleContext) -> Result<String, crate::ApplicationError> {
    eprint!("{}", locale.text("cli.telegram_token_prompt"));
    let _ = std::io::Write::flush(&mut std::io::stderr());
    let mut line = String::new();
    std::io::stdin()
        .read_line(&mut line)
        .map_err(|error| crate::ApplicationError::from_message(error.to_string()))?;
    let value = line.trim().to_owned();
    if acceptable_token(&value) {
        Ok(value)
    } else {
        Err(crate::ApplicationError::from_message(
            locale.text("cli.telegram_token_invalid"),
        ))
    }
}
