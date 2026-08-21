//! Minimal markdown rendering for the conversation panel.
//!
//! Supports the subset models actually emit in coding conversations:
//! fenced code blocks, inline code, bold and italic, headings,
//! blockquotes, bullet and ordered lists, links, and horizontal rules.
//! Anything unrecognized degrades to plain text instead of breaking the
//! render. No external markdown crate: the dependency budget stays lean
//! and the output is tuned for ratatui styling.

use ratatui::style::Style;
use ratatui::text::{Line, Span};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::tui::theme;

pub(crate) fn render_markdown(text: &str, width: usize) -> Vec<Line<'static>> {
    let width = width.max(1);
    let mut lines = Vec::new();
    let mut in_code = false;
    let mut code_lines: Vec<String> = Vec::new();
    // 表格块：连续 `|` 开头的行；块结束时若第二行是分隔行则按表格
    // 渲染，否则退化为普通段落（GFM 宽松失败路径）。
    let mut table_lines: Vec<String> = Vec::new();

    for raw in text.split('\n') {
        if let Some(_fence) = raw.trim_start().strip_prefix("```") {
            flush_table(&mut lines, &std::mem::take(&mut table_lines), width);
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
        if raw.trim_start().starts_with('|') {
            table_lines.push(raw.trim().to_owned());
            continue;
        }
        flush_table(&mut lines, &std::mem::take(&mut table_lines), width);
        render_block_line(raw, width, &mut lines);
    }
    flush_table(&mut lines, &table_lines, width);
    if in_code {
        flush_code_block(&mut lines, &code_lines, width);
    }
    lines
}

/// 表格块出口：结构合法（首行表头、次行分隔行）→ 网格渲染；否则逐行
/// 走普通块渲染（降级不丢内容）。
fn flush_table(lines: &mut Vec<Line<'static>>, rows: &[String], width: usize) {
    if rows.is_empty() {
        return;
    }
    let Some(rendered) = render_table(rows, width) else {
        for row in rows {
            render_block_line(row, width, lines);
        }
        return;
    };
    lines.extend(rendered);
}

/// 把 `| a | b |` 拆成单元格（去掉首尾管道，`\|` 转义暂不支持）。
fn split_table_row(line: &str) -> Vec<String> {
    let mut body = line.trim();
    body = body.strip_prefix('|').unwrap_or(body);
    body = body.strip_suffix('|').unwrap_or(body);
    body.split('|').map(|cell| cell.trim().to_owned()).collect()
}

#[derive(Clone, Copy, PartialEq)]
enum ColumnAlign {
    Left,
    Center,
    Right,
}

fn parse_delimiter(cells: &[String]) -> Option<Vec<ColumnAlign>> {
    if cells.is_empty() {
        return None;
    }
    cells
        .iter()
        .map(|cell| {
            let left = cell.starts_with(':');
            let right = cell.ends_with(':');
            let dashes = &cell[usize::from(left)..cell.len() - usize::from(right)];
            if dashes.is_empty() || !dashes.chars().all(|ch| ch == '-') {
                return None;
            }
            Some(match (left, right) {
                (true, true) => ColumnAlign::Center,
                (false, true) => ColumnAlign::Right,
                _ => ColumnAlign::Left,
            })
        })
        .collect()
}

