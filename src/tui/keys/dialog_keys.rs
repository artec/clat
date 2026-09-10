//! Modal input ownership. A consumed key never reaches the composer.
use super::*;

impl App {
    pub(super) fn handle_modal_key(&mut self, key: KeyEvent) -> bool {
        if self.handle_permission_dialog_key(key) {
            return true;
        }

        // ask-user 对话框独占按键（S9）：worker 阻塞等待应答，直到选择、
        // 自定义提交或拒绝。
        if self.pending_ask_user.is_some() {
            self.handle_ask_dialog_key(key);
            return true;
        }

        // 信息弹窗（/help、/mcp）独占按键：Esc/Enter 关闭，↑/↓ 逐行、
        // PgUp/PgDn 翻页（步长＝绘制期记录的可视行数；钳制在最大滚
        // 动位）；/mcp 额外接受 `r` 重取状态。
        if self.info_dialog.is_some() {
            let max = self.info_scroll_max;
            let page = self.info_page.max(1);
            let is_mcp = self
                .info_dialog
                .as_ref()
                .is_some_and(|dialog| dialog.kind == InfoDialogKind::Mcp);
            let mut close = false;
            let mut refresh = false;
            if let Some(dialog) = self.info_dialog.as_mut() {
                match key.code {
                    KeyCode::Esc | KeyCode::Enter => close = true,
                    KeyCode::Char('r') | KeyCode::Char('R') if is_mcp => refresh = true,
                    KeyCode::Up => dialog.offset = dialog.offset.saturating_sub(1),
                    KeyCode::Down => dialog.offset = (dialog.offset + 1).min(max),
                    KeyCode::PageUp => dialog.offset = dialog.offset.saturating_sub(page),
                    KeyCode::PageDown => dialog.offset = (dialog.offset + page).min(max),
                    _ => {}
                }
            }
            if close {
                self.info_dialog = None;
            }
            if refresh {
                self.refresh_mcp_view();
            }
            return true;
        }

        // /perm 选择器：独占按键直到选择或取消。
        if self.permission_picker.is_some() {
            // 档位数据源（D-2 §2.6）：dsh = preset 投影（sandbox/mode
            // latest-wins fold 的 journal 值）；local = application 直读。
            let current = if let Some(dsh) = self.dsh.as_ref() {
                dsh.preset
                    .as_deref()
                    .and_then(PermissionMode::from_journal_value)
                    .unwrap_or_default()
            } else {
                self.application
                    .as_ref()
                    .map(|application| application.permission_mode())
                    .unwrap_or_default()
            };
            if let Some(picker) = self.permission_picker.as_mut() {
                let action = picker.handle_key(key, current);
                self.apply_permission_picker_action(action);
            }
            return true;
        }

        // /rename 弹框：独占按键（完整文本编辑 + Enter 提交 / Esc 取消）。
        if self.rename_dialog.is_some() {
            self.handle_rename_dialog_key(key);
            return true;
        }

        // /resume 会话选择器：独占按键直到恢复或取消。
        if self.session_picker.is_some() {
            if let Some(picker) = self.session_picker.as_mut() {
                let action = picker.handle_key(key);
                self.apply_resume_action(action);
            }
            return true;
        }

        // 二级选择器优先于编辑器接管按键。
        if let Some(picker) = self.picker.as_mut() {
            let action = picker.handle_key(key);
            self.apply_picker_action(action);
            return true;
        }

        if let Some(editor) = &mut self.editor {
            let action = editor.handle_key(key);
            self.apply_editor_action(action);
            return true;
        }

        false
    }
    fn handle_permission_dialog_key(&mut self, key: KeyEvent) -> bool {
        // A permission decision is pending: every key belongs to the dialog
        // until the user allows or denies it.
        if self.pending_permission.is_some() {
            // 决策键必须是"裸键"：raw 模式下 Ctrl+W / Alt+Y 等修饰组合也
            // 以 `Char(..)` 形态到达——不挡住它们，Ctrl+W 就成了"切档并
            // 放行"的快捷键（对抗审计 2026-08-19）。CLAT 的输入惯例里
            // Shift/Ctrl/Alt+Enter 都是换行语义，同样不得触发 allow。
            let plain = !key
                .modifiers
                .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SUPER);
            let requested_allow = match key.code {
                KeyCode::Enter => key.modifiers.is_empty(),
                KeyCode::Char('y') | KeyCode::Char('Y') => plain,
                _ => false,
            };
            let deny = match key.code {
                KeyCode::Esc => true,
                KeyCode::Char('n') | KeyCode::Char('N') => plain,
                _ => false,
            };
            // 升级键（P5）：只对 offered 集合生效；与 allow 同受审阅门
            //（未读完参数不允许任何放行类回答）；同样要求裸键。
            let escalate_project_write =
                plain && matches!(key.code, KeyCode::Char('w') | KeyCode::Char('W'));
            let escalate_full_access =
                plain && matches!(key.code, KeyCode::Char('f') | KeyCode::Char('F'));
            let mut blocked_allow = false;
            let mut allow = false;
            let mut escalation: Option<PermissionMode> = None;
            if let Some(pending) = self.pending_permission.as_mut() {
                let max_scroll = pending
                    .argument_line_count
                    .saturating_sub(pending.argument_page_size.max(1));
                match key.code {
                    KeyCode::Down => {
                        pending.argument_scroll =
                            pending.argument_scroll.saturating_add(1).min(max_scroll);
                    }
                    KeyCode::Up => {
                        pending.argument_scroll = pending.argument_scroll.saturating_sub(1);
                    }
                    KeyCode::PageDown => {
                        pending.argument_scroll = pending
                            .argument_scroll
                            .saturating_add(pending.argument_page_size.max(1))
                            .min(max_scroll);
                    }
                    KeyCode::PageUp => {
                        pending.argument_scroll = pending
                            .argument_scroll
                            .saturating_sub(pending.argument_page_size.max(1));
                    }
                    KeyCode::End => pending.argument_scroll = max_scroll,
                    KeyCode::Home => pending.argument_scroll = 0,
                    _ => {}
                }
                if requested_allow {
                    allow = pending.reviewed_to_end;
                    blocked_allow = !allow;
                }
                if escalate_project_write || escalate_full_access {
                    if pending.reviewed_to_end {
                        if escalate_project_write
                            && pending.escalations.contains(&PermissionMode::ProjectWrite)
                        {
                            escalation = Some(PermissionMode::ProjectWrite);
                        } else if escalate_full_access
                            && pending.escalations.contains(&PermissionMode::FullAccess)
                        {
                            escalation = Some(PermissionMode::FullAccess);
                        }
                    } else {
                        // 升级键与 allow 同门：未审完参数时提示而不是无声空转。
                        blocked_allow = true;
                    }
                }
            }
            if (allow || deny || escalation.is_some())
                && let Some(pending) = self.pending_permission.take()
            {
                // 升级 = 先切共享档位（下一次检查即生效）再放行本次调用。
                // 持久化失败不拦放行（内存已切换），警告留在最终 flash 里
                //——先 flash 会被下面的结果 flash 覆盖（对抗审计）。
                let mut persist_warning = None;
                if let Some(mode) = escalation
                    && let Some(application) = &self.application
                    && let Err(error) = application.set_permission_mode(mode)
                {
                    persist_warning = Some(error.to_string());
                }
                let decision = if allow || escalation.is_some() {
                    PermissionDecision::Allow
                } else {
                    PermissionDecision::Deny {
                        reason: "denied by user".into(),
                    }
                };
                let _ = pending.decision_tx.send(decision);
                if let Some(mode) = escalation {
                    match persist_warning {
                        Some(error) => self.flash_status(format!(
                            "permission mode: {mode} — call allowed (not saved to this session: {error})"
                        )),
                        None => {
                            self.flash_status(format!("permission mode: {mode} — call allowed"));
                        }
                    }
                } else if allow {
                    self.flash_status("permission granted");
                } else {
                    self.flash_status("permission denied — informing the model");
                }
            }
            if blocked_allow {
                self.flash_status("review all permission arguments before allowing");
            }
            return true;
        }

        false
    }
}
