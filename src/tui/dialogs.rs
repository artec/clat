use super::*;
use clat_core::client_ports::host::HostClient;
use serde_json::Value;

pub(super) struct PendingPermission {
    pub(super) request: PermissionRequest,
    pub(super) decision_tx: Sender<PermissionDecision>,
    pub(super) argument_scroll: usize,
    pub(super) argument_page_size: usize,
    pub(super) argument_line_count: usize,
    /// 从首行起连续进入过视口的行数；跳到 End 不会跨过未审阅区。
    pub(super) reviewed_through: usize,
    /// 只有参数最后一页实际进入过视口，才允许批准。
    pub(super) reviewed_to_end: bool,
    /// 升级选项（P5）：能让本次调用直接放行的更宽档位（宽度升序）。
    /// 弹框打开时由 `escalation_targets(当前档, effect)` 算出；键位
    /// `w`/`f` 按此集合生效——先 `set_permission_mode` 再回 Allow，
    /// approver 契约零改动。
    pub(super) escalations: Vec<PermissionMode>,
}

/// ask-user 对话框状态：选项模式（`custom = None`，selection 游标含
/// 末尾的"自定义输入"行）或自定义输入模式（`custom = Some`）。无选项时
/// 直接进入输入模式；Esc 拒绝（Declined → isError 结果，run 继续）。
pub(super) struct PendingAskUser {
    pub(super) question: crate::interaction::AskQuestion,
    pub(super) answer_tx: Sender<crate::interaction::AskAnswer>,
    pub(super) selection: usize,
    pub(super) custom: Option<String>,
}

/// 信息弹窗（/help、/mcp）的种类。两类共用滚动/翻页/绘制骨架；键位
/// 差异只有 /mcp 多一个 `r` 刷新。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum InfoDialogKind {
    Help,
    Mcp,
    Context,
    Skills,
    Memory,
    Goal,
    SubagentStatus,
    Remote,
}

pub(super) enum ContentView {
    Remote {
        title: &'static str,
        text: clat_core::client_ports::presentation::PresentedText,
    },
    Host {
        title: &'static str,
        value: Box<Value>,
    },
    Memory(crate::application::MemoryOverviewDto),
    Goal(Box<crate::application::GoalViewDto>),
    SubagentStatus(crate::application::SubagentStatusDto),
}

impl ContentView {
    pub(super) fn title(&self) -> &'static str {
        match self {
            Self::Remote { title, .. } | Self::Host { title, .. } => title,
            Self::Memory(_) => "/mem",
            Self::Goal(_) => "/goal",
            Self::SubagentStatus(_) => "/sub",
        }
    }

    pub(super) fn lines(&self, width: usize, locale: LocaleContext) -> Vec<Line<'static>> {
        let text = match self {
            Self::Remote { text, .. } => text.render(locale),
            Self::Host { value, .. } => {
                HostClient::info_dialog_text_localized(value, locale).unwrap_or_else(|e| e)
            }
            Self::Memory(view) => view.localized_text(locale),
            Self::Goal(view) => view.localized_text(locale),
            Self::SubagentStatus(view) => view.localized_text(locale),
        };
        text.lines()
            .enumerate()
            .flat_map(|(index, line)| {
                let style = if index == 0 {
                    theme::style(theme::Role::Bold)
                } else {
                    Style::default()
                };
                wrap_text(line, width)
                    .into_iter()
                    .map(move |text| Line::from(Span::styled(text, style)))
            })
            .collect()
    }
}

/// 打开中的信息弹窗：种类 + 当前滚动位（绘制期钳制在
/// `App::info_scroll_max`）。
pub(super) struct InfoDialog {
    pub(super) kind: InfoDialogKind,
    pub(super) offset: usize,
}

impl InfoDialog {
    pub(super) fn new(kind: InfoDialogKind) -> Self {
        Self { kind, offset: 0 }
    }
}

/// `/rename` 弹框：内嵌完整 `InputBuffer`（真实光标编辑，比 model
/// editor 的追加式 `EditPopup` 强一档），预填当前标题。Enter 提交
/// （空文本 flash 拒绝、不关框）、Esc 取消。门槛（2026-08-19 放宽）：
/// 有活动会话即可——不再要求 LLM 已起名。
pub(super) struct RenameDialog {
    pub(super) buffer: InputBuffer,
}

