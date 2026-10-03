//! `/resume` 会话选择器：二级面板，与 /model 选择器同款交互
//! （↑/↓/j/k 导航、Enter 确认、1-9 快选、Esc 取消、鼠标点击）。
//!
//! 会话列表按最近活动在前；会话是懒物化的——从未落盘的空会话不在
//! 列表里出现，列表里出现的都是可恢复的实质会话。
//!
//! dsh 数据形态（D-2 §2.5，2026-08-23 负责人返工后终版）：**单一分组
//! 列表**——全部工作区常显，每组一条「分组头行」（工作区名，不可
//! 选、Faint 样式、上下键自动跳过），行内不再带工作区标签；打开时
//! 光标定位到当前工作区组（活跃会话所属；无活跃会话→最近活跃组）。
//! local 形态字节级不变（dsh 分支只在 `dsh = Some` 时激活）。

use crate::SessionSummary;
use crate::dsh::files::DshSessionRow;
use crate::session::id::SessionId;
use crossterm::event::{KeyCode, KeyEvent, MouseButton, MouseEvent, MouseEventKind};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Wrap};

#[derive(Debug)]
pub(crate) enum ResumeAction {
    Continue,
    /// 用户确认恢复某个会话。
    Open(SessionId),
    Organize {
        id: SessionId,
        pinned: Option<bool>,
        archived: Option<bool>,
    },
    /// dsh 形态：恢复某个宿主会话（携带收养所需的 workspace_path）。
    OpenDsh(Box<DshResumeRow>),
    Cancel,
}

/// dsh 选择器的行数据（`DshSessionRow` 的 UI 适配拷贝——files.rs 协议
/// 层零改动，INV-U4）。
#[derive(Clone, Debug)]
pub(crate) struct DshResumeRow {
    pub(crate) session_id: String,
    pub(crate) workspace_title: String,
    pub(crate) workspace_path: String,
    pub(crate) title: Option<String>,
    pub(crate) activity_ms: i64,
}

impl DshResumeRow {
    fn from_source(row: &DshSessionRow) -> Self {
        Self {
            session_id: row.session_id.clone(),
            workspace_title: row.workspace_title.clone(),
            workspace_path: row.workspace_path.clone(),
            title: row.title.clone(),
            activity_ms: row.activity_ms,
        }
    }

    /// 行标题：会话标题或 id 尾 8 字符（无标题会话的稳定可读形式）。
    fn display_title(&self) -> String {
        self.title.clone().unwrap_or_else(|| {
            format!(
                "…{}",
                self.session_id
                    .chars()
                    .rev()
                    .take(8)
                    .collect::<Vec<_>>()
                    .into_iter()
                    .rev()
                    .collect::<String>()
            )
        })
    }
}

/// dsh 扁平行模型：分组头（工作区名，不可选）+ 会话行（引用
/// `all_rows` 下标）。分组按首次出现排序 = 最近活跃的工作区组在前
///（`files::read_sessions` 已按活跃时间降序）。
#[derive(Clone, Debug)]
enum DshPickerRow {
    Group(String),
    Session(usize),
}

pub(crate) struct DshResumeData {
    all_rows: Vec<DshResumeRow>,
    rows: Vec<DshPickerRow>,
    current_session: Option<String>,
}

impl DshResumeData {
    /// 会话行的行号序列（快选编号与数字键的映射基准）。
    fn session_positions(&self) -> Vec<usize> {
        self.rows
            .iter()
            .enumerate()
            .filter_map(|(index, row)| matches!(row, DshPickerRow::Session(_)).then_some(index))
            .collect()
    }

    fn session_at(&self, position: usize) -> Option<&DshResumeRow> {
        match self.rows.get(position) {
            Some(DshPickerRow::Session(index)) => self.all_rows.get(*index),
            _ => None,
        }
    }
}

pub(crate) struct SessionPicker {
    all_sessions: Vec<SessionSummary>,
    filter: String,
    filtering: bool,
    sessions: Vec<SessionSummary>,
    selected: usize,
    /// 当前会话 id，列表中标记 current。
    current: Option<SessionId>,
    /// dsh 数据形态（Some 时行为/渲染切 dsh 分支，local 字段不读）。
    dsh: Option<Box<DshResumeData>>,
    show_archived: bool,
    archive_confirmation: Option<SessionId>,
}

