use super::*;

impl App {
    pub(super) fn scroll_up(&mut self, amount: usize) {
        self.conversation_scroll_from_bottom = self
            .conversation_scroll_from_bottom
            .saturating_add(amount)
            .min(10_000);
    }

    pub(super) fn scroll_down(&mut self, amount: usize) {
        self.conversation_scroll_from_bottom =
            self.conversation_scroll_from_bottom.saturating_sub(amount);
    }

    /// 输入框文本的可用宽度：内容区宽减去行首前缀（`❯ ` / 两个空格）。
    /// 换行、光标定位与鼠标选区映射统一使用该宽度。
    pub(super) fn input_text_width(&self) -> usize {
        self.input_area
            .width
            .saturating_sub(2)
            .saturating_sub(INPUT_MARKER_WIDTH as u16)
            .max(1) as usize
    }

    pub(super) fn draw(&mut self, frame: &mut Frame) {
        let area = frame.area();
        // 未确权：不渲染主界面（会话/输入框/状态栏全部不可见），
        // 清屏后只画确权对话框——没有可滚动的层、没有可输入的框、
        // 没有闪烁的光标，确权是唯一可能的交互。
        if self.trust_prompt {
            frame.render_widget(Clear, area);
            render_trust_dialog(frame, area, self.project.root());
            return;
        }
        let tick = self.animation_tick();
        // 流式 assistant 前缀的活动帧：run 进行中为 spinner（等待首
        // token / 长思考时转录区不再是一动不动的 ⏺），run 结束落定。
        // 太阳帧保持圆形与灰色，不与状态栏的蓝色盲文 spinner 重复。
        let streaming = self.running.then(|| marker_frame(tick));
        self.conversation.set_stream_marker(streaming);
        // 瞬时提示到期回落为常驻状态（当前目录）。
        self.expire_status();
        // The input box grows with the number of wrapped lines, up to
        // eight content rows, Claude Code style. 行首箭头前缀占 2 列，
        // 换行宽度随之收窄。
        let input_width = area
            .width
            .saturating_sub(2)
            .saturating_sub(INPUT_MARKER_WIDTH as u16)
            .max(1) as usize;
        let input_rows =
            (self.input.line_count(input_width) + 2 + usize::from(!self.attachments.is_empty()))
                .clamp(3, 10);
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(3),
                Constraint::Min(6),
                Constraint::Length(input_rows as u16),
                Constraint::Length(1),
            ])
            .split(area);
        self.input_area = chunks[2];
        self.conversation_area = chunks[1];

        self.draw_header(frame, chunks[0]);
        self.draw_conversation(frame, chunks[1]);
        self.draw_input(frame, chunks[2]);
        // 状态栏：左边是 storage 等常规状态，最右边是模型遥测
        // （Wallet/Token · Cache% · Context current/total）。窄终端时
        // 左侧保底 MIN_STATUS_LEFT，右侧按优先级让位（TUI-L02）。
        // 左右各留 1 列边距，文字不贴终端边缘。
        let bar = chunks[3].inner(Margin::new(1, 0));
        // dsh 态右侧段：Wallet 隐藏（余额是本地 Monitor 对本地 key 的
        // 监视，与宿主模型无关），Cache/Context 按 DSH 口径投影（§2.4）；
        // 数据缺席整段隐藏（INV-U7）。
        let segments = if self.dsh.is_some() {
            self.dsh_status_segments()
        } else {
            status_suffix_segments(
                &self.config,
                &self.balance,
                // INV-C1：Cache 口径取当前模型路由的桶。桶缺席 = 该路由尚无
                // 数据（刚切来的模型），`--%` 是诚实值。
                current_route_usage(&self.usage_routes, &self.config),
                self.last_turn_usage.as_ref(),
            )
        };
        let budget = (bar.width.saturating_sub(MIN_STATUS_LEFT + 2)) as usize;
        let suffix = fit_status_suffix(&segments, budget);
        let status_line = if let Some(phase) = self.phases.phase {
            phase_line(
                tick,
                phase,
                self.phase_elapsed(),
                self.run_elapsed(),
                self.conversation.pending_steering_count(),
            )
        } else {
            Line::from(self.status.as_str())
        };
        if suffix.is_empty() {
            frame.render_widget(Paragraph::new(status_line), bar);
        } else {
            // 右侧后缀按内容宽度分配，剩余空间全部留给左侧状态。
            let suffix_width = UnicodeWidthStr::width(suffix.as_str()) as u16;
            let status_width = bar.width.saturating_sub(suffix_width + 2);
            let columns = Layout::default()
                .direction(Direction::Horizontal)
                .constraints([Constraint::Length(status_width), Constraint::Min(0)])
                .split(bar);
            frame.render_widget(
                Paragraph::new(status_line).wrap(Wrap { trim: false }),
                columns[0],
            );
            frame.render_widget(Paragraph::new(suffix).right_aligned(), columns[1]);
        }

        // 统一模态压暗层（弹窗规范 2026-08-19）：所有弹窗——异步的
        // 权限/ask 与同步的选择器/编辑器——绘制前全屏叠加 DIM，只降
        // 亮度、不清内容；弹窗保持全亮。起因是真实事故：权限框与背景
        // 同亮度被当成背景忽略。不支持 faint 的终端优雅退化为仅剩
        // 边框色对比。压暗必须先于下方弹窗链绘制。
        if self.pending_ask_user.is_some()
            || self.pending_permission.is_some()
            || self.session_picker.is_some()
            || self.picker.is_some()
            || self.editor.is_some()
            || self.info_dialog.is_some()
            || self.permission_picker.is_some()
            || self.rename_dialog.is_some()
        {
            frame.render_widget(
                Block::default().style(Style::default().add_modifier(Modifier::DIM)),
                frame.area(),
            );
        }

        if let Some(picker) = &self.session_picker {
            let height = (picker.row_count() as u16 + 4).min(popup_height_cap(area));
            let picker_area = centered_rect(84, height.max(6), area);
            self.editor_area = Some(picker_area);
            picker.draw(frame, picker_area);
        } else if let Some(picker) = &self.picker {
            // 高度=内容精确高度（行 + 空行 + 说明行 + 双边框）：不设
            // max 兜底——此前 max(8) 把小列表（单模型二级/短档案列表）
            // 人为撑高，钉底的说明行与内容之间出现多余空行、各级弹框
            // 观感不一（2026-08-22 用户反馈）。超高列表由 draw 内部的
            // 钉底布局裁剪内容行，说明行永不丢失。
            let height = (picker.row_count() as u16 + 4).min(popup_height_cap(area));
            // 84%：与其余弹窗统一（弹窗规范 2026-08-19）。94% 在宽终端
            // 上每边仅留 3%，视觉上与贴墙无异（用户实测报告撞墙）。
            let picker_area = centered_rect(84, height, area);
            self.editor_area = Some(picker_area);
            picker.draw(frame, picker_area);
        } else if let Some(editor) = &self.editor {
            // 同上：内容精确高度（编辑器收起时也有 7 行，无需 min 兜底）。
            let height = (editor.row_count() as u16 + 4).min(popup_height_cap(area));
            // 84%：同上——选择器与编辑器是仅有的两个 94% 弹窗，统一后
            // 全部弹窗同宽族、同最小边距（POPUP_H_MARGIN 钳制兜底）。
            let editor_area = centered_rect(84, height, area);
            self.editor_area = Some(editor_area);
            editor.draw(frame, editor_area);
        } else {
            self.editor_area = None;
            // 运行中也显示光标：输入框此时是 steering 编辑器；加载中
            // 输入被禁用，不显示光标（不暗示可输入）。
            if self.input_area.width > 2 && self.input_area.height > 2 && self.loading.is_none() {
                let (row, column) = self.input.cursor_position(self.input_text_width());
                let visible_rows = self.input_area.height.saturating_sub(2) as usize;
                let row = row.min(visible_rows.saturating_sub(1));
                // 光标跳过行首箭头前缀（`❯ ` / 两个空格）与附件徽标行。
                let attachment_offset = usize::from(!self.attachments.is_empty());
                frame.set_cursor_position((
                    self.input_area.x + 1 + INPUT_MARKER_WIDTH as u16 + column as u16,
                    self.input_area.y + 1 + (row + attachment_offset) as u16,
                ));
            }
        }

        if self.pending_ask_user.is_some() {
            self.draw_ask_dialog(frame);
        }
        if self.pending_permission.is_some() {
            self.draw_permission_dialog(frame);
        }
        if let Some(dialog) = &self.info_dialog {
            match dialog.kind {
                InfoDialogKind::Help => self.draw_help_dialog(frame),
                InfoDialogKind::Mcp => self.draw_mcp_dialog(frame),
            }
        }
        if let Some(picker) = &self.permission_picker {
            let current = self
                .application
                .as_ref()
                .map(|application| application.permission_mode())
                .unwrap_or_default();
            picker.draw(frame, area, current);
        }
        if self.rename_dialog.is_some() {
            self.draw_rename_dialog(frame);
        }
    }

    /// /mcp 弹窗内 `r` 刷新：从 Application 重取 MCP 状态并复位滚动
    /// （内容行数可能变化，旧滚动位不再有意义）。Application 缺席
    /// （未确权/已关闭）时保留原视图。
    pub(super) fn refresh_mcp_view(&mut self) {
        let refreshed = self
            .application
            .as_ref()
            .map(|application| application.mcp_status());
        if let Some(refreshed) = refreshed {
            self.mcp_view = Some(refreshed);
        }
        if let Some(dialog) = self.info_dialog.as_mut() {
            dialog.offset = 0;
        }
    }

    /// /help 帮助弹窗（2026-08-19）：黄框 + 压暗 + 四边边距（弹窗规范
    /// 同其余弹窗）；内容按内宽折行，超出可视高度滚动（↑/↓/PgUp/PgDn），
    /// 脚注常驻提示键位与是否还有下文。滚动位与翻页步长在绘制期
    /// 记录，供按键钳制。
    ///
    /// 高度内容驱动（2026-08-19 第三轮反馈）：行数 + 边框 + 脚注，钳在
    /// 高度预算内——短内容得到小框、上下留出真实边距；只有内容超长
    /// 时才贴满预算（旧实现恒取满额高度，内容再少也是整屏框）。
    pub(super) fn draw_help_dialog(&mut self, frame: &mut Frame) {
        let area = frame.area();
        let inner_width = popup_inner_width(84, area);
        let commands = self.help_commands.clone();
        let lines = help_dialog_lines(inner_width, &commands);
        let dialog = centered_rect(84, content_dialog_height(lines.len(), area), area);
        // 可视行数：内框（去边框）减空行与脚注各一行。
        let visible = (dialog.height.saturating_sub(2 + 2)) as usize;
        let max_scroll = lines.len().saturating_sub(visible);
        self.info_scroll_max = max_scroll;
        self.info_page = visible.max(1);
        let offset = self
            .info_dialog
            .as_ref()
            .map(|dialog| dialog.offset)
            .unwrap_or(0)
            .min(max_scroll);
        let mut body: Vec<Line<'static>> = lines.into_iter().skip(offset).take(visible).collect();
        let footer = if max_scroll > 0 {
            if offset < max_scroll {
                " ↑↓/PgUp/PgDn scroll · more below · Esc close "
            } else {
                " ↑↓/PgUp/PgDn scroll · end · Esc close "
            }
        } else {
            " Esc close "
        };
        // 空行钉在脚注上方（2026-08-21 统一：与其余弹窗的内容/脚注节奏
        // 一致），不占滚动内容窗口。
        body.push(Line::from(""));
        body.push(Line::from(Span::styled(
            footer.trim(),
            theme::style(theme::Role::Faint),
        )));
        clear_popup_with_guards(frame, dialog);
        frame.render_widget(Paragraph::new(body).block(popup_block("/help")), dialog);
    }

    /// /mcp 状态弹窗：连接概览 + 每服务器一行（名称 · 传输 · 协议 ·
    /// 版本 · 工具数）+ 失败条目（含 stderr 尾部，按内宽折行——它们是
    /// 用户来排查的正文）。骨架与 /help 相同：内容驱动高度、超预算滚
    /// 动、脚注键位（多一个 `r` 刷新）。数据是打开/刷新时缓存的
    /// `McpStatusDto`，弹窗自身不触碰会话与注册表。
    pub(super) fn draw_mcp_dialog(&mut self, frame: &mut Frame) {
        let area = frame.area();
        let inner_width = popup_inner_width(84, area);
        let view = self.mcp_view.clone().unwrap_or_default();
        let lines = mcp_dialog_lines(&view, inner_width);
        let dialog = centered_rect(84, content_dialog_height(lines.len(), area), area);
        let visible = (dialog.height.saturating_sub(2 + 2)) as usize;
        let max_scroll = lines.len().saturating_sub(visible);
        self.info_scroll_max = max_scroll;
        self.info_page = visible.max(1);
        let offset = self
            .info_dialog
            .as_ref()
            .map(|dialog| dialog.offset)
            .unwrap_or(0)
            .min(max_scroll);
        let mut body: Vec<Line<'static>> = lines.into_iter().skip(offset).take(visible).collect();
        let footer = if max_scroll > 0 {
            if offset < max_scroll {
                " ↑↓/PgUp/PgDn scroll · more below · r refresh · Esc close "
            } else {
                " ↑↓/PgUp/PgDn scroll · end · r refresh · Esc close "
            }
        } else {
            " r refresh · Esc close "
        };
        // 空行钉在脚注上方（2026-08-21 统一，同 /help）。
        body.push(Line::from(""));
        body.push(Line::from(Span::styled(
            footer.trim(),
            theme::style(theme::Role::Faint),
        )));
        clear_popup_with_guards(frame, dialog);
        frame.render_widget(Paragraph::new(body).block(popup_block("/mcp")), dialog);
    }

    /// ask-user 对话框：问题原文（按实际宽度换行）+ 选项列表（选择行
    /// 高亮，描述 dim）+ 自定义行 / 输入回显 + 键位脚注。窄屏降级为
    /// 问题 + 脚注（选项照常可按 ↑↓ 选中）。
    pub(super) fn draw_ask_dialog(&mut self, frame: &mut Frame) {
        let Some(pending) = self.pending_ask_user.as_ref() else {
            return;
        };
        let area = frame.area();
        let dialog = centered_rect(72, 12.min(popup_height_cap(area)), area);
        let inner_width = dialog.width.saturating_sub(2 + 2 * POPUP_TEXT_PADDING) as usize;

        let mut lines: Vec<Line<'static>> = Vec::new();
        for wrapped in wrap_text(&pending.question.question, inner_width) {
            lines.push(Line::from(wrapped));
        }
        lines.push(Line::from(""));

        if let Some(text) = &pending.custom {
            lines.push(Line::from(vec![
                Span::styled("❯ ", theme::style(theme::Role::UserMarker)),
                Span::raw(text.clone()),
                Span::styled("_", theme::style(theme::Role::Faint)),
            ]));
        } else {
            for (index, option) in pending.question.options.iter().enumerate() {
                let selected = index == pending.selection;
                let marker = if selected { "● " } else { "○ " };
                let mut spans = vec![Span::styled(
                    marker,
                    theme::style(if selected {
                        theme::Role::Selected
                    } else {
                        theme::Role::Faint
                    }),
                )];
                spans.push(if selected {
                    Span::styled(option.label.clone(), theme::style(theme::Role::Selected))
                } else {
                    Span::raw(option.label.clone())
                });
                lines.push(Line::from(spans));
                if let Some(description) = &option.description {
                    for wrapped in wrap_text(description, inner_width.saturating_sub(2)) {
                        lines.push(Line::from(Span::styled(
                            format!("   {wrapped}"),
                            theme::style(theme::Role::Faint),
                        )));
                    }
                }
            }
            if pending.question.allow_custom {
                let selected = pending.selection == pending.question.options.len();
                let marker = if selected { "● " } else { "○ " };
                lines.push(Line::from(vec![
                    Span::styled(
                        marker,
                        theme::style(if selected {
                            theme::Role::Selected
                        } else {
                            theme::Role::Faint
                        }),
                    ),
                    Span::styled("type a custom answer…", theme::style(theme::Role::Italic)),
                ]));
            }
        }

        lines.push(Line::from(""));
        let footer = if pending.custom.is_some() {
            "Enter send · Esc back / decline"
        } else {
            "↑↓ select · Enter confirm · c custom · Esc decline"
        };
        lines.push(Line::from(Span::styled(
            footer,
            theme::style(theme::Role::Faint),
        )));

        let mut spans: Vec<Line<'static>> = Vec::new();
        let visible = (dialog.height as usize).saturating_sub(2);
        // 底部对齐保留脚注与输入行：超出视口时从顶部截断问题文本。
        if lines.len() > visible {
            let tail = lines.split_off(lines.len() - visible.min(lines.len()));
            spans.extend(tail);
        } else {
            spans.extend(lines);
        }
        clear_popup_with_guards(frame, dialog);
        frame.render_widget(Paragraph::new(spans).block(popup_block("Question")), dialog);
    }

    pub(super) fn draw_permission_dialog(&mut self, frame: &mut Frame) {
        let Some(pending) = self.pending_permission.as_mut() else {
            return;
        };
        let area = frame.area();
        // `centered_rect(84, ..)` 的 84 是百分比，不是 84 列。预览
        // 必须使用本次真实矩形宽度换行，否则在 80 列终端里会按
        // 78 列排版、再被约 65 列的框裁掉危险命令尾部。
        let argument_width = permission_argument_width(area);
        let mut lines = vec![
            Line::from(Span::styled(
                "Permission required",
                theme::style(theme::Role::Bold),
            )),
            Line::from(""),
            Line::from(format!("tool:      {}", pending.request.tool)),
            Line::from(format!("effect:    {}", pending.request.effect)),
            Line::from(format!("reason:    {}", pending.request.reason)),
        ];
        // 危险字段摘要：参数的顶层键全部列出，一眼可见隐藏在长
        // JSON 深处的 command/path/url 等目标——批准前不可错过。
        if let Some(keys) = top_level_argument_keys(&pending.request.arguments) {
            lines.push(Line::from(format!("fields:    {keys}")));
        }
        // 写/执行类工具的专用预览：JSON 转义长串对内容审阅不友好
        // （write_file 的 content 会变成一行带 \n 的转义串）。改为
        // 人类可读形式：edit_file 显示 old→new 的迷你 diff，write_file
        // 显示目标与内容，run_command 突出命令与执行环境。其余工具
        // 回退完整 pretty JSON。两种形态共享同一滚动/强制审阅机制
        // ——预览行就是被审阅的参数。
        let argument_lines = match tool_argument_lines(
            &pending.request.tool,
            &pending.request.arguments,
            argument_width,
        ) {
            Some(preview) => preview,
            None => {
                // 完整 pretty JSON 逐行入列（不再静默截断到 8 行）；
                // 对框高不足时在尾部追加"还有 N 行未显示"的醒目计数。
                let pretty = serde_json::to_string_pretty(&pending.request.arguments)
                    .unwrap_or_else(|_| "<unavailable>".into());
                let mut json_lines = Vec::new();
                for source_line in pretty.split('\n') {
                    for wrapped in wrap_text(source_line, argument_width) {
                        json_lines.push(Line::from(format!("  {wrapped}")));
                    }
                }
                json_lines
            }
        };
        // 对话框最高占屏（减边距）。参数可滚动，且只有最后一页
        // 确实进入视口后才开放批准键，避免隐藏字段未审阅即放行。
        // 预算必须与 centered_rect 的实际钳制同源（popup_height_cap），
        // 否则分页页底会渲染在框外。
        let max_dialog_height = popup_height_cap(frame.area()).min(area.height.saturating_sub(2));
        // 升级提示独立成行（宽度永不超框）：预留数随之 +1。
        let escalation_hint = pending
            .escalations
            .iter()
            .map(|mode| match mode {
                PermissionMode::ProjectWrite => "w — Project Write",
                PermissionMode::FullAccess => "f — Full Access",
                PermissionMode::ReadOnly => "",
            })
            .filter(|hint| !hint.is_empty())
            .collect::<Vec<_>>()
            .join("      ·      ");
        let reserved = lines.len() + 5 + usize::from(!escalation_hint.is_empty()); // 状态 + 空行 + 快捷键 + 边框
        let available_for_arguments = (max_dialog_height as usize).saturating_sub(reserved);
        if available_for_arguments == 0 || argument_width < 8 {
            pending.argument_page_size = 0;
            pending.argument_line_count = argument_lines.len();
            let compact = vec![
                Line::from(Span::styled(
                    "Permission required",
                    theme::style(theme::Role::Bold),
                )),
                Line::from("Terminal is too small to review arguments."),
                Line::from("Maximize to continue · Esc / n — deny"),
            ];
            let height = (compact.len() as u16 + 2).min(max_dialog_height);
            let dialog = centered_rect(84, height, area);
            clear_popup_with_guards(frame, dialog);
            frame.render_widget(
                Paragraph::new(compact).block(popup_block("Permission")),
                dialog,
            );
            return;
        }
        pending.argument_page_size = available_for_arguments;
        pending.argument_line_count = argument_lines.len();
        let max_scroll = argument_lines.len().saturating_sub(available_for_arguments);
        pending.argument_scroll = pending.argument_scroll.min(max_scroll);
        let start = pending.argument_scroll;
        let shown = argument_lines
            .len()
            .saturating_sub(start)
            .min(available_for_arguments);
        lines.extend(argument_lines.into_iter().skip(start).take(shown));
        let end = start + shown;
        // 只累计从首行起连续看过的区间。End 跳跃只用于查看，不会
        // 越过中间未显示内容而解锁 Allow。
        pending.reviewed_through = advance_reviewed_through(pending.reviewed_through, start, end);
        pending.reviewed_to_end = pending.reviewed_through >= pending.argument_line_count;
        lines.push(Line::from(Span::styled(
            format!(
                "arguments lines {}–{} of {} · ↑/↓ PgUp/PgDn Home/End",
                start.saturating_add(1).min(pending.argument_line_count),
                end,
                pending.argument_line_count
            ),
            theme::style(theme::Role::Bold),
        )));
        lines.push(Line::from(""));
        let mut actions = if pending.reviewed_to_end {
            "Enter / y — allow      ·      Esc / n — deny".to_owned()
        } else {
            "Review through the final line to enable Allow · Esc / n — deny".to_owned()
        };
        // 升级提示（P5）：只列出本弹框 offered 的档位——切过去仍要问
        // 的档位不出现（Execute@Read Only 不提示切 Project Write）。
        // 独立成行，宽度永不超框（合并进动作行会在窄弹框里截断）。
        if pending.reviewed_to_end && !escalation_hint.is_empty() {
            actions = format!("{actions}\n{escalation_hint}");
        }
        for action in actions.split('\n') {
            lines.push(Line::from(Span::styled(
                action,
                theme::style(theme::Role::Bold),
            )));
        }

        let height = (lines.len() as u16 + 2).min(max_dialog_height);
        let dialog = centered_rect(84, height.max(10), area);
        clear_popup_with_guards(frame, dialog);
        frame.render_widget(
            Paragraph::new(lines).block(popup_block("Permission")),
            dialog,
        );
    }

    /// /rename 弹框：预填的 InputBuffer + 真实光标（与主输入框同一套
    /// 换行/光标算法，坐标天然一致）。行由 `visual_rows` 预折——不用
    /// Paragraph 的 wrap，保证光标列与显示列可换算。
    pub(super) fn draw_rename_dialog(&self, frame: &mut Frame) {
        let Some(dialog) = &self.rename_dialog else {
            return;
        };
        let area = frame.area();
        let inner_width = popup_inner_width(72, area);
        let mut lines: Vec<Line<'static>> = dialog
            .buffer
            .visual_rows(inner_width)
            .into_iter()
            .map(Line::from)
            .collect();
        lines.push(Line::from(""));
        // 脚注键位说明用 Faint 灰——与 /help /mcp /perm 弹窗统一
        //（2026-08-19 用户反馈：Bold 亮白与其他弹窗脚注不一致）。
        lines.push(Line::from(Span::styled(
            "Enter — rename      ·      Esc — cancel",
            theme::style(theme::Role::Faint),
        )));
        let height = (lines.len() as u16 + 2).min(popup_height_cap(area));
        // 高度 = 内容精确高度（输入行 + 空行 + 脚注 + 双边框）——旧的
        // `.max(6)` 最小高在单行输入时多出一行空行（2026-08-23 负责人
        // 报 bug）；多行输入随行数自然长高。
        let dialog_area = centered_rect(72, height, area);
        clear_popup_with_guards(frame, dialog_area);
        frame.render_widget(
            Paragraph::new(lines).block(popup_block("/rename")),
            dialog_area,
        );
        // popup_block：边框 1 列 + 水平 padding 1 列。
        let (row, column) = dialog.buffer.cursor_position(inner_width);
        let visible_rows = dialog_area.height.saturating_sub(2) as usize;
        let row = row.min(visible_rows.saturating_sub(1));
        frame.set_cursor_position((
            dialog_area.x + 2 + column as u16,
            dialog_area.y + 1 + row as u16,
        ));
    }

    pub(super) fn draw_header(&self, frame: &mut Frame, area: Rect) {
        // 数据源参数化（§2.1）：local 走 config（预设名/档位），dsh 走
        // DshState（model_label、无档位、宿主 cwd 第二行、●/○ 在线点）。
        // 前缀：local 裸 `CLAT`；dsh `CLAT ●/○ dsh`（绿实心/红空心在线
        // 点——◆ 菱形方案 2026-08-23 负责人 dogfood 后撤下，仅留 ●）。
        let connecting = self.dsh.is_none() && self.dsh_connect.is_some();
        let loading = self.loading.is_some() || connecting;
        let state = if loading {
            "loading"
        } else if self.running {
            "running"
        } else {
            "ready"
        };
        let (prefix, second_line, model, level): (
            Vec<Span<'static>>,
            String,
            String,
            Option<String>,
        ) = if let Some(dsh) = self.dsh.as_ref() {
            let marker = if dsh.connected {
                " ● dsh"
            } else {
                " ○ dsh"
            };
            let role = if dsh.connected {
                theme::Role::Success
            } else {
                theme::Role::Error
            };
            let mut prefix = vec![
                Span::styled("CLAT", theme::style(theme::Role::Bold)),
                Span::styled(marker, theme::style(role)),
            ];
            // 词汇违规徽标（§2.2，INV-D8 呈现）：标题栏追加 ` ⚠ N`
            //（Warning 角色，与状态栏 steering 徽标同族）。
            if dsh.unknown_events > 0 {
                prefix.push(Span::styled(
                    format!(" ⚠ {}", dsh.unknown_events),
                    theme::style(theme::Role::Warning),
                ));
            }
            (
                prefix,
                format!(
                    "project: {}",
                    abbreviate_home(std::path::Path::new(dsh.cwd().as_str()))
                ),
                dsh.model_label.clone(),
                // 档位段（档位接入 2026-08-23）：宿主 adapter 自有词汇的
                // 展示名（efforts 表解析；无档位 → None 隐藏段）。
                dsh.effort_display(),
            )
        } else if connecting {
            (
                vec![
                    Span::styled("CLAT", theme::style(theme::Role::Bold)),
                    Span::styled(" ○ dsh", theme::style(theme::Role::Error)),
                ],
                format!("project: {}", self.project.root().display()),
                "dsh web".to_owned(),
                None,
            )
        } else {
            let (model, level) = if self.config.is_configured() {
                let name = match self.config.preset.as_deref().and_then(preset_by_id) {
                    // 预设模型的 name 与 model id 重复（仅大小写不同），只展示名称。
                    Some(preset) => preset.name.to_owned(),
                    None => format!("{} · {}", self.config.protocol, self.config.model),
                };
                (
                    name,
                    effective_thinking_level(&self.config).map(|level| level.label().to_owned()),
                )
            } else {
                ("not configured — /model".to_owned(), None)
            };
            (
                vec![Span::styled("CLAT", theme::style(theme::Role::Bold))],
                format!("project: {}", self.project.root().display()),
                model,
                level,
            )
        };
        // 首行内容预算按前缀实际显示宽度扣除（local "CLAT" 4 列 /
        // dsh "CLAT ● dsh" 10 列）；宽度不足时逐级退化（TUI-L02），
        // 档位优先于模型名保留。
        let prefix_width = prefix
            .iter()
            .map(|span| UnicodeWidthStr::width(span.content.as_ref()))
            .sum::<usize>();
        let rest_budget = area.width.saturating_sub(2 + 2 + prefix_width as u16) as usize;
        let header = HeaderModel {
            version: env!("CARGO_PKG_VERSION"),
            state,
            model: model.as_str(),
            level: level.as_deref(),
        };
        let mut first_line = prefix;
        first_line.extend(compose_header_rest(&header, rest_budget));
        frame.render_widget(
            Paragraph::new(vec![Line::from(first_line), Line::from(second_line)])
                // 水平内边距 1 列：文字与边框字符之间留空，不贴框。
                .block(
                    Block::default()
                        .borders(Borders::ALL)
                        .padding(Padding::horizontal(1)),
                ),
            area,
        );
    }

    pub(super) fn draw_conversation(&mut self, frame: &mut Frame, area: Rect) {
        // 会话右标题（用户指定布局）：左上角 Conversation、右上角对称
        // 放当前会话名（effective：LLM/用户标题，否则首条消息派生）。
        // 超宽截断保头（标题语义在头部），留出左标题与边框的余量。
        let mut block = Block::default()
            .title(" Conversation ")
            .borders(Borders::ALL);
        if let Some(title) = self
            .session_title
            .as_deref()
            .filter(|title| !title.is_empty())
        {
            let budget = area.width.saturating_sub(16) as usize;
            let shown = if title.chars().count() > budget {
                let kept: String = title.chars().take(budget.saturating_sub(1)).collect();
                format!("{kept}…")
            } else {
                title.to_owned()
            };
            block = block.title(
                Line::from(Span::styled(
                    format!(" {shown} "),
                    theme::style(theme::Role::Faint),
                ))
                .right_aligned(),
            );
        }
        // 空会话：LOGO 欢迎页接管会话区（启动 / `/new` / `/clear` 后的
        // 起步画面）。0 行内容与画面一致——无滚动、无选区映射。
        if self.conversation.is_empty() {
            self.conversation_start = 0;
            self.conversation_rows = 0;
            frame.render_widget(&block, area);
            draw_welcome(frame, block.inner(area));
            self.draw_dsh_banner(frame, area);
            return;
        }
        // 折行宽度比 inner 少一列：滚动条列专属（见
        // conversation_wrap_width），宽字符字形不再铺进滚动条列。
        let inner_width = conversation_wrap_width(area);
        let total = self.conversation_total_lines(inner_width);
        let visible = area.height.saturating_sub(2) as usize;
        let max_start = total.saturating_sub(visible);
        let start = max_start.saturating_sub(self.conversation_scroll_from_bottom.min(max_start));
        // 记录视口信息，供鼠标事件把屏幕坐标映射回内容行。
        self.conversation_start = start;
        self.conversation_rows = total;
        // 每帧只克隆视口行（G3：O(viewport) 取代 O(历史) 全量 clone）。
        let mut visible_lines =
            self.conversation
                .visible_lines(start, visible, inner_width, self.card_visibility);
        // 会话选区按内容行号高亮，滚动后依然正确。
        if let Some((from, to)) = self
            .selection
            .filter(|selection| {
                selection.kind == SelectionKind::Conversation && !selection.is_empty()
            })
            .map(|selection| selection.ordered())
        {
            for (offset, line) in visible_lines.iter_mut().enumerate() {
                let row = start + offset;
                if row < from.row || row > to.row {
                    continue;
                }
                let highlight_from = if row == from.row { from.col } else { 0 };
                let highlight_to = if row == to.row { to.col } else { usize::MAX };
                *line = highlight_line(line, highlight_from, highlight_to);
            }
        }
        frame.render_widget(
            Paragraph::new(Text::from(visible_lines)).block(block.clone()),
            area,
        );

        let mut scrollbar_state = ScrollbarState::new(total)
            .position(scrollbar_position(start, max_start, total))
            .viewport_content_length(visible);
        frame.render_stateful_widget(
            Scrollbar::new(ScrollbarOrientation::VerticalRight)
                .thumb_symbol("┃")
                .begin_symbol(Some("▲"))
                .end_symbol(Some("▼"))
                .track_symbol(Some("│"))
                .style(theme::style(theme::Role::ScrollTrack))
                .thumb_style(theme::style(theme::Role::ScrollThumb)),
            block.inner(area),
            &mut scrollbar_state,
        );
        self.draw_dsh_banner(frame, area);
    }

    /// dsh 断线/重连中/流错误通知条（§2.2）：会话区顶部第一内行，
    /// Error 角色同族样式——渲染层叠加而非会话 item（零新组件红线），
    /// 空会话（欢迎页）路径同样可见。
    fn draw_dsh_banner(&self, frame: &mut Frame, area: Rect) {
        if let Some(banner) = self.dsh.as_ref().and_then(|dsh| dsh.banner.as_deref()) {
            let row = Rect {
                x: area.x + 1,
                y: area.y + 1,
                width: area.width.saturating_sub(2),
                height: 1,
            };
            frame.render_widget(
                Paragraph::new(Span::styled(
                    format!(" ⚠ {banner} "),
                    theme::style(theme::Role::Error),
                )),
                row,
            );
        }
    }

    /// 内容总行数（含分隔空行）；模型内建逐 item 渲染缓存（G3）。
    pub(super) fn conversation_total_lines(&mut self, width: usize) -> usize {
        self.conversation.ensure_rendered(width);
        self.conversation.total_lines(self.card_visibility)
    }

    pub(super) fn draw_input(&self, frame: &mut Frame, area: Rect) {
        // 标题只有两态：空闲 Message / 运行插话提示。loading 不进输入框
        // 标题——头部状态与底部状态栏已在报 loading，第三处是画蛇添足
        // （2026-08-19 用户反馈；输入禁用本身由 loading 门保证）。dsh 态
        // 的运行提示无 "recalls queued" 段（INV-U3 例外②：无栈式召回）。
        let title = if self.running {
            if self.dsh.is_some() {
                "Running — Enter steers · Esc cancels"
            } else {
                "Running — Enter steers · Esc recalls queued, then cancels"
            }
        } else {
            "Message"
        };
        // 右上角档位（与左上角 Message 对称；DSH composer Access 徽标
        // 对应物）。Full Access / danger-full-access 用警示黄——它是
        // "不再有任何弹窗"的档位，颜色是唯一的风险暗示。local 直读
        // application（单一数据源）；dsh 显示 preset 投影（journal 值 →
        // web 端产品标签，§2.6）。
        let mut block = Block::default()
            .title(format!(" {title} "))
            .borders(Borders::ALL);
        if let Some(dsh) = &self.dsh {
            if let Some(preset) = &dsh.preset {
                let style = if preset == "danger-full-access" {
                    theme::style(theme::Role::Warning)
                } else {
                    theme::style(theme::Role::Faint)
                };
                let label = dsh_preset_label(preset);
                block = block.title(
                    Line::from(format!(" {label} "))
                        .style(style)
                        .right_aligned(),
                );
            }
        } else if let Some(mode) = self
            .application
            .as_ref()
            .map(|application| application.permission_mode())
        {
            let style = if mode == PermissionMode::FullAccess {
                theme::style(theme::Role::Warning)
            } else {
                theme::style(theme::Role::Faint)
            };
            block = block.title(Line::from(format!(" {mode} ")).style(style).right_aligned());
        }
        // 输入框与聊天记录的用户消息同款排版：首行 `❯ ` 前缀，续行
        // 两个空格保持等宽左缩进，文本按扣除前缀后的宽度换行。与
        // 光标定位、鼠标选区映射共用同一换行算法，三者坐标一致。
        let width = area
            .width
            .saturating_sub(2)
            .saturating_sub(INPUT_MARKER_WIDTH as u16)
            .max(1) as usize;
        let mut lines: Vec<Line<'static>> = self
            .input
            .visual_rows(width)
            .into_iter()
            .enumerate()
            .map(|(index, row)| {
                let prefix = if index == 0 { "❯ " } else { "  " };
                Line::from(vec![Span::raw(prefix), Span::raw(row)])
            })
            .collect();
        // 附件徽标行（M6）：插在最前，占一个内容行（input_rows 与光标
        // 行号都随之 +1）。超长截断保文件名尾部（文件名语义在后段）。
        if !self.attachments.is_empty() {
            let chips = self
                .attachments
                .iter()
                .map(|path| {
                    let name = path
                        .file_name()
                        .map(|name| name.to_string_lossy().into_owned())
                        .unwrap_or_else(|| path.to_string_lossy().into_owned());
                    format!("📷 {name}")
                })
                .collect::<Vec<_>>()
                .join("  ");
            lines.insert(
                0,
                Line::from(Span::styled(chips, theme::style(theme::Role::Faint))),
            );
        }
        if let Some((from, to)) = self
            .selection
            .filter(|selection| selection.kind == SelectionKind::Input && !selection.is_empty())
            .map(|selection| selection.ordered())
        {
            for (row, line) in lines.iter_mut().enumerate() {
                if row < from.row || row > to.row {
                    continue;
                }
                // 选区列是文本坐标，高亮时整体平移前缀宽度。
                let highlight_from = if row == from.row {
                    from.col + INPUT_MARKER_WIDTH
                } else {
                    0
                };
                let highlight_to = if row == to.row {
                    to.col.saturating_add(INPUT_MARKER_WIDTH)
                } else {
                    usize::MAX
                };
                *line = highlight_line(line, highlight_from, highlight_to);
            }
        }
        frame.render_widget(Paragraph::new(Text::from(lines)).block(block), area);
    }
}

/// Maps the first visible row (`start`, in 0..=max_start) to ratatui's
/// scrollbar position domain 0..=content_length-1, where 0 puts the thumb
/// at the very top and content_length-1 at the very bottom. Passing the
/// raw row index leaves the thumb short of the bottom by one viewport.
pub(super) fn scrollbar_position(start: usize, max_start: usize, content_length: usize) -> usize {
    if max_start == 0 {
        return 0;
    }
    let domain = content_length.saturating_sub(1);
    (start.saturating_mul(domain) / max_start).min(domain)
}
