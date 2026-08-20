use unicode_width::UnicodeWidthChar;

#[derive(Clone, Debug, Default)]
pub(crate) struct InputBuffer {
    text: String,
    cursor: usize,
    history: Vec<String>,
    history_index: Option<usize>,
    draft: String,
}

impl InputBuffer {
    pub fn new(history: Vec<String>) -> Self {
        Self {
            history,
            ..Self::default()
        }
    }

    pub fn clear(&mut self) {
        self.text.clear();
        self.cursor = 0;
        self.history_index = None;
        self.draft.clear();
    }

    pub fn take(&mut self) -> String {
        let value = std::mem::take(&mut self.text);
        self.cursor = 0;
        self.history_index = None;
        self.draft.clear();
        value
    }

    /// 只读视图（弹框预填/提交校验用；不改变光标与历史状态）。
    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn remember(&mut self, value: String) {
        if value.trim().is_empty() {
            return;
        }
        if self.history.last() != Some(&value) {
            self.history.push(value);
        }
        self.history_index = None;
        self.draft.clear();
    }

    pub fn insert_char(&mut self, ch: char) {
        self.leave_history();
        self.text.insert(self.cursor, ch);
        self.cursor += ch.len_utf8();
    }

    /// Inserts a line break (Shift+Enter), turning the input into a
    /// multi-line message.
    pub fn insert_newline(&mut self) {
        self.insert_char('\n');
    }

    pub fn insert_str(&mut self, value: &str) {
        self.leave_history();
        self.text.insert_str(self.cursor, value);
        self.cursor += value.len();
    }

    /// 召回的插话回填（ESC 栈式召回多次使用）：先发的想法靠前——
    /// 新召回的整行插到现有内容**之前**、换行分隔。召回顺序是 LIFO
    ///（steer3 先回），prepend 语义使多次召回后按发送顺序排列
    /// （steer1\nsteer2\nsteer3）；用户在召回后新敲的字自然排在最后。
    pub fn prepend_recalled_line(&mut self, text: &str) {
        let existing = self.take();
        if existing.is_empty() {
            self.insert_str(text);
        } else {
            self.insert_str(&format!("{text}\n{existing}"));
        }
    }

    pub fn backspace(&mut self) {
        self.leave_history();
        if self.cursor == 0 {
            return;
        }
        let previous = previous_boundary(&self.text, self.cursor);
        self.text.drain(previous..self.cursor);
        self.cursor = previous;
    }

    pub fn delete(&mut self) {
        self.leave_history();
        if self.cursor >= self.text.len() {
            return;
        }
        let next = next_boundary(&self.text, self.cursor);
        self.text.drain(self.cursor..next);
    }

    pub fn left(&mut self) {
        self.cursor = previous_boundary(&self.text, self.cursor);
    }

    pub fn right(&mut self) {
        self.cursor = next_boundary(&self.text, self.cursor);
    }

    pub fn home(&mut self) {
        self.cursor = 0;
    }

    pub fn end(&mut self) {
        self.cursor = self.text.len();
    }

    pub fn history_is_empty(&self) -> bool {
        self.history.is_empty()
    }

    pub fn history_previous(&mut self) {
        if self.history.is_empty() {
            return;
        }
        let next = match self.history_index {
            None => {
                self.draft = self.text.clone();
                self.history.len() - 1
            }
            Some(0) => 0,
            Some(index) => index - 1,
        };
        self.history_index = Some(next);
        self.text = self.history[next].clone();
        self.cursor = self.text.len();
    }

    pub fn history_next(&mut self) {
        let Some(index) = self.history_index else {
            return;
        };
        if index + 1 < self.history.len() {
            let next = index + 1;
            self.history_index = Some(next);
            self.text = self.history[next].clone();
        } else {
            self.history_index = None;
            self.text = std::mem::take(&mut self.draft);
        }
        self.cursor = self.text.len();
    }

