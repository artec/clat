use super::*;

impl App {
    pub(in crate::tui) fn plugin_key(&mut self, key: KeyEvent) -> bool {
        let Some(dialog) = self.plugins.dialog.as_mut() else {
            return false;
        };
        dialog.expire();
        if key.code == KeyCode::Esc {
            if dialog.view == View::Config {
                dialog.config.clear();
                dialog.view = View::Review;
            } else if matches!(dialog.view, View::Review | View::Remove) {
                dialog.clear_review();
                dialog.remove = None;
                dialog.view = View::Installed;
            } else {
                self.close_plugin_dialog();
            }
            return true;
        }
        if dialog.pending {
            return true;
        }
        if dialog.view == View::Config || dialog.searching {
            if dialog.view == View::Config
                && key.code == KeyCode::Enter
                && key.modifiers == KeyModifiers::SHIFT
            {
                bounded_insert(&mut dialog.config, "\n");
            } else if key.code == KeyCode::Enter {
                dialog.searching = false;
                if dialog.view == View::Config {
                    dialog.view = View::Review;
                }
            } else {
                edit(
                    if dialog.view == View::Config {
                        &mut dialog.config
                    } else {
                        &mut dialog.query
                    },
                    key,
                );
            }
            dialog.selected = 0;
            return true;
        }
        if dialog.view == View::Review {
            self.plugin_review_key(key);
            return true;
        }
        if dialog.view == View::Remove {
            if key.code == KeyCode::Char('y') && key.modifiers.is_empty() {
                self.remove_plugin();
            }
            return true;
        }
        self.plugin_browse_key(key);
        true
    }
    pub(super) fn plugin_review_key(&mut self, key: KeyEvent) {
        let dialog = self.plugins.dialog.as_mut().unwrap();
        match key.code {
            KeyCode::Down => {
                dialog.scroll = (dialog.scroll + 1).min(dialog.total.saturating_sub(dialog.page))
            }
            KeyCode::Up => dialog.scroll = dialog.scroll.saturating_sub(1),
            KeyCode::PageDown => {
                dialog.scroll =
                    (dialog.scroll + dialog.page).min(dialog.total.saturating_sub(dialog.page))
            }
            KeyCode::PageUp => dialog.scroll = dialog.scroll.saturating_sub(dialog.page),
            KeyCode::Char(' ')
                if key.modifiers.is_empty()
                    && dialog.reviewed >= dialog.total
                    && dialog.total > 0 =>
            {
                dialog.accepted = !dialog.accepted
            }
            KeyCode::Char('c')
                if key.modifiers.is_empty()
                    && dialog.review.as_ref().is_some_and(|ticket| {
                        matches!(
                            ticket.review["action"].as_str(),
                            Some("install" | "update" | "configure")
                        )
                    }) =>
            {
                dialog.accepted = false;
                dialog.view = View::Config;
            }
            KeyCode::Enter if key.modifiers.is_empty() => self.commit_plugin_review(),
            _ => {}
        }
    }
    pub(super) fn plugin_browse_key(&mut self, key: KeyEvent) {
        let dialog = self.plugins.dialog.as_mut().unwrap();
        let count = dialog.rows().len();
        match key.code {
            KeyCode::Down => dialog.selected = (dialog.selected + 1).min(count.saturating_sub(1)),
            KeyCode::Up => dialog.selected = dialog.selected.saturating_sub(1),
            KeyCode::Char('/') => dialog.searching = true,
            KeyCode::Tab => {
                dialog.view = if dialog.view == View::Installed {
                    View::Market
                } else {
                    View::Installed
                };
                dialog.selected = 0;
                dialog.query.clear();
                let market = dialog.view == View::Market;
                self.request_plugin_list(market);
            }
            KeyCode::Char('v') if dialog.view == View::Market => {
                dialog.market_generation = if dialog.market_generation == 1 { 2 } else { 1 };
                self.request_plugin_list(true);
            }
            KeyCode::Char('r') => {
                let market = dialog.view == View::Market;
                self.request_plugin_list(market);
            }
            KeyCode::Enter | KeyCode::Char('i') if dialog.view == View::Market => {
                self.request_plugin_review("install")
            }
            KeyCode::Char('u') if dialog.view == View::Installed => {
                self.request_plugin_review("update")
            }
            KeyCode::Char('e') if dialog.view == View::Installed => {
                self.request_plugin_review("enable")
            }
            KeyCode::Char('c') if dialog.view == View::Installed => {
                self.request_plugin_review("configure")
            }
            KeyCode::Char('b') if dialog.view == View::Installed => {
                self.request_plugin_review("rollback")
            }
            KeyCode::Char('d' | 'x') if dialog.view == View::Installed => {
                if let Some(row) = dialog.selected() {
                    dialog.remove = Some((label(&row, "id"), key.code == KeyCode::Char('x')));
                    dialog.view = View::Remove;
                }
            }
            _ => {}
        }
    }
    pub(in crate::tui) fn plugin_paste(&mut self, text: &str) -> bool {
        let Some(dialog) = self.plugins.dialog.as_mut() else {
            return false;
        };
        if !dialog.pending {
            if dialog.view == View::Config {
                bounded_insert(&mut dialog.config, text);
            } else if dialog.searching {
                bounded_insert(&mut dialog.query, text);
            }
        }
        true
    }
}

fn bounded_insert(buffer: &mut InputBuffer, text: &str) {
    if buffer.text().len().saturating_add(text.len()) <= 64 * 1024 {
        buffer.insert_str(text);
    }
}
fn edit(buffer: &mut InputBuffer, key: KeyEvent) {
    match key.code {
        KeyCode::Backspace => buffer.backspace(),
        KeyCode::Delete => buffer.delete(),
        KeyCode::Left => buffer.left(),
        KeyCode::Right => buffer.right(),
        KeyCode::Home => buffer.home(),
        KeyCode::End => buffer.end(),
        KeyCode::Char('u') if key.modifiers == KeyModifiers::CONTROL => buffer.clear(),
        KeyCode::Char(c)
            if !key
                .modifiers
                .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SUPER) =>
        {
            bounded_insert(buffer, &c.to_string())
        }
        _ => {}
    }
}
