use super::*;

pub(in crate::tui::plugins) fn plugin_lines(
    dialog: &PluginDialog,
    width: usize,
) -> Vec<Line<'static>> {
    let text = match dialog.view {
        View::BuiltinLeave => dialog.locale.text("extension.config_tui_leave").to_owned(),
        View::Review => review_text(dialog),
        View::Config => dialog.locale.render(
            &UiMessage::new("tui.plugin_config_body")
                .arg("count", dialog.config.text().chars().count())
                .arg(
                    "mask",
                    "*".repeat(dialog.config.text().chars().count().min(160)),
                ),
        ),
        View::Remove => dialog
            .remove
            .as_ref()
            .map(|(id, uninstall)| {
                dialog.locale.render(
                    &UiMessage::new("tui.plugin_remove_body")
                        .arg(
                            "action",
                            dialog.locale.text(if *uninstall {
                                "tui.uninstall_verb"
                            } else {
                                "tui.disable_verb"
                            }),
                        )
                        .arg("id", id.clone())
                        .arg(
                            "detail",
                            dialog.locale.text(if *uninstall {
                                "tui.plugin_remove_artifacts"
                            } else {
                                "tui.plugin_disable_detail"
                            }),
                        ),
                )
            })
            .unwrap_or_default(),
        View::Installed | View::Builtins | View::Market => return browse_lines(dialog, width),
    };
    text.lines()
        .enumerate()
        .flat_map(|(index, line)| {
            let role = if index == 0 {
                theme::Role::Bold
            } else if dialog.view == View::Review
                && dialog.review.as_ref().is_some_and(|ticket| {
                    ticket.review["review"]["packages"]
                        .as_array()
                        .is_some_and(|packages| {
                            packages.iter().any(|p| p["runtime"] == "mcp-stdio")
                        })
                })
                && index == 1
            {
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
    let heading = dialog.locale.render(
        &UiMessage::new("tui.plugin_browse_heading")
            .arg(
                "heading",
                dialog.locale.text(match dialog.view {
                    View::Market => "tui.plugin_market_heading",
                    View::Builtins => "extension.builtins",
                    _ => "tui.plugin_installed_heading",
                }),
            )
            .arg("generation", dialog.market_generation)
            .arg("query", dialog.query.text())
            .arg(
                "editing",
                if dialog.searching {
                    dialog.locale.text("tui.plugin_editing")
                } else {
                    ""
                },
            ),
    );
    let mut lines = vec![Line::from(Span::styled(
        fit_display_width(&heading, width),
        theme::style(theme::Role::Bold),
    ))];
    let rows = dialog.rows();
    for (index, row) in rows.iter().enumerate() {
        let body = format!(
            "{} · {} · {}",
            localized_label(row, "name", dialog.locale),
            label(row, "version"),
            if dialog.view == View::Builtins {
                dialog.locale.text("extension.bundled").into()
            } else {
                label(row, "runtime")
            }
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
            dialog.locale.text("tui.plugin_empty"),
            theme::style(theme::Role::Dim),
        )));
    }
    append_selected(dialog, width, &mut lines);
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

fn review_text(dialog: &PluginDialog) -> String {
    dialog
        .review
        .as_ref()
        .map(|ticket| {
            let packages = &ticket.review["review"]["packages"];
            let native = packages
                .as_array()
                .is_some_and(|packages| packages.iter().any(|p| p["runtime"] == "mcp-stdio"));
            let warning = if native {
                dialog.locale.text("tui.plugin_native_risk")
            } else {
                ""
            };
            dialog.locale.render(
                &UiMessage::new("tui.plugin_review_body")
                    .arg("action", label(&ticket.review, "action"))
                    .arg("warning", warning)
                    .arg("example", r#"{"plugin.id":{...}}"#)
                    .arg("empty", "{}")
                    .arg(
                        "packages",
                        serde_json::to_string_pretty(packages).unwrap_or_default(),
                    ),
            )
        })
        .unwrap_or_default()
}

fn append_selected(dialog: &PluginDialog, width: usize, lines: &mut Vec<Line<'static>>) {
    if let Some(row) = dialog.selected() {
        if dialog.view == View::Builtins {
            append_builtin(dialog, &row, width, lines);
            return;
        }
        lines.extend(styled_lines(
            &label(&row, "id"),
            width,
            theme::style(theme::Role::Faint),
        ));
        let state = if row["enabled"] == true {
            dialog.locale.text("tui.plugin_enabled_state")
        } else if row.get("enabled").is_some() {
            dialog.locale.text("tui.plugin_disabled_state")
        } else {
            dialog.locale.text("tui.plugin_market_state")
        };
        let role = if row["enabled"] == true {
            theme::Role::Success
        } else {
            theme::Role::Dim
        };
        lines.extend(styled_lines(
            &dialog.locale.render(
                &UiMessage::new("tui.plugin_publisher")
                    .arg("name", label(&row, "name"))
                    .arg("state", state)
                    .arg("publisher", label(&row, "publisher")),
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
            &dialog
                .locale
                .render(&UiMessage::new("tui.plugin_health").arg("health", label(&row, "health"))),
            width,
            theme::style(theme::Role::Faint),
        ));
    }
}

fn append_builtin(
    dialog: &PluginDialog,
    row: &Value,
    width: usize,
    lines: &mut Vec<Line<'static>>,
) {
    super::super::builtin::append_status(dialog, row, width, lines);
    let count = row["instances"].as_array().map_or(0, |rows| {
        rows.iter().filter(|row| row["phase"] == "active").count()
    });
    let parts = [
        localized_label(row, "summary", dialog.locale),
        dialog
            .locale
            .text(if count == 0 {
                "web.disabled"
            } else {
                "extension.active"
            })
            .into(),
        dialog
            .locale
            .render(&UiMessage::new("extension.active_instances").arg("count", count)),
        label(row, "id"),
    ];
    for part in parts
        .into_iter()
        .chain(builtin_protection_text(row, dialog.locale))
    {
        lines.extend(styled_lines(&part, width, theme::style(theme::Role::Dim)));
    }
}

fn builtin_protection_text(row: &Value, locale: LocaleContext) -> Vec<String> {
    let mut reasons = std::collections::BTreeMap::new();
    let mut protected = false;
    for instance in row["instances"].as_array().into_iter().flatten() {
        if instance["configurable"] != false {
            continue;
        }
        protected = true;
        for root in instance["required_by"].as_array().into_iter().flatten() {
            if let Some(id) = root["service_id"].as_str() {
                reasons
                    .entry(id.to_string())
                    .or_insert_with(|| localized_label(root, "reason", locale));
            }
        }
    }
    let heading = locale.text(if protected {
        "extension.required_contract"
    } else {
        "extension.static_configuration"
    });
    std::iter::once(heading.to_owned())
        .chain(reasons.into_values())
        .collect()
}
