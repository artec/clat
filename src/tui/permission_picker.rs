//! `/perm` 弹框（权限三档的冷切换入口，2026-08-19）。`/permission` 是
//! 同义别名。
//!
//! 三行模式选择器（仿 /model /resume 的弹窗惯例）：↑↓ 移动、Enter 选
//! 择、Esc 取消；当前档行有标记。**Full Access 有确认子态**（P4）——
//! 冷切换没有待批的调用上下文，误触直达"不再有任何弹窗"，需要第二
//! 次明确按 Enter；权限弹框内的热升级（w/f 键）有完整调用上下文在
//! 眼前，单键直切（有意的不对称，对 DSH RiskConfirmation 的裁剪）。
//! 已处于 FA 时再选 FA 无确认（无变化）。

use crate::permission::PermissionMode;
use crate::tui::theme;
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

/// 三档的说明文案（弹框行 + /help 共用）。
pub(crate) fn mode_description(mode: PermissionMode) -> &'static str {
    match mode {
        PermissionMode::ReadOnly => "every side effect asks before it runs",
        PermissionMode::ProjectWrite => {
            "file edits, reads, and network tools run; commands and destructive tools ask"
        }
        PermissionMode::FullAccess => "no approval prompts at all",
    }
}

/// dsh 档词汇（D-2 §2.5）：显示名与 web 端 transform 后的产品标签一
/// 致；description 为宿主 permission-presets 原文（read-only 在钉靶
/// 源无 description——留空不造文）。三档 enum 复用 `PermissionMode`
///（journal 值词汇与 DSH preset 同源，permission.rs:229）。
pub(crate) fn dsh_mode_label(mode: PermissionMode) -> &'static str {
    match mode {
        PermissionMode::ReadOnly => "Read Only",
        PermissionMode::ProjectWrite => "Workspace Write",
        PermissionMode::FullAccess => "Full Access",
    }
}

pub(crate) fn dsh_mode_description(mode: PermissionMode) -> &'static str {
    match mode {
        PermissionMode::ReadOnly => "",
        PermissionMode::ProjectWrite => {
            "Write inside the workspace and permitted temporary directories; wider retries \
             require approval."
        }
        PermissionMode::FullAccess => "Full access without approval prompts.",
    }
}

pub(crate) const MODES: [PermissionMode; 3] = [
    PermissionMode::ReadOnly,
    PermissionMode::ProjectWrite,
    PermissionMode::FullAccess,
];

/// 词汇源：local（CLAT 文案）或 dsh（DSH preset 词汇 + prompt 通道
/// Apply——/permission 命令经宿主落定，非 set_permission_mode）。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PermissionSource {
    Local,
    Dsh,
}

pub(crate) struct PermissionPicker {
    selected: usize,
    /// FA 确认子态：true 时 Enter 生效、Esc 退回列表。
    confirming_full_access: bool,
    source: PermissionSource,
}

pub(crate) enum PermissionPickerAction {
    Continue,
    Cancel,
    Apply(PermissionMode),
}

impl PermissionPicker {
    pub(crate) fn new(current: PermissionMode) -> Self {
        Self {
            selected: MODES
                .iter()
                .position(|mode| *mode == current)
                .unwrap_or_default(),
            confirming_full_access: false,
            source: PermissionSource::Local,
        }
    }

    /// dsh 形态构造（键位与 FA 确认子态同 local，仅词汇与 Apply 通道
    /// 不同——INV-U9）。
    pub(crate) fn new_dsh(current: PermissionMode) -> Self {
        Self {
            selected: MODES
                .iter()
                .position(|mode| *mode == current)
                .unwrap_or_default(),
            confirming_full_access: false,
            source: PermissionSource::Dsh,
        }
    }

    fn label(&self, mode: PermissionMode) -> String {
        match self.source {
            // local 的 Display 词汇即产品标签（Read Only / Project Write /
            // Full Access）。
            PermissionSource::Local => mode.to_string(),
            PermissionSource::Dsh => dsh_mode_label(mode).to_owned(),
        }
    }