impl SessionPicker {
    pub(super) fn paste_filter(&mut self, text: &str) {
        self.filtering = true;
        self.filter.extend(
            text.chars()
                .filter(|c| !c.is_control())
                .take(256usize.saturating_sub(self.filter.chars().count())),
        );
        self.apply_filter();
    }

    fn handle_filter_key(&mut self, key: KeyEvent) -> bool {
        if !self.filtering {
            if key.code == KeyCode::Char('/') {
                self.filtering = true;
                return true;
            }
            if key.code == KeyCode::Esc && !self.filter.is_empty() {
                self.filter.clear();
                self.apply_filter();
                return true;
            }
            return false;
        }
        match key.code {
            KeyCode::Esc => {
                self.filtering = false;
                self.filter.clear();
            }
            KeyCode::Enter => {
                self.filtering = false;
            }
            KeyCode::Backspace => {
                self.filter.pop();
            }
            KeyCode::Char(ch)
                if !key.modifiers.intersects(
                    crossterm::event::KeyModifiers::CONTROL | crossterm::event::KeyModifiers::ALT,
                ) =>
            {
                self.filter.push(ch);
            }
            _ => {}
        }
        self.apply_filter();
        true
    }

    fn apply_filter(&mut self) {
        let query = self.filter.to_lowercase();
        self.sessions = self
            .all_sessions
            .iter()
            .filter(|session| {
                if session.archived != self.show_archived
                    && Some(&session.id) != self.current.as_ref()
                {
                    return false;
                }
                format!("{} {}", session.title.as_deref().unwrap_or(""), session.id)
                    .to_lowercase()
                    .contains(&query)
            })
            .cloned()
            .collect();
        self.selected = 0;
        if let Some(dsh) = self.dsh.as_mut() {
            dsh.rows.clear();
            let mut groups = Vec::new();
            for row in &dsh.all_rows {
                if !groups.contains(&row.workspace_path) {
                    groups.push(row.workspace_path.clone());
                }
            }
            for path in groups {
                let indices: Vec<_> = dsh
                    .all_rows
                    .iter()
                    .enumerate()
                    .filter(|(_, row)| {
                        row.workspace_path == path
                            && format!("{} {}", row.display_title(), row.session_id)
                                .to_lowercase()
                                .contains(&query)
                    })
                    .map(|(i, _)| i)
                    .collect();
                if let Some(first) = indices.first() {
                    dsh.rows.push(DshPickerRow::Group(
                        dsh.all_rows[*first].workspace_title.clone(),
                    ));
                    dsh.rows
                        .extend(indices.into_iter().map(DshPickerRow::Session));
                }
            }
            self.selected = dsh.session_positions().first().copied().unwrap_or(0);
        }
    }

    pub fn new(sessions: Vec<SessionSummary>, current: Option<SessionId>) -> Self {
        let mut picker = Self {
            all_sessions: sessions.clone(),
            filter: String::new(),
            filtering: false,
            sessions,
            selected: 0,
            current,
            dsh: None,
            show_archived: false,
            archive_confirmation: None,
        };
        picker.apply_filter();
        picker
    }

    pub(in crate::tui) fn replace_sessions(&mut self, sessions: Vec<SessionSummary>) {
        let selected = self.sessions.get(self.selected).map(|s| s.id.clone());
        self.all_sessions = sessions;
        self.archive_confirmation = None;
        self.apply_filter();
        if let Some(selected) = selected {
            self.selected = self
                .sessions
                .iter()
                .position(|s| s.id == selected)
                .unwrap_or(0);
        }
    }

