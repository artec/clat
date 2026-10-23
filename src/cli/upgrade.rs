use clat::client_ports::presentation::{LocaleContext, UiMessage};
use clat::client_ports::upgrade::{UpgradeOutcome, UpgradeProgress, upgrade_with_progress};
use std::io::{self, Write};
use std::process::ExitCode;

pub(super) fn run_upgrade(check_only: bool, locale: LocaleContext) -> ExitCode {
    let mut output = io::stdout().lock();
    let outcome = upgrade_with_progress(check_only, |phase| {
        write_line(&mut output, &progress_line_localized(phase, locale));
    });
    match outcome {
        Ok(outcome) => {
            for line in outcome_lines_localized(outcome, locale) {
                write_line(&mut output, &line);
            }
            ExitCode::SUCCESS
        }
        Err(error) => {
            write_line(
                &mut io::stderr().lock(),
                &locale
                    .render(&UiMessage::new("cli.upgrade_error").arg("detail", error.to_string())),
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

#[cfg(test)]
fn progress_line(phase: UpgradeProgress) -> String {
    progress_line_localized(phase, LocaleContext::default())
}
#[cfg(test)]
fn outcome_lines(outcome: UpgradeOutcome) -> Vec<String> {
    outcome_lines_localized(outcome, LocaleContext::default())
}
fn progress_line_localized(phase: UpgradeProgress, locale: LocaleContext) -> String {
    let msg = match phase {
        UpgradeProgress::Checking { current } => {
            UiMessage::new("cli.upgrade_check").arg("current", current)
        }
        UpgradeProgress::Found { tag } => UiMessage::new("cli.upgrade_found").arg("tag", tag),
        UpgradeProgress::Downloading => UiMessage::new("cli.upgrade_download"),
        UpgradeProgress::Verifying => UiMessage::new("cli.upgrade_verify"),
        UpgradeProgress::Extracting => UiMessage::new("cli.upgrade_extract"),
        UpgradeProgress::Verified => UiMessage::new("cli.upgrade_verified"),
        UpgradeProgress::Installing => UiMessage::new("cli.upgrade_install"),
        UpgradeProgress::Installed { tag } => {
            UiMessage::new("cli.upgrade_installed").arg("tag", tag)
        }
    };
    locale.render(&msg)
}
fn outcome_lines_localized(outcome: UpgradeOutcome, locale: LocaleContext) -> Vec<String> {
    let msg = match outcome {
        UpgradeOutcome::UpToDate { latest } => UiMessage::new("cli.upgrade_current")
            .arg("version", env!("CARGO_PKG_VERSION"))
            .arg("latest", latest),
        UpgradeOutcome::Available { tag } => {
            UiMessage::new("cli.upgrade_available").arg("tag", tag)
        }
        UpgradeOutcome::Installed { .. } => UiMessage::new("cli.upgrade_restart"),
    };
    vec![locale.render(&msg)]
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

#[cfg(test)]
mod language_tests {
    use super::*;
    use clat::client_ports::presentation::Locale;
    #[test]
    fn upgrade_progress_keeps_versions_and_outcomes_across_three_interface_languages() {
        for locale in [Locale::English, Locale::Simplified, Locale::Traditional] {
            let locale = LocaleContext::new(locale);
            let found = progress_line_localized(
                UpgradeProgress::Found {
                    tag: "v2.2.0".into(),
                },
                locale,
            );
            assert!(found.contains("v2.2.0"));
            assert!(!found.contains(['\x1b', '\r', '\n']));
            let available = outcome_lines_localized(
                UpgradeOutcome::Available {
                    tag: "v2.2.0".into(),
                },
                locale,
            );
            assert!(available[0].contains("clat upgrade"));
            let installed = outcome_lines_localized(
                UpgradeOutcome::Installed {
                    tag: "v2.2.0".into(),
                },
                locale,
            );
            assert_ne!(installed, available);
            if locale.locale != Locale::English {
                assert!(!found.contains("Found new version"));
                assert!(!installed[0].contains("Restart running CLAT"));
            }
        }
    }
}
