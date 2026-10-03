//! Current conversation body search. Composer text and history are untouched.
use super::*;
use conversation::BodyMatch;

#[derive(Default)]
pub(super) struct ConversationFind {
    query: InputBuffer,
    active: Option<(u64, usize)>,
}

impl App {
    pub(super) fn sync_find_anchor(&mut self) {
        if self.discovery.find.is_none() {
            return;
        }
        let matches = self.body_matches();
        let active = self.discovery.find.as_ref().and_then(|f| f.active);
        let hit = matches
            .iter()
            .find(|m| Some((m.identity, m.offset)) == active)
            .or(matches.first());
        if let Some(hit) = hit {
            self.discovery.find.as_mut().unwrap().active = Some((hit.identity, hit.offset));
            let total = self.conversation.total_lines(self.card_visibility);
            self.conversation_scroll_from_bottom =
                total.saturating_sub(self.conversation_visible_rows() + hit.row);
        }
    }

    pub(super) fn highlight_find_match(&mut self, lines: &mut [Line<'static>], start: usize) {
        let active = self.discovery.find.as_ref().and_then(|f| f.active);
        if active.is_none() {
            return;
        }
        if let Some(hit) = self
            .body_matches()
            .iter()
            .find(|m| Some((m.identity, m.offset)) == active)
            && let Some(line) = hit.row.checked_sub(start).and_then(|i| lines.get_mut(i))
        {
            *line = highlight_line(line, 0, usize::MAX);
        }
    }

    pub(super) fn find_cursor(&self, frame: &mut Frame) {
        if let Some(find) = &self.discovery.find {
            let column = 7 + unicode_width::UnicodeWidthStr::width(find.query.text());
            frame.set_cursor_position((
                self.input_area.x + (column as u16).min(self.input_area.width.saturating_sub(2)),
                self.input_area.y.saturating_sub(4),
            ));
        }
    }

    pub(super) fn paste_conversation_find(&mut self, text: &str) -> bool {
        let Some(find) = &mut self.discovery.find else {
            return false;
        };
        find.query.insert_str(
            &text
                .chars()
                .filter(|c| !c.is_control())
                .take(256usize.saturating_sub(find.query.text().chars().count()))
                .collect::<String>(),
        );
        find.active = None;
        self.move_find_match(0);
        true
    }

    pub(super) fn handle_conversation_find_key(&mut self, key: KeyEvent) -> bool {
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('f') {
            self.discovery.picker = None;
            self.discovery.find = Some(ConversationFind::default());
            return true;
        }
        let Some(find) = self.discovery.find.as_mut() else {
            return false;
        };
        match key.code {
            KeyCode::Esc => {
                self.discovery.find = None;
            }
            KeyCode::Enter => {
                self.move_find_match(if key.modifiers.contains(KeyModifiers::SHIFT) {
                    -1
                } else {
                    1
                })
            }
            KeyCode::Char('l') if key.modifiers.contains(KeyModifiers::ALT) => {
                // One bounded page. Esc cancels the search, not an already-started read.
                self.conversation_scroll_from_bottom = self.conversation_rows;
                self.maybe_load_older_conversation();
            }
            KeyCode::Backspace => {
                find.query.backspace();
                find.active = None;
                self.move_find_match(0);
            }
            KeyCode::Char(ch)
                if !key
                    .modifiers
                    .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
            {
                if find.query.text().chars().count() < 256 {
                    find.query.insert_char(ch);
                }
                find.active = None;
                self.move_find_match(0);
            }
            _ => {}
        }
        true
    }

    fn body_matches(&mut self) -> Vec<BodyMatch> {
        let query = self
            .discovery
            .find
            .as_ref()
            .map(|f| f.query.text())
            .unwrap_or("")
            .to_owned();
        let width = conversation_wrap_width(self.conversation_area);
        self.conversation
            .find_body(&query, width, self.card_visibility)
    }

    fn move_find_match(&mut self, direction: isize) {
        let matches = self.body_matches();
        if matches.is_empty() {
            return;
        }
        let active = self.discovery.find.as_ref().and_then(|f| f.active);
        let index = matches
            .iter()
            .position(|m| Some((m.identity, m.offset)) == active)
            .unwrap_or(0);
        let index = (index as isize + direction).rem_euclid(matches.len() as isize) as usize;
        let hit = &matches[index];
        self.discovery.find.as_mut().unwrap().active = Some((hit.identity, hit.offset));
        let total = self.conversation.total_lines(self.card_visibility);
        self.conversation_scroll_from_bottom =
            total.saturating_sub(self.conversation_visible_rows() + hit.row);
    }

    pub(super) fn draw_conversation_find(&mut self, frame: &mut Frame) {
        if self.discovery.find.is_none() {
            return;
        }
        let matches = self.body_matches();
        let find = self.discovery.find.as_ref().unwrap();
        let index = matches
            .iter()
            .position(|m| Some((m.identity, m.offset)) == find.active);
        let scope = if self.conversation_has_more {
            "loaded window only · Alt+L earlier page"
        } else {
            "all messages loaded"
        };
        let preview = index
            .map(|i| matches[i].preview.as_str())
            .unwrap_or("User / agent original body only");
        let area = Rect::new(
            self.input_area.x,
            self.input_area.y.saturating_sub(5),
            self.input_area.width,
            5.min(self.input_area.y),
        );
        clear_popup_with_guards(frame, area);
        frame.render_widget(
            Paragraph::new(vec![
                Line::from(format!(
                    "Find: {} · {}/{}",
                    find.query.text(),
                    index.map_or(0, |i| i + 1),
                    matches.len()
                )),
                Line::from(format!(
                    "{scope} · Enter next · Shift+Enter previous · Esc cancel"
                )),
                Line::from(preview.to_owned()),
            ])
            .block(popup_block("Find in conversation")),
            area,
        );
    }
}