    /// dsh 形态构造：全部工作区常显（分组头 + 会话行）；光标定位到
    /// 当前工作区组的首个会话行——活跃会话所属组；无活跃会话或无
    /// 匹配 → 最近活跃组（活跃降序首行所属组）。
    pub fn new_dsh(rows: Vec<DshSessionRow>, current_session: Option<String>) -> Self {
        let all_rows: Vec<DshResumeRow> = rows.iter().map(DshResumeRow::from_source).collect();
        // 分组：按 workspace_path 首次出现（all_rows 已按活跃降序）。
        let mut group_order: Vec<String> = Vec::new();
        for row in &all_rows {
            if !group_order.contains(&row.workspace_path) {
                group_order.push(row.workspace_path.clone());
            }
        }
        let mut flat = Vec::new();
        let mut first_session_row: Option<usize> = None;
        for path in &group_order {
            let header = all_rows
                .iter()
                .find(|row| &row.workspace_path == path)
                .map(|row| row.workspace_title.clone())
                .unwrap_or_default();
            flat.push(DshPickerRow::Group(header));
            for (index, row) in all_rows.iter().enumerate() {
                if row.workspace_path == *path {
                    if first_session_row.is_none() {
                        first_session_row = Some(flat.len());
                    }
                    flat.push(DshPickerRow::Session(index));
                }
            }
        }
        // 定位：当前会话所属组的首个会话行；缺省 → 首组首个会话行
        //（= 最近活跃组）。
        let selected = current_session
            .as_deref()
            .and_then(|session| all_rows.iter().position(|row| row.session_id == session))
            .and_then(|row_index| {
                let path = all_rows[row_index].workspace_path.clone();
                flat.iter().position(|row| match row {
                    DshPickerRow::Session(i) => all_rows[*i].workspace_path == path,
                    DshPickerRow::Group(_) => false,
                })
            })
            .or(first_session_row)
            .unwrap_or(0);
        Self {
            all_sessions: Vec::new(),
            filter: String::new(),
            filtering: false,
            sessions: Vec::new(),
            selected,
            current: None,
            show_archived: false,
            archive_confirmation: None,
            dsh: Some(Box::new(DshResumeData {
                all_rows,
                rows: flat,
                current_session,
            })),
        }
    }

    pub fn row_count(&self) -> usize {
        match self.dsh.as_deref() {
            Some(dsh) => dsh.rows.len(),
            None => self.sessions.len(),
        }
    }

