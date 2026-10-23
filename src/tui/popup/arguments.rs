//! Permission review and transcript previews share one layout and safety path.
use super::*;
use serde_json::Value;

#[cfg(test)]
pub(crate) fn tool_argument_lines(
    tool: &str,
    arguments: &serde_json::Value,
    width: usize,
) -> Option<Vec<Line<'static>>> {
    tool_argument_lines_localized(tool, arguments, width, LocaleContext::default())
}
pub(crate) fn tool_argument_lines_localized(
    tool: &str,
    arguments: &serde_json::Value,
    width: usize,
    locale: LocaleContext,
) -> Option<Vec<Line<'static>>> {
    let object = arguments.as_object()?;
    match tool {
        "edit_file" => edit_preview(object, width, locale),
        "write_file" => write_preview(object, width, locale),
        "run_command" => command_preview(object, width, locale),
        _ => None,
    }
}
fn visible(text: &str) -> String {
    text.chars()
        .map(|ch| match ch {
            '\t' => "    ".to_owned(),
            '\n' => ch.to_string(),
            '\x00'..='\x1f' => format!("^{}", (b'@' + ch as u8) as char),
            '\x7f' => "^?".to_owned(),
            _ => ch.to_string(),
        })
        .collect()
}
fn push_header(lines: &mut Vec<Line<'static>>, title: String, width: usize) {
    for logical in visible(&title).split('\n') {
        for wrapped in wrap_text(logical, width.saturating_sub(2)) {
            lines.push(Line::from(Span::styled(
                format!("  {wrapped}"),
                theme::style(theme::Role::Bold),
            )));
        }
    }
}
fn push_wrapped(lines: &mut Vec<Line<'static>>, prefix: &str, text: &str, width: usize) {
    for logical in visible(text).split('\n') {
        for wrapped in wrap_text(logical, width.saturating_sub(prefix.len() + 2)) {
            lines.push(Line::from(format!("{prefix} {wrapped}")));
        }
    }
}
fn edit_preview(
    object: &serde_json::Map<String, Value>,
    width: usize,
    locale: LocaleContext,
) -> Option<Vec<Line<'static>>> {
    let path = object.get("path")?.as_str()?;
    let old = object.get("old_str")?.as_str()?;
    let new = object.get("new_str").and_then(Value::as_str).unwrap_or("");
    let mut lines = Vec::new();
    push_header(
        &mut lines,
        locale.render(&UiMessage::new("tui.preview_edit").arg("path", path)),
        width,
    );
    lines.push(Line::from(Span::styled(
        locale.text("tui.preview_old"),
        theme::style(theme::Role::Dim),
    )));
    push_wrapped(&mut lines, "-", old, width);
    lines.push(Line::from(Span::styled(
        locale.text("tui.preview_new"),
        theme::style(theme::Role::Dim),
    )));
    push_wrapped(&mut lines, "+", new, width);
    Some(lines)
}
fn write_preview(
    object: &serde_json::Map<String, Value>,
    width: usize,
    locale: LocaleContext,
) -> Option<Vec<Line<'static>>> {
    let path = object.get("path")?.as_str()?;
    let content = object.get("content")?.as_str()?;
    let mut lines = Vec::new();
    push_header(
        &mut lines,
        locale.render(
            &UiMessage::new("tui.preview_write")
                .arg("path", path)
                .arg("bytes", content.len()),
        ),
        width,
    );
    for logical in visible(content).split('\n') {
        for wrapped in wrap_text(logical, width.saturating_sub(2)) {
            lines.push(Line::from(format!("  {wrapped}")));
        }
    }
    Some(lines)
}
fn command_preview(
    object: &serde_json::Map<String, Value>,
    width: usize,
    locale: LocaleContext,
) -> Option<Vec<Line<'static>>> {
    let command = object.get("command")?.as_str()?;
    let timeout = object
        .get("timeout_seconds")
        .and_then(Value::as_u64)
        .unwrap_or(120);
    let network = object
        .get("network")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let sandbox = object
        .get("sandbox")
        .and_then(Value::as_str)
        .unwrap_or("auto");
    let mut lines = Vec::new();
    push_header(&mut lines, format!("$ {command}"), width);
    let metadata = locale.render(
        &UiMessage::new("tui.preview_command")
            .arg("timeout", timeout)
            .arg("sandbox", sandbox)
            .arg(
                "network",
                locale.text(if network {
                    "tui.network_requested"
                } else {
                    "tui.network_blocked"
                }),
            ),
    );
    for wrapped in wrap_text(&metadata, width.saturating_sub(2)) {
        lines.push(Line::from(Span::styled(
            format!("  {wrapped}"),
            theme::style(theme::Role::Dim),
        )));
    }
    Some(lines)
}
