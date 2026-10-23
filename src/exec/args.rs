use super::{ExecArgs, UiMessage};

/// Compatibility parser keeps its English error contract for existing callers.
pub fn parse_exec_args<I: IntoIterator<Item = String>>(args: I) -> Result<ExecArgs, String> {
    parse_exec_args_presented(args).map_err(|message| message.english())
}
pub fn parse_exec_args_presented<I: IntoIterator<Item = String>>(
    args: I,
) -> Result<ExecArgs, UiMessage> {
    let mut parsed = ExecArgs::default();
    let mut iter = args.into_iter();
    let mut positional_only = false;
    while let Some(word) = iter.next() {
        if word == "--" && !positional_only {
            positional_only = true;
            continue;
        }
        if positional_only {
            set_prompt(&mut parsed, word)?;
        } else {
            parse_word(&mut parsed, &word, &mut iter)?;
        }
    }
    if parsed.continue_session && parsed.session.is_some() {
        return Err(UiMessage::new("cli.argument_continue_session"));
    }
    if parsed.command.is_some() && parsed.prompt.is_some() {
        return Err(UiMessage::new("cli.argument_command_prompt"));
    }
    if parsed.json && parsed.command.is_some() {
        return Err(UiMessage::new("cli.argument_json_command"));
    }
    Ok(parsed)
}
fn set_prompt(parsed: &mut ExecArgs, value: String) -> Result<(), UiMessage> {
    if parsed.prompt.is_some() {
        return Err(UiMessage::new("cli.argument_extra").arg("argument", value));
    }
    parsed.prompt = Some(value);
    Ok(())
}
fn parse_word(
    parsed: &mut ExecArgs,
    word: &str,
    iter: &mut impl Iterator<Item = String>,
) -> Result<(), UiMessage> {
    match word {
        "--continue" => parsed.continue_session = true,
        "--yes" => parsed.yes = true,
        "--trust" => parsed.trust = true,
        "--quiet" => parsed.quiet = true,
        "--json" => parsed.json = true,
        "--command" => {
            let value = iter
                .next()
                .ok_or_else(|| UiMessage::new("cli.argument_command"))?;
            if !value.starts_with('/') || value.trim().is_empty() {
                return Err(UiMessage::new("cli.argument_command_invalid").arg("value", value));
            }
            parsed.command = Some(value);
        }
        "--session" => {
            let value = iter
                .next()
                .ok_or_else(|| UiMessage::new("cli.argument_session"))?;
            if value.trim().is_empty() {
                return Err(UiMessage::new("cli.argument_empty_session"));
            }
            parsed.session = Some(value);
        }
        other if other.starts_with('-') && other != "-" => {
            return Err(UiMessage::new("cli.argument_unknown").arg("argument", other));
        }
        other => set_prompt(parsed, other.into())?,
    }
    Ok(())
}