    pub(super) fn display_row_count(&self) -> usize {
        self.row_count() + usize::from(self.filtering || !self.filter.is_empty())
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> ResumeAction {
        if self.handle_filter_key(key) {
            return ResumeAction::Continue;
        }
        if let Some(data) = self.dsh.take() {
            let mut data = data;
            let action = self.handle_key_dsh(&mut data, key);
            self.dsh = Some(data);
            return action;
        }
        if let Some(action) = self.organization_key(key) {
            return action;
        }
        if self.sessions.is_empty() {
            return match key.code {
                KeyCode::Esc => ResumeAction::Cancel,
                KeyCode::Enter if self.filter.is_empty() => ResumeAction::Cancel,
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

    /// dsh 键位：与 local 逐键一致；上下键在会话行之间移动（分组头
    /// 不可选，自动跳过）。
    fn handle_key_dsh(&mut self, dsh: &mut DshResumeData, key: KeyEvent) -> ResumeAction {
        let positions = dsh.session_positions();
        if positions.is_empty() {
            return match key.code {
                KeyCode::Esc => ResumeAction::Cancel,
                KeyCode::Enter if self.filter.is_empty() => ResumeAction::Cancel,
                _ => ResumeAction::Continue,
            };
        }
        let ordinal = positions
            .iter()
            .position(|position| *position == self.selected)
            .unwrap_or(0);
        match key.code {
            KeyCode::Esc => ResumeAction::Cancel,
            KeyCode::Up | KeyCode::Char('k') => {
                let previous = (ordinal + positions.len() - 1) % positions.len();
                self.selected = positions[previous];
                ResumeAction::Continue
            }
            KeyCode::Down | KeyCode::Char('j') => {
                let next = (ordinal + 1) % positions.len();
                self.selected = positions[next];
                ResumeAction::Continue
            }
            KeyCode::Enter => match dsh.session_at(self.selected) {
                Some(row) => ResumeAction::OpenDsh(Box::new(row.clone())),
                None => ResumeAction::Continue,
            },
            KeyCode::Char(ch) if ch.is_ascii_digit() && ch != '0' => {
                let ordinal = (ch as usize - '1' as usize).min(8);
                match positions
                    .get(ordinal)
                    .and_then(|position| dsh.session_at(*position))
                {
                    Some(row) => ResumeAction::OpenDsh(Box::new(row.clone())),
                    None => ResumeAction::Continue,
                }
            }
            _ => ResumeAction::Continue,
        }
    }

    pub fn handle_mouse(&mut self, mouse: MouseEvent, area: Rect) -> ResumeAction {
        if mouse.kind != MouseEventKind::Down(MouseButton::Left) || self.row_count() == 0 {
            return ResumeAction::Continue;
        }
        if mouse.column < area.x || mouse.column >= area.x + area.width {
            return ResumeAction::Continue;
        }
        // 跳过边框行（顶部标题 + 底部说明）。
        if mouse.row <= area.y || mouse.row >= area.y + area.height.saturating_sub(1) {
            return ResumeAction::Continue;
        }
        let header = u16::from(self.filtering || !self.filter.is_empty());
        if mouse.row <= area.y + header {
            return ResumeAction::Continue;
        }
        let row = mouse.row.saturating_sub(area.y + 1 + header) as usize;
        match self.dsh.as_deref() {
            Some(dsh) => match dsh.session_at(row) {
                Some(row_data) => ResumeAction::OpenDsh(Box::new(row_data.clone())),
                None => ResumeAction::Continue,
            },
            None => match self.sessions.get(row) {
                Some(session) => ResumeAction::Open(session.id.clone()),
                None => ResumeAction::Continue,
            },
        }
    }

    pub fn draw(&self, frame: &mut Frame, area: Rect) {
        crate::tui::clear_popup_with_guards(frame, area);
        let title = if self.show_archived {
            "/resume · archived"
        } else {
            "/resume"
        };
        let block = crate::tui::popup_block(title);
        let row_width = block.inner(area).width as usize;
        let mut lines = Vec::new();
        if self.filtering || !self.filter.is_empty() {
            lines.push(Line::from(format!(
                "Filter title / ID: {} · Enter browse · Esc clear",
                self.filter
            )));
        }
        if let Some(dsh) = self.dsh.as_deref() {
            self.draw_dsh(dsh, &mut lines, row_width);
        } else {
            self.draw_local(&mut lines, row_width);
        }
        frame.render_widget(
            Paragraph::new(lines)
                .block(block)
                .wrap(Wrap { trim: false }),
            area,
        );
    }

    fn draw_local(&self, lines: &mut Vec<Line<'static>>, row_width: usize) {
        if self.sessions.is_empty() {
            lines.push(Line::from(if self.filter.is_empty() {
                "no previous conversations in this project"
            } else {
                "no matching conversations (title / ID)"
            }));
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled(
                "Esc close",
                Style::default().add_modifier(Modifier::DIM),
            )));
        } else {
            for (index, session) in self.sessions.iter().enumerate() {
                let style = if index == self.selected {
                    crate::tui::theme::style(crate::tui::theme::Role::Selected)
                } else {
                    Style::default()
                };
                let number = if index < 9 {
                    format!("{}", index + 1)
                } else {
                    " ".to_owned()
                };
                let title = format!(
                    "{}{}{}",
                    if session.pinned { "★ " } else { "" },
                    session.title.as_deref().unwrap_or("(untitled)"),
                    if session.archived { " [archived]" } else { "" }
                );
                let current = Some(&session.id) == self.current.as_ref();
                // VP-3 四轮定稿：✓ 锚定名称——数字列之后、标题之前。
                let body = format!("{title:<32}{} msgs", session.message_count);
                lines.push(Line::from(Span::styled(
                    crate::tui::numbered_picker_row(&number, &body, current, row_width),
                    style,
                )));
            }
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled(
                if self.archive_confirmation.is_some() {
                    "A again: confirm archive (content retained) · Esc cancel"
                } else if self.show_archived {
                    "↑↓ · Enter open · / find · P pin · A restore · F2 active · Esc"
                } else {
                    "↑↓ · Enter open · / find · P pin · A archive · F2 archived · Esc"
                },
                Style::default().add_modifier(Modifier::DIM),
            )));
        }
    }