/// GFM 表格渲染：列宽 = 内容最大显示宽（表头参与）；总宽超预算时从
/// 最宽列逐列压缩（下限 1），单元格超宽截断加 `…`。表头加粗、分隔行
/// 退隐色；单元格内联样式（bold/code）保留。
fn render_table(rows: &[String], width: usize) -> Option<Vec<Line<'static>>> {
    if rows.len() < 2 {
        return None;
    }
    let header = split_table_row(&rows[0]);
    let aligns = parse_delimiter(&split_table_row(&rows[1]))?;
    let columns = header.len();
    if columns == 0 || aligns.len() != columns {
        return None;
    }
    let body: Vec<Vec<String>> = rows[2..]
        .iter()
        .map(|row| {
            let cells = split_table_row(row);
            let mut fixed = cells;
            fixed.resize(columns, String::new());
            fixed
        })
        .collect();

    // 列宽以**去标记后的纯文本**测量（渲染走 parse_inline，宽度口径
    // 必须一致，否则 `**bold**` 会让单元格错位）。
    let plain = |cell: &str| -> String {
        parse_inline(cell)
            .into_iter()
            .map(|(text, _)| text)
            .collect()
    };
    let header_plain: Vec<String> = header.iter().map(|cell| plain(cell)).collect();
    let body_plain: Vec<Vec<String>> = body
        .iter()
        .map(|row| row.iter().map(|cell| plain(cell)).collect())
        .collect();

    // 每列自然宽度 = 表头与全部单元格的最大显示宽度。
    let mut widths = vec![0usize; columns];
    for column in 0..columns {
        widths[column] = UnicodeWidthStr::width(header_plain[column].as_str());
        for row in &body_plain {
            widths[column] = widths[column].max(UnicodeWidthStr::width(row[column].as_str()));
        }
    }
    // 行布局 `| a | b |`：总宽 = Σ(w_i + 3) + 1（与分隔行逐字符一致）；
    // 超预算从最宽列压缩（下限 1）。
    let total = |widths: &[usize]| widths.iter().map(|w| w + 3).sum::<usize>() + 1;
    while total(&widths) > width {
        let widest = widths
            .iter()
            .enumerate()
            .max_by_key(|(index, w)| (*w, columns - *index))
            .map(|(index, _)| index)
            .unwrap_or(0);
        if widths[widest] <= 1 {
            break;
        }
        widths[widest] -= 1;
    }

    let row_line =
        |plain_cells: &[String], raw_cells: &[String], is_header: bool| -> Line<'static> {
            let mut spans = vec![Span::raw("| ")];
            for column in 0..columns {
                let w = widths[column];
                let plain_width = UnicodeWidthStr::width(plain_cells[column].as_str());
                let truncated = plain_width > w;
                let text = if truncated {
                    // 截断加省略号：留 1 列给 …，按列截断（截断单元格退化为
                    // 纯文本，内联样式不保留——仅溢出场景）。
                    let mut kept = String::new();
                    let mut used = 0usize;
                    for ch in plain_cells[column].chars() {
                        let cw = UnicodeWidthChar::width(ch).unwrap_or(0);
                        if used + cw > w.saturating_sub(1) {
                            break;
                        }
                        kept.push(ch);
                        used += cw;
                    }
                    format!("{kept}…")
                } else {
                    plain_cells[column].clone()
                };
                let text_width = UnicodeWidthStr::width(text.as_str());
                let pad = w.saturating_sub(text_width);
                let (left_pad, right_pad) = match aligns[column] {
                    ColumnAlign::Left => (0, pad),
                    ColumnAlign::Center => (pad / 2, pad - pad / 2),
                    ColumnAlign::Right => (pad, 0),
                };
                if left_pad > 0 {
                    spans.push(Span::raw(" ".repeat(left_pad)));
                }
                if is_header {
                    spans.push(Span::styled(text, theme::style(theme::Role::Bold)));
                } else if truncated {
                    spans.push(Span::raw(text));
                } else {
                    let inline = parse_inline(&raw_cells[column]);
                    spans.extend(
                        inline
                            .into_iter()
                            .map(|(text, style)| Span::styled(text, style)),
                    );
                }
                if right_pad > 0 {
                    spans.push(Span::raw(" ".repeat(right_pad)));
                }
                if column + 1 < columns {
                    spans.push(Span::raw(" | "));
                }
            }
            spans.push(Span::raw(" |"));
            Line::from(spans)
        };

    let mut out = vec![row_line(&header_plain, &header, true)];
    let separator: String = {
        let mut s = String::from("|");
        for w in &widths {
            s.push_str(&"-".repeat(w + 2));
            s.push('|');
        }
        s
    };
    out.push(Line::from(Span::styled(
        separator,
        theme::style(theme::Role::Faint),
    )));
    for (plain_row, raw_row) in body_plain.iter().zip(&body) {
        out.push(row_line(plain_row, raw_row, false));
    }
    Some(out)
}

