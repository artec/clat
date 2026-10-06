use super::*;

impl App {
    pub(in crate::tui) fn draw_plugin_dialog(&mut self, frame: &mut Frame) {
        if self.pending_permission.is_some() || self.pending_ask_user.is_some() {
            self.close_plugin_dialog(); // Permission/question UI always owns the visible modal.
            return;
        }
        let Some(dialog) = self.plugins.dialog.as_mut() else {
            return;
        };
        dialog.expire();
        let area = centered_rect(84, popup_height_cap(frame.area()), frame.area());
        frame.render_widget(Clear, area);
        let block = Block::default()
            .borders(Borders::ALL)
            .title(" /plugin · host management ");
        let inner = block.inner(area);
        frame.render_widget(block, area);
        let width = inner.width as usize;
        let mut lines = plugin_lines(dialog, width);
        let footer = footer(dialog);
        let body_height = inner.height.saturating_sub(4) as usize;
        if dialog.view == View::Review {
            if dialog.review_width != width || dialog.page != body_height.max(1) {
                dialog.reviewed = 0;
                dialog.accepted = false;
                dialog.scroll = 0;
                dialog.review_width = width;
            }
            dialog.total = lines.len();
            dialog.page = body_height.max(1);
            dialog.scroll = dialog.scroll.min(lines.len().saturating_sub(body_height));
            if dialog.scroll <= dialog.reviewed {
                dialog.reviewed = dialog
                    .reviewed
                    .max((dialog.scroll + body_height).min(lines.len()));
            }
            lines = lines
                .into_iter()
                .skip(dialog.scroll)
                .take(body_height)
                .collect();
        } else if matches!(dialog.view, View::Installed | View::Market) {
            lines = browse_viewport(dialog, width, body_height);
        }
        frame.render_widget(
            Paragraph::new(lines),
            Rect::new(inner.x, inner.y, inner.width, body_height as u16),
        );
        let bottom = vec![
            Line::from(dialog.status.clone()),
            Line::from(footer),
            Line::from(if dialog.view == View::Review {
                format!(
                    "[{}] I approve all verified permissions · read {}/{} lines",
                    if dialog.accepted { "x" } else { " " },
                    dialog.reviewed,
                    dialog.total
                )
            } else {
                "Installed: E enable · U update · C configure · B rollback · D disable · X uninstall".into()
            }),
        ];
        frame.render_widget(
            Paragraph::new(bottom).wrap(Wrap { trim: false }),
            Rect::new(
                inner.x,
                inner.y.saturating_add(body_height as u16),
                inner.width,
                inner.height.saturating_sub(body_height as u16),
            ),
        );
    }
}

pub(super) fn plugin_lines(dialog: &PluginDialog, width: usize) -> Vec<Line<'static>> {
    let text = match dialog.view {
        View::Review => dialog.review.as_ref().map(|ticket| {
            let packages = &ticket.review["review"]["packages"];
            let native = packages.as_array().is_some_and(|packages| packages.iter().any(|p| p["runtime"] == "mcp-stdio"));
            let warning = if native { "Native MCP runs with your account's file/network/process access.\n" } else { "" };
            format!("Verified packages · {} · permissions and config schemas\n{warning}Config JSON: {{\"plugin.id\":{{...}}}}; blank keeps update config.\nConfigure requires an explicit plugin.id object; {{}} clears its fields.\n{}",
                label(&ticket.review,"action"), serde_json::to_string_pretty(packages).unwrap_or_default())
        }).unwrap_or_default(),
        View::Config => format!("Configuration is sent only to this host, never saved in input history.\n{} characters entered (all values masked):\n{}",
            dialog.config.text().chars().count(), "*".repeat(dialog.config.text().chars().count().min(160))),
        View::Remove => dialog.remove.as_ref().map(|(id, uninstall)| format!("Confirm {} {id}?\n{}",
            if *uninstall {"uninstall"} else {"disable"}, if *uninstall {"Managed artifacts are removed."} else {"The plugin remains installed, but its contributions are revoked."})).unwrap_or_default(),
        View::Installed | View::Market => return browse_lines(dialog, width),
    };
    text.lines()
        .flat_map(|line| wrap_text(line, width.max(1)).into_iter().map(Line::from))
        .collect()
}

