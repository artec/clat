use clat_core::client_ports::presentation::{LocaleContext, UiMessage};
use serde_json::Value;

pub(super) fn notice(locale: LocaleContext, status: &Value, stop_backend: bool) -> String {
    let Some(work) = work_lines(locale, status) else {
        let key = if stop_backend {
            "desktop.tray_stop_busy"
        } else {
            "desktop.quit_busy"
        };
        return locale.text(key).to_string();
    };
    let key = match (stop_backend, activity_running(status)) {
        (false, true) => "desktop.quit_detail_leave",
        (false, false) => "desktop.quit_detail_leave_held",
        (true, true) => "desktop.quit_detail_stop",
        (true, false) => "desktop.quit_detail_held",
    };
    locale.render(&UiMessage::new(key).arg("work", work))
}

fn activity_running(status: &Value) -> bool {
    status
        .get("activity")
        .and_then(Value::as_array)
        .is_some_and(|rows| {
            rows.iter().any(|row| {
                row.get("items")
                    .and_then(Value::as_array)
                    .is_some_and(|items| {
                        items
                            .iter()
                            .any(|item| item.get("running") != Some(&Value::Bool(false)))
                    })
            })
        })
}

fn work_lines(locale: LocaleContext, status: &Value) -> Option<String> {
    let rows = status.get("activity")?.as_array()?;
    let blocks: Vec<String> = rows
        .iter()
        .filter_map(|row| project_block(locale, row))
        .collect();
    if blocks.is_empty() {
        None
    } else {
        Some(blocks.join("\n\n"))
    }
}

fn project_block(locale: LocaleContext, row: &Value) -> Option<String> {
    let items = row.get("items")?.as_array()?;
    if items.is_empty() {
        return None;
    }
    let mut lines = Vec::new();
    if let Some(project) = row
        .get("project")
        .and_then(Value::as_str)
        .filter(|name| !name.is_empty())
    {
        lines.push(
            locale.render(&UiMessage::new("desktop.activity.project").arg("project", project)),
        );
    }
    let limit = 8;
    for item in items.iter().take(limit) {
        lines.push(locale.render(
            &UiMessage::new("desktop.activity.bullet").arg("item", item_line(locale, item)),
        ));
    }
    let extra = items.len().saturating_sub(limit);
    if extra > 0 {
        let more =
            locale.render(&UiMessage::new("desktop.activity.more").arg("count", extra as u64));
        lines.push(locale.render(&UiMessage::new("desktop.activity.bullet").arg("item", more)));
    }
    Some(lines.join("\n"))
}

fn item_line(locale: LocaleContext, item: &Value) -> String {
    let kind = item.get("kind").and_then(Value::as_str).unwrap_or("");
    let name = opaque_name(item.get("name").and_then(Value::as_str).unwrap_or(""));
    let key = match (kind, name.is_empty()) {
        ("terminal", false) => "desktop.activity.terminal_named",
        ("terminal", true) => "desktop.activity.terminal",
        ("job", false) => "desktop.activity.job_named",
        ("job", true) => "desktop.activity.job",
        ("conversation", _) => "desktop.activity.conversation",
        ("automatic", _) => "desktop.activity.automatic",
        ("compaction", _) => "desktop.activity.compaction",
        ("upload", _) => "desktop.activity.upload",
        ("download", _) => "desktop.activity.download",
        ("git", _) => "desktop.activity.git",
        ("files", _) => "desktop.activity.files",
        ("schedule", _) => "desktop.activity.schedule",
        ("agent", _) => "desktop.activity.agent",
        ("approval", _) => "desktop.activity.approval",
        ("question", _) => "desktop.activity.question",
        ("steering", _) => "desktop.activity.steering",
        ("binding", _) => "desktop.activity.binding",
        ("draft", _) => "desktop.activity.draft",
        ("queue", _) => "desktop.activity.queue",
        ("input", _) => "desktop.activity.input",
        _ => "desktop.activity.unknown",
    };
    if key.ends_with("_named") {
        locale.render(&UiMessage::new(key).arg("name", name))
    } else {
        locale.text(key).to_string()
    }
}

fn opaque_name(value: &str) -> String {
    let collapsed = value.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut chars = collapsed.chars();
    let short: String = chars.by_ref().take(80).collect();
    if chars.next().is_some() {
        format!("{short}…")
    } else {
        short
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clat_core::client_ports::presentation::Locale;
    use serde_json::json;

    #[test]
    fn quit_notice_names_the_terminal_and_separates_leaving_from_stopping() {
        let locale = LocaleContext::new(Locale::English);
        let status = json!({"activity":[{"project":"WXRD","items":[{"kind":"terminal","name":"zsh","running":true}]}]});
        let work = work_lines(locale, &status).expect("named work");
        assert!(work.contains("WXRD") && work.contains("zsh"), "{work}");
        assert_eq!(
            notice(locale, &status, false),
            locale.render(&UiMessage::new("desktop.quit_detail_leave").arg("work", work.clone()))
        );
        assert_eq!(
            notice(locale, &status, true),
            locale.render(&UiMessage::new("desktop.quit_detail_stop").arg("work", work))
        );
        let zh = LocaleContext::new(Locale::Simplified);
        let chinese = notice(zh, &status, false);
        assert!(
            chinese.contains("WXRD") && chinese.contains("zsh"),
            "{chinese}"
        );
        assert_eq!(
            chinese,
            zh.render(
                &UiMessage::new("desktop.quit_detail_leave")
                    .arg("work", work_lines(zh, &status).unwrap())
            )
        );
    }

    #[test]
    fn a_draft_is_not_described_as_running_work() {
        let locale = LocaleContext::new(Locale::English);
        let status = json!({"activity":[{"project":"WXRD","items":[{"kind":"draft","name":"","running":false}]}]});
        let work = work_lines(locale, &status).unwrap();
        assert!(work.contains("WXRD"));
        assert!(!work.contains("zsh"));
        assert_eq!(
            notice(locale, &status, false),
            locale.render(
                &UiMessage::new("desktop.quit_detail_leave_held").arg("work", work.clone())
            )
        );
        assert_eq!(
            notice(locale, &status, true),
            locale.render(&UiMessage::new("desktop.quit_detail_held").arg("work", work))
        );
    }

    #[test]
    fn missing_activity_keeps_the_short_confirmation() {
        let locale = LocaleContext::new(Locale::English);
        let status = json!({"ready":false});
        assert_eq!(
            notice(locale, &status, false),
            locale.text("desktop.quit_busy")
        );
        assert_eq!(
            notice(locale, &status, true),
            locale.text("desktop.tray_stop_busy")
        );
    }
}
