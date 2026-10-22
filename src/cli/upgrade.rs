use clat::client_ports::upgrade::{UpgradeOutcome, UpgradeProgress, upgrade_with_progress};
use std::io::{self, Write};
use std::process::ExitCode;

pub(super) fn run_upgrade(check_only: bool) -> ExitCode {
    let mut output = io::stdout().lock();
    let outcome = upgrade_with_progress(check_only, |phase| {
        write_line(&mut output, &progress_line(phase));
    });
    match outcome {
        Ok(outcome) => {
            for line in outcome_lines(outcome) {
                write_line(&mut output, &line);
            }
            ExitCode::SUCCESS
        }
        Err(error) => {
            write_line(
                &mut io::stderr().lock(),
                &format!("✗ clat: upgrade failed: {error}"),
            );
            ExitCode::FAILURE
        }
    }
}

fn write_line(output: &mut impl Write, line: &str) {
    // A closed output pipe must not interrupt verification, replacement or cleanup.
    let _ = writeln!(output, "{line}");
    let _ = output.flush();
}

fn progress_line(phase: UpgradeProgress) -> String {
    match phase {
        UpgradeProgress::Checking { current } => {
            format!("⟳ Checking for upgrade... (current version {current})")
        }
        UpgradeProgress::Found { tag } => format!("✓ Found new version {tag}."),
        UpgradeProgress::Downloading => "⟳ Downloading upgrade...".into(),
        UpgradeProgress::Verifying => "⟳ Verifying signature and checksum...".into(),
        UpgradeProgress::Extracting => "⟳ Extracting files...".into(),
        UpgradeProgress::Verified => "✓ Verification successful.".into(),
        UpgradeProgress::Installing => "⟳ Installing upgrade...".into(),
        UpgradeProgress::Installed { tag } => {
            format!("✓ Upgrade successful! CLAT {tag} is installed.")
        }
    }
}

fn outcome_lines(outcome: UpgradeOutcome) -> Vec<String> {
    match outcome {
        UpgradeOutcome::UpToDate { latest } => vec![format!(
            "✓ CLAT {} is up to date (latest release {latest}).",
            env!("CARGO_PKG_VERSION")
        )],
        UpgradeOutcome::Available { tag } => vec![format!("Run `clat upgrade` to install {tag}.")],
        UpgradeOutcome::Installed { .. } => {
            vec!["Restart running CLAT processes to use the new version.".into()]
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn progress_output_failure_is_observational_and_does_not_panic() {
        struct ClosedPipe;
        impl Write for ClosedPipe {
            fn write(&mut self, _: &[u8]) -> io::Result<usize> {
                Err(io::ErrorKind::BrokenPipe.into())
            }
            fn flush(&mut self) -> io::Result<()> {
                Err(io::ErrorKind::BrokenPipe.into())
            }
        }
        write_line(&mut ClosedPipe, &progress_line(UpgradeProgress::Installing));
    }

    #[test]
    fn upgrade_progress_renders_each_phase_without_terminal_control_codes() {
        let phases = [
            UpgradeProgress::Checking { current: "2.1.0" },
            UpgradeProgress::Found {
                tag: "v2.2.0".into(),
            },
            UpgradeProgress::Downloading,
            UpgradeProgress::Verifying,
            UpgradeProgress::Extracting,
            UpgradeProgress::Verified,
            UpgradeProgress::Installing,
            UpgradeProgress::Installed {
                tag: "v2.2.0".into(),
            },
        ];
        let lines = phases.into_iter().map(progress_line).collect::<Vec<_>>();
        assert!(lines[0].contains("current version 2.1.0"));
        assert_eq!(lines[1], "✓ Found new version v2.2.0.");
        assert_eq!(lines[5], "✓ Verification successful.");
        assert!(lines[7].contains("Upgrade successful!"));
        assert!(
            lines
                .iter()
                .all(|line| !line.contains(['\x1b', '\r', '\n']))
        );
    }

    #[test]
    fn check_and_current_outcomes_never_claim_installation() {
        let available = outcome_lines(UpgradeOutcome::Available {
            tag: "v2.2.0".into(),
        });
        assert_eq!(available, ["Run `clat upgrade` to install v2.2.0."]);
        let current = outcome_lines(UpgradeOutcome::UpToDate {
            latest: "v2.1.0".into(),
        });
        assert!(current[0].starts_with("✓ CLAT"));
        assert!(!current[0].contains("Upgrade successful"));
        let installed = outcome_lines(UpgradeOutcome::Installed {
            tag: "v2.2.0".into(),
        });
        assert!(installed[0].contains("Restart running CLAT"));
    }
}
