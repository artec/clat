use super::*;

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
pub(super) fn draw_welcome(frame: &mut Frame, inner: Rect) {
    if inner.width == 0 || inner.height == 0 {
        return;
    }
    let welcome = crate::tui::logo::welcome_lines();
    let content_width = welcome.iter().map(Line::width).max().unwrap_or(0) as u16;
    if inner.width < content_width {
        let hint = Line::from(Span::styled(
            format!(
                "clat v{} · type a message to begin",
                env!("CARGO_PKG_VERSION")
            ),
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

/// /help 弹窗内容：命令与键位两节，逐条 `命令 — 说明`，按弹窗内宽
/// 折行（wrap_text）。节标题 Bold，条目默认色。命令节从 `core.commands`
/// 目录派生（INV-C4：`ShowHelp` 载荷），键位节保持前端本地——键位是
/// 终端前端的概念。
pub(super) fn help_dialog_lines(width: usize, commands: &[CommandInfo]) -> Vec<Line<'static>> {
    let keys: &[(&str, &str)] = &[
        ("Enter", "submit; while a run is active, submit steering"),
        ("Shift+Enter, Alt+Enter, Ctrl+J", "insert a line break"),
        (
            "Up / Down",
            "recall input history (or scroll the conversation)",
        ),
        ("PgUp / PgDn, mouse wheel", "scroll the conversation"),
        ("Shift+Tab", "cycle the thinking level"),
        ("Ctrl+O", "cycle tool cards (collapsed / expanded / hidden)"),
        ("drag", "select text and copy it on release"),
        ("Ctrl+C", "re-copy the selection; otherwise quit"),
        ("Shift+drag", "the terminal's own selection, then Cmd+C"),
        ("Esc", "cancel the running request; otherwise clear input"),
    ];
    let mut lines = Vec::new();
    for (title, entry_lines) in [
        ("Commands", command_help_entries(commands)),
        (
            "Keys",
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
fn command_help_entries(commands: &[CommandInfo]) -> Vec<String> {
    commands
        .iter()
        .map(|info| {
            let mut names = format!("/{}", info.name);
            for alias in &info.aliases {
                names.push_str(&format!(", /{alias}"));
            }
            format!("  {names} — {}", info.description)
        })
        .collect()
}

/// /mcp 弹窗内容行。结构：概览行（`connected/configured`）→ 空行 →
/// 每服务器一行 `● name  transport · protocol · v版本 · N tools`
///（名称默认色，其余 dim；这些字段短，保持单行不折）→ 空行 →
/// `Failures` 节（失败消息按内宽折行，dim——含挂载失败时的 stderr
/// 尾部，是排查的主要正文）。
pub(super) fn mcp_dialog_lines(view: &McpStatusDto, width: usize) -> Vec<Line<'static>> {
    // connecting（后台启动中的 server）非零时追加以 "· N connecting"
    // 段——启动落定前 /mcp 是三态视图（INV-M4）。
    let overview = if view.connecting > 0 {
        format!(
            "MCP servers: {}/{} connected · {} connecting",
            view.connected, view.configured, view.connecting
        )
    } else {
        format!(
            "MCP servers: {}/{} connected",
            view.connected, view.configured
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
                count => format!("{count} tools"),
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
            "Failures",
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
) -> Vec<Line<'static>> {
    let mut lines = vec![Line::from(Span::styled(
        format!("Context estimate · {}", view.unit),
        theme::style(theme::Role::Bold),
    ))];
    let rows = [
        ("Base prompt", view.base_prompt_estimate),
        ("Project instructions", view.project_instructions_estimate),
        ("Plan policy", view.plan_policy_estimate),
        ("Skill catalog", view.skill_catalog_estimate),
        ("Goal policy", view.goal_policy_estimate),
        ("Tool schemas", view.tool_schemas_estimate),
        ("History / compaction view", view.history_estimate),
        ("Output reserve", view.output_reserve_estimate),
        ("Input estimate", view.input_estimate),
        ("Total estimate", view.total_estimate),
    ];
    lines.push(Line::from(""));
    for (label, value) in rows {
        lines.push(Line::from(vec![
            Span::raw(format!("{label}: ")),
            Span::styled(value.to_string(), theme::style(theme::Role::Dim)),
        ]));
    }
    lines.push(Line::from(vec![
        Span::raw("Memory injection: "),
        Span::styled(
            format!(
                "{} / {} bytes",
                view.memory_estimate, view.memory_budget_bytes
            ),
            theme::style(theme::Role::Dim),
        ),
    ]));
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        format!("Estimator: {}", view.estimator),
        theme::style(theme::Role::Faint),
    )));
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        "Tools",
        theme::style(theme::Role::Bold),
    )));
    for wrapped in wrap_text(&format!("  {}", view.tool_names.join(", ")), width) {
        lines.push(Line::from(wrapped));
    }
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        "Skills",
        theme::style(theme::Role::Bold),
    )));
    for wrapped in wrap_text(&format!("  {}", view.skill_names.join(", ")), width) {
        lines.push(Line::from(wrapped));
    }
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        "Skill diagnostics",
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

pub(super) fn render_trust_dialog(frame: &mut Frame, area: Rect, root: &Path) {
    // 文本按对话框 84% 宽的“最小合理终端”（约 80 列内宽）换行；
    // 更宽的终端留白更多，更窄的终端由 Paragraph 截断右侧。
    // 边框 2 列与弹窗内边距 2×POPUP_TEXT_PADDING 列先扣掉。
    let inner_width = 84usize.saturating_sub(2 + 2 * POPUP_TEXT_PADDING as usize);
    let mut lines = vec![
        Line::from(Span::styled(
            "Trust this project?",
            theme::style(theme::Role::Bold),
        )),
        Line::from(""),
        Line::from("CLAT reads and modifies files, and runs tools inside:"),
    ];
    for wrapped in wrap_text(&root.display().to_string(), inner_width.saturating_sub(2)) {
        lines.push(Line::from(format!("  {wrapped}")));
    }
    lines.push(Line::from(""));
    lines.push(Line::from(
        "Trusting is remembered per directory. Review the project (e.g. its",
    ));
    lines.push(Line::from(
        "README and configs) before granting an agent access to it.",
    ));
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        "Enter / y — trust this project      ·      Esc / n — exit CLAT",
        theme::style(theme::Role::Bold),
    )));

    let height = (lines.len() as u16 + 2).min(popup_height_cap(area));
    let dialog = centered_rect(84, height.max(10), area);
    clear_popup_with_guards(frame, dialog);
    frame.render_widget(Paragraph::new(lines).block(popup_block("Trust")), dialog);
}
