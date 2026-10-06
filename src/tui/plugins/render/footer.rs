use super::*;

pub(super) fn lines(
    dialog: &PluginDialog,
    width: usize,
    total: usize,
    budget: usize,
) -> Vec<Line<'static>> {
    let mut status = vec![Line::from(Span::styled(
        fit_display_width(&dialog.status, width),
        theme::style(theme::Role::Dim),
    ))];
    if dialog.view == View::Review {
        let digits = total.to_string().len();
        let consent = format!(
            "[{}] I approve all verified permissions · read {:>digits$}/{total} lines",
            if dialog.accepted { "x" } else { " " },
            dialog.reviewed
        );
        status.push(Line::from(Span::styled(
            fit_display_width(&consent, width),
            theme::style(if dialog.accepted {
                theme::Role::Success
            } else {
                theme::Role::Warning
            }),
        )));
    }
    let mut hints = wrap_text(footer(dialog), width.max(1));
    if dialog.view == View::Installed {
        hints.extend(wrap_text(
            "E enable · U update · C configure · B rollback · D disable · X uninstall",
            width.max(1),
        ));
    }
    hints.truncate(budget.saturating_sub(status.len()));
    status.truncate(budget);
    status.push(Line::from(""));
    status.extend(
        hints
            .into_iter()
            .map(|hint| Line::from(Span::styled(hint, theme::style(theme::Role::Faint)))),
    );
    status
}

fn footer(dialog: &PluginDialog) -> &'static str {
    if dialog.pending {
        "Working… · Esc closes; pending review will be cancelled"
    } else if dialog.view == View::Review {
        "↑↓/PgDn read · Space approve · C configs · Enter activate · Esc cancel"
    } else if dialog.view == View::Config {
        "Enter keep · Ctrl+U clear · Esc discard · Shift+Enter newline"
    } else if dialog.view == View::Remove {
        "Y explicitly confirms · Esc cancels"
    } else {
        "Tab installed/market · / search · R refresh · V market v1/v2 · Esc close"
    }
}
