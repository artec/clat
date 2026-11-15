use clat::client_ports::{
    history::{HistoryCommandRequest, parse_history_command, unpack_dsh_archive},
    host::HostClient,
    presentation::{LocaleContext, UiMessage},
};
use std::process::ExitCode;
pub(super) fn run(args: impl Iterator<Item = String>, locale: LocaleContext) -> ExitCode {
    let args = args.collect::<Vec<_>>();
    if args.first().is_some_and(|arg| arg == "unpack-dsh") {
        return unpack(&args[1..], locale);
    }
    let request = match parse_history_command(&args.join(" ")) {
        Ok(request) => request,
        Err(error) => {
            eprintln!("{}", locale.render(&error));
            return ExitCode::from(2);
        }
    };
    match execute(request, locale) {
        Ok(value) => {
            println!(
                "{}",
                serde_json::to_string_pretty(&value).unwrap_or_default()
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

fn unpack(args: &[String], locale: LocaleContext) -> ExitCode {
    if !(2..=3).contains(&args.len()) {
        eprintln!(
            "{}",
            locale.render(&UiMessage::new("history.archive_cli_help"))
        );
        return ExitCode::from(2);
    }
    use std::path::Path;
    match unpack_dsh_archive(
        Path::new(&args[0]),
        Path::new(&args[1]),
        args.get(2).map(Path::new),
    ) {
        Ok(()) => {
            println!(
                "{}",
                locale.render(
                    &UiMessage::new("history.archive_cli_done").arg("directory", args[1].clone())
                )
            );
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("{}", locale.render(&error.presentation()));
            ExitCode::FAILURE
        }
    }
}
fn execute(
    request: HistoryCommandRequest,
    locale: LocaleContext,
) -> Result<serde_json::Value, String> {
    HostClient::connect_current_project(2691, locale)?
        .call_with_receipt(request.method(), &request.params())
        .map_err(|e| locale.render(&e.presentation_or_fallback()))
}
