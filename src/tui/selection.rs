use super::*;

/// 鼠标选区所在的组件。
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum SelectionKind {
    Conversation,
    Input,
}

/// 内容坐标系中的位置：第几行、第几列（均为从 0 开始）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(super) struct SelectionPos {
    pub(super) row: usize,
    pub(super) col: usize,
}

/// 鼠标拖拽选区。anchor 是按下位置，head 随拖动更新；两者按内容坐标
/// 排序即得到选区范围，因此滚动或内容增长后依然指向原文本行。
#[derive(Clone, Copy)]
pub(super) struct TextSelection {
    pub(super) kind: SelectionKind,
    pub(super) anchor: SelectionPos,
    pub(super) head: SelectionPos,
    /// 鼠标按键是否仍按住；松开后保留选区供 Cmd+C 复用。
    pub(super) active: bool,
}

impl TextSelection {
    pub(super) fn ordered(&self) -> (SelectionPos, SelectionPos) {
        if self.anchor <= self.head {
            (self.anchor, self.head)
        } else {
            (self.head, self.anchor)
        }
    }

    pub(super) fn is_empty(&self) -> bool {
        self.anchor == self.head
    }
}

/// 组件边框内的内容区域。
fn content_rect(area: Rect) -> Rect {
    Rect::new(
        area.x + 1,
        area.y + 1,
        area.width.saturating_sub(2),
        area.height.saturating_sub(2),
    )
}

/// 会话文本的折行宽度：inner（边框内 `width - 2`）再为右侧滚动条列
/// 预留 1 列——行尾宽字符（CJK/emoji 占 2 列）的字形不得铺进滚动条
/// 列（ratatui diff 的 `to_skip` 会吞掉被字形覆盖的滚动条符号补发，
/// 用户实测：纯 ASCII 行不遮挡，被遮挡的行必含非英文字符）。渲染与
/// 选区复制必须共用本函数——复制旁路宽度会让拷出的行与显示的折行
/// 错位（2026-08-19 审计发现的回归）。
pub(super) fn conversation_wrap_width(area: Rect) -> usize {
    area.width.saturating_sub(3).max(1) as usize
}

/// 屏幕坐标 → 内容坐标。指针必须落在内容区内，否则返回 None。
pub(super) fn content_pos(area: Rect, x: u16, y: u16) -> Option<SelectionPos> {
    let inner = content_rect(area);
    if x < inner.x || x >= inner.x + inner.width || y < inner.y || y >= inner.y + inner.height {
        return None;
    }
    Some(SelectionPos {
        row: (y - inner.y) as usize,
        col: (x - inner.x) as usize,
    })
}

/// 屏幕坐标 → 内容坐标（越界时钳制在内容区内，拖动出界时使用）。
pub(super) fn clamped_pos(area: Rect, rows: usize, x: u16, y: u16) -> SelectionPos {
    let inner = content_rect(area);
    SelectionPos {
        row: (y.saturating_sub(inner.y) as usize).min(rows.saturating_sub(1)),
        col: (x.saturating_sub(inner.x) as usize).min(inner.width as usize),
    }
}

/// 将一行中列区间 [from, to) 内的字符加反显（REVERSED）样式，其余
/// 保持原样。span 可能被选区从中切开，按字符切成连续的选/未选片段。
pub(super) fn highlight_line(line: &Line<'static>, from: usize, to: usize) -> Line<'static> {
    let mut spans: Vec<Span<'static>> = Vec::new();
    let mut column = 0usize;
    for span in &line.spans {
        let style = span.style;
        // (是否选中, 文本) 的连续片段
        let mut runs: Vec<(bool, String)> = Vec::new();
        for ch in span.content.chars() {
            let width = UnicodeWidthChar::width(ch).unwrap_or(0);
            let selected = column + width > from && column < to;
            match runs.last_mut() {
                Some((is_selected, buffer)) if *is_selected == selected => buffer.push(ch),
                _ => runs.push((selected, ch.to_string())),
            }
            column += width;
        }
        for (selected, text) in runs {
            let style = if selected {
                style.add_modifier(Modifier::REVERSED)
            } else {
                style
            };
            spans.push(Span::styled(text, style));
        }
    }
    Line::from(spans)
}

/// 按显示列区间截取纯文本：与 [from, to) 有重叠的字符整字入选区，
/// 因此宽字符（CJK）从中间被点到时不会被切成半个。
pub(super) fn slice_by_columns(text: &str, from: usize, to: usize) -> String {
    let mut out = String::new();
    let mut column = 0usize;
    for ch in text.chars() {
        let width = UnicodeWidthChar::width(ch).unwrap_or(0);
        if column + width > from && column < to {
            out.push(ch);
        }
        column += width;
    }
    out
}

/// base64 编码（OSC 52 剪贴板写入需要）。项目不引第三方 base64 依赖，
/// 这十几行足够覆盖该场景。
pub(super) fn base64_encode(data: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let word = (chunk[0] as u32) << 16
            | (*chunk.get(1).unwrap_or(&0) as u32) << 8
            | (*chunk.get(2).unwrap_or(&0) as u32);
        out.push(TABLE[(word >> 18 & 63) as usize] as char);
        out.push(TABLE[(word >> 12 & 63) as usize] as char);
        out.push(if chunk.len() > 1 {
            TABLE[(word >> 6 & 63) as usize] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            TABLE[(word & 63) as usize] as char
        } else {
            '='
        });
    }
    out
}

/// 通过 OSC 52 把文本写入系统剪贴板。iTerm2 / WezTerm / kitty /
/// VS Code 等终端支持；不支持的终端（如 macOS Terminal.app）会静默
/// 忽略，用户仍可按住 Shift 用终端原生方式选择复制。
pub(super) fn copy_to_clipboard(text: &str) -> bool {
    if text.is_empty() {
        return false;
    }
    let mut out = stdout();
    write!(out, "\x1b]52;c;{}\x1b\\", base64_encode(text.as_bytes()))
        .and_then(|_| out.flush())
        .is_ok()
}