    fn description(&self, mode: PermissionMode) -> &'static str {
        match self.source {
            PermissionSource::Local => mode_description(mode),
            PermissionSource::Dsh => dsh_mode_description(mode),
        }
    }

    pub(crate) fn handle_key(
        &mut self,
        key: ratatui::crossterm::event::KeyEvent,
        current: PermissionMode,
    ) -> PermissionPickerAction {
        use ratatui::crossterm::event::KeyCode;
        use ratatui::crossterm::event::KeyModifiers;
        // 提交/取消只认裸键（对抗审计 2026-08-19）：CLAT 开着
        // keyboard-enhancement，Shift/Ctrl/Alt+Enter 以独立事件到达——
        // 主输入里它们是换行肌肉记忆，在选择器里却会套用选择（甚至
        // 进入 FA 确认）。方向键不受限。
        let plain_enter = key.code == KeyCode::Enter && key.modifiers == KeyModifiers::NONE;
        let plain_esc = key.code == KeyCode::Esc && key.modifiers == KeyModifiers::NONE;
        if self.confirming_full_access {
            if plain_enter {
                self.confirming_full_access = false;
                return PermissionPickerAction::Apply(PermissionMode::FullAccess);
            }
            // Esc 只退出确认，回列表（用户可能想改选别的档）。
            if plain_esc {
                self.confirming_full_access = false;
                return PermissionPickerAction::Continue;
            }
            return PermissionPickerAction::Continue;
        }
        match key.code {
            KeyCode::Up => {
                self.selected = self.selected.saturating_sub(1);
                PermissionPickerAction::Continue
            }
            KeyCode::Down => {
                self.selected = (self.selected + 1).min(MODES.len() - 1);
                PermissionPickerAction::Continue
            }
            KeyCode::Esc if plain_esc => PermissionPickerAction::Cancel,
            KeyCode::Enter if plain_enter => {
                let mode = MODES[self.selected];
                // 从非 FA 切到 FA 需要二次确认（P4）；已在 FA 则无变化。
                if mode == PermissionMode::FullAccess && current != PermissionMode::FullAccess {
                    self.confirming_full_access = true;
                    PermissionPickerAction::Continue
                } else {
                    PermissionPickerAction::Apply(mode)
                }
            }
            _ => PermissionPickerAction::Continue,
        }
    }

    pub(crate) fn draw(&self, frame: &mut Frame, area: Rect, current: PermissionMode) {
        // 行预算与弹框实际内宽一致（popup_inner_width 与 centered_rect
        // 的宽度钳制同源，见 popup_width_matches_centered_rect 锁）——
        // 预折行取代 Paragraph 的自动换行，行数即内容高度，页脚永不被
        // 二次折行挤出框外。
        let width = crate::tui::popup_inner_width(84, area);
        let lines: Vec<Line<'static>> = if self.confirming_full_access {
            let mut lines = vec![
                Line::from(Span::styled(
                    "Enable Full Access?",
                    theme::style(theme::Role::Bold),
                )),
                Line::from(""),
            ];
            for wrapped in [
                "Full access removes every confirmation prompt — file edits, \
                 commands, network, and destructive tools will run directly.",
                "Only use it when you trust the current task. You can switch \
                 back any time with /perm.",
            ]
            .iter()
            .flat_map(|text| crate::tui::wrap_text(text, width))
            {
                lines.push(Line::from(wrapped));
            }
            lines.push(Line::from(""));
            // 脚注键位说明用 Faint 灰——与 /help /mcp 弹窗的脚注样式
            // 统一（2026-08-19 用户反馈：Bold 亮白与其他弹窗不一致）。
            lines.push(Line::from(Span::styled(
                "Enter — enable Full Access      ·      Esc — back",
                theme::style(theme::Role::Faint),
            )));
            lines
        } else {
            let mut lines = Vec::new();
            // 标签列宽：local 14（既有布局，快照零回归）；dsh 16
            //（"Workspace Write" 15 字符）。
            let label_width = match self.source {
                PermissionSource::Local => 14,
                PermissionSource::Dsh => 16,
            };
            for (index, mode) in MODES.iter().enumerate() {
                // 五轮微调（2026-09-04）：前导空格交给 `picker_row`
                //（` ✓ 名称`）——名称恒定第 3 列，✓ 不顶左缘、与名称
                // 恰隔一空格。
                let row = format!(
                    "{:<width$}{}",
                    self.label(*mode),
                    self.description(*mode),
                    width = label_width,
                );
                let row = crate::tui::picker_row(&row, *mode == current, width);
                let line = if index == self.selected {
                    Line::from(Span::styled(row, theme::style(theme::Role::Selected)))
                } else {
                    Line::from(row)
                };
                lines.push(line);
            }
            lines.push(Line::from(""));
            // 脚注键位说明用 Faint 灰（同上，弹窗脚注样式统一）。
            lines.push(Line::from(Span::styled(
                "↑/↓ select · Enter apply · Esc cancel",
                theme::style(theme::Role::Faint),
            )));
            lines
        };
        let height = (lines.len() as u16 + 2).min(crate::tui::popup_height_cap(area));
        let dialog = crate::tui::centered_rect(84, height.max(7), area);
        crate::tui::clear_popup_with_guards(frame, dialog);
        frame.render_widget(
            Paragraph::new(lines).block(crate::tui::popup_block("/perm")),
            dialog,
        );
    }
}