    /// dsh 行：分组头（Faint，不可选）+ 会话行
    /// `{n} {✓| } {title:<32}{activity}`——工作区归属由分组头表达，
    /// 行内不再带标签；✓ 锚定名称，居数字列之后（VP-3 四轮定稿）。
    fn draw_dsh(&self, dsh: &DshResumeData, lines: &mut Vec<Line<'static>>, row_width: usize) {
        if dsh.rows.is_empty() {
            lines.push(Line::from(if self.filter.is_empty() {
                "no dsh sessions — /new to start one"
            } else {
                "no matching conversations (title / ID)"
            }));
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled(
                "Esc close",
                Style::default().add_modifier(Modifier::DIM),
            )));
            return;
        }
        let mut session_ordinal = 0usize;
        for (index, row) in dsh.rows.iter().enumerate() {
            match row {
                DshPickerRow::Group(title) => {
                    lines.push(Line::from(Span::styled(
                        format!(" {title}"),
                        crate::tui::theme::style(crate::tui::theme::Role::Faint),
                    )));
                }
                DshPickerRow::Session(row_index) => {
                    let row = &dsh.all_rows[*row_index];
                    let style = if index == self.selected {
                        crate::tui::theme::style(crate::tui::theme::Role::Selected)
                    } else {
                        Style::default()
                    };
                    let number = if session_ordinal < 9 {
                        format!("{}", session_ordinal + 1)
                    } else {
                        " ".to_owned()
                    };
                    session_ordinal += 1;
                    let current = dsh.current_session.as_deref() == Some(row.session_id.as_str());
                    // VP-3 四轮定稿：✓ 锚定名称——数字列之后、标题之前。
                    let body = format!(
                        "{:<32}{}",
                        row.display_title(),
                        format_activity(row.activity_ms)
                    );
                    lines.push(Line::from(Span::styled(
                        crate::tui::numbered_picker_row(&number, &body, current, row_width),
                        style,
                    )));
                }
            }
        }
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            "↑↓ select · Enter resume · 1-9 quick pick · / filter · Esc close",
            Style::default().add_modifier(Modifier::DIM),
        )));
    }
}

impl SessionPicker {
    fn organization_key(&mut self, key: KeyEvent) -> Option<ResumeAction> {
        if key.code == KeyCode::F(2) {
            self.show_archived = !self.show_archived;
            self.archive_confirmation = None;
            self.apply_filter();
            return Some(ResumeAction::Continue);
        }
        let session = self.sessions.get(self.selected)?;
        if key.code == KeyCode::Char('p') || key.code == KeyCode::Char('P') {
            self.archive_confirmation = None;
            return Some(ResumeAction::Organize {
                id: session.id.clone(),
                pinned: Some(!session.pinned),
                archived: None,
            });
        }
        if key.code == KeyCode::Char('a') || key.code == KeyCode::Char('A') {
            if session.archived || self.archive_confirmation.as_ref() == Some(&session.id) {
                return Some(ResumeAction::Organize {
                    id: session.id.clone(),
                    pinned: None,
                    archived: Some(!session.archived),
                });
            }
            self.archive_confirmation = Some(session.id.clone());
            return Some(ResumeAction::Continue);
        }
        self.archive_confirmation = None;
        None
    }
}

/// 活跃时间的稳定呈现：epoch ms → `MM-DD HH:MM`（UTC，纯函数——
/// 相对时间会随真实时钟漂移，快照需要确定性）。
fn format_activity(epoch_ms: i64) -> String {
    let days = epoch_ms.div_euclid(86_400_000);
    let ms_of_day = epoch_ms.rem_euclid(86_400_000);
    let minutes = ms_of_day / 60_000;
    let (_, month, day) = civil_from_days(days);
    format!(
        "{month:02}-{day:02} {:02}:{:02}",
        (minutes / 60) % 24,
        minutes % 60
    )
}

