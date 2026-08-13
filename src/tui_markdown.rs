//! Minimal markdown rendering for the conversation panel.
//!
//! Supports the subset models actually emit in coding conversations:
//! fenced code blocks, inline code, bold and italic, headings,
//! blockquotes, bullet and ordered lists, links, and horizontal rules.
//! Anything unrecognized degrades to plain text instead of breaking the
//! render. No external markdown crate: the dependency budget stays lean
//! and the output is tuned for ratatui styling.

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

/// Background and foreground for fenced and inline code.
pub(crate) const CODE_BG: Color = Color::Rgb(36, 39, 46);
pub(crate) const CODE_FG: Color = Color::Rgb(185, 214, 232);

pub(crate) fn render_markdown(text: &str, width: usize) -> Vec<Line<'static>> {
    let width = width.max(1);
    let mut lines = Vec::new();
    let mut in_code = false;
    let mut code_lines: Vec<String> = Vec::new();

    for raw in text.split('\n') {
        if let Some(_fence) = raw.trim_start().strip_prefix("```") {
            if in_code {
                flush_code_block(&mut lines, &code_lines, width);
                code_lines.clear();
                in_code = false;
            } else {
                in_code = true;
            }
            continue;
        }
        if in_code {
            code_lines.push(raw.to_owned());
            continue;
        }
        render_block_line(raw, width, &mut lines);
    }
    if in_code {
        flush_code_block(&mut lines, &code_lines, width);
    }
    lines
}

/// Renders a fenced code block as a solid, softly highlighted rectangle:
/// each line is padded to the full width so the background reads as one
/// block. Lines longer than the width are left to the widget to clip
/// (code is never re-wrapped).
fn flush_code_block(lines: &mut Vec<Line<'static>>, code: &[String], width: usize) {
    let style = Style::default().fg(CODE_FG).bg(CODE_BG);
    for line in code {
        let used = UnicodeWidthStr::width(line.as_str());
        let padding = " ".repeat(width.saturating_sub(used));
        lines.push(Line::from(Span::styled(format!("{line}{padding}"), style)));
    }
}

fn render_block_line(raw: &str, width: usize, lines: &mut Vec<Line<'static>>) {
    let trimmed = raw.trim_end();
    if trimmed.trim().is_empty() {
        lines.push(Line::from(""));
        return;
    }
    if let Some(rest) = trimmed.strip_prefix("### ") {
        push_heading(lines, rest, width, 3);
        return;
    }
    if let Some(rest) = trimmed.strip_prefix("## ") {
        push_heading(lines, rest, width, 2);
        return;
    }
    if let Some(rest) = trimmed.strip_prefix("# ") {
        push_heading(lines, rest, width, 1);
        return;
    }
    if let Some(rest) = trimmed.strip_prefix("> ") {
        push_blockquote(lines, rest, width);
        return;
    }
    if trimmed == "---" || trimmed == "***" || trimmed == "___" {
        let rule = "─".repeat(width);
        lines.push(Line::from(Span::styled(
            rule,
            Style::default().fg(Color::DarkGray),
        )));
        return;
    }
    if let Some(rest) = trimmed
        .strip_prefix("- ")
        .or_else(|| trimmed.strip_prefix("* "))
        .or_else(|| trimmed.strip_prefix("+ "))
        .or_else(|| trimmed.strip_prefix("• "))
        .or_else(|| strip_ordered_prefix(trimmed))
    {
        push_list_item(lines, rest, width);
        return;
    }
    let segments = parse_inline(trimmed);
    lines.extend(wrap_styled(segments, width));
}

fn strip_ordered_prefix(text: &str) -> Option<&str> {
    let (number, rest) = text.split_once(". ")?;
    (!number.is_empty() && number.chars().all(|ch| ch.is_ascii_digit())).then_some(rest)
}

