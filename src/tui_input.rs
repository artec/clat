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

    pub fn text(&self) -> &str {
        &self.text
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
        assert_eq!(input.text(), "你好");
        input.home();
        input.delete();
        assert_eq!(input.text(), "好");
    }

    #[test]
    fn navigates_persisted_history_and_restores_draft() {
        let mut input = InputBuffer::new(vec!["one".into(), "two".into()]);
        input.insert_str("draft");
        input.history_previous();
        assert_eq!(input.text(), "two");
        input.history_previous();
        assert_eq!(input.text(), "one");
        input.history_next();
        assert_eq!(input.text(), "two");
        input.history_next();
        assert_eq!(input.text(), "draft");
    }

    #[test]
    fn insert_newline_splits_lines_and_moves_cursor() {
        let mut input = InputBuffer::default();
        input.insert_str("ab");
        input.insert_newline();
        input.insert_str("cd");
        assert_eq!(input.text(), "ab\ncd");
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
}