/// Hinnant civil 算法（与 control_storage/timestamp.rs 同源；纯前端
/// 用途不值得跨模块暴露内核函数）。
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn explicit_archive_confirmation_filter_precedence_and_restore_are_distinct() {
        let row = |id: &str, archived| SessionSummary {
            id: SessionId::new(id),
            title: Some(id.into()),
            created_at_ms: 0,
            last_activity_ms: 0,
            message_count: 2,
            turns: 1,
            pinned: false,
            archived,
        };
        let mut picker = SessionPicker::new(vec![row("active", false), row("old", true)], None);
        assert_eq!(picker.row_count(), 1);
        assert!(matches!(
            picker.handle_key(KeyEvent::from(KeyCode::Char('a'))),
            ResumeAction::Continue
        ));
        assert!(matches!(
            picker.handle_key(KeyEvent::from(KeyCode::Char('a'))),
            ResumeAction::Organize {
                archived: Some(true),
                ..
            }
        ));
        picker.handle_key(KeyEvent::from(KeyCode::F(2)));
        assert_eq!(picker.sessions[0].id.as_str(), "old");
        assert!(matches!(
            picker.handle_key(KeyEvent::from(KeyCode::Char('a'))),
            ResumeAction::Organize {
                archived: Some(false),
                ..
            }
        ));
        picker.paste_filter("old");
        picker.replace_sessions(vec![row("active", false), row("old", true)]);
        assert!(picker.show_archived);
        assert_eq!(picker.filter, "old");
        assert_eq!(picker.sessions[0].id.as_str(), "old");
        picker.handle_key(KeyEvent::from(KeyCode::Esc));
        picker.handle_key(KeyEvent::from(KeyCode::Char('/')));
        assert!(matches!(
            picker.handle_key(KeyEvent::from(KeyCode::Char('p'))),
            ResumeAction::Continue
        ));
        assert_eq!(picker.filter, "p");
        assert!(picker.sessions.is_empty());
    }
    #[test]
    fn resume_filter_preserves_identity_and_is_recoverable_from_empty_results() {
        let sessions = [("id-one", "中文设计"), ("id-two", "Fix the parser")]
            .into_iter()
            .map(|(id, title)| SessionSummary {
                id: SessionId::new(id),
                title: Some(title.into()),
                created_at_ms: 0,
                last_activity_ms: 0,
                message_count: 2,
                turns: 1,
                pinned: false,
                archived: false,
            })
            .collect();
        let mut picker = SessionPicker::new(sessions, None);
        picker.paste_filter("中文");
        assert_eq!(picker.sessions.len(), 1);
        picker.handle_key(KeyEvent::from(KeyCode::Enter));
        assert!(
            matches!(picker.handle_key(KeyEvent::from(KeyCode::Enter)), ResumeAction::Open(id) if id.as_str() == "id-one")
        );
        picker.filter = "id-two".into();
        picker.apply_filter();
        assert_eq!(picker.sessions[0].id.as_str(), "id-two");
        picker.filter = "missing".into();
        picker.apply_filter();
        assert!(picker.sessions.is_empty());
        assert!(matches!(
            picker.handle_key(KeyEvent::from(KeyCode::Enter)),
            ResumeAction::Continue
        ));
        picker.handle_key(KeyEvent::from(KeyCode::Esc));
        assert_eq!(picker.sessions.len(), 2);
        let mut dsh = SessionPicker::new_dsh(fixture_rows(), None);
        dsh.paste_filter("adapter");
        assert_eq!(dsh.dsh.as_ref().unwrap().session_positions().len(), 1);
        dsh.handle_key(KeyEvent::from(KeyCode::Enter));
        assert!(
            matches!(dsh.handle_key(KeyEvent::from(KeyCode::Enter)), ResumeAction::OpenDsh(row) if row.session_id == "session-b1")
        );
    }

    /// 活跃时间格式：固定 epoch 的稳定输出（快照确定性锚）。
    #[test]
    fn activity_format_is_stable_and_utc() {
        // 2026-08-23T14:05:00Z = 1787493900000 ms（整分）。
        assert_eq!(format_activity(1_787_493_900_000), "08-23 14:05");
        assert_eq!(format_activity(0), "01-01 00:00");
    }

    fn fixture_rows() -> Vec<DshSessionRow> {
        // 活跃降序：a1(5k) → b1(4k) → a2(3k)：alpha 组最近活跃在前，
        // 组内行 a1、a2 分列（分组不改变组间排序）。
        vec![
            DshSessionRow {
                session_id: "session-a1".into(),
                workspace_title: "alpha".into(),
                workspace_path: "/w/alpha".into(),
                title: Some("Fix the flaky test".into()),
                created_at_ms: 0,
                activity_ms: 5_000,
            },
            DshSessionRow {
                session_id: "session-b1".into(),
                workspace_title: "beta".into(),
                workspace_path: "/w/beta".into(),
                title: Some("Port the adapter".into()),
                created_at_ms: 0,
                activity_ms: 4_000,
            },
            DshSessionRow {
                session_id: "session-a2".into(),
                workspace_title: "alpha".into(),
                workspace_path: "/w/alpha".into(),
                title: None,
                created_at_ms: 0,
                activity_ms: 3_000,
            },
        ]
    }

    /// 分组行模型（2026-08-23 返工终版）：常显全部分组、头行不可选
    /// （上下键跳过、数字键按会话序数）、打开定位当前工作区组、
    /// 无活跃会话→最近活跃组。
    #[test]
    fn grouped_rows_skip_headers_and_position_on_open() {
        // 有活跃会话（beta 组）→ 光标落在 beta 组首个会话行。
        let mut picker = SessionPicker::new_dsh(fixture_rows(), Some("session-b1".into()));
        match picker.handle_key(KeyEvent::from(KeyCode::Enter)) {
            ResumeAction::OpenDsh(row) => {
                assert_eq!(row.session_id, "session-b1");
                assert_eq!(row.workspace_path, "/w/beta");
            }
            other => panic!("opens positioned at the current workspace group: {other:?}"),
        }
        // 无活跃会话 → 最近活跃组（alpha）的首个会话行。
        let picker = SessionPicker::new_dsh(fixture_rows(), None);
        let positions = picker.dsh.as_ref().unwrap().session_positions();
        assert_eq!(
            picker.selected, positions[0],
            "no active session falls back to the most recently active group"
        );
        // 上下键跳过分组头：从首行向上绕到最末会话行（不是头行）。
        let mut picker = SessionPicker::new_dsh(fixture_rows(), None);
        picker.handle_key(KeyEvent::from(KeyCode::Up));
        let positions = picker.dsh.as_ref().unwrap().session_positions();
        assert_eq!(
            picker.selected,
            *positions.last().unwrap(),
            "Up wraps across headers onto the last session row"
        );
        // 全遍历：Down 一圈恰按序经过每个会话行（头行永不落）。
        let mut picker = SessionPicker::new_dsh(fixture_rows(), None);
        let positions = picker.dsh.as_ref().unwrap().session_positions();
        for expected in positions.iter() {
            assert_eq!(picker.selected, *expected);
            picker.handle_key(KeyEvent::from(KeyCode::Down));
        }
        // 数字键按显示序的会话序数（分组归并后：a1=1、a2=2、b1=3）。
        let mut picker = SessionPicker::new_dsh(fixture_rows(), None);
        match picker.handle_key(KeyEvent::from(KeyCode::Char('2'))) {
            ResumeAction::OpenDsh(row) => assert_eq!(row.session_id, "session-a2"),
            other => panic!("digit quick-pick counts sessions, not headers: {other:?}"),
        }
        let mut picker = SessionPicker::new_dsh(fixture_rows(), None);
        match picker.handle_key(KeyEvent::from(KeyCode::Char('3'))) {
            ResumeAction::OpenDsh(row) => assert_eq!(row.session_id, "session-b1"),
            other => panic!("digits cross group boundaries by display order: {other:?}"),
        }
    }
}