fn browse_lines(dialog: &PluginDialog, width: usize) -> Vec<Line<'static>> {
    let mut lines = vec![Line::from(format!(
        "{} · market v{} · filter: {}{}",
        if dialog.view == View::Market {
            "Signed market · Enter installs"
        } else {
            "Installed on this host"
        },
        dialog.market_generation,
        dialog.query.text(),
        if dialog.searching { " [editing]" } else { "" }
    ))];
    for (index, row) in dialog.rows().into_iter().enumerate() {
        let state = if row["enabled"] == true {
            "enabled"
        } else if row.get("enabled").is_some() {
            "disabled"
        } else {
            "market"
        };
        let text = format!(
            "{} {} · {} · {} · {}\n  {} {} {}",
            if index == dialog.selected { ">" } else { " " },
            label(row, "name"),
            label(row, "id"),
            label(row, "version"),
            label(row, "runtime"),
            state,
            label(row, "health"),
            label(row, "summary")
        );
        let text = wrap_text(&text.replace('\n', " "), width.max(1))
            .into_iter()
            .next()
            .unwrap_or_default();
        lines.push(Line::from(text));
    }
    if dialog.rows().is_empty() {
        lines.push(Line::from(
            "No matching plugins. Installed management stays available offline.",
        ));
    }
    if let Some(row) = dialog.selected() {
        lines.push(Line::from(format!(
            "Selected health: {} · publisher: {}",
            label(&row, "health"),
            label(&row, "publisher")
        )));
    }
    lines
}

fn browse_viewport(dialog: &PluginDialog, width: usize, height: usize) -> Vec<Line<'static>> {
    let all = browse_lines(dialog, width);
    let rows = dialog.rows();
    let visible = height.saturating_sub(7).max(1);
    let first = dialog.selected.saturating_sub(visible.saturating_sub(1));
    let mut lines = all.into_iter().take(1).collect::<Vec<_>>();
    for (index, row) in rows.iter().enumerate().skip(first).take(visible) {
        let text = format!(
            "{} {} · {} · {}",
            if index == dialog.selected { ">" } else { " " },
            label(row, "id"),
            label(row, "version"),
            label(row, "runtime")
        );
        lines.push(Line::from(text));
    }
    if rows.is_empty() {
        lines.push(Line::from("No matching plugins."));
    }
    if let Some(row) = dialog.selected() {
        let detail = format!(
            "{} · {} · publisher: {}\n{}\nHealth: {}",
            label(&row, "name"),
            if row["enabled"] == true {
                "enabled"
            } else {
                "disabled/market"
            },
            label(&row, "publisher"),
            label(&row, "summary"),
            label(&row, "health")
        );
        for line in detail.lines() {
            lines.extend(wrap_text(line, width.max(1)).into_iter().map(Line::from));
        }
    }
    lines.truncate(height);
    lines
}

fn footer(dialog: &PluginDialog) -> &'static str {
    if dialog.pending {
        "Working… · Esc closes; pending review will be cancelled"
    } else if dialog.view == View::Review {
        "↑↓/PgDn read · Space approve · C masked configs · Enter activate · Esc cancel"
    } else if dialog.view == View::Config {
        "JSON keyed by package ID · values hidden · Enter keep · Ctrl+U clear · Esc discard"
    } else if dialog.view == View::Remove {
        "Y explicitly confirms · Esc cancels"
    } else {
        "Tab installed/market · / search · R refresh · V market v1/v2 · Esc close"
    }
}

#[cfg(test)]
pub(super) fn browse_viewport_for_tests(
    dialog: &PluginDialog,
    width: usize,
    height: usize,
) -> String {
    browse_viewport(dialog, width, height)
        .into_iter()
        .flat_map(|line| line.spans.into_iter().map(|span| span.content.into_owned()))
        .collect()
}
