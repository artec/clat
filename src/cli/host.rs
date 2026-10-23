use clat::client_ports::presentation::{LocaleContext, UiMessage};
use std::process::ExitCode;

pub(super) fn run_default_tui(locale: LocaleContext) -> ExitCode {
    use clat::client_ports::host::HostClient;
    use std::io::{IsTerminal, Write};
    let run = || -> Result<(), String> {
        if !std::io::stdin().is_terminal() || !std::io::stdout().is_terminal() {
            return Err(locale.text("cli.terminal_required").into());
        }
        let project = clat::Project::current().map_err(|error| error.to_string())?;
        let trust = HostClient::project_needs_trust(&project)?;
        if trust {
            eprintln!(
                "{}",
                locale.render(
                    &UiMessage::new("cli.trust_project")
                        .arg("path", project.root().display().to_string())
                )
            );
            std::io::stderr()
                .flush()
                .map_err(|error| error.to_string())?;
            let mut answer = String::new();
            std::io::stdin()
                .read_line(&mut answer)
                .map_err(|error| error.to_string())?;
            if !matches!(answer.trim(), "y" | "Y" | "yes") {
                return Ok(());
            }
        }
        let client = HostClient::spawn_or_attach(&project, trust)?;
        clat::tui::run_host_client_with_locale(project, client, locale)
            .map_err(|error| error.to_string())
    };
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("clat: {error}");
            ExitCode::FAILURE
        }
    }
}

pub(super) fn run_host_command(
    args: impl Iterator<Item = String>,
    locale: LocaleContext,
) -> ExitCode {
    let mut args = args.peekable();
    if args.peek().is_some_and(|arg| arg == "start") {
        args.next();
        return match clat::client_ports::host::HostClient::start_command_presented(args) {
            Ok(message) => {
                println!("{}", locale.render(&message));
                ExitCode::SUCCESS
            }
            Err(error) => {
                eprintln!("clat: {}", error.render(locale));
                ExitCode::FAILURE
            }
        };
    }
    let args = match clat::client_ports::host::HostManagementArgs::parse_presented(args) {
        Ok(args) => args,
        Err(error) => {
            eprintln!("clat: {}", locale.render(&error));
            return ExitCode::from(2);
        }
    };
    match args.execute_presented() {
        Ok(message) => {
            println!("{}", locale.render(&message));
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!(
                "{}",
                locale.render(&UiMessage::new("common.error").arg("detail", error))
            );
            ExitCode::FAILURE
        }
    }
}