impl RenameDialog {
    pub(super) fn new(prefill: &str) -> Self {
        let mut buffer = InputBuffer::new(Vec::new());
        buffer.insert_str(prefill);
        Self { buffer }
    }
}

/// 空会话欢迎页：LOGO + 版本行 + 起步提示，双向居中于会话区内框。
/// 窄到放不下 LOGO 的终端退化为单行提示（ASCII 字形无法有意义地缩放）。
pub(super) fn draw_welcome(frame: &mut Frame, inner: Rect, version: &str, locale: LocaleContext) {
    if inner.width == 0 || inner.height == 0 {
        return;
    }
    let welcome = crate::tui::logo::welcome_lines_localized(version, locale);
    let content_width = welcome.iter().map(Line::width).max().unwrap_or(0) as u16;
    if inner.width < content_width {
        let hint = Line::from(Span::styled(
            locale.render(&UiMessage::new("tui.welcome").arg("version", version)),
            theme::style(theme::Role::Dim),
        ));
        let x = inner.x + inner.width.saturating_sub(hint.width() as u16) / 2;
        let y = inner.y + inner.height / 2;
        frame.render_widget(
            Paragraph::new(hint).alignment(Alignment::Center),
            Rect::new(x, y, inner.width.saturating_sub(x - inner.x), 1),
        );
        return;
    }
    let content_height = welcome.len() as u16;
    let x = inner.x + (inner.width - content_width) / 2;
    let y = inner.y + inner.height.saturating_sub(content_height) / 2;
    frame.render_widget(
        Paragraph::new(Text::from(welcome)).alignment(Alignment::Center),
        Rect::new(
            x,
            y,
            content_width,
            content_height.min(inner.height.saturating_sub(y - inner.y)),
        ),
    );
}

/// /help 弹窗内容：命令、Composer、键位三节，逐条 `命令 — 说明`，按
/// 弹窗内宽折行（wrap_text）。节标题 Bold，条目默认色。命令节从
/// `core.commands` 目录派生（INV-C4：`ShowHelp` 载荷，INV-SC-1 折叠序）；
/// Composer 节与键位节保持前端本地（INV-SC-3：附件草稿是 TUI 状态，
/// 不进 core `CommandRegistry`——命令真实存在于 attachments.rs 的
/// 本地拦截，不列死条目）。Composer 条目主名在前、别名随后（CP-2
/// A5/A6）；键位节按任务域四组排布（CP-2 A4：输入与提交 /
/// 运行控制 / 浏览与显示 / 选择与复制，组序即行序——与命令节七组
/// 的"组序即序"设计语言统一），Ctrl+V 归位 Keys 节；Think 披露新增
/// Ctrl+R 后为十二行；2026-09-16 增加 Alt+S 手动建议入口。
pub(super) fn help_dialog_lines(
    width: usize,
    commands: &[CommandInfo],
    locale: LocaleContext,
    dsh: bool,
) -> Vec<Line<'static>> {
    let composer: &[(&str, &str)] = &[
        ("/attach PATH…", locale.text("tui.attach_help")),
        ("/pi, /paste-image", locale.text("tui.paste_help")),
        ("/ac, /attach-clear", locale.text("tui.clear_help")),
        ("/plan", locale.text("tui.plan_help")),
        ("/goal", locale.text("tui.goal_help")),
        ("/resume", locale.text("tui.resume_help")),
    ];
    // CP-2 A4（2026-09-02 负责人拍板的权威顺序表）：行 1-4 输入与提交，
    // 5-6 运行控制，7-9 浏览与显示，10-12 选择与复制。
    let keys: &[(&str, &str)] = &[
        ("Enter", locale.text("tui.key_submit")),
        (
            "Shift+Enter, Alt+Enter, Ctrl+J",
            locale.text("tui.key_newline"),
        ),
        ("Ctrl+V", locale.text("tui.key_paste")),
        ("Ctrl+G", locale.text("tui.key_suggest")),
        ("Ctrl+Shift+L", locale.text("tui.language_keys")),
        ("Up / Down", locale.text("tui.key_history")),
        ("Esc", locale.text("tui.key_cancel")),
        ("Shift+Tab", locale.text("tui.key_think")),
        (
            locale.text("tui.scroll_shortcut"),
            locale.text("tui.key_scroll"),
        ),
        ("Ctrl+O", locale.text("tui.key_cards")),
        ("Ctrl+R", locale.text("tui.key_details")),
        (
            locale.text("tui.drag_shortcut"),
            locale.text("tui.key_drag"),
        ),
        ("Ctrl+C", locale.text("tui.key_quit")),
        (
            locale.text("tui.shift_drag_shortcut"),
            locale.text("tui.key_terminal"),
        ),
    ];
    let mut lines = Vec::new();
    for (title, entry_lines) in [
        (
            locale.text("tui.commands"),
            command_help_entries(commands, locale, dsh),
        ),
        (
            locale.text("tui.composer"),
            composer
                .iter()
                .map(|(name, description)| format!("  {name} — {description}"))
                .collect::<Vec<_>>(),
        ),
        (
            locale.text("tui.keys"),
            keys.iter()
                .map(|(name, description)| format!("  {name} — {description}"))
                .collect::<Vec<_>>(),
        ),
    ] {
        if !lines.is_empty() {
            lines.push(Line::from(""));
        }
        lines.push(Line::from(Span::styled(
            title,
            theme::style(theme::Role::Bold),
        )));
        for entry in entry_lines {
            for wrapped in wrap_text(&entry, width) {
                lines.push(Line::from(wrapped));
            }
        }
    }
    lines
}

