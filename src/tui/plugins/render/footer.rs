use super::*;

pub(super) fn lines(
    dialog: &PluginDialog,
    width: usize,
    total: usize,
    budget: usize,
) -> Vec<Line<'static>> {
    let mut status = vec![Line::from(Span::styled(
        fit_display_width(&dialog.status.render(dialog.locale), width),
        theme::style(theme::Role::Dim),
    ))];
    if dialog.view == View::Review {
        let digits = total.to_string().len();
        let consent = dialog.locale.render(
            &UiMessage::new("tui.plugin_consent")
                .arg("mark", if dialog.accepted { "x" } else { " " })
                .arg("read", format!("{:>digits$}", dialog.reviewed))
                .arg("total", total),
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
            dialog.locale.text("tui.plugin_actions"),
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
        dialog.locale.text("tui.plugin_working")
    } else if dialog.view == View::Review {
        dialog.locale.text("tui.plugin_review_keys")
    } else if dialog.view == View::Config {
        dialog.locale.text("tui.plugin_config_keys")
    } else if dialog.view == View::Remove {
        dialog.locale.text("tui.plugin_confirm_keys")
    } else {
        dialog.locale.text("tui.plugin_browse_keys")
    }
}