/// 不变量 P4：冷切换进入 Full Access 必须经过确认子态——未确认的
/// Enter 不产生 Apply。pre-fix（无确认门）上第一个 Enter 断言失败。
#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::crossterm::event::KeyCode;

    fn key(code: KeyCode) -> ratatui::crossterm::event::KeyEvent {
        ratatui::crossterm::event::KeyEvent::from(code)
    }

    #[test]
    fn full_access_requires_a_confirm_step() {
        let mut picker = PermissionPicker::new(PermissionMode::ProjectWrite);
        // 选中 FA 行（列表第三行）。
        picker.handle_key(key(KeyCode::Down), PermissionMode::ProjectWrite);
        picker.handle_key(key(KeyCode::Down), PermissionMode::ProjectWrite);
        // 第一次 Enter：进入确认子态，不 Apply。
        assert!(matches!(
            picker.handle_key(key(KeyCode::Enter), PermissionMode::ProjectWrite),
            PermissionPickerAction::Continue
        ));
        // 再次 Enter：生效。
        assert!(matches!(
            picker.handle_key(key(KeyCode::Enter), PermissionMode::ProjectWrite),
            PermissionPickerAction::Apply(PermissionMode::FullAccess)
        ));

        // Esc 在确认子态里只退回列表，随后可改选别档。
        let mut picker = PermissionPicker::new(PermissionMode::ProjectWrite);
        picker.handle_key(key(KeyCode::Down), PermissionMode::ProjectWrite);
        picker.handle_key(key(KeyCode::Down), PermissionMode::ProjectWrite);
        picker.handle_key(key(KeyCode::Enter), PermissionMode::ProjectWrite);
        assert!(matches!(
            picker.handle_key(key(KeyCode::Esc), PermissionMode::ProjectWrite),
            PermissionPickerAction::Continue
        ));
        assert!(matches!(
            picker.handle_key(key(KeyCode::Up), PermissionMode::ProjectWrite),
            PermissionPickerAction::Continue
        ));
        assert!(matches!(
            picker.handle_key(key(KeyCode::Enter), PermissionMode::ProjectWrite),
            PermissionPickerAction::Apply(PermissionMode::ProjectWrite)
        ));
    }

    /// 非 FA 档位（与已处于 FA 的 FA）单键直选——确认门只属于升权。
    #[test]
    fn other_modes_apply_directly() {
        let mut picker = PermissionPicker::new(PermissionMode::FullAccess);
        assert!(matches!(
            picker.handle_key(key(KeyCode::Enter), PermissionMode::FullAccess),
            PermissionPickerAction::Apply(PermissionMode::FullAccess)
        ));
        let mut picker = PermissionPicker::new(PermissionMode::FullAccess);
        picker.handle_key(key(KeyCode::Up), PermissionMode::FullAccess);
        picker.handle_key(key(KeyCode::Up), PermissionMode::FullAccess);
        assert!(matches!(
            picker.handle_key(key(KeyCode::Enter), PermissionMode::FullAccess),
            PermissionPickerAction::Apply(PermissionMode::ReadOnly)
        ));
    }
}