    /// Number of visual rows the input occupies when wrapped at `width`,
    /// accounting for explicit line breaks. Always at least one.
    pub fn line_count(&self, width: usize) -> usize {
        let width = width.max(1);
        let mut rows = 1usize;
        let mut column = 0usize;
        for ch in self.text.chars() {
            let ch_width = UnicodeWidthChar::width(ch).unwrap_or(0);
            if ch == '\n' {
                rows += 1;
                column = 0;
            } else if column + ch_width > width {
                rows += 1;
                column = ch_width;
            } else {
                column += ch_width;
            }
        }
        rows
    }

    /// The (row, column) of the cursor within the wrapped input.
    pub fn cursor_position(&self, width: usize) -> (usize, usize) {
        let width = width.max(1);
        let mut row = 0usize;
        let mut column = 0usize;
        for (index, ch) in self.text.char_indices() {
            if index >= self.cursor {
                break;
            }
            let ch_width = UnicodeWidthChar::width(ch).unwrap_or(0);
            if ch == '\n' {
                row += 1;
                column = 0;
            } else if column + ch_width > width {
                row += 1;
                column = ch_width;
            } else {
                column += ch_width;
            }
        }
        (row, column)
    }

    /// 每个视觉行的文本，按宽度硬换行，与 `cursor_position` 使用同一
    /// 换行算法。渲染、光标定位与鼠标选区映射共用这一份布局。
    pub fn visual_rows(&self, width: usize) -> Vec<String> {
        let width = width.max(1);
        let mut rows = vec![String::new()];
        let mut column = 0usize;
        for ch in self.text.chars() {
            let ch_width = UnicodeWidthChar::width(ch).unwrap_or(0);
            if ch == '\n' {
                rows.push(String::new());
                column = 0;
            } else if column + ch_width > width {
                rows.push(String::new());
                rows.last_mut().expect("row").push(ch);
                column = ch_width;
            } else {
                rows.last_mut().expect("row").push(ch);
                column += ch_width;
            }
        }
        rows
    }

    /// 视觉坐标 (row, col) 处的字符字节边界：指向该字符的起始位置，
    /// 列超出行尾时落到下一行首字符（或文本末尾），因此可直接当作
    /// 选区边界使用。
    pub fn char_index_at(&self, width: usize, row: usize, col: usize) -> usize {
        let width = width.max(1);
        let mut current_row = 0usize;
        let mut column = 0usize;
        for (index, ch) in self.text.char_indices() {
            let ch_width = UnicodeWidthChar::width(ch).unwrap_or(0);
            // 触发换行的字符本身就是新行的第一个字符，先算出它的
            // 显示位置再比较，否则首字符永远匹配不到目标行。
            let display_row = if ch != '\n' && column + ch_width > width {
                current_row + 1
            } else {
                current_row
            };
            let display_col = if display_row > current_row { 0 } else { column };
            if display_row == row && display_col >= col {
                return index;
            }
            if ch == '\n' {
                current_row += 1;
                column = 0;
            } else if column + ch_width > width {
                current_row += 1;
                column = ch_width;
            } else {
                column += ch_width;
            }
        }
        self.text.len()
    }

    /// 光标移动到指定字节位置（单击输入框定位光标用）。
    pub fn set_cursor(&mut self, index: usize) {
        self.cursor = index.min(self.text.len());
    }

    /// 删除字节区间 [start, end) 的文本并把光标移到 start，返回被删
    /// 除的内容（剪切选区用）。
    pub fn remove_range(&mut self, start: usize, end: usize) -> String {
        self.leave_history();
        let removed = self.text.drain(start..end).collect();
        self.cursor = start;
        removed
    }

    fn leave_history(&mut self) {
        if self.history_index.is_some() {
            self.history_index = None;
            self.draft.clear();
        }
    }
}

fn previous_boundary(text: &str, cursor: usize) -> usize {
    if cursor == 0 {
        return 0;
    }
    text[..cursor]
        .char_indices()
        .next_back()
        .map(|(index, _)| index)
        .unwrap_or(0)
}