/// 命令节的折行前条目：`  /name, /alias — description`（主名+别名全部
/// 展示——目录派生后别名不再藏在分发器里）。
fn command_help_entries(commands: &[CommandInfo], locale: LocaleContext, dsh: bool) -> Vec<String> {
    commands
        .iter()
        .chain(clat_core::client_ports::presentation::client_commands().iter())
        .map(|info| {
            let mut names = format!("/{}", info.name);
            for alias in &info.aliases {
                names.push_str(&format!(", /{alias}"));
            }
            format!(
                "  {names} — {}",
                if dsh && info.name != "language" {
                    Some(UiMessage::new(&format!("tui.dsh_command_{}", info.name)))
                } else {
                    clat_core::client_ports::presentation::command_presentation(
                        &info.name,
                        &info.description,
                    )
                }
                .map(|m| locale.render(&m))
                .unwrap_or_else(|| info.description.clone())
            )
        })
        .collect()
}

/// /mcp 弹窗内容行。结构：概览行（`connected/configured`）→ 空行 →
/// 每服务器一行 `● name  transport · protocol · v版本 · N tools`
///（名称默认色，其余 dim；这些字段短，保持单行不折）→ 空行 →
/// `Failures` 节（失败消息按内宽折行，dim——含挂载失败时的 stderr
/// 尾部，是排查的主要正文）。
pub(super) fn mcp_dialog_lines(
    view: &McpStatusDto,
    width: usize,
    locale: LocaleContext,
) -> Vec<Line<'static>> {
    // connecting（后台启动中的 server）非零时追加以 "· N connecting"
    // 段——启动落定前 /mcp 是三态视图（INV-M4）。
    let overview = if view.connecting > 0 {
        locale.render(
            &UiMessage::new("tui.message_mcp_servers_value_value_connected_value_connecting")
                .arg("value1", (view.connected).to_string())
                .arg("value2", (view.configured).to_string())
                .arg("value3", (view.connecting).to_string()),
        )
    } else {
        locale.render(
            &UiMessage::new("tui.message_mcp_servers_value_value_connected")
                .arg("value1", (view.connected).to_string())
                .arg("value2", (view.configured).to_string()),
        )
    };
    let mut lines = vec![Line::from(vec![Span::styled(
        overview,
        theme::style(theme::Role::Bold),
    )])];
    if !view.servers.is_empty() {
        lines.push(Line::from(""));
        for server in &view.servers {
            let tools = match server.tools {
                1 => "1 tool".to_owned(),
                count => locale.render(
                    &UiMessage::new("tui.message_value_tools").arg("count", (count).to_string()),
                ),
            };
            lines.push(Line::from(vec![
                Span::raw("● "),
                Span::raw(server.name.clone()),
                Span::styled(
                    format!(
                        "  {} · {} · v{} · {}",
                        server.transport, server.protocol_version, server.server_version, tools
                    ),
                    theme::style(theme::Role::Dim),
                ),
            ]));
        }
    }
    if !view.failures.is_empty() {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            locale.text("tui.failures"),
            theme::style(theme::Role::Bold),
        )));
        for failure in &view.failures {
            for wrapped in wrap_text(&format!("  {failure}"), width) {
                lines.push(Line::from(Span::styled(
                    wrapped,
                    theme::style(theme::Role::Dim),
                )));
            }
        }
    }
    lines
}

