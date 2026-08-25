use super::*;
use crate::dsh::backend::DshTask;

/// 粘贴的图片附件判定（M6，纯函数可测）：**整条**粘贴（trim 后）恰好
/// 是一个存在的图片文件**绝对路径**时返回它（`~` 展开；Windows 盘符
/// 路径同放行）。防误判优先：相对路径、含空白/换行、扩展名不认识、
/// 文件不存在、超过 4MB 一律 None——宁可漏判当文本插入，不可把用户
/// 的文字吞成附件。相对路径被排除是刻意的：存在性检查相对进程 cwd
/// 解析，裸文件名（"logo.png"）碰巧同名就会被误判。
pub(super) fn pasted_image_path(text: &str) -> Option<std::path::PathBuf> {
    let trimmed = text.trim();
    if trimmed.is_empty() || trimmed.chars().any(char::is_whitespace) {
        return None;
    }
    if !trimmed.starts_with('/') && !trimmed.starts_with('~') && !trimmed.contains(":\\") {
        return None; // 只认绝对路径 / ~ 前缀 / Windows 盘符
    }
    let candidate = std::path::PathBuf::from(if let Some(rest) = trimmed.strip_prefix('~') {
        let home = std::env::var("HOME").ok()?;
        if rest.is_empty() {
            home
        } else {
            format!("{home}/{rest}")
        }
    } else {
        trimmed.to_owned()
    });
    crate::media::media_type_for_path(&candidate)?;
    let metadata = std::fs::metadata(&candidate).ok()?;
    if !metadata.is_file() {
        return None;
    }
    if metadata.len() > crate::media::MAX_ATTACHMENT_BYTES {
        return None;
    }
    Some(candidate)
}

impl App {
    pub(super) fn handle_ui_event(&mut self, event: UiEvent) {
        match event {
            UiEvent::Terminal(event) => self.handle_terminal_event(event),
            UiEvent::Worker(message) => self.handle_worker_message(message),
            UiEvent::Dsh(event) => self.handle_dsh_event(event),
            UiEvent::Application(ApplicationEvent::MonitorUpdated(value)) => {
                self.balance = value;
            }
            UiEvent::Application(ApplicationEvent::CompactionUpdated(status)) => match status {
                CompactionStatus::Started => self.flash_status("compacting…"),
                CompactionStatus::Finished { note, succeeded } => {
                    self.flash_status(note);
                    // 仅当历史确实收缩（成功且 replace 事件族耐久落盘）时，
                    // 压缩前的水位才过期（TUI-L05：失败/nothing-to-compact
                    // 保留原读数，直到下一次 run 上报新的 usage）。
                    if succeeded {
                        self.last_turn_usage = None;
                    }
                }
            },
            // A4-1（W1-21）：MCP/WASM 启动失败一次性响亮提示（详情 /mcp）。
            UiEvent::Application(ApplicationEvent::McpStartupNotice { failures }) => {
                self.flash_status(format!(
                    "mcp: {failures} tool/server registration issue(s) — /mcp for detail"
                ));
            }
            UiEvent::Application(ApplicationEvent::LanguageIntelligenceNotice { message }) => {
                self.flash_status(message);
            }
            // N2：自动命名/改名落盘成功——右标题即时更新，无需重拉快照。
            UiEvent::Application(ApplicationEvent::TitleUpdated { title }) => {
                self.session_title = Some(title);
            }
            UiEvent::Application(ApplicationEvent::ProcessFinished {
                session_id,
                exit_code,
                signal,
                timed_out,
                cancelled,
                terminated,
            }) => {
                let state = if timed_out {
                    "timed out".to_owned()
                } else if cancelled {
                    "cancelled".to_owned()
                } else if terminated {
                    "terminated".to_owned()
                } else if let Some(signal) = signal {
                    format!("signal {signal}")
                } else {
                    exit_code.map_or_else(|| "finished".into(), |code| format!("exit {code}"))
                };
                self.flash_status(format!("process {session_id} · {state}"));
                self.notify();
            }
        }
    }

