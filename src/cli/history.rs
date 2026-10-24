use clat::client_ports::{
    history::{HistoryCommandRequest, parse_history_command},
    host::HostClient,
    presentation::{LocaleContext, UiMessage},
};
use std::process::ExitCode;
pub(super) fn run(args: impl Iterator<Item = String>, locale: LocaleContext) -> ExitCode {
    let request = match parse_history_command(&args.collect::<Vec<_>>().join(" ")) {
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
fn execute(
    request: HistoryCommandRequest,
    locale: LocaleContext,
) -> Result<serde_json::Value, String> {
    HostClient::connect_current_project(2691, locale)?
        .call_with_receipt(request.method(), &request.params())
        .map_err(|e| locale.render(&e.presentation_or_fallback()))
}
