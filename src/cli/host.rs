use std::process::ExitCode;

pub(super) fn run_default_tui() -> ExitCode {
    use clat::client_ports::host::HostClient;
    use std::io::{IsTerminal, Write};
    let run = || -> Result<(), String> {
        if !std::io::stdin().is_terminal() || !std::io::stdout().is_terminal() {
            return Err("interactive terminal required; use `clat exec` for scripts".into());
        }
        let project = clat::Project::current().map_err(|error| error.to_string())?;
        let trust = HostClient::project_needs_trust(&project)?;
        if trust {
            eprintln!(
                "Trust project {}? The host can access files and execute tools in it. [y/N]",
                project.root().display()
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
        clat::tui::run_host_client(project, client).map_err(|error| error.to_string())
    };
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("clat: {error}");
            ExitCode::FAILURE
        }
    }
}

pub(super) fn run_host_command(args: impl Iterator<Item = String>) -> ExitCode {
    let mut args = args.peekable();
    if args.peek().is_some_and(|arg| arg == "start") {
        args.next();
        return match clat::client_ports::host::HostClient::start_command(args) {
            Ok(message) => {
                println!("{message}");
                ExitCode::SUCCESS
            }
            Err(error) => {
                eprintln!("clat: {error}");
                ExitCode::FAILURE
            }
        };
    }
    let args = match clat::client_ports::host::HostManagementArgs::parse(args) {
        Ok(args) => args,
        Err(error) => {
            eprintln!("clat: {error}");
            return ExitCode::from(2);
        }
    };
    match args.execute() {
        Ok(message) => {
            println!("{message}");
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("clat: host: {error}");
            ExitCode::FAILURE
        }
    }
}