fn push_heading(lines: &mut Vec<Line<'static>>, text: &str, width: usize, level: u8) {
    let style = match level {
        1 => Style::default()
            .fg(Color::LightCyan)
            .add_modifier(Modifier::BOLD),
        _ => Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD),
    };
    let segments: Vec<(String, Style)> = parse_inline(text)
        .into_iter()
        .map(|(text, _)| (text, style))
        .collect();
    lines.extend(wrap_styled(segments, width));
}

fn push_blockquote(lines: &mut Vec<Line<'static>>, text: &str, width: usize) {
    let style = Style::default()
        .fg(Color::Gray)
        .add_modifier(Modifier::ITALIC);
    let bar = Span::styled("▎ ", Style::default().fg(Color::Cyan));
    let segments: Vec<(String, Style)> = parse_inline(text)
        .into_iter()
        .map(|(text, _)| (text, style))
        .collect();
    for line in wrap_styled(segments, width.saturating_sub(2)) {
        let mut spans = vec![bar.clone()];
        spans.extend(line.spans);
        lines.push(Line::from(spans));
    }
}

fn push_list_item(lines: &mut Vec<Line<'static>>, text: &str, width: usize) {
    let bullet = Span::styled("• ", Style::default().fg(Color::Cyan));
    let segments = parse_inline(text);
    for line in wrap_styled(segments, width.saturating_sub(2)) {
        let mut spans = vec![bullet.clone()];
        spans.extend(line.spans);
        lines.push(Line::from(spans));
    }
}

/// Splits a paragraph into styled segments: `` `code` ``, `**bold**`,
/// `*italic*`, `[label](url)`, and plain text. Unclosed or empty
/// constructs fall back to plain text so a stray `*` never eats the rest
/// of the message.
fn parse_inline(text: &str) -> Vec<(String, Style)> {
    let base = Style::default();
    let bold = base.add_modifier(Modifier::BOLD);
    let italic = base.add_modifier(Modifier::ITALIC);
    let code = Style::default().fg(CODE_FG).bg(CODE_BG);
    let link = Style::default()
        .fg(Color::LightBlue)
        .add_modifier(Modifier::UNDERLINED);

    let mut segments: Vec<(String, Style)> = Vec::new();
    let mut plain = String::new();
    let mut chars = text.chars().peekable();

    while let Some(ch) = chars.next() {
        match ch {
            '*' if chars.peek() == Some(&'*') => {
                chars.next(); // consume the second '*' of the opening pair
                let mut candidate = String::from("**");
                let mut closed = false;
                while let Some(c) = chars.next() {
                    candidate.push(c);
                    if c == '*' && chars.peek() == Some(&'*') {
                        candidate.push(chars.next().expect("peeked"));
                        closed = true;
                        break;
                    }
                }
                // Only slice when closed: the closing delimiter guarantees
                // the byte indices sit on character boundaries, even with
                // multi-byte CJK content inside.
                let inner = closed.then(|| &candidate[2..candidate.len() - 2]);
                if let Some(inner) = inner.filter(|inner| !inner.is_empty()) {
                    flush_plain(&mut plain, &mut segments, base);
                    segments.push((inner.to_owned(), bold));
                } else {
                    plain.push_str(&candidate);
                }
            }
            '*' => {
                let mut candidate = String::from('*');
                let mut closed = false;
                for c in chars.by_ref() {
                    candidate.push(c);
                    if c == '*' {
                        closed = true;
                        break;
                    }
                }
                let inner = closed.then(|| &candidate[1..candidate.len() - 1]);
                if let Some(inner) = inner.filter(|inner| !inner.is_empty()) {
                    flush_plain(&mut plain, &mut segments, base);
                    segments.push((inner.to_owned(), italic));
                } else {
                    plain.push_str(&candidate);
                }
            }
            '`' => {
                let mut candidate = String::from('`');
                let mut closed = false;
                for c in chars.by_ref() {
                    candidate.push(c);
                    if c == '`' {
                        closed = true;
                        break;
                    }
                }
                let inner = closed.then(|| &candidate[1..candidate.len() - 1]);
                if let Some(inner) = inner.filter(|inner| !inner.is_empty()) {
                    flush_plain(&mut plain, &mut segments, base);
                    segments.push((inner.to_owned(), code));
                } else {
                    plain.push_str(&candidate);
                }
            }
            '[' => {
                let mut candidate = String::from('[');
                let mut label_closed = false;
                for c in chars.by_ref() {
                    candidate.push(c);
                    if c == ']' {
                        label_closed = true;
                        break;
                    }
                }
                if label_closed && chars.peek() == Some(&'(') {
                    candidate.push(chars.next().expect("peeked"));
                    let mut link_closed = false;
                    for c in chars.by_ref() {
                        candidate.push(c);
                        if c == ')' {
                            link_closed = true;
                            break;
                        }
                    }
                    if link_closed {
                        let label = candidate[1..]
                            .split(']')
                            .next()
                            .unwrap_or_default()
                            .to_owned();
                        if !label.is_empty() {
                            flush_plain(&mut plain, &mut segments, base);
                            segments.push((label, link));
                            continue;
                        }
                    }
                }
                plain.push_str(&candidate);
            }
            other => plain.push(other),
        }
    }
    flush_plain(&mut plain, &mut segments, base);
    segments
}

