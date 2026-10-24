//! Pure rendering of retained history values. No backend or session mutation.
use crate::i18n::LocaleContext;
use serde_json::Value;
pub(super) fn display(history: &Value, locale: LocaleContext) -> String {
    let mut parts = vec![locale.text("history.title").to_owned()];
    let kind = history["kind"].as_str().unwrap_or("");
    if kind == "read" {
        parts.push(history["data"]["json"].as_str().unwrap_or("").into());
        if history["data"]["next_offset"].is_u64() {
            parts.push(locale.text("history.bounded").into());
        }
    } else {
        let rows = history["data"][if kind == "search" { "hits" } else { "events" }].as_array();
        let selected = history["selected"].as_u64().unwrap_or(0) as usize;
        if let Some(rows) = rows {
            for (index, row) in rows.iter().enumerate() {
                let marker = if index == selected { "›" } else { " " };
                let title = row["title"]
                    .as_str()
                    .or(row["event_type"].as_str())
                    .unwrap_or("");
                let text = row["excerpt"]
                    .as_str()
                    .or(row["preview"].as_str())
                    .unwrap_or("");
                parts.push(format!(
                    "{marker} {} · {title}\n{} · {}\n{text}",
                    index + 1,
                    row["reference"]["session"].as_str().unwrap_or(""),
                    row["reference"]["seq"]
                ));
            }
            if rows.is_empty() {
                parts.push(locale.text("history.empty").into());
            }
        }
        parts.push(locale.text("history.selection_help").into());
        if history["data"]["truncated"] == true || history["data"]["next_cursor"].is_string() {
            parts.push(locale.text("history.bounded").into());
        }
    }
    parts.join("\n\n")
}