/// Renders a fenced code block as a solid, softly highlighted rectangle:
/// each line is padded to the full width so the background reads as one
/// block. Lines longer than the width are left to the widget to clip
/// (code is never re-wrapped).
fn flush_code_block(lines: &mut Vec<Line<'static>>, code: &[String], width: usize) {
    let style = theme::style(theme::Role::Code);
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
            theme::style(theme::Role::Faint),
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
        1 => theme::style(theme::Role::HeadingPrimary),
        _ => theme::style(theme::Role::Heading),
    };
    let segments: Vec<(String, Style)> = parse_inline(text)
        .into_iter()
        .map(|(text, _)| (text, style))
        .collect();
    lines.extend(wrap_styled(segments, width));
}

fn push_blockquote(lines: &mut Vec<Line<'static>>, text: &str, width: usize) {
    let style = theme::style(theme::Role::QuoteText);
    let bar = Span::styled("▎ ", theme::style(theme::Role::QuoteBar));
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
    let bullet = Span::styled("• ", theme::style(theme::Role::ListBullet));
    let indent = Span::raw("  ");
    let segments = parse_inline(text);
    for (i, line) in wrap_styled(segments, width.saturating_sub(2))
        .into_iter()
        .enumerate()
    {
        // 折行续行用悬挂缩进对齐首行文本，不再重复 bullet——否则一个
        // 列表项在 CJK 折行后看起来像多个。
        let marker = if i == 0 {
            bullet.clone()
        } else {
            indent.clone()
        };
        let mut spans = vec![marker];
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
    let bold = theme::style(theme::Role::Bold);
    let italic = theme::style(theme::Role::Italic);
    let code = theme::style(theme::Role::Code);
    let link = theme::style(theme::Role::Link);

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
/// Characters that must never start a line (kinsoku 行首禁则): a break
/// never lands right before one of these — it stays glued to the preceding
/// character and moves down with it.
const FORBIDDEN_LINE_START: &[char] = &[
    '，', '。', '；', '：', '、', '！', '？', '）', '】', '」', '』', '》', '…', '·', '”', '’',
];

/// Characters that must never dangle at the end of a line (行尾禁则):
/// opening brackets/quotes stay glued to the character that follows them.
const FORBIDDEN_LINE_END: &[char] = &['（', '【', '「', '『', '《', '“', '‘'];

/// Break-opportunity test between two adjacent characters (a practical
/// subset of UAX #14): a break is allowed when either side is a full-width
/// character (CJK ideographs, kana, fullwidth punctuation). Between two
/// narrow characters there is no break — ASCII words, paths and URLs stay
/// atomic. Zero-width characters (combining marks, ZWJ, variation
/// selectors) never sit next to a break.
fn can_break_before(prev: char, ch: char) -> bool {
    let prev_width = UnicodeWidthChar::width(prev).unwrap_or(0);
    let ch_width = UnicodeWidthChar::width(ch).unwrap_or(0);
    if prev_width == 0 || ch_width == 0 {
        return false;
    }
    if FORBIDDEN_LINE_START.contains(&ch) || FORBIDDEN_LINE_END.contains(&prev) {
        return false;
    }
    prev_width > 1 || ch_width > 1
}

fn wrap_styled(segments: Vec<(String, Style)>, width: usize) -> Vec<Line<'static>> {
    let width = width.max(1);
    let mut lines: Vec<Line<'static>> = Vec::new();
    let mut current: Vec<Span<'static>> = Vec::new();
    let mut current_width = 0usize;
    let mut word: Vec<Span<'static>> = Vec::new();
    let mut word_width = 0usize;
    let mut word_last: Option<char> = None;

    fn flush_word(
        width: usize,
        lines: &mut Vec<Line<'static>>,
        current: &mut Vec<Span<'static>>,
        current_width: &mut usize,
        word: &mut Vec<Span<'static>>,
        word_width: &mut usize,
    ) {
        if *word_width == 0 {
            return;
        }
        if *current_width > 0 && *current_width + *word_width > width {
            lines.push(Line::from(std::mem::take(current)));
            *current_width = 0;
        }
        current.append(word);
        merge_last_two(current);
        *current_width += *word_width;
        *word_width = 0;
    }

    for (text, style) in &segments {
        for ch in text.chars() {
            let ch_width = UnicodeWidthChar::width(ch).unwrap_or(0);
            if ch == ' ' {
                flush_word(
                    width,
                    &mut lines,
                    &mut current,
                    &mut current_width,
                    &mut word,
                    &mut word_width,
                );
                word_last = None;
                if current_width > 0 {
                    push_char(&mut current, ' ', *style);
                    current_width += 1;
                }
                continue;
            }
            // A break opportunity (CJK boundary) ends the word here; the
            // width check inside flush_word turns it into a line break only
            // when the line is actually full.
            if word_width > 0 && word_last.is_some_and(|prev| can_break_before(prev, ch)) {
                flush_word(
                    width,
                    &mut lines,
                    &mut current,
                    &mut current_width,
                    &mut word,
                    &mut word_width,
                );
            }
            if word_width > 0 && word_width + ch_width > width {
                // The word alone exceeds the line (long URL / path): break
                // inside the word rather than letting the line overflow.
                if current_width > 0 {
                    lines.push(Line::from(std::mem::take(&mut current)));
                }
                current.append(&mut word);
                merge_last_two(&mut current);
                current_width = word_width;
                word_width = 0;
            }
            push_char(&mut word, ch, *style);
            word_width += ch_width;
            word_last = Some(ch);
        }
    }
    flush_word(
        width,
        &mut lines,
        &mut current,
        &mut current_width,
        &mut word,
        &mut word_width,
    );
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
    use ratatui::style::Modifier;

    fn code_bg() -> ratatui::style::Color {
        theme::style(theme::Role::Code).bg.unwrap_or_default()
    }

    fn styles_of(line: &Line<'static>) -> Vec<Style> {
        line.spans.iter().map(|span| span.style).collect()
    }

    #[test]
    fn renders_fenced_code_block_with_full_width_background() {
        let lines = render_markdown("```\nfn main() {}\n```", 20);
        assert_eq!(lines.len(), 1);
        let span = &lines[0].spans[0];
        assert_eq!(span.content, "fn main() {}        ");
        assert_eq!(
            span.style.bg,
            Some(theme::style(theme::Role::Code).bg.unwrap_or_default())
        );
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
                .any(|(text, style)| text == "code" && style.bg == Some(code_bg()))
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

    #[test]
    fn renders_gfm_tables_with_bold_header_and_faint_separator() {
        // 全左对齐、无内联标记：精确断言网格。
        let text = "| file | lines |\n| --- | --- |\n| a.rs | 12 |\n| b.rs | 345 |";
        let lines = render_markdown(text, 40);
        let plain: Vec<String> = lines
            .iter()
            .map(|l| l.spans.iter().map(|s| s.content.clone()).collect())
            .collect();
        assert_eq!(
            plain,
            vec![
                "| file | lines |",
                "|------|-------|",
                "| a.rs | 12    |",
                "| b.rs | 345   |",
            ]
        );
        assert!(
            lines[0]
                .spans
                .iter()
                .any(|s| s.content == "file" && s.style.add_modifier.contains(Modifier::BOLD))
        );
        assert!(
            lines[1]
                .spans
                .iter()
                .any(|s| s.content == "|------|-------|")
        );
    }

    #[test]
    fn table_alignment_and_inline_markup_survive() {
        // 居中对齐 + 单元格 bold：对齐对称、宽度口径去标记测量。
        let text = "| name | status |\n| --- | :---: |\n| alpha | ok |\n| beta | **retrying** |";
        let lines = render_markdown(text, 60);
        let plain: Vec<String> = lines
            .iter()
            .map(|l| l.spans.iter().map(|s| s.content.clone()).collect())
            .collect();
        // 列宽 = max(name=4/alpha=5, status=6/retrying=8) = {5, 8}。
        assert_eq!(plain[0], "| name  |  status  |");
        // 居中：ok 两侧留白。
        assert_eq!(plain[2], "| alpha |    ok    |");
        assert_eq!(plain[3], "| beta  | retrying |");
        // bold 分段仍在。
        assert!(
            lines[3]
                .spans
                .iter()
                .any(|s| s.content == "retrying" && s.style.add_modifier.contains(Modifier::BOLD))
        );
    }

    #[test]
    fn table_without_delimiter_row_degrades_to_paragraphs() {
        let text = "| not | a | table |\n| second | row | here |";
        let lines = render_markdown(text, 40);
        let joined: String = lines
            .iter()
            .flat_map(|l| l.spans.iter().map(|s| s.content.clone()))
            .collect();
        assert!(joined.contains("| not | a | table |"));
        assert!(!joined.contains("---|"));
    }

    #[test]
    fn wide_tables_shrink_columns_to_fit() {
        let text = "| a_very_long_header | another_long_column |\n| --- | --- |\n| value | v |";
        let lines = render_markdown(text, 20);
        for line in &lines {
            let width: usize = line.spans.iter().map(UnicodeWidthStr::width).sum();
            assert!(width <= 20, "row must fit: {width}");
        }
    }

    #[test]
    fn cjk_runs_fill_each_line_instead_of_jumping_wholesale() {
        // A space-free CJK run is one "word" to a space-only wrapper: it
        // jumps wholesale to the next line and the first line breaks early
        // at the last space (the `~/.clat/sessions/ | 下；` wrap the user
        // hit on 2026-08-19). CJK boundaries must be break opportunities so
        // the first line fills up to the width.
        let segments = vec![("hi 一二三四".to_owned(), Style::default())];
        let lines = wrap_styled(segments, 10);
        let plain: Vec<String> = lines.iter().map(plain_text).collect();
        assert_eq!(plain, vec!["hi 一二三".to_owned(), "四".to_owned()]);
    }

    #[test]
    fn cjk_punctuation_never_starts_a_line() {
        // 行首禁则：，。；等不能出现在行首，断行时与前一字符粘连下移。
        let segments = vec![("一二三，四".to_owned(), Style::default())];
        let lines = wrap_styled(segments, 6);
        let plain: Vec<String> = lines.iter().map(plain_text).collect();
        assert_eq!(plain, vec!["一二".to_owned(), "三，四".to_owned()]);
    }

    #[test]
    fn opening_bracket_never_ends_a_line() {
        // 行尾禁则：（不能悬在行尾，断行时与后一字符粘连。
        let segments = vec![("一（二三".to_owned(), Style::default())];
        let lines = wrap_styled(segments, 4);
        let plain: Vec<String> = lines.iter().map(plain_text).collect();
        assert_eq!(
            plain,
            vec!["一".to_owned(), "（二".to_owned(), "三".to_owned()]
        );
    }

    #[test]
    fn mixed_cjk_and_ascii_keeps_ascii_words_atomic() {
        // 中英混排无空格（在path下）：CJK 与 ASCII 之间是断点，ASCII 词
        // 内部不拆。旧实现按超宽词强制断行，把 path 拆成了 在p/at/h下。
        let segments = vec![("在path下".to_owned(), Style::default())];
        let lines = wrap_styled(segments, 5);
        let plain: Vec<String> = lines.iter().map(plain_text).collect();
        assert_eq!(
            plain,
            vec!["在".to_owned(), "path".to_owned(), "下".to_owned()]
        );
    }

    #[test]
    fn every_wrapped_line_stays_within_width() {
        // 回归守卫：无论断点怎么落，产出行的显示宽度不得超过 width。
        let segments = vec![(
            "状态：正在恢复会话与投影检查点，确保重启后转录完整。".to_owned(),
            Style::default(),
        )];
        let lines = wrap_styled(segments, 10);
        assert!(lines.len() >= 2);
        for line in &lines {
            assert!(
                UnicodeWidthStr::width(plain_text(line).as_str()) <= 10,
                "line overflows: {}",
                plain_text(line)
            );
        }
    }

    #[test]
    fn wrapped_list_continuations_use_hanging_indent_not_bullets() {
        // CJK 折行修复后列表项会跨多行：续行必须是悬挂缩进，不能再带
        // `• `——否则一个列表项渲染成 N 个。
        let text = "- 会话是 DSH 兼容的 append-only 日志：每段对话一个 zstd 分帧 JSONL 文件，在 ~/.clat/sessions/ 下；先写后做（第一条用户消息在调模型之前已落盘），中途崩溃恢复到上一个完整批次";
        let lines = render_markdown(text, 78);
        let plain: Vec<String> = lines.iter().map(plain_text).collect();
        assert!(plain.len() >= 3, "should wrap: {plain:?}");
        assert!(plain[0].starts_with("• "));
        for line in &plain[1..] {
            assert!(
                line.starts_with("  ") && !line.trim_start().starts_with('•'),
                "continuation must hang-indent, not repeat the bullet: {line}"
            );
        }
    }
}