fn flush_plain(plain: &mut String, segments: &mut Vec<(String, Style)>, style: Style) {
    if !plain.is_empty() {
        segments.push((std::mem::take(plain), style));
    }
}

/// Wraps styled segments to the target width, breaking at spaces when
/// possible and hard-breaking inside over-long words (or CJK runs).
/// Consecutive characters with the same style are merged into one span so
/// long messages stay cheap to render.
fn wrap_styled(segments: Vec<(String, Style)>, width: usize) -> Vec<Line<'static>> {
    let width = width.max(1);
    let mut lines: Vec<Line<'static>> = Vec::new();
    let mut current: Vec<Span<'static>> = Vec::new();
    let mut current_width = 0usize;
    let mut word: Vec<Span<'static>> = Vec::new();
    let mut word_width = 0usize;

    for (text, style) in &segments {
        for ch in text.chars() {
            let ch_width = UnicodeWidthChar::width(ch).unwrap_or(0);
            if ch == ' ' {
                if word_width > 0 {
                    if current_width > 0 && current_width + word_width > width {
                        lines.push(Line::from(std::mem::take(&mut current)));
                        current_width = 0;
                    }
                    current.append(&mut word);
                    merge_last_two(&mut current);
                    current_width += word_width;
                    word_width = 0;
                }
                if current_width > 0 {
                    push_char(&mut current, ' ', *style);
                    current_width += 1;
                }
                continue;
            }
            if word_width > 0 && word_width + ch_width > width {
                if current_width > 0 {
                    lines.push(Line::from(std::mem::take(&mut current)));
                    current_width = 0;
                }
                current.append(&mut word);
                merge_last_two(&mut current);
                current_width += word_width;
                word_width = 0;
            }
            push_char(&mut word, ch, *style);
            word_width += ch_width;
        }
    }
    if current_width > 0 && current_width + word_width > width {
        lines.push(Line::from(std::mem::take(&mut current)));
    }
    current.append(&mut word);
    merge_last_two(&mut current);
    if !current.is_empty() {
        lines.push(Line::from(current));
    }
    lines
}

/// Appends a character to the last span when its style matches, keeping
/// same-styled runs as a single span.
fn push_char(spans: &mut Vec<Span<'static>>, ch: char, style: Style) {
    if let Some(last) = spans.last_mut()
        && last.style == style
    {
        last.content.to_mut().push(ch);
        return;
    }
    spans.push(Span::styled(ch.to_string(), style));
}