pub(super) fn context_dialog_lines(
    view: &ContextEstimateSnapshot,
    width: usize,
    locale: LocaleContext,
) -> Vec<Line<'static>> {
    let mut lines = vec![Line::from(Span::styled(
        locale.render(
            &UiMessage::new("tui.message_context_estimate_value")
                .arg("value1", (view.unit).to_string()),
        ),
        theme::style(theme::Role::Bold),
    ))];
    let rows = context_rows(view, locale);
    lines.push(Line::from(""));
    for (label, value) in rows {
        lines.push(Line::from(vec![
            Span::raw(format!("{label}: ")),
            Span::styled(value.to_string(), theme::style(theme::Role::Dim)),
        ]));
    }
    lines.push(Line::from(vec![
        Span::raw(locale.text("tui.visual_safety_label")),
        Span::styled(
            format!("{}.0x", view.image_token_safety_factor),
            theme::style(theme::Role::Dim),
        ),
    ]));
    lines.push(Line::from(vec![
        Span::raw(locale.text("tui.memory_injection_label")),
        Span::styled(
            locale.render(
                &UiMessage::new("tui.bytes_ratio")
                    .arg("value", view.memory_estimate)
                    .arg("budget", view.memory_budget_bytes),
            ),
            theme::style(theme::Role::Dim),
        ),
    ]));
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        locale.render(
            &UiMessage::new("tui.message_estimator_value")
                .arg("value1", (view.estimator).to_string()),
        ),
        theme::style(theme::Role::Faint),
    )));
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        locale.text("tui.tools"),
        theme::style(theme::Role::Bold),
    )));
    for wrapped in wrap_text(&format!("  {}", view.tool_names.join(", ")), width) {
        lines.push(Line::from(wrapped));
    }
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        locale.text("tui.skills"),
        theme::style(theme::Role::Bold),
    )));
    for wrapped in wrap_text(&format!("  {}", view.skill_names.join(", ")), width) {
        lines.push(Line::from(wrapped));
    }
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        locale.text("tui.skill_diagnostics"),
        theme::style(theme::Role::Bold),
    )));
    if view.skill_diagnostics.is_empty() {
        lines.push(Line::from(Span::styled(
            "  none",
            theme::style(theme::Role::Dim),
        )));
    } else {
        for diagnostic in &view.skill_diagnostics {
            let name = diagnostic.name.as_deref().unwrap_or("-");
            let text = format!(
                "  {} / {} / {}: {}",
                diagnostic.source, name, diagnostic.kind, diagnostic.message
            );
            for wrapped in wrap_text(&text, width) {
                lines.push(Line::from(Span::styled(
                    wrapped,
                    theme::style(theme::Role::Dim),
                )));
            }
        }
    }
    lines
}