    /// 处理一条终端事件：按键/粘贴/鼠标。
    ///
    /// 项目确权门拦截**一切**终端事件：只有按键转给 handle_key（其
    /// 中的确权分支只认 Enter/y 与 Esc/n）。鼠标滚动、拖拽选区、粘贴
    /// 全部吞掉——否则确权框后面还能滚动会话、往输入框粘贴内容，
    /// 甚至选区高亮会盖住对话框边框，看起来像边框被切掉。
    fn handle_terminal_event(&mut self, event: Event) {
        // 焦点事件（B-1，DECSET 1004）：纯状态记录，先于一切门——确权
        // 框/加载门后面焦点照样变化，铃铛判定需要如实的三态；不驱动
        // 任何交互语义（无按键/鼠标语义可借道）。
        match event {
            Event::FocusGained => self.focused = Some(true),
            Event::FocusLost => self.focused = Some(false),
            _ => {}
        }
        if self.trust_prompt {
            if let Event::Key(key) = event
                && key.kind == KeyEventKind::Press
            {
                self.handle_key(key);
            }
            return;
        }
        // 会话加载门（2026-08-19）：后台挂载完成前禁止一切交互——无
        // 会话可提交、无可滚内容、无可粘贴目标；唯一出口是退出键。
        if self.loading.is_some() {
            if let Event::Key(key) = event
                && key.kind == KeyEventKind::Press
                && key.modifiers.contains(KeyModifiers::CONTROL)
                && matches!(key.code, KeyCode::Char('c' | 'C'))
            {
                self.should_quit = true;
            }
            return;
        }
        match event {
            Event::Key(key) if key.kind == KeyEventKind::Press => self.handle_key(key),
            Event::Paste(text) => self.handle_paste(&text),
            // 帮助弹窗模态期间吞掉鼠标：后面无可选内容，避免选区高亮
            // 盖住对话框边框（同确权门的做法）。
            Event::Mouse(mouse) if self.info_dialog.is_none() => self.handle_mouse(mouse),
            _ => {}
        }
    }

    /// 最近一次必须重绘的时刻：状态栏瞬时提示到期、思考动画换帧。
    /// None 表示可以无限挂起等待下一条消息。
    pub(super) fn next_repaint_deadline(&self) -> Option<Instant> {
        let now = Instant::now();
        let mut deadline = self.status_until.filter(|until| *until > now);
        if self.phases.phase.is_some() {
            let frame = now + SPINNER_FRAME;
            deadline = Some(deadline.map_or(frame, |current| current.min(frame)));
        }
        // 加载中每帧轮询后台挂载：没有这条 deadline，空闲时主循环会
        // 无限挂起在 recv 上，交接永远不被发现。
        if self.loading.is_some() {
            let poll = now + Duration::from_millis(50);
            deadline = Some(deadline.map_or(poll, |current| current.min(poll)));
        }
        // dsh 断线自动重连（§0-2）：到点必须唤醒，否则无事件时主循环
        // 无限挂起、重连任务永不发出。
        if let Some(dsh) = &self.dsh
            && let Some(at) = dsh.reconnect_deadline()
        {
            deadline = Some(deadline.map_or(at, |current| current.min(at)));
        }
        deadline
    }