/// Merges the last two spans when an append operation left two adjacent
/// spans with the same style.
fn merge_last_two(spans: &mut Vec<Span<'static>>) {
    let len = spans.len();
    if len >= 2 && spans[len - 2].style == spans[len - 1].style {
        let tail = spans.pop().expect("just checked");
        let head = spans.last_mut().expect("just checked");
        head.content.to_mut().push_str(&tail.content);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn styles_of(line: &Line<'static>) -> Vec<Style> {
        line.spans.iter().map(|span| span.style).collect()
    }

    #[test]
    fn renders_fenced_code_block_with_full_width_background() {
        let lines = render_markdown("```\nfn main() {}\n```", 20);
        assert_eq!(lines.len(), 1);
        let span = &lines[0].spans[0];
        assert_eq!(span.content, "fn main() {}        ");
        assert_eq!(span.style.bg, Some(CODE_BG));
    }

    #[test]
    fn parses_bold_inline_code_and_links() {
        let segments = parse_inline("use **bold** and `code` and [docs](https://x)");
        assert!(
            segments
                .iter()
                .any(|(text, style)| text == "bold" && style.add_modifier == Modifier::BOLD)
        );
        assert!(
            segments
                .iter()
                .any(|(text, style)| text == "code" && style.bg == Some(CODE_BG))
        );
        assert!(
            segments
                .iter()
                .any(|(text, style)| text == "docs" && style.add_modifier == Modifier::UNDERLINED)
        );
    }

    #[test]
    fn unclosed_constructs_degrade_to_plain_text() {
        let segments = parse_inline("an unclosed **bold");
        assert_eq!(
            segments,
            vec![("an unclosed **bold".to_owned(), Style::default())]
        );
    }

    #[test]
    fn unclosed_constructs_with_cjk_never_panic() {
        // Multi-byte content must not be sliced at byte offsets that land
        // inside a character.
        let segments = parse_inline("测试*文字");
        assert_eq!(segments.len(), 1);
        assert_eq!(segments[0].0, "测试*文字");

        let segments = parse_inline("测试**文字");
        assert_eq!(segments.len(), 1);
        assert_eq!(segments[0].0, "测试**文字");

        let segments = parse_inline("测试`文字");
        assert_eq!(segments.len(), 1);
        assert_eq!(segments[0].0, "测试`文字");

        let lines = render_markdown("**未闭合的加粗", 20);
        assert_eq!(lines.len(), 1);
    }

    #[test]
    fn closed_bold_with_cjk_parses_on_char_boundaries() {
        let segments = parse_inline("**测试**");
        assert!(
            segments
                .iter()
                .any(|(text, style)| text == "测试" && style.add_modifier.contains(Modifier::BOLD))
        );
    }

    #[test]
    fn wraps_styled_text_at_width() {
        let segments = vec![("aaaa bbbb".to_owned(), Style::default())];
        let lines = wrap_styled(segments, 5);
        assert_eq!(lines.len(), 2);
        assert_eq!(plain_text(&lines[0]), "aaaa ");
        assert_eq!(plain_text(&lines[1]), "bbbb");
    }

    #[test]
    fn hard_breaks_long_words_and_cjk() {
        let segments = vec![("你好世界".to_owned(), Style::default())];
        let lines = wrap_styled(segments, 4);
        assert_eq!(lines.len(), 2);
        assert_eq!(plain_text(&lines[0]), "你好");
        assert_eq!(plain_text(&lines[1]), "世界");
    }

    #[test]
    fn renders_headings_lists_and_blockquotes() {
        let text = "# Title\n\n- one\n- two\n\n> noted";
        let lines = render_markdown(text, 30);
        let joined: Vec<String> = lines.iter().map(plain_text).collect();
        assert!(joined.iter().any(|line| line == "Title"));
        assert!(joined.iter().any(|line| line == "• one"));
        assert!(joined.iter().any(|line| line == "▎ noted"));
        let title = lines
            .iter()
            .find(|line| plain_text(line) == "Title")
            .expect("title");
        assert!(styles_of(title)[0].add_modifier.contains(Modifier::BOLD));
    }

    fn plain_text(line: &Line<'static>) -> String {
        line.spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect()
    }
}
