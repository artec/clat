//! `/resume` 会话选择器：二级面板，与 /model 选择器同款交互
//! （↑/↓/j/k 导航、Enter 确认、1-9 快选、Esc 取消、鼠标点击）。
//!
//! 会话列表按最近活动在前；会话是懒物化的——从未落盘的空会话不在
//! 列表里出现，列表里出现的都是可恢复的实质会话。

use crate::SessionSummary;
use crate::session::id::SessionId;
use crossterm::event::{KeyCode, KeyEvent, MouseButton, MouseEvent, MouseEventKind};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Clear, Paragraph, Wrap};

pub(crate) enum ResumeAction {
    Continue,
    /// 用户确认恢复某个会话。
    Open(SessionId),
    Cancel,
}

pub(crate) struct SessionPicker {
    sessions: Vec<SessionSummary>,
    selected: usize,
    /// 当前会话 id，列表中标记 current。
    current: Option<SessionId>,
}

impl SessionPicker {
    pub fn new(sessions: Vec<SessionSummary>, current: Option<SessionId>) -> Self {
        Self {
            sessions,
            selected: 0,
            current,
        }
    }

    pub fn row_count(&self) -> usize {
        self.sessions.len()
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> ResumeAction {
        if self.sessions.is_empty() {
            return match key.code {
                KeyCode::Esc | KeyCode::Enter => ResumeAction::Cancel,
                _ => ResumeAction::Continue,
            };
        }
        match key.code {
            KeyCode::Esc => ResumeAction::Cancel,
            KeyCode::Up | KeyCode::Char('k') => {
                self.selected = (self.selected + self.row_count() - 1) % self.row_count();
                ResumeAction::Continue
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.selected = (self.selected + 1) % self.row_count();
                ResumeAction::Continue
            }
            KeyCode::Enter => ResumeAction::Open(self.sessions[self.selected].id.clone()),
            KeyCode::Char(ch) if ch.is_ascii_digit() && ch != '0' => {
                let index = (ch as usize - '1' as usize).min(8);
                match self.sessions.get(index) {
                    Some(session) => ResumeAction::Open(session.id.clone()),
                    None => ResumeAction::Continue,
                }
            }
            _ => ResumeAction::Continue,
        }
    }

    pub fn handle_mouse(&mut self, mouse: MouseEvent, area: Rect) -> ResumeAction {
        if mouse.kind != MouseEventKind::Down(MouseButton::Left) || self.sessions.is_empty() {
            return ResumeAction::Continue;
        }
        if mouse.column < area.x || mouse.column >= area.x + area.width {
            return ResumeAction::Continue;
        }
        // 跳过边框行（顶部标题 + 底部说明）。
        if mouse.row <= area.y || mouse.row >= area.y + area.height.saturating_sub(1) {
            return ResumeAction::Continue;
        }
        let row = mouse.row.saturating_sub(area.y + 1) as usize;
        match self.sessions.get(row) {
            Some(session) => ResumeAction::Open(session.id.clone()),
            None => ResumeAction::Continue,
        }
    }

    pub fn draw(&self, frame: &mut Frame, area: Rect) {
        frame.render_widget(Clear, area);
        let mut lines = Vec::new();
        if self.sessions.is_empty() {
            lines.push(Line::from("no previous conversations in this project"));
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled(
                "Esc close",
                Style::default().add_modifier(Modifier::DIM),
            )));
        } else {
            for (index, session) in self.sessions.iter().enumerate() {
                let style = if index == self.selected {
                    crate::tui_theme::style(crate::tui_theme::Role::Selected)
                } else {
                    Style::default()
                };
                let number = if index < 9 {
                    format!("{}", index + 1)
                } else {
                    " ".to_owned()
                };
                let title = session.title.clone().unwrap_or_else(|| "(untitled)".into());
                let marker = if Some(&session.id) == self.current.as_ref() {
                    "●"
                } else {
                    " "
                };
                lines.push(Line::from(vec![
                    Span::styled(format!("{number}  "), style),
                    Span::raw(format!("{marker} ")),
                    Span::styled(format!("{title:<32}"), style),
                    Span::styled(format!("{} msgs", session.message_count), style),
                ]));
            }
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled(
                "↑↓ select · Enter resume · 1-9 quick pick · Esc close",
                Style::default().add_modifier(Modifier::DIM),
            )));
        }
        frame.render_widget(
            Paragraph::new(lines)
                .block(crate::tui::popup_block(" /resume "))
                .wrap(Wrap { trim: false }),
            area,
        );
    }
}
