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
    sessions: Vec<SessionSummary>,
    selected: usize,
    /// 当前会话 id，列表中标记 current。
    current: Option<SessionId>,
    /// dsh 数据形态（Some 时行为/渲染切 dsh 分支，local 字段不读）。
    dsh: Option<Box<DshResumeData>>,
}

impl SessionPicker {
    pub fn new(sessions: Vec<SessionSummary>, current: Option<SessionId>) -> Self {
        Self {
            sessions,
            selected: 0,
            current,
            dsh: None,
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
            sessions: Vec::new(),
            selected,
            current: None,
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

    pub fn handle_key(&mut self, key: KeyEvent) -> ResumeAction {
        if let Some(data) = self.dsh.take() {
            let mut data = data;
            let action = self.handle_key_dsh(&mut data, key);
            self.dsh = Some(data);
            return action;
        }
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

    /// dsh 键位：与 local 逐键一致；上下键在会话行之间移动（分组头
    /// 不可选，自动跳过）。
    fn handle_key_dsh(&mut self, dsh: &mut DshResumeData, key: KeyEvent) -> ResumeAction {
        let positions = dsh.session_positions();
        if positions.is_empty() {
            return match key.code {
                KeyCode::Esc | KeyCode::Enter => ResumeAction::Cancel,
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
        let row = mouse.row.saturating_sub(area.y + 1) as usize;
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
        let block = crate::tui::popup_block("/resume");
        let row_width = block.inner(area).width as usize;
        let mut lines = Vec::new();
        if let Some(dsh) = self.dsh.as_deref() {
            self.draw_dsh(dsh, &mut lines, row_width);
        } else if self.sessions.is_empty() {
            lines.push(Line::from("no previous conversations in this project"));
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
                let title = session.title.clone().unwrap_or_else(|| "(untitled)".into());
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
                "↑↓ select · Enter resume · 1-9 quick pick · Esc close",
                Style::default().add_modifier(Modifier::DIM),
            )));
        }
        frame.render_widget(
            Paragraph::new(lines)
                .block(block)
                .wrap(Wrap { trim: false }),
            area,
        );
    }

    /// dsh 行：分组头（Faint，不可选）+ 会话行
    /// `{n} {✓| } {title:<32}{activity}`——工作区归属由分组头表达，
    /// 行内不再带标签；✓ 锚定名称，居数字列之后（VP-3 四轮定稿）。
    fn draw_dsh(&self, dsh: &DshResumeData, lines: &mut Vec<Line<'static>>, row_width: usize) {
        if dsh.rows.is_empty() {
            lines.push(Line::from("no dsh sessions — /new to start one"));
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
            "↑↓ select · Enter resume · 1-9 quick pick · Esc close",
            Style::default().add_modifier(Modifier::DIM),
        )));
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