    fn handle_key(&mut self, key: KeyEvent) {
        // Ctrl+O：工具卡三态循环（collapsed → expanded → hidden），任何
        // 时刻可用——纯呈现状态，不持久化（G5）。
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('o') {
            self.card_visibility = self.card_visibility.next();
            self.flash_status(format!("tool cards: {:?}", self.card_visibility));
            return;
        }
        // Ctrl+C：**有选区时优先复制**。原因：Cmd+C 被终端自身截留
        //（鼠标上报模式又禁用了终端原生拖选，终端复制的是空选区），
        // 而多数终端把 Ctrl+Shift+C 编码成 ^C——Ctrl+C 是选区复制唯一
        // 可靠到达的键。无选区（或复制无内容可复制）才走退出。
        if key.modifiers.contains(KeyModifiers::CONTROL)
            && matches!(key.code, KeyCode::Char('c') | KeyCode::Char('C'))
        {
            if self
                .selection
                .as_ref()
                .is_some_and(|selection| !selection.is_empty())
                && self.copy_selection()
            {
                return;
            }
            // Shift 组合（Ctrl+Shift+C）意图是复制而非退出：没选中任何
            // 内容时给出提示，不退出。
            if key.modifiers.contains(KeyModifiers::SHIFT) {
                self.flash_status("nothing selected to copy — drag to select");
                return;
            }
            self.should_quit = true;
            return;
        }

        // 项目确权门优先于一切按键交互：未信任的目录只认
        // Enter/y（信任并持久化）与 Esc/n（直接退出 CLAT）。
        // 信任成功后才初始化项目资源（会话/历史/MCP），失败保持阻断。
        if self.trust_prompt {
            let trust = matches!(
                key.code,
                KeyCode::Enter | KeyCode::Char('y') | KeyCode::Char('Y')
            );
            let leave = matches!(
                key.code,
                KeyCode::Esc | KeyCode::Char('n') | KeyCode::Char('N')
            );
            if trust {
                // authorize_and_mount 消费 bootstrap（lease → preflight →
                // 控制面提交 → 挂载）；失败时重开 bootstrap 保持阻断。
                let bootstrap = self
                    .bootstrap
                    .take()
                    .ok_or_else(|| "bootstrap scope is unavailable".to_owned());
                let trusted = bootstrap.and_then(|bootstrap| {
                    bootstrap
                        .with_permission_modes()
                        .authorize_and_mount(ProjectAuthorization::grant())
                        .map_err(|error| error.to_string())
                });
                match trusted {
                    Ok(application) => {
                        self.application = Some(application);
                        // 订阅余额/压缩事件流：确权晚于 run() 启动期的
                        // wire_application_events，此处补挂（同时修复
                        // 确权路径从未订阅的历史缺口）。
                        self.wire_application_events();
                        if let Err(error) = self.adopt_snapshot() {
                            self.flash_status(format!("failed to trust project: {error}"));
                            return;
                        }
                        self.trust_prompt = false;
                        self.flash_status("project trusted — welcome");
                    }
                    Err(error) => {
                        self.bootstrap =
                            BootstrapApplication::open_default(self.project.clone()).ok();
                        self.flash_status(format!("failed to trust project: {error}"));
                    }
                }
            } else if leave {
                self.should_quit = true;
            }
            return;
        }

        // Cmd+C / Ctrl+Shift+C 复制选区，Cmd+X / Ctrl+Shift+X 剪切输入
        // 选区。没有选区时不拦截，按键走原有处理。Cmd+V / Ctrl+Shift+V
        // 的粘贴由终端通过 bracketed paste（Event::Paste）送达，这里
        // 仅拦截字符本身，避免把 'v' 插进输入。
        let copy_or_cut = key.modifiers.contains(KeyModifiers::SUPER)
            || (key.modifiers.contains(KeyModifiers::CONTROL)
                && key.modifiers.contains(KeyModifiers::SHIFT));
        if copy_or_cut && self.editor.is_none() && self.picker.is_none() {
            match key.code {
                KeyCode::Char('c') | KeyCode::Char('C') if self.copy_selection() => {
                    return;
                }
                KeyCode::Char('x') | KeyCode::Char('X') if self.cut_selection() => {
                    return;
                }
                KeyCode::Char('v') | KeyCode::Char('V') => return,
                _ => {}
            }
        }

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
            return;
        }

        // ask-user 对话框独占按键（S9）：worker 阻塞等待应答，直到选择、
        // 自定义提交或拒绝。
        if self.pending_ask_user.is_some() {
            self.handle_ask_dialog_key(key);
            return;
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
            return;
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
            return;
        }

        // /rename 弹框：独占按键（完整文本编辑 + Enter 提交 / Esc 取消）。
        if self.rename_dialog.is_some() {
            self.handle_rename_dialog_key(key);
            return;
        }

        // /resume 会话选择器：独占按键直到恢复或取消。
        if self.session_picker.is_some() {
            if let Some(picker) = self.session_picker.as_mut() {
                let action = picker.handle_key(key);
                self.apply_resume_action(action);
            }
            return;
        }

        // 二级选择器优先于编辑器接管按键。
        if let Some(picker) = self.picker.as_mut() {
            let action = picker.handle_key(key);
            self.apply_picker_action(action);
            return;
        }

        if let Some(editor) = &mut self.editor {
            let action = editor.handle_key(key);
            self.apply_editor_action(action);
            return;
        }

