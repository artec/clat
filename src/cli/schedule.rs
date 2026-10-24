use clat::client_ports::{
    host::HostClient,
    presentation::{LocaleContext, UiMessage},
    schedules::parse_schedule_command,
};
use std::process::ExitCode;
pub(super) fn run(args: impl Iterator<Item = String>, locale: LocaleContext) -> ExitCode {
    let request = match parse_schedule_command(&args.collect::<Vec<_>>().join(" ")) {
        Ok(request) => request,
        Err(message) => {
            eprintln!("{}", locale.render(&message));
            return ExitCode::from(2);
        }
    };
    let result = HostClient::connect_current_project(2691, locale).and_then(|client| {
        client
            .call_with_receipt(request.method(), &request.params())
            .map_err(|e| locale.render(&e.presentation_or_fallback()))
    });
    match result {
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