fn next_boundary(text: &str, cursor: usize) -> usize {
    if cursor >= text.len() {
        return text.len();
    }
    text[cursor..]
        .char_indices()
        .nth(1)
        .map(|(index, _)| cursor + index)
        .unwrap_or(text.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn supports_unicode_cursor_editing() {
        let mut input = InputBuffer::default();
        input.insert_str("你a好");
        input.left();
        input.backspace();
        assert_eq!(&input.text, "你好");
        input.home();
        input.delete();
        assert_eq!(&input.text, "好");
    }

    #[test]
    fn navigates_persisted_history_and_restores_draft() {
        let mut input = InputBuffer::new(vec!["one".into(), "two".into()]);
        input.insert_str("draft");
        input.history_previous();
        assert_eq!(&input.text, "two");
        input.history_previous();
        assert_eq!(&input.text, "one");
        input.history_next();
        assert_eq!(&input.text, "two");
        input.history_next();
        assert_eq!(&input.text, "draft");
    }

    /// ESC 栈式召回的回填序（2026-08-21 dogfood 修复）：召回是 LIFO
    ///（steer3 先回），回填按**发送顺序**排列且换行分隔——修复前是
    /// 追加在光标后且无分隔（"steer3steer2steer1" 一行）。用户在召回
    /// 后新敲的字排在最后（最新）。
    #[test]
    fn recalled_steering_lines_stack_in_send_order() {
        let mut input = InputBuffer::default();
        input.prepend_recalled_line("steer3");
        input.prepend_recalled_line("steer2");
        input.prepend_recalled_line("steer1");
        assert_eq!(&input.text, "steer1\nsteer2\nsteer3");
        // 召回后继续敲字：新内容在最后，不插队。
        input.end();
        input.insert_str(" plus more");
        assert_eq!(&input.text, "steer1\nsteer2\nsteer3 plus more");
        // 空输入的首次召回：原样填入、无多余换行。
        let mut fresh = InputBuffer::default();
        fresh.prepend_recalled_line("only");
        assert_eq!(&fresh.text, "only");
    }

    #[test]
    fn insert_newline_splits_lines_and_moves_cursor() {
        let mut input = InputBuffer::default();
        input.insert_str("ab");
        input.insert_newline();
        input.insert_str("cd");
        assert_eq!(&input.text, "ab\ncd");
        assert_eq!(input.line_count(10), 2);
        assert_eq!(input.cursor_position(10), (1, 2));
    }

    #[test]
    fn line_count_wraps_long_lines_and_counts_breaks() {
        let mut input = InputBuffer::default();
        input.insert_str("你好世界\nhi");
        assert_eq!(input.line_count(4), 3);
        assert_eq!(input.cursor_position(4), (2, 2));
    }

    #[test]
    fn visual_rows_match_the_cursor_wrap_algorithm() {
        let mut input = InputBuffer::default();
        input.insert_str("你好世界\nhi there");
        // 宽度 4：CJK 每行两个汉字，硬换行后从 "hi there" 继续按宽度断行。
        assert_eq!(
            input.visual_rows(4),
            vec![
                "你好".to_string(),
                "世界".to_string(),
                "hi t".to_string(),
                "here".to_string()
            ]
        );
        // 空输入仍然占一行
        assert_eq!(InputBuffer::default().visual_rows(4), vec![String::new()]);
    }

    #[test]
    fn char_index_at_maps_visual_positions_to_byte_boundaries() {
        let mut input = InputBuffer::default();
        input.insert_str("你好世界\nhi");
        // 行 1 列 0 → "世" 的字节起点（每个汉字 3 字节）
        assert_eq!(input.char_index_at(4, 1, 0), 6);
        // 行 1 列 2 → "界" 的起点（"世" 占据行 1 的列 0-2）
        assert_eq!(input.char_index_at(4, 1, 2), 9);
        // 列超过行尾 → 换行符位置
        assert_eq!(input.char_index_at(4, 1, 4), 12);
        // 超出末行 → 文本末尾
        assert_eq!(input.char_index_at(4, 9, 0), input.text.len());
    }

    #[test]
    fn remove_range_cuts_text_and_moves_cursor() {
        let mut input = InputBuffer::default();
        input.insert_str("hello world");
        let removed = input.remove_range(0, 5);
        assert_eq!(removed, "hello");
        assert_eq!(&input.text, " world");
        assert_eq!(input.cursor_position(20), (0, 0));
    }
}