        match key.code {
            // 输入在 run 进行中保持可用：Enter 变为插话（DSH "steer
            // while running, send while idle"），其余编辑键不变。
            KeyCode::Enter => {
                // Claude Code style: Shift+Enter (or Alt+Enter) inserts a
                // line break, plain Enter submits. Ctrl+J is the fallback
                // for terminals that cannot distinguish Shift+Enter.
                if key
                    .modifiers
                    .intersects(KeyModifiers::SHIFT | KeyModifiers::ALT)
                {
                    self.input.insert_newline();
                } else if self.running {
                    self.steer_input();
                } else {
                    self.submit_input();
                }
            }
            KeyCode::Char('j') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.input.insert_newline();
            }
            KeyCode::Backspace => self.input.backspace(),
            KeyCode::Delete => self.input.delete(),
            KeyCode::Left => self.input.left(),
            KeyCode::Right => self.input.right(),
            KeyCode::Home => self.input.home(),
            KeyCode::End => self.input.end(),
            KeyCode::Up => {
                // With no input history to recall, the arrows scroll the
                // conversation instead of doing nothing.
                if self.input.history_is_empty() {
                    self.scroll_up(WHEEL_SCROLL_ROWS);
                } else {
                    self.input.history_previous();
                }
            }
            KeyCode::Down => {
                if self.input.history_is_empty() {
                    self.scroll_down(WHEEL_SCROLL_ROWS);
                } else {
                    self.input.history_next();
                }
            }
            KeyCode::PageUp => self.scroll_up(PAGE_SCROLL_ROWS),
            KeyCode::PageDown => self.scroll_down(PAGE_SCROLL_ROWS),
            // Shift+Tab 循环思考档位（Low→High→Max→Low）。不 gate
            // running：配置每次 run 重读，对下一次 run 生效；当前 run
            // 不受影响。dsh 态同键循环宿主档位（档位接入 2026-08-23，
            // /model 的 efforts 表——adapter 自有词汇，非本地枚举）。
            KeyCode::BackTab => {
                if self.dsh.is_some() {
                    self.cycle_dsh_effort();
                } else {
                    self.cycle_thinking_level();
                }
            }
            KeyCode::Esc => {
                if self.running {
                    // dsh：无栈式召回（DSH 无 recall API，INV-U3 例外②）
                    // ——直接取消宿主 turn。
                    if self.dsh.is_some() {
                        self.cancel_dsh();
                        return;
                    }
                    // 栈式 ESC（INV-SV4）：先撤最近的用户动作——有未
                    // claim 的插话先召回（文本退回编辑框，可改可重发，
                    // run 不受影响）；队列空了才轮到取消 run。已被
                    // claim 的消息 core 侧返回 None，自然落到取消路径。
                    let recalled = self
                        .application
                        .as_ref()
                        .and_then(|application| application.recall_pending_steering());
                    match recalled {
                        Some(text) => {
                            // core 召回成功 ⇒ pending 区必含对应回显
                            //（入队与回显同路径）；区侧同步弹出最后一条。
                            // 回填按发送顺序排列、换行分隔（多次召回时
                            // 先发的想法靠前——见 prepend_recalled_line）。
                            self.conversation.recall_pending_steering();
                            self.input.prepend_recalled_line(&text);
                            self.flash_status("steering recalled — edit it, Enter requeues");
                        }
                        None => {
                            if let Some(handle) = &self.run_handle {
                                handle.cancel();
                                self.flash_status("cancelling…");
                            }
                        }
                    }
                } else if let Some(handle) = self
                    .compact_handle
                    .as_ref()
                    .filter(|handle| !handle.is_finished())
                {
                    // 与 Run 取消一致：只发令牌不 join——摘要请求带 60s
                    // 总截止，join 最长会冻结 UI 一分钟；完成事件（失败
                    // 文本）异步回流覆盖状态栏。
                    handle.cancel();
                    self.flash_status("cancelling compaction…");
                } else {
                    // 空闲 Esc：清输入连同未发送的附件。
                    self.input.clear();
                    self.attachments.clear();
                }
            }
            KeyCode::Char(ch) => self.input.insert_char(ch),
            _ => {}
        }
    }

    fn handle_paste(&mut self, text: &str) {
        // 选择器、问答对话框、信息弹窗与权限选择器没有文本输入目标，
        // 忽略粘贴；/rename 弹框有自己的编辑目标。
        if self.picker.is_none()
            && self.pending_ask_user.is_none()
            && self.info_dialog.is_none()
            && self.permission_picker.is_none()
        {
            if let Some(dialog) = &mut self.rename_dialog {
                dialog.buffer.insert_str(text);
            } else if let Some(editor) = &mut self.editor {
                editor.handle_paste(text);
            } else if !self.running
                && let Some(image) = pasted_image_path(text)
            {
                // 拖图进终端 = 粘贴绝对路径：识别为附件而非文本。仅
                // 空闲态——运行中的粘贴是 steering 文本（附件只能随
                // 新消息走）。判定失败（混合文本/不存在/超大）回落为
                // 普通文本插入。
                self.attachments.push(image);
                let count = self.attachments.len();
                self.flash_status(format!(
                    "image attached ({count}) — Enter sends it with your message · Esc drops it"
                ));
            } else {
                self.input.insert_str(text);
            }
        }
    }

    /// /perm 选择器的动作：应用 = 写共享 cell（下一次权限检查
    /// 生效，P3）+ flash；取消只关框。Application 缺席（未确权）不可达
    /// ——弹框只在 Some 时打开。
    fn apply_permission_picker_action(
        &mut self,
        action: crate::tui::permission_picker::PermissionPickerAction,
    ) {
        use crate::tui::permission_picker::PermissionPickerAction;
        match action {
            PermissionPickerAction::Continue => {}
            PermissionPickerAction::Cancel => {
                self.permission_picker = None;
                self.flash_status("permission mode unchanged");
            }
            PermissionPickerAction::Apply(mode) => {
                self.permission_picker = None;
                // dsh（§2.5 拍板通道核实）：/permission 走 prompt 通道
                //（web 客户端 PermissionSelect 同款），宿主落定文本经
                // 事件流回显、preset 投影由 sandbox/mode fold 刷新。
                if let Some(dsh) = self.dsh.as_ref() {
                    match dsh.current_session().map(str::to_owned) {
                        Some(session) => {
                            dsh.send_task(DshTask::Prompt {
                                session,
                                steer: false,
                                text: format!("/permission {}", mode.journal_value()),
                            });
                            self.flash_status("switching permission…");
                        }
                        None => self.flash_status("no active session"),
                    }
                    return;
                }
                if let Some(application) = &self.application {
                    // journal 写失败不回滚内存档位（本进程行为已生效），
                    // 只提示；同值切换零事件。
                    if let Err(error) = application.set_permission_mode(mode) {
                        self.flash_status(format!(
                            "permission mode: {mode} (not saved to this session: {error})"
                        ));
                    } else {
                        self.flash_status(format!("permission mode: {mode}"));
                    }
                }
            }
        }
    }

    /// /rename 弹框键位：完整单行编辑（InputBuffer），Enter 提交（空文
    /// 本 flash 拒绝、不关框）、Esc 取消。提交走
    /// `Application::rename_session`（Force + User 语义 + 清洗）。
    fn handle_rename_dialog_key(&mut self, key: KeyEvent) {
        enum Outcome {
            Pending,
            Commit(String),
            Close,
        }
        let mut outcome = Outcome::Pending;
        if let Some(dialog) = self.rename_dialog.as_mut() {
            let buffer = &mut dialog.buffer;
            match key.code {
                KeyCode::Enter => {
                    if key
                        .modifiers
                        .intersects(KeyModifiers::SHIFT | KeyModifiers::ALT)
                    {
                        buffer.insert_newline();
                    } else {
                        let text = buffer.text().trim().to_owned();
                        if text.is_empty() {
                            self.flash_status("name is empty");
                        } else {
                            outcome = Outcome::Commit(text);
                        }
                    }
                }
                KeyCode::Char('j') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    buffer.insert_newline();
                }
                KeyCode::Esc => outcome = Outcome::Close,
                KeyCode::Backspace => buffer.backspace(),
                KeyCode::Delete => buffer.delete(),
                KeyCode::Left => buffer.left(),
                KeyCode::Right => buffer.right(),
                KeyCode::Home => buffer.home(),
                KeyCode::End => buffer.end(),
                KeyCode::Char(ch) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                    buffer.insert_char(ch);
                }
                _ => {}
            }
        }
        match outcome {
            Outcome::Pending => {}
            Outcome::Close => self.rename_dialog = None,
            Outcome::Commit(name) => {
                // dsh（§2.5）：session.rename API；标题即时更新（宿主
                // 回执只回 status，不带回标题）。
                if let Some(dsh) = self.dsh.as_ref() {
                    match dsh.current_session().map(str::to_owned) {
                        Some(session) => {
                            dsh.send_task(DshTask::Rename {
                                session,
                                title: name.clone(),
                            });
                            self.session_title = Some(name);
                            self.rename_dialog = None;
                            self.flash_status("renaming…");
                        }
                        None => self.flash_status("no active session"),
                    }
                    return;
                }
                match self
                    .application
                    .as_mut()
                    .map(|application| application.rename_session(&name))
                {
                    Some(Ok(RenameOutcome::Renamed { title })) => {
                        self.session_title = Some(title);
                        self.rename_dialog = None;
                        self.flash_status("conversation renamed");
                    }
                    Some(Ok(RenameOutcome::Invalid)) => self.flash_status("name is empty"),
                    Some(Ok(RenameOutcome::NoSession)) => {
                        self.rename_dialog = None;
                        self.flash_status("no active conversation");
                    }
                    Some(Err(error)) => {
                        self.flash_status(format!("rename failed: {error}"));
                    }
                    None => self.flash_status("project application is unavailable"),
                }
            }
        }
    }

    /// ask-user 对话框键位。选项模式：↑↓ 移动（末行是"自定义输入"），
    /// Enter 选中，c 直接进输入，Esc 拒绝。输入模式：Enter 提交非空
    /// 文本，Backspace 删字符，Esc 有选项时返回选项、无选项时拒绝。
    fn handle_ask_dialog_key(&mut self, key: KeyEvent) {
        enum Resolution {
            Pending,
            Answer(crate::interaction::AskAnswer),
        }
        let mut resolution = Resolution::Pending;
        if let Some(pending) = self.pending_ask_user.as_mut() {
            let has_options = !pending.question.options.is_empty();
            if let Some(text) = pending.custom.as_mut() {
                match key.code {
                    KeyCode::Enter if !text.trim().is_empty() => {
                        resolution = Resolution::Answer(crate::interaction::AskAnswer::Custom(
                            std::mem::take(text),
                        ));
                    }
                    KeyCode::Backspace => {
                        text.pop();
                    }
                    KeyCode::Esc => {
                        if has_options {
                            pending.custom = None;
                        } else {
                            resolution =
                                Resolution::Answer(crate::interaction::AskAnswer::Declined);
                        }
                    }
                    KeyCode::Char(ch) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                        text.push(ch);
                    }
                    _ => {}
                }
            } else {
                // 行数 = 选项 + 可选的"自定义输入"行。
                let rows =
                    pending.question.options.len() + usize::from(pending.question.allow_custom);
                let custom_row = pending.question.allow_custom;
                match key.code {
                    KeyCode::Up => pending.selection = pending.selection.saturating_sub(1),
                    KeyCode::Down => {
                        pending.selection = (pending.selection + 1).min(rows.saturating_sub(1))
                    }
                    KeyCode::Char('c') | KeyCode::Char('C') if custom_row => {
                        pending.custom = Some(String::new());
                    }
                    KeyCode::Enter => {
                        if custom_row && pending.selection == pending.question.options.len() {
                            pending.custom = Some(String::new());
                        } else if let Some(option) = pending.question.options.get(pending.selection)
                        {
                            resolution = Resolution::Answer(
                                crate::interaction::AskAnswer::Selected(option.label.clone()),
                            );
                        }
                    }
                    KeyCode::Esc => {
                        resolution = Resolution::Answer(crate::interaction::AskAnswer::Declined);
                    }
                    _ => {}
                }
            }
        }
        if let Resolution::Answer(answer) = resolution
            && let Some(pending) = self.pending_ask_user.take()
        {
            let note = match &answer {
                crate::interaction::AskAnswer::Selected(label)
                | crate::interaction::AskAnswer::Custom(label) => {
                    format!("answered: {label}")
                }
                crate::interaction::AskAnswer::Declined => {
                    "declined — the model continues without an answer".to_owned()
                }
            };
            let _ = pending.answer_tx.send(answer);
            self.flash_status(note);
        }
    }

    fn handle_mouse(&mut self, mouse: MouseEvent) {
        if let (Some(picker), Some(area)) = (self.session_picker.as_mut(), self.editor_area) {
            let action = picker.handle_mouse(mouse, area);
            self.apply_resume_action(action);
            return;
        }
        if let (Some(picker), Some(area)) = (self.picker.as_mut(), self.editor_area) {
            let action = picker.handle_mouse(mouse, area);
            self.apply_picker_action(action);
            return;
        }
        if let (Some(editor), Some(area)) = (&mut self.editor, self.editor_area) {
            let action = editor.handle_mouse(mouse, area);
            self.apply_editor_action(action);
            return;
        }
        // The wheel always scrolls the conversation, wherever the pointer
        // is: terminals report wheel positions unreliably, and a scoped
        // check would swallow the event.
        match mouse.kind {
            MouseEventKind::ScrollUp => self.scroll_up(WHEEL_SCROLL_ROWS),
            MouseEventKind::ScrollDown => self.scroll_down(WHEEL_SCROLL_ROWS),
            MouseEventKind::Down(MouseButton::Left) => {
                // 在会话/输入框内按下：开始拖拽选区；点在其他位置清空。
                self.selection =
                    self.selection_target(mouse.column, mouse.row)
                        .map(|(kind, pos)| TextSelection {
                            kind,
                            anchor: pos,
                            head: pos,
                            active: true,
                        });
            }
            MouseEventKind::Drag(MouseButton::Left) => {
                if let Some(kind) = self
                    .selection
                    .as_ref()
                    .filter(|selection| selection.active)
                    .map(|selection| selection.kind)
                {
                    let head = self.clamped_selection_pos(kind, mouse.column, mouse.row);
                    if let Some(selection) = self.selection.as_mut() {
                        selection.head = head;
                    }
                }
            }
            MouseEventKind::Up(MouseButton::Left)
                if self
                    .selection
                    .as_ref()
                    .is_some_and(|selection| selection.active) =>
            {
                self.finish_mouse_selection();
            }
            _ => {}
        }
    }

    /// 按下位置落在哪个组件的内容区内，返回对应的选区类型和内容坐标。
    fn selection_target(&self, x: u16, y: u16) -> Option<(SelectionKind, SelectionPos)> {
        if let Some(pos) = content_pos(self.conversation_area, x, y) {
            // 视口内的行号加上滚动偏移才是内容行号。
            let row =
                (self.conversation_start + pos.row).min(self.conversation_rows.saturating_sub(1));
            Some((
                SelectionKind::Conversation,
                SelectionPos { row, col: pos.col },
            ))
        } else if content_pos(self.input_area, x, y).is_some() {
            // 输入框行数由内容决定，按下位置必然在有效行内；列坐标
            // 跳过行首箭头前缀，与文本坐标对齐。
            let pos = clamped_pos(
                self.input_area,
                self.input.visual_rows(self.input_text_width()).len(),
                x,
                y,
            );
            Some((
                SelectionKind::Input,
                SelectionPos {
                    row: pos.row,
                    col: pos.col.saturating_sub(INPUT_MARKER_WIDTH),
                },
            ))
        } else {
            None
        }
    }

    /// 拖动出界时把 head 钳制在组件内容区内。
    fn clamped_selection_pos(&self, kind: SelectionKind, x: u16, y: u16) -> SelectionPos {
        match kind {
            SelectionKind::Conversation => {
                let pos = clamped_pos(self.conversation_area, self.conversation_rows, x, y);
                let row = (self.conversation_start + pos.row)
                    .min(self.conversation_rows.saturating_sub(1));
                SelectionPos { row, col: pos.col }
            }
            SelectionKind::Input => {
                let pos = clamped_pos(
                    self.input_area,
                    self.input.visual_rows(self.input_text_width()).len(),
                    x,
                    y,
                );
                SelectionPos {
                    row: pos.row,
                    col: pos.col.saturating_sub(INPUT_MARKER_WIDTH),
                }
            }
        }
    }

    /// 松开鼠标：空选区时单击定位输入光标；非空选区保持高亮，等用户
    /// 显式复制（Cmd+C / Ctrl+Shift+C）。选中即复制会静默覆盖系统
    /// 剪贴板，且 OSC 52 在不支持的终端上假报成功，已移除。
    /// 松开鼠标：空选区时单击定位输入光标；非空选区保持高亮并**立即
    /// 复制到系统剪贴板**（OSC 52）。"选中即复制"于 2026-08-19 按用户
    /// 决策恢复——覆盖系统剪贴板正是预期行为；Ctrl+C 保留为显式重试
    /// 路径（复制失败或想重发时），Shift+拖选走终端原生选区。
    fn finish_mouse_selection(&mut self) {
        let Some(selection) = self.selection.as_mut() else {
            return;
        };
        selection.active = false;
        if selection.is_empty() {
            let kind = selection.kind;
            let (row, col) = (selection.head.row, selection.head.col);
            if kind == SelectionKind::Input {
                let index = self.input.char_index_at(self.input_text_width(), row, col);
                self.input.set_cursor(index);
            }
            self.selection = None;
            return;
        }
        if let Some(text) = self.selection_text().filter(|text| !text.is_empty()) {
            let count = text.chars().count();
            // FIX-5/CA-08：编码后经注入写出口（测试零真实终端副作用）。
            let copied =
                osc52_copy_bytes(&text).is_some_and(|bytes| (self.clipboard_writer)(&bytes));
            if copied {
                self.flash_status(format!(
                    "copied {count} chars · Shift+drag uses the terminal's own selection"
                ));
            } else {
                self.flash_status("clipboard copy failed — Ctrl+C retries");
            }
        }
    }

    /// 提取当前选区的文本。会话区按渲染行拼接（跨行以 \n 连接），
    /// 输入框按视觉行拼接。
    pub(super) fn selection_text(&mut self) -> Option<String> {
        let selection = self.selection?;
        if selection.is_empty() {
            return None;
        }
        let (from, to) = selection.ordered();
        let mut pieces = Vec::new();
        match selection.kind {
            SelectionKind::Conversation => {
                // 复制的折行宽度必须与渲染同源：长行在错误宽度下重取
                // 行文本，拷出的内容与显示错位。
                let width = conversation_wrap_width(self.conversation_area);
                let total = self.conversation_total_lines(width);
                let last = to.row.min(total.saturating_sub(1));
                for row in from.row..=last {
                    let text = self
                        .conversation
                        .row_plain_text(row, width, self.card_visibility);
                    let start = if row == from.row { from.col } else { 0 };
                    let end = if row == to.row { to.col } else { usize::MAX };
                    pieces.push(slice_by_columns(&text, start, end));
                }
            }
            SelectionKind::Input => {
                let width = self.input_area.width.saturating_sub(2).max(1) as usize;
                let rows = self.input.visual_rows(width);
                let last = to.row.min(rows.len().saturating_sub(1));
                for (row, text) in rows.iter().enumerate().take(last + 1).skip(from.row) {
                    let start = if row == from.row { from.col } else { 0 };
                    let end = if row == to.row { to.col } else { usize::MAX };
                    pieces.push(slice_by_columns(text, start, end));
                }
            }
        }
        Some(pieces.join("\n"))
    }

    /// 输入框选区对应的字节区间（剪切用），按源文本而不是视觉行计算。
    fn input_selection_range(&self) -> Option<(usize, usize)> {
        let selection = self
            .selection
            .filter(|selection| selection.kind == SelectionKind::Input)?;
        if selection.is_empty() {
            return None;
        }
        let width = self.input_text_width();
        let a = self
            .input
            .char_index_at(width, selection.anchor.row, selection.anchor.col);
        let b = self
            .input
            .char_index_at(width, selection.head.row, selection.head.col);
        Some(if a <= b { (a, b) } else { (b, a) })
    }

    /// Cmd+C / Ctrl+Shift+C：复制当前选区。返回是否拦截了按键。
    fn copy_selection(&mut self) -> bool {
        let Some(text) = self.selection_text().filter(|text| !text.is_empty()) else {
            return false;
        };
        let count = text.chars().count();
        // FIX-5/CA-08：编码后经注入写出口。
        let copied = osc52_copy_bytes(&text).is_some_and(|bytes| (self.clipboard_writer)(&bytes));
        if copied {
            self.flash_status(format!("copied {count} chars"));
        } else {
            self.flash_status("clipboard copy failed");
        }
        true
    }

    /// Cmd+X / Ctrl+Shift+X：剪切输入框选区（复制并从输入中删除）。
    fn cut_selection(&mut self) -> bool {
        if self.running {
            return false;
        }
        let Some((start, end)) = self.input_selection_range() else {
            return false;
        };
        let text = self.input.remove_range(start, end);
        if text.is_empty() {
            return false;
        }
        let count = text.chars().count();
        // FIX-5/CA-08：编码后经注入写出口。
        if let Some(bytes) = osc52_copy_bytes(&text) {
            let _ = (self.clipboard_writer)(&bytes);
        }
        self.flash_status(format!("cut {count} chars"));
        self.selection = None;
        true
    }
}