/// /skill 弹窗内容行（SC-2）：概览行 → 每技能一行
/// `● name  source · [requires-execution] — description`（描述按内宽折
/// 行）→ `Diagnostics` 节（发现诊断，dim——同 /context 的呈现纪律）。
pub(super) fn skills_dialog_lines(
    view: &SkillsOverviewDto,
    width: usize,
    locale: LocaleContext,
) -> Vec<Line<'static>> {
    let mut lines = vec![Line::from(Span::styled(
        locale.render(
            &UiMessage::new("tui.message_skills_value")
                .arg("value1", (view.entries.len()).to_string()),
        ),
        theme::style(theme::Role::Bold),
    ))];
    if !view.entries.is_empty() {
        lines.push(Line::from(""));
    }
    for entry in &view.entries {
        let mut meta =
            locale.render(&UiMessage::new("tui.skill_layer").arg("source", entry.source.clone()));
        if entry.requires_execution {
            meta.push_str(locale.text("tui.skill_execution"));
        }
        lines.push(Line::from(vec![
            Span::raw("● "),
            Span::raw(entry.name.clone()),
            Span::styled(format!("  {meta}"), theme::style(theme::Role::Dim)),
        ]));
        for wrapped in wrap_text(&format!("  {}", entry.description), width) {
            lines.push(Line::from(Span::styled(
                wrapped,
                theme::style(theme::Role::Dim),
            )));
        }
    }
    if !view.diagnostics.is_empty() {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            locale.text("tui.diagnostics"),
            theme::style(theme::Role::Bold),
        )));
        for diagnostic in &view.diagnostics {
            let name = diagnostic.name.as_deref().unwrap_or("-");
            let text = format!(
                "  {} / {} / {}: {}",
                diagnostic.source, name, diagnostic.kind, diagnostic.message
            );
            for wrapped in wrap_text(&text, width) {
                lines.push(Line::from(Span::styled(
                    wrapped,
                    theme::style(theme::Role::Dim),
                )));
            }
        }
    }
    lines
}

pub(super) fn render_trust_dialog(
    frame: &mut Frame,
    area: Rect,
    root: &Path,
    locale: LocaleContext,
) {
    // 文本按对话框 84% 宽的“最小合理终端”（约 80 列内宽）换行；
    // 更宽的终端留白更多，更窄的终端由 Paragraph 截断右侧。
    // 边框 2 列与弹窗内边距 2×POPUP_TEXT_PADDING 列先扣掉。
    let inner_width = 84usize.saturating_sub(2 + 2 * POPUP_TEXT_PADDING as usize);
    let mut lines = vec![
        Line::from(Span::styled(
            locale.text("tui.trust_this_project"),
            theme::style(theme::Role::Bold),
        )),
        Line::from(""),
        Line::from(locale.text("tui.clat_reads_and_modifies_files_and_runs_tools_inside")),
    ];
    for wrapped in wrap_text(&root.display().to_string(), inner_width.saturating_sub(2)) {
        lines.push(Line::from(format!("  {wrapped}")));
    }
    lines.push(Line::from(""));
    lines.push(Line::from(locale.text(
        "tui.trusting_is_remembered_per_directory_review_the_project_e_g_its",
    )));
    lines.push(Line::from(locale.text(
        "tui.readme_and_configs_before_granting_an_agent_access_to_it",
    )));
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        locale.text("tui.enter_y_trust_this_project_esc_n_exit_clat"),
        theme::style(theme::Role::Bold),
    )));

    let height = (lines.len() as u16 + 2).min(popup_height_cap(area));
    let dialog = centered_rect(84, height.max(10), area);
    clear_popup_with_guards(frame, dialog);
    frame.render_widget(
        Paragraph::new(lines).block(popup_block(locale.text("tui.trust_title"))),
        dialog,
    );
}

fn context_rows(
    view: &ContextEstimateSnapshot,
    locale: LocaleContext,
) -> [(&'static str, u64); 16] {
    [
        (locale.text("web.base_prompt"), view.base_prompt_estimate),
        (
            locale.text("web.project_instructions"),
            view.project_instructions_estimate,
        ),
        (locale.text("web.plan_policy"), view.plan_policy_estimate),
        (
            locale.text("web.skill_catalog"),
            view.skill_catalog_estimate,
        ),
        (
            locale.text("tui.invoked_skill"),
            view.invoked_skill_estimate,
        ),
        (locale.text("web.goal_policy"), view.goal_policy_estimate),
        (locale.text("web.tool_schemas"), view.tool_schemas_estimate),
        (
            locale.text("web.history_compaction_view"),
            view.history_estimate,
        ),
        (locale.text("tui.images"), view.image_count),
        (
            locale.text("tui.images_before_projection"),
            view.image_original_count,
        ),
        (
            locale.text("tui.older_images_omitted"),
            view.image_offloaded_count,
        ),
        (locale.text("tui.image_bytes"), view.image_bytes),
        (
            locale.text("web.visual_token_estimate"),
            view.image_token_estimate,
        ),
        (
            locale.text("web.output_reserve"),
            view.output_reserve_estimate,
        ),
        (locale.text("web.input_estimate"), view.input_estimate),
        (locale.text("web.total_estimate"), view.total_estimate),
    ]
}
