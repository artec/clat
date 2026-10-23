use super::*;
use crate::i18n::{LocaleContext, UiMessage};
/// Terminal control surface for the core-owned WeChat binding/pairing state.
/// QR payloads are rendered directly and never printed as text or persisted.
pub fn run_wechat_command_with_locale<I>(project: Project, args: I, locale: LocaleContext) -> u8
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
    let result = execute_action(&application, &words, action, locale);
    let action_ok = match result {
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

fn run_terminal_wechat_binding(
    application: &TrustedProjectApplication,
    replace: bool,
    locale: LocaleContext,
) -> Result<(), crate::ApplicationError> {
    if application.wechat_binding()?.status.bound && !replace {
        return Err(crate::ApplicationError::from_message(
            locale.text("cli.wechat_existing"),
        ));
    }
    let (mut binding, content) = crate::im::BindingSession::start()
        .map_err(|error| crate::ApplicationError::from_message(error.to_string()))?;
    let rendered =
        crate::im::qr_terminal(&content).map_err(crate::ApplicationError::from_message)?;
    println!("{}", locale.text("cli.wechat_scan"));
    println!("{rendered}");
    println!("{}", locale.text("cli.wechat_not_authorized"));
    let deadline = std::time::Instant::now() + Duration::from_secs(8 * 60);
    let mut verify_code: Option<String> = None;
    while std::time::Instant::now() < deadline {
        let step = binding
            .poll(verify_code.as_deref())
            .map_err(|error| crate::ApplicationError::from_message(error.to_string()))?;
        verify_code = None;
        match step {
            crate::im::BindingStep::Waiting => {}
            crate::im::BindingStep::Scanned => println!("{}", locale.text("cli.wechat_scanned")),
            crate::im::BindingStep::NeedVerifyCode => {
                verify_code = Some(read_verification_code(locale)?);
            }
            crate::im::BindingStep::VerifyCodeBlocked => {
                return Err(crate::ApplicationError::from_message(
                    locale.text("cli.wechat_verify_blocked"),
                ));
            }
            crate::im::BindingStep::Expired => {
                return Err(crate::ApplicationError::from_message(
                    locale.text("cli.wechat_qr_expired"),
                ));
            }
            crate::im::BindingStep::AlreadyBound => {
                return Err(crate::ApplicationError::from_message(
                    locale.text("cli.wechat_already_bound"),
                ));
            }
            crate::im::BindingStep::Confirmed(credentials) => {
                application.replace_wechat_binding(&credentials)?;
                println!("{}", locale.text("cli.wechat_confirmed"));
                println!("{}", locale.text("cli.wechat_next"));
                return Ok(());
            }
        }
    }
    Err(crate::ApplicationError::from_message(
        locale.text("cli.wechat_deadline"),
    ))
}

fn read_verification_code(locale: LocaleContext) -> Result<String, crate::ApplicationError> {
    use std::io::Write as _;
    print!("{}", locale.text("cli.wechat_verify"));
    std::io::stdout()
        .flush()
        .map_err(|e| crate::ApplicationError::from_message(e.to_string()))?;
    let mut input = String::new();
    std::io::stdin()
        .read_line(&mut input)
        .map_err(|e| crate::ApplicationError::from_message(e.to_string()))?;
    let input = input.trim();
    if input.is_empty() || input.len() > 32 || !input.bytes().all(|b| b.is_ascii_alphanumeric()) {
        return Err(crate::ApplicationError::from_message(
            locale.text("cli.wechat_verify_invalid"),
        ));
    }
    Ok(input.to_owned())
}

fn parse_action(
    args: impl IntoIterator<Item = String>,
    locale: LocaleContext,
) -> Result<(Vec<String>, &'static str), u8> {
    let words = args.into_iter().collect::<Vec<_>>();
    let action = match words.first().map(String::as_str) {
        Some("bind") => "bind",
        Some("status") => "status",
        Some("pair") => "pair",
        Some("unbind") => "unbind",
        Some(other) => {
            eprintln!(
                "{}",
                locale.render(&UiMessage::new("cli.wechat_unknown").arg("action", other))
            );
            eprintln!("{}", locale.text("cli.wechat_usage"));
            return Err(2);
        }
        None => {
            eprintln!("{}", locale.text("cli.wechat_usage"));
            return Err(2);
        }
    };
    let valid = match action {
        "bind" => words.len() == 1 || words.as_slice() == ["bind", "--replace"],
        "unbind" => words.as_slice() == ["unbind", "--confirm"],
        _ => words.len() == 1,
    };
    if !valid {
        eprintln!(
            "{}",
            locale.render(&UiMessage::new("cli.wechat_args").arg("action", action))
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
        "status" => application.wechat_binding().map(|snapshot| {
            let status = snapshot.status;
            println!(
                "{}",
                locale.render(
                    &UiMessage::new("cli.wechat_status")
                        .arg(
                            "state",
                            locale.text(if status.bound {
                                "cli.wechat_bound"
                            } else {
                                "cli.wechat_unbound"
                            })
                        )
                        .arg("users", status.paired_users)
                        .arg("chats", status.mapped_chats)
                )
            );
        }),
        "pair" => application.issue_wechat_pairing_code().map(|challenge| {
            println!(
                "{}",
                locale.render(&UiMessage::new("cli.wechat_code").arg("code", challenge.code))
            );
            println!("{}", locale.text("cli.wechat_expiry"));
            println!("{}", locale.text("cli.wechat_pair_hint"));
        }),
        "unbind" => application.revoke_wechat_binding().map(|()| {
            println!("{}", locale.text("cli.wechat_removed"));
        }),
        "bind" => run_terminal_wechat_binding(
            application,
            words.get(1).is_some_and(|word| word == "--replace"),
            locale,
        ),
        _ => unreachable!(),
    }
}
