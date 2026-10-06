use super::*;

pub(in crate::tui::plugins) fn plugin_lines(
    dialog: &PluginDialog,
    width: usize,
) -> Vec<Line<'static>> {
    let text = match dialog.view {
        View::Review => dialog.review.as_ref().map(|ticket| {
            let packages = &ticket.review["review"]["packages"];
            let native = packages.as_array().is_some_and(|packages| packages.iter().any(|p| p["runtime"] == "mcp-stdio"));
            let warning = if native { "Native MCP runs with your account's file/network/process access.\n" } else { "" };
            format!("Verified packages · {} · permissions and config schemas\n{warning}Config JSON: {{\"plugin.id\":{{...}}}}; blank keeps update config.\nConfigure requires an explicit plugin.id object; {{}} clears its fields.\n{}", label(&ticket.review,"action"), serde_json::to_string_pretty(packages).unwrap_or_default())
        }).unwrap_or_default(),
        View::Config => format!("Configuration is sent only to this host, never saved in input history.\n{} characters entered (all values masked):\n{}", dialog.config.text().chars().count(), "*".repeat(dialog.config.text().chars().count().min(160))),
        View::Remove => dialog.remove.as_ref().map(|(id, uninstall)| format!("Confirm {} {id}?\n{}", if *uninstall {"uninstall"} else {"disable"}, if *uninstall {"Managed artifacts are removed."} else {"The plugin remains installed, but its contributions are revoked."})).unwrap_or_default(),
        View::Installed | View::Market => return browse_lines(dialog, width),
    };
    text.lines()
        .enumerate()
        .flat_map(|(index, line)| {
            let role = if index == 0 {
                theme::Role::Bold
            } else if line.starts_with("Native MCP") {
                theme::Role::Warning
            } else {
                theme::Role::Dim
            };
            styled_lines(line, width, theme::style(role))
        })
        .collect()
}

fn styled_lines(text: &str, width: usize, style: ratatui::style::Style) -> Vec<Line<'static>> {
    wrap_text(text, width.max(1))
        .into_iter()
        .map(|text| Line::from(Span::styled(text, style)))
        .collect()
}

fn browse_lines(dialog: &PluginDialog, width: usize) -> Vec<Line<'static>> {
    let heading = format!(
        "{} · market v{} · filter: {}{}",
        if dialog.view == View::Market {
            "Signed market · Enter installs"
        } else {
            "Installed on this host"
        },
        dialog.market_generation,
        dialog.query.text(),
        if dialog.searching { " [editing]" } else { "" }
    );
    let mut lines = vec![Line::from(Span::styled(
        fit_display_width(&heading, width),
        theme::style(theme::Role::Bold),
    ))];
    let rows = dialog.rows();
    for (index, row) in rows.iter().enumerate() {
        let body = format!(
            "{} · {} · {}",
            label(row, "id"),
            label(row, "version"),
            label(row, "runtime")
        );
        let role = if index == dialog.selected {
            theme::Role::Selected
        } else if row["enabled"] == true {
            theme::Role::Success
        } else if row.get("enabled").is_none() {
            theme::Role::ModelAccent
        } else {
            theme::Role::Dim
        };
        lines.push(Line::from(Span::styled(
            picker_row(&body, row["enabled"] == true, width),
            theme::style(role),
        )));
    }
    if rows.is_empty() {
        lines.push(Line::from(Span::styled(
            "No matching plugins.",
            theme::style(theme::Role::Dim),
        )));
    }
    if let Some(row) = dialog.selected() {
        let state = if row["enabled"] == true {
            "enabled"
        } else if row.get("enabled").is_some() {
            "disabled"
        } else {
            "market"
        };
        let role = if row["enabled"] == true {
            theme::Role::Success
        } else {
            theme::Role::Dim
        };
        lines.extend(styled_lines(
            &format!(
                "{} · {state} · publisher: {}",
                label(&row, "name"),
                label(&row, "publisher")
            ),
            width,
            theme::style(role),
        ));
        lines.extend(styled_lines(
            &label(&row, "summary"),
            width,
            theme::style(theme::Role::Dim),
        ));
        lines.extend(styled_lines(
            &format!("Health: {}", label(&row, "health")),
            width,
            theme::style(theme::Role::Faint),
        ));
    }
    lines
}

pub(in crate::tui::plugins) fn browse_viewport(
    dialog: &PluginDialog,
    width: usize,
    height: usize,
) -> Vec<Line<'static>> {
    let all = browse_lines(dialog, width);
    let rows = dialog.rows();
    if height == 0 {
        return vec![];
    }
    if height == 1 && !rows.is_empty() {
        return all.into_iter().skip(dialog.selected + 1).take(1).collect();
    }
    if all.len() <= height {
        return all;
    }
    let detail_start = 1 + rows.len();
    let detail = all.iter().skip(detail_start).cloned().collect::<Vec<_>>();
    let visible = height.saturating_sub(1 + detail.len()).max(1);
    let first = dialog.selected.saturating_sub(visible.saturating_sub(1));
    let mut lines = all.iter().take(1).cloned().collect::<Vec<_>>();
    lines.extend(all.iter().skip(first + 1).take(visible).cloned());
    lines.extend(detail);
    lines.truncate(height);
    lines
}
