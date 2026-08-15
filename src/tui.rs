use crate::presets::preset_by_id;
use crate::providers::{ProviderRuntime, fetch_deepseek_balance, fetch_glm_quota};
use crate::storage::{Storage, StoredMessage};
use crate::tui_input::InputBuffer;
use crate::tui_markdown::render_markdown;
use crate::tui_model::{EditorAction, ModelEditor, ModelPicker, PickerAction};
use crate::tui_sessions::{ResumeAction, SessionPicker};
use crate::tui_worker::{UiEvent, WorkerMessage, execute_run};
use crate::{
    CancelToken, ModelConfig, ModelEvent, ModelItem, PermissionDecision, PermissionRequest,
    Project, RunEvent, ToolRegistry, Usage, register_native_read_tools,
};
use crossterm::event::{
    self, DisableBracketedPaste, DisableMouseCapture, Event, KeyCode, KeyEvent, KeyEventKind,
    KeyModifiers, KeyboardEnhancementFlags, MouseButton, MouseEvent, MouseEventKind,
    PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
};
use crossterm::execute;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{
    Block, Borders, Clear, Paragraph, Scrollbar, ScrollbarOrientation, ScrollbarState, Wrap,
};
use ratatui::{DefaultTerminal, Frame};
use std::collections::HashMap;
use std::env;
use std::io::{self, Write, stdout};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ChatRole {
    User,
    Assistant,
}

/// Spinner frames for the "thinking" indicator, advancing on every render
/// tick.
const SPINNER_FRAMES: [&str; 8] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧"];

/// Official DeepSeek palette used by the harness turn-status shimmer: the
/// text sits in the brand blue while a lighter band glides across it.
const DEEPSEEK_500: Color = Color::Rgb(65, 118, 230);
const DEEPSEEK_200: Color = Color::Rgb(211, 226, 255);

const THINKING_TEXT: &str = "Thinking…";
/// One full band cycle, matching the harness animation duration (1.8s at
/// ~12.5 render ticks per second).
const SHIMMER_CYCLE_TICKS: f64 = 22.5;
/// Softness of the light band, in characters.
const SHIMMER_SIGMA: f64 = 1.2;

/// Builds the animated status line: a rotating spinner plus "Thinking…" in
/// the fixed DeepSeek blue, with a soft light band sweeping front to back.
/// This replicates the harness text-shimmer (a gradient
/// deepseek-500 → deepseek-200 → deepseek-500 gliding across the text and
/// wrapping around), and appends the elapsed thinking time like the
/// harness clock.
fn thinking_line(tick: u64, elapsed: Option<Duration>) -> Line<'static> {
    let frame = SPINNER_FRAMES[(tick as usize) % SPINNER_FRAMES.len()];
    let base = Style::default().fg(DEEPSEEK_500);

    let mut spans = vec![
        Span::styled(frame, base.add_modifier(Modifier::BOLD)),
        Span::styled(" ", base),
    ];

    let text_len = THINKING_TEXT.chars().count() as f64;
    let band_center = (tick as f64 % SHIMMER_CYCLE_TICKS) / SHIMMER_CYCLE_TICKS * text_len;
    for (index, ch) in THINKING_TEXT.chars().enumerate() {
        let position = index as f64 + 0.5;
        let mut distance = (position - band_center).abs();
        if distance > text_len / 2.0 {
            distance = text_len - distance; // the band wraps around the text
        }
        let intensity = (-(distance * distance) / (2.0 * SHIMMER_SIGMA * SHIMMER_SIGMA)).exp();
        spans.push(Span::styled(
            ch.to_string(),
            Style::default().fg(blend_color(DEEPSEEK_500, DEEPSEEK_200, intensity)),
        ));
    }

    if let Some(elapsed) = elapsed {
        spans.push(Span::styled(
            format!(" {:>3}s", elapsed.as_secs()),
            Style::default().fg(Color::DarkGray),
        ));
    }
    Line::from(spans)
}

/// Linear blend between two RGB colors; `amount` is in 0..=1.
fn blend_color(from: Color, to: Color, amount: f64) -> Color {
    fn channel(a: u8, b: u8, amount: f64) -> u8 {
        (a as f64 + (b as f64 - a as f64) * amount).round() as u8
    }
    match (from, to) {
        (Color::Rgb(fr, fg, fb), Color::Rgb(tr, tg, tb)) => Color::Rgb(
            channel(fr, tr, amount),
            channel(fg, tg, amount),
            channel(fb, tb, amount),
        ),
        _ => from,
    }
}

/// 配置是否指向 DeepSeek 官方端点。
fn is_deepseek_endpoint(config: &ModelConfig) -> bool {
    config.endpoint.to_lowercase().contains("deepseek.com")
}

/// 配置是否指向 GLM Coding Plan 端点（国内 open.bigmodel.cn 或海外
/// api.z.ai）。
fn is_glm_endpoint(config: &ModelConfig) -> bool {
    let endpoint = config.endpoint.to_lowercase();
    endpoint.contains("bigmodel.cn") || endpoint.contains("z.ai")
}

/// 余额监控线程读取的共享上下文：配置或密钥变更后主循环原地更新，
/// 线程每次查询前取最新值，无需重启。
#[derive(Clone)]
struct BalanceContext {
    config: ModelConfig,
    runtime: ProviderRuntime,
}

/// 查询一次余额/额度（DeepSeek 余额或 GLM 窗口百分比）。非厂商
/// 端点或未配置密钥返回 None——状态栏对应段落随之隐藏。
fn fetch_balance_value(context: &BalanceContext) -> Option<String> {
    if !is_deepseek_endpoint(&context.config) && !is_glm_endpoint(&context.config) {
        return None;
    }
    let api_key = context.runtime.value(0)?.trim().to_owned();
    if api_key.is_empty() {
        return None;
    }
    if is_glm_endpoint(&context.config) {
        fetch_glm_quota(&context.config.endpoint, &api_key)
    } else {
        fetch_deepseek_balance(&context.config.endpoint, &api_key)
    }
}

/// 余额后台监控：启动立即查询一次，此后每 `BALANCE_REFRESH_INTERVAL`
/// 主动巡查一次；`trigger` 收到信号（配置变更、模型运行结束）则立即
/// 额外查询。查询在别的端点/密钥上跳过但保持周期。结果经统一事件
/// 通道送回主循环，线程随通道关闭退出。
fn spawn_balance_monitor(
    ui: Sender<UiEvent>,
    context: Arc<Mutex<BalanceContext>>,
    trigger: Receiver<()>,
) {
    thread::spawn(move || {
        let mut next_sweep = Instant::now();
        loop {
            let wait = next_sweep.saturating_duration_since(Instant::now());
            match trigger.recv_timeout(wait) {
                // 被触发（立即查询）或巡查周期到：两者都执行查询。
                Ok(()) | Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            }
            next_sweep = Instant::now() + BALANCE_REFRESH_INTERVAL;
            let value = context
                .lock()
                .ok()
                .map(|context| fetch_balance_value(&context));
            let value = match value {
                Some(value) => value,
                // 锁中毒：跳过本轮，下个周期再试。
                None => continue,
            };
            if ui.send(UiEvent::Balance(value)).is_err() {
                break;
            }
        }
    });
}

/// 会话累计的缓存命中百分比文本（如 "87%"）。无输入 token 或服务端
/// 未上报缓存命中时不显示（返回 None）。
fn cache_hit_percent(usage: &Usage) -> Option<String> {
    let cached = usage.cached_input_tokens?;
    if usage.input_tokens == 0 || cached == 0 {
        return None;
    }
    let percent = cached as f64 / usage.input_tokens as f64 * 100.0;
    Some(format!("{}%", percent.round() as u64))
}

/// 缩短路径用于状态栏展示：home 前缀替换为 `~`
/// （如 `~/Documents/GitHub/clat`），非 home 路径原样返回。
fn abbreviate_home(path: &Path) -> String {
    let home = env::var_os("HOME")
        .or_else(|| env::var_os("USERPROFILE"))
        .map(PathBuf::from);
    match home {
        Some(home) => abbreviate_with(path, &home),
        None => path.display().to_string(),
    }
}

fn abbreviate_with(path: &Path, home: &Path) -> String {
    match path.strip_prefix(home) {
        Ok(rest) if rest.as_os_str().is_empty() => "~".to_string(),
        Ok(rest) => format!("~/{}", rest.display()),
        Err(_) => path.display().to_string(),
    }
}

/// 鼠标选区所在的组件。
#[derive(Clone, Copy, PartialEq, Eq)]
enum SelectionKind {
    Conversation,
    Input,
}

/// 内容坐标系中的位置：第几行、第几列（均为从 0 开始）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct SelectionPos {
    row: usize,
    col: usize,
}

/// 鼠标拖拽选区。anchor 是按下位置，head 随拖动更新；两者按内容坐标
/// 排序即得到选区范围，因此滚动或内容增长后依然指向原文本行。
#[derive(Clone, Copy)]
struct TextSelection {
    kind: SelectionKind,
    anchor: SelectionPos,
    head: SelectionPos,
    /// 鼠标按键是否仍按住；松开后保留选区供 Cmd+C 复用。
    active: bool,
}

impl TextSelection {
    fn ordered(&self) -> (SelectionPos, SelectionPos) {
        if self.anchor <= self.head {
            (self.anchor, self.head)
        } else {
            (self.head, self.anchor)
        }
    }

    fn is_empty(&self) -> bool {
        self.anchor == self.head
    }
}

/// 组件边框内的内容区域。
fn content_rect(area: Rect) -> Rect {
    Rect::new(
        area.x + 1,
        area.y + 1,
        area.width.saturating_sub(2),
        area.height.saturating_sub(2),
    )
}

/// 屏幕坐标 → 内容坐标。指针必须落在内容区内，否则返回 None。
fn content_pos(area: Rect, x: u16, y: u16) -> Option<SelectionPos> {
    let inner = content_rect(area);
    if x < inner.x || x >= inner.x + inner.width || y < inner.y || y >= inner.y + inner.height {
        return None;
    }
    Some(SelectionPos {
        row: (y - inner.y) as usize,
        col: (x - inner.x) as usize,
    })
}

/// 屏幕坐标 → 内容坐标（越界时钳制在内容区内，拖动出界时使用）。
fn clamped_pos(area: Rect, rows: usize, x: u16, y: u16) -> SelectionPos {
    let inner = content_rect(area);
    SelectionPos {
        row: (y.saturating_sub(inner.y) as usize).min(rows.saturating_sub(1)),
        col: (x.saturating_sub(inner.x) as usize).min(inner.width as usize),
    }
}

/// 将一行中列区间 [from, to) 内的字符加反显（REVERSED）样式，其余
/// 保持原样。span 可能被选区从中切开，按字符切成连续的选/未选片段。
fn highlight_line(line: &Line<'static>, from: usize, to: usize) -> Line<'static> {
    let mut spans: Vec<Span<'static>> = Vec::new();
    let mut column = 0usize;
    for span in &line.spans {
        let style = span.style;
        // (是否选中, 文本) 的连续片段
        let mut runs: Vec<(bool, String)> = Vec::new();
        for ch in span.content.chars() {
            let width = UnicodeWidthChar::width(ch).unwrap_or(0);
            let selected = column + width > from && column < to;
            match runs.last_mut() {
                Some((is_selected, buffer)) if *is_selected == selected => buffer.push(ch),
                _ => runs.push((selected, ch.to_string())),
            }
            column += width;
        }
        for (selected, text) in runs {
            let style = if selected {
                style.add_modifier(Modifier::REVERSED)
            } else {
                style
            };
            spans.push(Span::styled(text, style));
        }
    }
    Line::from(spans)
}

/// 一行的纯文本（拼接所有 span），用于选区复制。
fn line_plain_text(line: &Line<'_>) -> String {
    let mut text = String::new();
    for span in &line.spans {
        text.push_str(&span.content);
    }
    text
}

/// 按显示列区间截取纯文本：与 [from, to) 有重叠的字符整字入选区，
/// 因此宽字符（CJK）从中间被点到时不会被切成半个。
fn slice_by_columns(text: &str, from: usize, to: usize) -> String {
    let mut out = String::new();
    let mut column = 0usize;
    for ch in text.chars() {
        let width = UnicodeWidthChar::width(ch).unwrap_or(0);
        if column + width > from && column < to {
            out.push(ch);
        }
        column += width;
    }
    out
}

/// base64 编码（OSC 52 剪贴板写入需要）。项目不引第三方 base64 依赖，
/// 这十几行足够覆盖该场景。
fn base64_encode(data: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let word = (chunk[0] as u32) << 16
            | (*chunk.get(1).unwrap_or(&0) as u32) << 8
            | (*chunk.get(2).unwrap_or(&0) as u32);
        out.push(TABLE[(word >> 18 & 63) as usize] as char);
        out.push(TABLE[(word >> 12 & 63) as usize] as char);
        out.push(if chunk.len() > 1 {
            TABLE[(word >> 6 & 63) as usize] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            TABLE[(word & 63) as usize] as char
        } else {
            '='
        });
    }
    out
}

/// 通过 OSC 52 把文本写入系统剪贴板。iTerm2 / WezTerm / kitty /
/// VS Code 等终端支持；不支持的终端（如 macOS Terminal.app）会静默
/// 忽略，用户仍可按住 Shift 用终端原生方式选择复制。
fn copy_to_clipboard(text: &str) -> bool {
    if text.is_empty() {
        return false;
    }
    let mut out = stdout();
    write!(out, "\x1b]52;c;{}\x1b\\", base64_encode(text.as_bytes()))
        .and_then(|_| out.flush())
        .is_ok()
}

/// 构建厂商状态栏后缀，渲染在底部状态栏最右边（间隔号连接）：
/// - DeepSeek：`￥余额 · 缓存% · Thinking · 强度`
/// - GLM Coding Plan：`剩余额度% · 缓存% · Thinking · 强度`（5 小时
///   窗口剩余额度，替代 DeepSeek 余额的位置）
///
/// 思考开关关闭时省略后两项；槽位无值（未配置 key 或查询失败）返回
/// 空，状态栏保持原样。思考强度按官方规则展示为 High/Max，未显式
/// 设置时按官方默认 high 显示 High。
fn deepseek_status_prefix(config: &ModelConfig, balance: &Option<String>, usage: &Usage) -> String {
    if !is_deepseek_endpoint(config) && !is_glm_endpoint(config) {
        return String::new();
    }
    let Some(balance) = balance else {
        return String::new();
    };
    // DeepSeek 槽位存余额文本，展示加货币符号；GLM 槽位存 5 小时
    // 窗口剩余额度百分比（如 "62%"），原样展示。
    let first = if is_deepseek_endpoint(config) {
        format!("￥{balance}")
    } else {
        balance.clone()
    };
    let mut parts = vec![first];
    if let Some(percent) = cache_hit_percent(usage) {
        parts.push(percent);
    }

    // 未显式设置 thinking 时，DeepSeek 服务端默认开启思考模式。
    let thinking_enabled = config
        .extra_body
        .get("thinking")
        .and_then(|thinking| thinking.get("type"))
        .and_then(serde_json::Value::as_str)
        .map(|kind| kind == "enabled")
        .unwrap_or(true);
    if thinking_enabled {
        parts.push("Thinking".into());
        let effort = config
            .extra_body
            .get("reasoning_effort")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("high");
        let label = if effort.eq_ignore_ascii_case("max") {
            "Max"
        } else {
            "High"
        };
        parts.push(label.into());
    }
    parts.join(" · ")
}

/// Rows moved per mouse-wheel notch (and per Up/Down press while those
/// keys scroll the conversation). Claude Code moves about two rows per
/// notch; tune this to taste.
const WHEEL_SCROLL_ROWS: usize = 2;
/// Rows moved per PageUp/PageDown.
const PAGE_SCROLL_ROWS: usize = 8;

/// 输入框行首前缀（首行 `❯ `，续行两个空格）的显示宽度。文本换行、
/// 光标定位与鼠标选区映射统一扣除该宽度，保持三者坐标一致。
const INPUT_MARKER_WIDTH: usize = 2;

/// 思考动画的帧间隔：thinking 期间后台按时刻唤醒主循环换帧。
const SPINNER_FRAME: Duration = Duration::from_millis(80);

/// 余额/额度后台监控的主动刷新周期。官方平台的用量数据本身有约
/// 5 分钟延迟，更频繁的查询拿不到新数字，只是给官方服务器徒增压力。
/// 与旧的"被动刷新 + 最小间隔门禁"不同：这里是监控线程的巡查周期，
/// 配置变更与模型运行结束会立即触发一次计划外刷新。
const BALANCE_REFRESH_INTERVAL: Duration = Duration::from_secs(5 * 60);

/// 瞬时提示（copied N chars、model switched 等）在状态栏停留的时长，
/// 到期回落为默认的当前目录显示——与 Claude Code 等终端工具一致：
/// 提示是瞬态的，目录才是常驻信息。
const STATUS_TTL: Duration = Duration::from_secs(4);

/// 瞬时提示是否已到期：无过期时刻（常驻状态）视为未到期。
fn status_expired(until: Option<Instant>, now: Instant) -> bool {
    until.is_some_and(|until| now >= until)
}

/// User messages get a soft dark block with bright text, in the spirit of
/// Claude Code, so they stand apart from the model's plain replies. The
/// explicit foreground keeps the block readable on light terminals too.
const USER_BG: Color = Color::Rgb(48, 50, 60);
const USER_FG: Color = Color::Rgb(233, 234, 239);

fn user_message_style() -> Style {
    Style::default().fg(USER_FG).bg(USER_BG)
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ChatMessage {
    role: ChatRole,
    content: String,
}

impl ChatMessage {
    fn user(content: impl Into<String>) -> Self {
        Self {
            role: ChatRole::User,
            content: content.into(),
        }
    }

    fn assistant(content: impl Into<String>) -> Self {
        Self {
            role: ChatRole::Assistant,
            content: content.into(),
        }
    }

    fn from_stored(message: StoredMessage) -> Option<Self> {
        let role = match message.role.as_str() {
            "user" => ChatRole::User,
            "assistant" => ChatRole::Assistant,
            _ => return None,
        };
        Some(Self {
            role,
            content: message.content,
        })
    }

    fn model_item(&self) -> ModelItem {
        match self.role {
            ChatRole::User => ModelItem::user_text(self.content.clone()),
            ChatRole::Assistant => ModelItem::assistant_text(self.content.clone()),
        }
    }
}

pub fn run(project: Project) -> io::Result<()> {
    let mut terminal = ratatui::init();

    // ratatui::init() 安装的 panic hook 只恢复 raw mode 和备用屏幕，
    // 不覆盖下面手动启用的鼠标模式、bracketed paste 和 kitty 键盘增强。
    // 这里补充一个 hook，确保这些模式在 panic 时也被清理，避免用户的
    // 终端残留异常状态。
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = execute!(
            stdout(),
            DisableMouseCapture,
            DisableBracketedPaste,
            PopKeyboardEnhancementFlags
        );
        let _ = stdout().write_all(b"\x1b[?1006l");
        let _ = stdout().flush();
        default_hook(info);
    }));

    // Enable mouse reporting without any-event tracking (1003): CLAT only
    // needs clicks (1000), drags (1002), and the wheel via SGR coordinates
    // (1006). crossterm's EnableMouseCapture also turns 1003 on, and in
    // that mode some terminals report the wheel as motion events that
    // crossterm parses as Moved instead of ScrollUp/ScrollDown.
    let mut out = stdout();
    if let Err(error) = out
        .write_all(b"\x1b[?1000h\x1b[?1002h\x1b[?1006h")
        .and_then(|_| out.flush())
    {
        ratatui::restore();
        return Err(error);
    }
    // bracketed paste（DEC 2004）：粘贴以专用转义序列包裹，crossterm
    // 解析为 Event::Paste。没有它，多行粘贴的换行会逐个触发 Enter 把
    // 输入提前提交。
    if let Err(error) = execute!(out, crossterm::event::EnableBracketedPaste) {
        ratatui::restore();
        return Err(error);
    }
    // Ask supporting terminals (iTerm2, WezTerm, kitty, VS Code, …) to
    // report key modifiers so Shift+Enter is distinguishable from Enter.
    // Terminals without the kitty protocol silently ignore this.
    let _ = execute!(
        stdout(),
        PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES)
    );

    let result = App::new(project)
        .map_err(io::Error::other)
        .and_then(|mut app| app.run(&mut terminal));

    let _ = execute!(stdout(), PopKeyboardEnhancementFlags);
    let paste_result = execute!(stdout(), DisableBracketedPaste);
    let mouse_result = execute!(stdout(), DisableMouseCapture);
    ratatui::restore();
    result.and(mouse_result).and(paste_result)
}

struct PendingPermission {
    request: PermissionRequest,
    decision_tx: Sender<PermissionDecision>,
    argument_scroll: usize,
    argument_page_size: usize,
    argument_line_count: usize,
    /// 从首行起连续进入过视口的行数；跳到 End 不会跨过未审阅区。
    reviewed_through: usize,
    /// 只有参数最后一页实际进入过视口，才允许批准。
    reviewed_to_end: bool,
}

struct App {
    project: Project,
    storage: Storage,
    /// 当前会话 id；`None` 表示项目尚未受信（延迟初始化前不可对话）。
    /// 确权门保证所有用到它的路径只在 Some 时可达。
    session_id: Option<i64>,
    config: ModelConfig,
    provider_runtime: ProviderRuntime,
    /// 本会话共享的工具注册表：内建工具 + MCP 工具。MCP 子进程随
    /// 注册表存活（App 生命周期），跨多次 Run 复用，不在每次运行间
    /// 反复启停服务器。
    tools: Arc<ToolRegistry>,
    messages: Vec<ChatMessage>,
    input: InputBuffer,
    /// 启动时项目未受信：渲染确权对话框并拦截一切按键，直到用户
    /// 信任（Enter，持久化）或退出（Esc）。
    trust_prompt: bool,
    /// 常驻状态栏文本（当前项目目录），瞬时提示过期后回落到这里。
    default_status: String,
    status: String,
    /// 瞬时提示的过期时刻；None 表示当前 status 即常驻内容。
    status_until: Option<Instant>,
    editor: Option<ModelEditor>,
    /// 二级模型选择器；与 editor 互斥，/model 命令打开。
    picker: Option<ModelPicker>,
    /// /resume 会话选择器；打开期间独占按键与鼠标。
    session_picker: Option<SessionPicker>,
    running: bool,
    /// 统一事件通道：输入线程、余额监控、worker 的消息都汇到这里。
    /// `None` 表示尚未启动（run() 建立通道后填充）。
    events: Option<Receiver<UiEvent>>,
    /// 统一事件通道的发送端克隆：start_run 移交 worker，刷新触发用。
    event_sender: Option<Sender<UiEvent>>,
    pending_permission: Option<PendingPermission>,
    cancel_token: Option<CancelToken>,
    thinking: bool,
    thinking_since: Option<Instant>,
    spinner_tick: u64,
    assistant_message_index: Option<usize>,
    /// Rendered markdown per message, keyed by (index, content length,
    /// width) so stable messages are not re-parsed every frame.
    markdown_cache: HashMap<(usize, usize, usize), Vec<Line<'static>>>,
    conversation_scroll_from_bottom: usize,
    input_area: Rect,
    editor_area: Option<Rect>,
    conversation_area: Rect,
    /// 当前会话内容首行在总行数中的偏移（draw 时记录，供鼠标选区映射）。
    conversation_start: usize,
    /// 会话内容的总行数（draw 时记录，供鼠标选区映射）。
    conversation_rows: usize,
    /// 鼠标拖拽建立的文本选区；坐标保存在内容坐标系中，滚动后高亮
    /// 与复制仍然有效。
    selection: Option<TextSelection>,
    should_quit: bool,
    /// 余额/额度当前值：监控线程查询后经事件通道写回，状态栏读取。
    balance: Option<String>,
    /// 余额监控的共享上下文（配置/密钥变更后原地更新）与立即刷新
    /// 触发器。
    balance_context: Arc<Mutex<BalanceContext>>,
    balance_trigger: Option<Sender<()>>,
    /// 本会话累计 token 用量，用于状态栏缓存命中百分比。
    session_usage: Usage,
}

impl App {
    /// 两阶段构造（A-02）：
    ///
    /// 1. **最小构造**——只打开全局存储、查询信任表。未受信目录在
    ///    此阶段不建会话、不读项目历史、不发任何网络请求、不启动
    ///    任何 MCP 子进程（恶意项目可用本地文件劫持 `npx` 等查询
    ///    cwd 的命令，确权前绝不能替它拉起进程）。
    /// 2. **项目初始化**（已受信时立即执行；未受信时在确权成功后
    ///    执行）——加载会话/消息/历史/模型配置，并以 `~/.clat` 为
    ///    固定 cwd 启动 MCP 服务器。
    fn new(project: Project) -> Result<Self, String> {
        let storage = Storage::open_default().map_err(|error| error.to_string())?;
        let trusted = storage.is_project_trusted(project.root());

        // 最小工具集（纯注册，无进程、无 I/O）。
        let mut tools = ToolRegistry::new();
        register_native_read_tools(&mut tools);
        let tools = Arc::new(tools);

        let (config, provider_runtime) = if trusted {
            Self::load_model_state(&storage)?
        } else {
            let config = ModelConfig::default();
            let runtime = ProviderRuntime::for_protocol(config.protocol);
            (config, runtime)
        };

        // 状态栏初始显示当前打开的项目目录（home 缩写为 ~）。
        let status = abbreviate_home(project.root());
        let balance_context = Arc::new(Mutex::new(BalanceContext {
            config: config.clone(),
            runtime: provider_runtime.clone(),
        }));

        let mut app = Self {
            project,
            storage,
            session_id: None,
            tools,
            config,
            provider_runtime,
            messages: Vec::new(),
            input: InputBuffer::new(Vec::new()),
            trust_prompt: !trusted,
            default_status: status.clone(),
            status,
            status_until: None,
            editor: None,
            picker: None,
            session_picker: None,
            running: false,
            events: None,
            event_sender: None,
            pending_permission: None,
            cancel_token: None,
            thinking: false,
            thinking_since: None,
            spinner_tick: 0,
            assistant_message_index: None,
            markdown_cache: HashMap::new(),
            conversation_scroll_from_bottom: 0,
            input_area: Rect::default(),
            editor_area: None,
            conversation_area: Rect::default(),
            conversation_start: 0,
            conversation_rows: 0,
            selection: None,
            should_quit: false,
            balance: None,
            balance_context,
            balance_trigger: None,
            session_usage: Usage::default(),
        };
        if trusted {
            app.initialize_project()?;
        }
        Ok(app)
    }

    /// 全局模型配置（与项目无关，但同样推迟到确权后加载以保持
    /// "未信任阶段零读取"的简单不变量）。预设代表厂商官方参数，
    /// 随版本演进（如 DeepSeek 补充流式 usage 开关）；启动时重刷
    /// 预设参数，存量配置自动获得修复，用户手动改过的配置
    /// （preset 为 None）保持原样。
    fn load_model_state(storage: &Storage) -> Result<(ModelConfig, ProviderRuntime), String> {
        let (mut config, runtime) = storage
            .load_model_state()
            .map_err(|error| error.to_string())?
            .unwrap_or_else(|| {
                let config = ModelConfig::default();
                let runtime = ProviderRuntime::for_protocol(config.protocol);
                (config, runtime)
            });
        if let Some(preset) = config.preset.as_deref().and_then(preset_by_id) {
            preset.apply(&mut config);
        }
        Ok((config, runtime))
    }

    /// 项目级资源初始化：会话、历史消息、输入历史、模型配置与
    /// MCP 服务器。只在项目已受信后调用（构造时已信任，或确权
    /// 成功后）。任何失败向上报告——确权流程据此保持阻断。
    fn initialize_project(&mut self) -> Result<(), String> {
        // 默认恢复最近会话；无可恢复会话时 session_id 保持 None，
        // 首条内容写入时才按需建会话（见 current_session）。
        if let Some(session_id) = self
            .storage
            .current_session(&self.project)
            .map_err(|error| error.to_string())?
        {
            self.session_id = Some(session_id);
            self.messages = self
                .storage
                .load_messages(session_id)
                .map_err(|error| error.to_string())?
                .into_iter()
                .filter_map(ChatMessage::from_stored)
                .collect();
            let history = self
                .storage
                .load_input_history(session_id, 500)
                .map_err(|error| error.to_string())?;
            self.input = InputBuffer::new(history);
        }
        self.markdown_cache.clear();

        let (config, provider_runtime) = Self::load_model_state(&self.storage)?;
        self.config = config;
        self.provider_runtime = provider_runtime;

        // MCP 服务器是全局能力，但绝不在项目目录里启动：固定以
        // `~/.clat` 为工作目录，杜绝未受信项目通过 cwd 劫持命令。
        let mut tools = ToolRegistry::new();
        register_native_read_tools(&mut tools);
        let mcp_config = crate::mcp_client::load_mcp_config(self.storage.root())
            .map_err(|error| error.to_string())?;
        let (mcp_connected, mcp_failures) =
            crate::mcp_client::register_mcp_tools(&mut tools, &mcp_config, self.storage.root());
        self.tools = Arc::new(tools);
        if !mcp_config.is_empty() {
            if mcp_failures.is_empty() {
                self.flash_status(format!("mcp: {mcp_connected} server(s) connected"));
            } else {
                self.flash_status(format!("mcp: {}", mcp_failures.join("; ")));
            }
        }
        self.refresh_balance_now();
        Ok(())
    }

    /// 写路径使用的会话 id：`None` 表示尚无会话——/new 之后或
    /// 从未有过会话——此时**按需创建**（首条内容写入才落盘），
    /// 空会话永远不进库。确权门保证只在已信任状态可达。
    fn current_session(&mut self) -> Result<i64, String> {
        match self.session_id {
            Some(id) => Ok(id),
            None => {
                let id = self
                    .storage
                    .create_session(&self.project)
                    .map_err(|error| error.to_string())?;
                self.session_id = Some(id);
                Ok(id)
            }
        }
    }

    fn run(&mut self, terminal: &mut DefaultTerminal) -> io::Result<()> {
        // 统一事件通道与后台监控：主循环从此只与消息打交道。
        //
        // - 输入线程阻塞读终端事件（event::read），按键到达即刻转发，
        //   零轮询、零节流——不再有 60ms/16ms 的自适应间隔；
        // - 余额监控线程启动立即查询，此后每 5 分钟巡查一次；
        // - 无消息时主循环挂起在 recv_timeout 上，唤醒时刻是"最近
        //   一次必须重绘的deadline"（状态栏 TTL 到期、思考动画换帧），
        //   空闲时一次唤醒都没有。
        let (event_sender, events) = mpsc::channel::<UiEvent>();
        self.event_sender = Some(event_sender.clone());
        self.events = Some(events);

        let input_sender = event_sender.clone();
        // detach：通道关闭（run 返回）后 send 失败线程自行退出；
        // 阻塞中的 read 随进程结束回收，不影响终端恢复。
        thread::spawn(move || {
            while let Ok(event) = event::read()
                && input_sender.send(UiEvent::Terminal(event)).is_ok()
            {}
        });

        let (balance_trigger, trigger_receiver) = mpsc::channel::<()>();
        self.balance_trigger = Some(balance_trigger);
        spawn_balance_monitor(
            event_sender.clone(),
            Arc::clone(&self.balance_context),
            trigger_receiver,
        );

        let events = self
            .events
            .take()
            .expect("events channel was just installed");
        while !self.should_quit {
            let deadline = self.next_repaint_deadline();
            let received = match deadline {
                Some(deadline) => events.recv_timeout(deadline - Instant::now()),
                // 没有任何未来重绘需求：无限挂起直到下一条消息。
                None => events
                    .recv()
                    .map_err(|_| mpsc::RecvTimeoutError::Disconnected),
            };
            match received {
                Ok(event) => {
                    self.handle_ui_event(event);
                    // 批量收割全部已就绪事件并合并成一帧绘制。不能用
                    // `while let Terminal = try_recv()`：模式不匹配也会
                    // 消费 Worker/Permission 消息，令运行永久等待。
                    // 权限审阅期间每个导航键后都要先绘制，确保只有
                    // 真正进入过视口的连续参数页会计入 reviewed。
                    if self.pending_permission.is_none() {
                        while let Ok(event) = events.try_recv() {
                            self.handle_ui_event(event);
                            if self.pending_permission.is_some() {
                                break;
                            }
                        }
                    }
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            }
            self.expire_status();
            terminal.draw(|frame| self.draw(frame))?;
        }
        // 退出时清场：只删空会话（历史遗留/异常路径兜底）。
        // 非空会话**绝不归档**——否则 resume 过的会话下次启动
        // "消失"（v0.3.3 前的实机事故）。
        if let Some(current) = self.session_id {
            let _ = self.storage.delete_session_if_empty(current);
        }
        Ok(())
    }

    fn handle_ui_event(&mut self, event: UiEvent) {
        match event {
            UiEvent::Terminal(event) => self.handle_terminal_event(event),
            UiEvent::Worker(message) => self.handle_worker_message(message),
            UiEvent::Balance(value) => self.balance = value,
        }
    }

    /// 处理一条终端事件：按键/粘贴/鼠标。
    ///
    /// 项目确权门拦截**一切**终端事件：只有按键转给 handle_key（其
    /// 中的确权分支只认 Enter/y 与 Esc/n）。鼠标滚动、拖拽选区、粘贴
    /// 全部吞掉——否则确权框后面还能滚动会话、往输入框粘贴内容，
    /// 甚至选区高亮会盖住对话框边框，看起来像边框被切掉。
    fn handle_terminal_event(&mut self, event: Event) {
        if self.trust_prompt {
            if let Event::Key(key) = event
                && key.kind == KeyEventKind::Press
            {
                self.handle_key(key);
            }
            return;
        }
        match event {
            Event::Key(key) if key.kind == KeyEventKind::Press => self.handle_key(key),
            Event::Paste(text) => self.handle_paste(&text),
            Event::Mouse(mouse) => self.handle_mouse(mouse),
            _ => {}
        }
    }

    /// 最近一次必须重绘的时刻：状态栏瞬时提示到期、思考动画换帧。
    /// None 表示可以无限挂起等待下一条消息。
    fn next_repaint_deadline(&self) -> Option<Instant> {
        let now = Instant::now();
        let mut deadline = self.status_until.filter(|until| *until > now);
        if self.thinking {
            let frame = now + SPINNER_FRAME;
            deadline = Some(deadline.map_or(frame, |current| current.min(frame)));
        }
        deadline
    }

    fn handle_key(&mut self, key: KeyEvent) {
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
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
                let trusted = self
                    .storage
                    .trust_project(self.project.root())
                    .map_err(|error| error.to_string())
                    .and_then(|()| self.initialize_project());
                match trusted {
                    Ok(()) => {
                        self.trust_prompt = false;
                        self.flash_status("project trusted — welcome");
                    }
                    Err(error) => self.flash_status(format!("failed to trust project: {error}")),
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
            let requested_allow = matches!(
                key.code,
                KeyCode::Enter | KeyCode::Char('y') | KeyCode::Char('Y')
            );
            let deny = matches!(
                key.code,
                KeyCode::Esc | KeyCode::Char('n') | KeyCode::Char('N')
            );
            let mut blocked_allow = false;
            let mut allow = false;
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
            }
            if (allow || deny)
                && let Some(pending) = self.pending_permission.take()
            {
                let decision = if allow {
                    PermissionDecision::Allow
                } else {
                    PermissionDecision::Deny {
                        reason: "denied by user".into(),
                    }
                };
                let _ = pending.decision_tx.send(decision);
                if allow {
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
            KeyCode::Enter if !self.running => {
                // Claude Code style: Shift+Enter (or Alt+Enter) inserts a
                // line break, plain Enter submits. Ctrl+J is the fallback
                // for terminals that cannot distinguish Shift+Enter.
                if key
                    .modifiers
                    .intersects(KeyModifiers::SHIFT | KeyModifiers::ALT)
                {
                    self.input.insert_newline();
                } else {
                    self.submit_input();
                }
            }
            KeyCode::Char('j')
                if !self.running && key.modifiers.contains(KeyModifiers::CONTROL) =>
            {
                self.input.insert_newline();
            }
            KeyCode::Backspace if !self.running => self.input.backspace(),
            KeyCode::Delete if !self.running => self.input.delete(),
            KeyCode::Left if !self.running => self.input.left(),
            KeyCode::Right if !self.running => self.input.right(),
            KeyCode::Home if !self.running => self.input.home(),
            KeyCode::End if !self.running => self.input.end(),
            KeyCode::Up if !self.running => {
                // With no input history to recall, the arrows scroll the
                // conversation instead of doing nothing.
                if self.input.history_is_empty() {
                    self.scroll_up(WHEEL_SCROLL_ROWS);
                } else {
                    self.input.history_previous();
                }
            }
            KeyCode::Down if !self.running => {
                if self.input.history_is_empty() {
                    self.scroll_down(WHEEL_SCROLL_ROWS);
                } else {
                    self.input.history_next();
                }
            }
            KeyCode::PageUp => self.scroll_up(PAGE_SCROLL_ROWS),
            KeyCode::PageDown => self.scroll_down(PAGE_SCROLL_ROWS),
            KeyCode::Esc => {
                if self.running {
                    if let Some(cancel) = &self.cancel_token {
                        cancel.cancel();
                        self.flash_status("cancelling…");
                    }
                } else {
                    self.input.clear();
                }
            }
            KeyCode::Char(ch) if !self.running => self.input.insert_char(ch),
            _ => {}
        }
    }

    fn handle_paste(&mut self, text: &str) {
        // 选择器没有文本输入目标，忽略粘贴。
        if self.picker.is_none() {
            if let Some(editor) = &mut self.editor {
                editor.handle_paste(text);
            } else if !self.running {
                self.input.insert_str(text);
            }
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

    /// 松开鼠标：空选区时单击定位输入光标；非空选区保持高亮并立即
    /// 复制到系统剪贴板（OSC 52）。
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
            if copy_to_clipboard(&text) {
                self.flash_status(format!("copied {count} chars"));
            } else {
                self.flash_status("clipboard copy failed");
            }
        }
    }

    /// 提取当前选区的文本。会话区按渲染行拼接（跨行以 \n 连接），
    /// 输入框按视觉行拼接。
    fn selection_text(&mut self) -> Option<String> {
        let selection = self.selection?;
        if selection.is_empty() {
            return None;
        }
        let (from, to) = selection.ordered();
        let mut pieces = Vec::new();
        match selection.kind {
            SelectionKind::Conversation => {
                let width = self.conversation_area.width.saturating_sub(2).max(1) as usize;
                let lines = self.conversation_lines(width);
                let last = to.row.min(lines.len().saturating_sub(1));
                for (row, line) in lines.iter().enumerate().take(last + 1).skip(from.row) {
                    let text = line_plain_text(line);
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
        if copy_to_clipboard(&text) {
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
        let _ = copy_to_clipboard(&text);
        self.flash_status(format!("cut {count} chars"));
        self.selection = None;
        true
    }

    /// 写入一条瞬时提示：显示 `STATUS_TTL` 后自动回落到常驻状态
    /// （当前目录）。
    fn flash_status(&mut self, message: impl Into<String>) {
        self.status = message.into();
        self.status_until = Some(Instant::now() + STATUS_TTL);
    }

    /// draw 前调用：瞬时提示到期后回落为常驻状态。
    fn expire_status(&mut self) {
        if status_expired(self.status_until, Instant::now()) {
            self.status = self.default_status.clone();
            self.status_until = None;
        }
    }

    /// 触发余额监控线程立即重新查询一次：把最新配置/密钥写入共享
    /// 上下文并发信号。用于配置变更与模型运行结束（额度刚被消耗）。
    fn refresh_balance_now(&mut self) {
        if let Ok(mut context) = self.balance_context.lock() {
            context.config = self.config.clone();
            context.runtime = self.provider_runtime.clone();
        }
        if let Some(trigger) = &self.balance_trigger {
            let _ = trigger.send(());
        }
    }

    /// 处理 /resume 选择器的动作：确认则切换会话，取消则关闭。
    fn apply_resume_action(&mut self, action: ResumeAction) {
        match action {
            ResumeAction::Continue => {}
            ResumeAction::Cancel => {
                self.session_picker = None;
            }
            ResumeAction::Open(session_id) => {
                self.session_picker = None;
                match self.switch_session(session_id) {
                    Ok(()) => self.flash_status("conversation resumed"),
                    Err(error) => self.flash_status(format!("failed to resume: {error}")),
                }
            }
        }
    }

    /// 切换到指定会话（/resume 确认时）：离开的会话若为空（历史
    /// 遗留行）物理删除，非空会话**原样保留**（仍可再次 resume）；
    /// 随后加载目标会话并重置视图状态。
    fn switch_session(&mut self, session_id: i64) -> Result<(), String> {
        if let Some(current) = self.session_id
            && current != session_id
        {
            self.storage
                .delete_session_if_empty(current)
                .map_err(|error| error.to_string())?;
        }
        // INV5：只读 resume 也算"打开"——触碰时间戳，让下次启动
        // 回到这里而不是最后写入过的会话。
        self.storage
            .touch_session(session_id)
            .map_err(|error| error.to_string())?;
        // /resume 列表只含未归档会话，无需 unarchive。
        self.messages = self
            .storage
            .load_messages(session_id)
            .map_err(|error| error.to_string())?
            .into_iter()
            .filter_map(ChatMessage::from_stored)
            .collect();
        self.session_id = Some(session_id);
        // 输入历史随会话切换：恢复目标会话自己的历史（含内存中
        // 未持久化的导航状态一并重置）。
        let history = self
            .storage
            .load_input_history(session_id, 500)
            .map_err(|error| error.to_string())?;
        self.input = InputBuffer::new(history);
        self.markdown_cache.clear();
        self.conversation_scroll_from_bottom = 0;
        self.assistant_message_index = None;
        Ok(())
    }

    /// 处理二级选择器的动作。确认预设时：同端点且已存有密钥 → 直接
    /// 保存切换；跨厂商或缺密钥 → 转入编辑器补密钥（清空旧厂商密钥，
    /// 避免把一家厂商的 key 发给另一家）。
    fn apply_picker_action(&mut self, action: PickerAction) {
        match action {
            PickerAction::Continue => {}
            PickerAction::Cancel => {
                self.picker = None;
                self.flash_status("model selection cancelled");
            }
            PickerAction::EditCustom => {
                self.picker = None;
                self.editor = Some(ModelEditor::new(
                    &self.config,
                    self.provider_runtime.clone(),
                ));
                self.flash_status("editing model configuration");
            }
            PickerAction::SelectPreset(preset) => {
                let mut config = self.config.clone();
                preset.apply(&mut config);
                let same_endpoint = self.config.endpoint.trim_end_matches('/')
                    == preset.endpoint.trim_end_matches('/');
                let key_present = self
                    .provider_runtime
                    .value(0)
                    .is_some_and(|value| !value.trim().is_empty());
                if same_endpoint && key_present {
                    match self
                        .storage
                        .save_model_state(&config, &self.provider_runtime)
                    {
                        Ok(()) => {
                            self.config = config;
                            // 端点或密钥可能已变化，触发立即重新查询。
                            self.refresh_balance_now();
                            self.picker = None;
                            self.flash_status(format!("model switched to {}", preset.name));
                        }
                        Err(error) => self.flash_status(format!("failed to save model: {error}")),
                    }
                } else {
                    self.picker = None;
                    let mut editor = ModelEditor::new(&config, self.provider_runtime.clone());
                    editor.apply_preset_and_focus_key(preset);
                    self.editor = Some(editor);
                    self.flash_status(format!("enter the API key for {}", preset.vendor));
                }
            }
        }
    }

    fn apply_editor_action(&mut self, action: EditorAction) {
        match action {
            EditorAction::Continue => {}
            EditorAction::Cancel => {
                self.editor = None;
                self.flash_status("model configuration cancelled");
            }
            EditorAction::Save(saved) => {
                let (config, runtime) = *saved;
                match self.storage.save_model_state(&config, &runtime) {
                    Ok(()) => {
                        self.config = config;
                        self.provider_runtime = runtime;
                        // 端点或密钥可能已变化，触发立即重新查询。
                        self.refresh_balance_now();
                        self.flash_status(format!(
                            "model saved: {} · {}",
                            self.config.protocol, self.config.model
                        ));
                        self.editor = None;
                    }
                    Err(error) => {
                        self.flash_status(format!("failed to save model: {error}"));
                    }
                }
            }
        }
    }

    fn submit_input(&mut self) {
        let value = self.input.take();
        let value = value.trim().to_owned();
        if value.is_empty() {
            return;
        }
        // 输入历史归属会话，但**命令输入不建会话**：空会话语义以
        // 聊天历史（messages/message_items）为准，`/help`、`/model`
        // 这类 UI 操作不该在库里留下任何行。只有发往模型的实质
        // 输入（start_run）才按需建会话。
        let session_for_history = if value.starts_with('/') {
            self.session_id
        } else {
            match self.current_session() {
                Ok(id) => Some(id),
                Err(error) => {
                    self.flash_status(format!("failed to open conversation: {error}"));
                    return;
                }
            }
        };
        let _ = self.storage.record_input(session_for_history, &value);
        // 命令也进内存历史：↑ 仍可召回；无会话时的命令（record_input
        // 已静默丢弃）不落盘。
        self.input.remember(value.clone());

        match value.as_str() {
            "/model" => {
                // Claude Code 风格：先选厂商（一级），再选该厂商的模型
                // （二级）；Custom 入口仍进入完整编辑器。
                self.editor = None;
                self.picker = Some(ModelPicker::new(&self.config));
                self.flash_status("select a model");
            }
            "/help" => {
                self.status =
                    "/model · /new · /clear · /resume · /quit · ↑/↓ input history · PgUp/PgDn chat"
                        .into();
            }
            "/resume" => match self.storage.list_sessions(&self.project) {
                Ok(sessions) => {
                    let current = self.session_id.unwrap_or(-1);
                    self.session_picker = Some(SessionPicker::new(sessions, current));
                }
                Err(error) => self.flash_status(format!("failed to list conversations: {error}")),
            },
            "/new" | "/clear" => {
                // 纯内存切换：session_id 置 None，首条内容写入时才
                // 落盘建会话（/new 十次不产生任何库行）。
                self.session_id = None;
                self.messages.clear();
                self.markdown_cache.clear();
                self.conversation_scroll_from_bottom = 0;
                self.assistant_message_index = None;
                self.input = InputBuffer::new(Vec::new());
                self.flash_status("new conversation");
            }
            "/quit" | "/exit" => self.should_quit = true,
            command if command.starts_with('/') => {
                self.flash_status(format!("unknown command: {command}"));
            }
            prompt => self.start_run(prompt.to_owned()),
        }
    }

    fn start_run(&mut self, prompt: String) {
        if !self.config.is_configured() {
            self.flash_status("model is not configured — run /model first");
            return;
        }
        let session_id = match self.current_session() {
            Ok(session_id) => session_id,
            Err(error) => {
                self.flash_status(error);
                return;
            }
        };

        // Build the model context before touching display state, so the new
        // prompt is appended exactly once. Persisted items are the source of
        // truth for context; legacy sessions that only have display messages
        // are seeded from them once.
        let mut history_items = match self.storage.load_items(session_id) {
            Ok(items) if !items.is_empty() => items,
            _ => {
                let seeded: Vec<ModelItem> =
                    self.messages.iter().map(ChatMessage::model_item).collect();
                for item in &seeded {
                    let _ = self.storage.append_item(session_id, item);
                }
                seeded
            }
        };
        let user_item = ModelItem::user_text(prompt.clone());
        history_items.push(user_item.clone());

        self.messages.push(ChatMessage::user(prompt.clone()));
        if let Err(error) = self.storage.append_message(session_id, "user", &prompt) {
            self.flash_status(format!("failed to persist user message: {error}"));
        }
        if let Err(error) = self.storage.append_item(session_id, &user_item) {
            self.flash_status(format!("failed to persist user context: {error}"));
        }
        self.conversation_scroll_from_bottom = 0;

        let project = self.project.clone();
        let config = self.config.clone();
        let provider_runtime = self.provider_runtime.clone();
        let tools = Arc::clone(&self.tools);
        let sender = self
            .event_sender
            .clone()
            .expect("event channel is installed by run()");
        let cancel = CancelToken::new();
        self.cancel_token = Some(cancel.clone());
        self.running = true;
        self.assistant_message_index = None;
        self.flash_status("starting model…");

        // worker 直接把消息发进统一通道（Terminal/Balance 同流），
        // 主循环消息驱动即时处理。
        let worker_events = sender.clone();
        thread::spawn(move || {
            let result = execute_run(
                crate::tui_worker::RunRequest {
                    project,
                    config,
                    provider_runtime,
                    tools,
                    history_items,
                    prompt,
                    cancel,
                },
                worker_events.clone(),
            );
            let _ = worker_events.send(UiEvent::Worker(WorkerMessage::Done(result)));
        });
    }

    /// 主循环收到一条 worker 消息：流事件、权限请求或运行结束。
    fn handle_worker_message(&mut self, message: WorkerMessage) {
        match message {
            WorkerMessage::Event(event) => self.handle_run_event(event),
            WorkerMessage::PermissionRequest {
                request,
                decision_tx,
            } => {
                self.thinking = false;
                self.thinking_since = None;
                self.pending_permission = Some(PendingPermission {
                    request,
                    decision_tx,
                    argument_scroll: 0,
                    argument_page_size: 1,
                    argument_line_count: 0,
                    reviewed_through: 0,
                    reviewed_to_end: false,
                });
                self.flash_status("permission required — review arguments, then allow or deny");
            }
            WorkerMessage::Done(result) => {
                self.finish_run(result);
            }
        }
    }

    fn handle_run_event(&mut self, event: RunEvent) {
        let was_thinking = self.thinking;
        self.thinking = false;
        match event {
            RunEvent::ModelRequested {
                turn,
                provider,
                model,
            } => {
                self.flash_status(format!("{provider}/{model} · turn {turn}"));
            }
            RunEvent::ModelStream {
                turn,
                event: ModelEvent::TextDelta { delta },
            }
            | RunEvent::ModelStream {
                turn,
                event: ModelEvent::RefusalDelta { delta },
            } => {
                self.append_assistant_delta(&delta);
                self.flash_status(format!("answering · turn {turn}"));
            }
            RunEvent::ModelStream {
                event: ModelEvent::ReasoningDelta { .. },
                ..
            } => {
                if !was_thinking {
                    self.thinking_since = Some(Instant::now());
                }
                self.thinking = true;
            }
            RunEvent::ToolRequested { call } => {
                self.flash_status(format!("tool → {} {}", call.name, call.arguments));
            }
            RunEvent::PermissionDenied { tool, reason } => {
                self.flash_status(format!("permission ✗ {tool} — {reason}"));
            }
            RunEvent::ToolFinished { result } => {
                if result.is_error {
                    self.flash_status(format!("tool ✗ {}", result.tool_name));
                } else {
                    self.flash_status(format!("tool ✓ {}", result.tool_name));
                }
            }
            _ => {}
        }
    }

    fn append_assistant_delta(&mut self, delta: &str) {
        let index = match self.assistant_message_index {
            Some(index) => index,
            None => {
                self.messages.push(ChatMessage::assistant(String::new()));
                let index = self.messages.len() - 1;
                self.assistant_message_index = Some(index);
                index
            }
        };
        self.messages[index].content.push_str(delta);
        self.conversation_scroll_from_bottom = 0;
    }

    fn finish_run(
        &mut self,
        result: Result<crate::tui_worker::RunDone, crate::tui_worker::RunFailure>,
    ) {
        self.running = false;
        self.cancel_token = None;
        self.thinking = false;
        self.thinking_since = None;
        // run 刚消耗了额度：触发监控线程立即重新查询一次（计划外，
        // 不影响 5 分钟巡查周期）。
        self.refresh_balance_now();
        match result {
            Ok(done) => {
                // 累计会话用量，供状态栏缓存命中百分比使用。
                self.session_usage.add_assign(&done.usage);
                if self.assistant_message_index.is_none() && !done.output.trim().is_empty() {
                    self.messages.push(ChatMessage::assistant(done.output));
                    self.assistant_message_index = Some(self.messages.len() - 1);
                }
                self.persist_current_assistant(false);
                self.persist_items(done.new_items);
                if done.cancelled {
                    self.flash_status(format!("cancelled · {} model turns", done.turns));
                } else {
                    self.flash_status(format!("completed · {} model turns", done.turns));
                }
            }
            Err(failure) => {
                self.session_usage.add_assign(&failure.usage);
                // RunError carries exact completed context plus any partial
                // assistant delta from the failing provider turn.
                self.persist_current_assistant(false);
                self.persist_items(failure.new_items);
                self.flash_status(format!(
                    "run failed after {} model turns: {}",
                    failure.turns, failure.error
                ));
            }
        }
        self.assistant_message_index = None;
        self.conversation_scroll_from_bottom = 0;
    }

    fn persist_items(&mut self, items: Vec<ModelItem>) {
        let session_id = match self.current_session() {
            Ok(session_id) => session_id,
            Err(error) => {
                self.flash_status(error);
                return;
            }
        };
        for item in items {
            if let Err(error) = self.storage.append_item(session_id, &item) {
                self.flash_status(format!("failed to persist conversation context: {error}"));
            }
        }
    }

    fn persist_current_assistant(&mut self, also_item: bool) {
        let Some(index) = self.assistant_message_index else {
            return;
        };
        let session_id = match self.current_session() {
            Ok(session_id) => session_id,
            Err(error) => {
                self.flash_status(error);
                return;
            }
        };
        let content = self.messages[index].content.clone();
        if content.trim().is_empty() {
            return;
        }
        if let Err(error) = self
            .storage
            .append_message(session_id, "assistant", &content)
        {
            self.flash_status(format!("failed to persist assistant message: {error}"));
        }
        if also_item {
            let item = ModelItem::assistant_text(content);
            if let Err(error) = self.storage.append_item(session_id, &item) {
                self.flash_status(format!("failed to persist assistant context: {error}"));
            }
        }
    }

    fn scroll_up(&mut self, amount: usize) {
        self.conversation_scroll_from_bottom = self
            .conversation_scroll_from_bottom
            .saturating_add(amount)
            .min(10_000);
    }

    fn scroll_down(&mut self, amount: usize) {
        self.conversation_scroll_from_bottom =
            self.conversation_scroll_from_bottom.saturating_sub(amount);
    }

    /// 输入框文本的可用宽度：内容区宽减去行首前缀（`❯ ` / 两个空格）。
    /// 换行、光标定位与鼠标选区映射统一使用该宽度。
    fn input_text_width(&self) -> usize {
        self.input_area
            .width
            .saturating_sub(2)
            .saturating_sub(INPUT_MARKER_WIDTH as u16)
            .max(1) as usize
    }

    fn draw(&mut self, frame: &mut Frame) {
        let area = frame.area();
        // 未确权：不渲染主界面（会话/输入框/状态栏全部不可见），
        // 清屏后只画确权对话框——没有可滚动的层、没有可输入的框、
        // 没有闪烁的光标，确权是唯一可能的交互。
        if self.trust_prompt {
            frame.render_widget(Clear, area);
            render_trust_dialog(frame, area, self.project.root());
            return;
        }
        self.spinner_tick += 1;
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
        let input_rows = (self.input.line_count(input_width) + 2).clamp(3, 10);
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
        // 状态栏：左边是 storage 等常规状态，最右边是 DeepSeek 前缀
        // （余额 · 缓存% · Thinking · 强度）。
        let deepseek = deepseek_status_prefix(&self.config, &self.balance, &self.session_usage);
        let status_line = if self.thinking {
            let elapsed = self.thinking_since.map(|since| since.elapsed());
            thinking_line(self.spinner_tick, elapsed)
        } else {
            Line::from(self.status.as_str())
        };
        if deepseek.is_empty() {
            frame.render_widget(Paragraph::new(status_line), chunks[3]);
        } else {
            // 右侧前缀按内容宽度分配，剩余空间全部留给左侧状态。
            let prefix_width = UnicodeWidthStr::width(deepseek.as_str()) as u16;
            let status_width = chunks[3].width.saturating_sub(prefix_width + 2);
            let columns = Layout::default()
                .direction(Direction::Horizontal)
                .constraints([Constraint::Length(status_width), Constraint::Min(0)])
                .split(chunks[3]);
            frame.render_widget(
                Paragraph::new(status_line).wrap(Wrap { trim: false }),
                columns[0],
            );
            frame.render_widget(Paragraph::new(deepseek).right_aligned(), columns[1]);
        }

        if let Some(picker) = &self.session_picker {
            let height = (picker.row_count() as u16 + 4).min(area.height.saturating_sub(2));
            let picker_area = centered_rect(84, height.max(6), area);
            self.editor_area = Some(picker_area);
            picker.draw(frame, picker_area);
        } else if let Some(picker) = &self.picker {
            let height = (picker.row_count() as u16 + 4).min(area.height.saturating_sub(2));
            let picker_area = centered_rect(94, height.max(8), area);
            self.editor_area = Some(picker_area);
            picker.draw(frame, picker_area);
        } else if let Some(editor) = &self.editor {
            let height = (editor.row_count() as u16 + 4).min(area.height.saturating_sub(2));
            let editor_area = centered_rect(94, height.max(8), area);
            self.editor_area = Some(editor_area);
            editor.draw(frame, editor_area);
        } else {
            self.editor_area = None;
            if !self.running && self.input_area.width > 2 && self.input_area.height > 2 {
                let (row, column) = self.input.cursor_position(self.input_text_width());
                let visible_rows = self.input_area.height.saturating_sub(2) as usize;
                let row = row.min(visible_rows.saturating_sub(1));
                // 光标跳过行首箭头前缀（`❯ ` / 两个空格）。
                frame.set_cursor_position((
                    self.input_area.x + 1 + INPUT_MARKER_WIDTH as u16 + column as u16,
                    self.input_area.y + 1 + row as u16,
                ));
            }
        }

        if self.pending_permission.is_some() {
            self.draw_permission_dialog(frame);
        }
    }

    fn draw_permission_dialog(&mut self, frame: &mut Frame) {
        let Some(pending) = self.pending_permission.as_mut() else {
            return;
        };
        let area = frame.area();
        let inner_width = 84usize.saturating_sub(2);
        let mut lines = vec![
            Line::from(Span::styled(
                "Permission required",
                Style::default().add_modifier(Modifier::BOLD),
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
        lines.push(Line::from("arguments:"));
        // 完整 pretty JSON 逐行入列（不再静默截断到 8 行）；对框高
        // 不足时在尾部追加"还有 N 行未显示"的醒目计数。
        let pretty = serde_json::to_string_pretty(&pending.request.arguments)
            .unwrap_or_else(|_| "<unavailable>".into());
        let mut argument_lines = Vec::new();
        for source_line in pretty.split('\n') {
            for wrapped in wrap_text(source_line, inner_width.saturating_sub(4)) {
                argument_lines.push(Line::from(format!("  {wrapped}")));
            }
        }
        // 对话框最高占屏（减边距）。参数可滚动，且只有最后一页
        // 确实进入视口后才开放批准键，避免隐藏字段未审阅即放行。
        let max_dialog_height = area.height.saturating_sub(2);
        let reserved = lines.len() + 5; // 状态 + 空行 + 快捷键 + 边框
        let available_for_arguments = (max_dialog_height as usize).saturating_sub(reserved);
        if available_for_arguments == 0 {
            pending.argument_page_size = 0;
            pending.argument_line_count = argument_lines.len();
            let compact = vec![
                Line::from(Span::styled(
                    "Permission required",
                    Style::default().add_modifier(Modifier::BOLD),
                )),
                Line::from("Terminal is too small to review arguments."),
                Line::from("Maximize to continue · Esc / n — deny"),
            ];
            let height = (compact.len() as u16 + 2).min(max_dialog_height);
            let dialog = centered_rect(84, height, area);
            frame.render_widget(Clear, dialog);
            frame.render_widget(
                Paragraph::new(compact)
                    .block(Block::default().borders(Borders::ALL).title(" Permission ")),
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
            Style::default().add_modifier(Modifier::BOLD),
        )));
        lines.push(Line::from(""));
        let actions = if pending.reviewed_to_end {
            "Enter / y — allow      ·      Esc / n — deny"
        } else {
            "Review through the final line to enable Allow · Esc / n — deny"
        };
        lines.push(Line::from(Span::styled(
            actions,
            Style::default().add_modifier(Modifier::BOLD),
        )));

        let height = (lines.len() as u16 + 2).min(max_dialog_height);
        let dialog = centered_rect(84, height.max(10), area);
        frame.render_widget(Clear, dialog);
        frame.render_widget(
            Paragraph::new(lines)
                .block(Block::default().borders(Borders::ALL).title(" Permission ")),
            dialog,
        );
    }

    fn draw_header(&self, frame: &mut Frame, area: Rect) {
        let model = if self.config.is_configured() {
            match self.config.preset.as_deref().and_then(preset_by_id) {
                // 预设模型的 name 与 model id 重复（仅大小写不同），只展示名称。
                Some(preset) => preset.name.to_owned(),
                None => format!("{} · {}", self.config.protocol, self.config.model),
            }
        } else {
            "not configured — /model".into()
        };
        let state = if self.running { "running" } else { "ready" };
        frame.render_widget(
            Paragraph::new(vec![
                Line::from(vec![
                    Span::styled("CLAT", Style::default().add_modifier(Modifier::BOLD)),
                    Span::raw(format!(
                        " v{}  {state}  ·  {model}",
                        env!("CARGO_PKG_VERSION")
                    )),
                ]),
                Line::from(format!("project: {}", self.project.root().display())),
            ])
            .block(Block::default().borders(Borders::ALL)),
            area,
        );
    }

    fn draw_conversation(&mut self, frame: &mut Frame, area: Rect) {
        let inner_width = area.width.saturating_sub(2).max(1) as usize;
        let lines = self.conversation_lines(inner_width);
        let total = lines.len();
        let visible = area.height.saturating_sub(2) as usize;
        let max_start = total.saturating_sub(visible);
        let start = max_start.saturating_sub(self.conversation_scroll_from_bottom.min(max_start));
        // 记录视口信息，供鼠标事件把屏幕坐标映射回内容行。
        self.conversation_start = start;
        self.conversation_rows = total;
        let mut visible_lines = lines
            .into_iter()
            .skip(start)
            .take(visible)
            .collect::<Vec<_>>();
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
        let block = Block::default().title("Conversation").borders(Borders::ALL);
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
                .style(Style::default().fg(Color::DarkGray))
                .thumb_style(Style::default().fg(Color::Cyan)),
            block.inner(area),
            &mut scrollbar_state,
        );
    }

    /// Builds all conversation lines, caching the per-message markdown
    /// rendering so a long history is not re-parsed on every frame. Only
    /// the streaming message (whose length changes) and resized panels
    /// miss the cache.
    fn conversation_lines(&mut self, width: usize) -> Vec<Line<'static>> {
        let mut lines = Vec::new();
        if self.messages.is_empty() {
            lines.push(Line::from("No messages yet."));
            lines.push(Line::from(""));
            return lines;
        }
        for index in 0..self.messages.len() {
            let key = (index, self.messages[index].content.len(), width);
            if let Some(cached) = self.markdown_cache.get(&key) {
                lines.extend(cached.clone());
            } else {
                let rendered = message_lines(&self.messages[index], width);
                self.markdown_cache.insert(key, rendered.clone());
                lines.extend(rendered);
            }
            // One uniform blank row after every message, including the
            // last, keeps the spacing regular and the panel bottom clear.
            lines.push(Line::from(""));
        }
        lines
    }

    fn draw_input(&self, frame: &mut Frame, area: Rect) {
        let title = if self.running { "Running" } else { "Message" };
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
        frame.render_widget(
            Paragraph::new(Text::from(lines))
                .block(Block::default().title(title).borders(Borders::ALL)),
            area,
        );
    }
}

/// Renders one message into display lines, Claude Code style: user
/// messages are a solid background block hugging the text exactly,
/// prefixed with a bright yellow `❯`; assistant messages are plain
/// markdown text prefixed with `⏺`. Spacing between messages is handled
/// uniformly by the caller, never by padding rows inside the block.
fn message_lines(message: &ChatMessage, width: usize) -> Vec<Line<'static>> {
    let text_width = width.saturating_sub(2).max(1);
    let mut lines = Vec::new();
    match message.role {
        ChatRole::User => {
            let style = user_message_style();
            let marker = style.fg(Color::Yellow).add_modifier(Modifier::BOLD);
            let wrapped = wrap_text(&message.content, text_width.saturating_sub(2).max(1));
            for (index, line) in wrapped.iter().enumerate() {
                let (prefix, prefix_style) = if index == 0 {
                    ("❯ ", marker)
                } else {
                    ("  ", style)
                };
                let used = UnicodeWidthStr::width(prefix) + UnicodeWidthStr::width(line.as_str());
                let padding = " ".repeat(width.saturating_sub(used));
                lines.push(Line::from(vec![
                    Span::styled(prefix.to_owned(), prefix_style),
                    Span::styled(line.clone(), style),
                    Span::styled(padding, style),
                ]));
            }
        }
        ChatRole::Assistant => {
            let marker = Span::styled("⏺ ", Style::default().fg(Color::Gray));
            for (index, line) in render_markdown(&message.content, text_width)
                .into_iter()
                .enumerate()
            {
                let mut spans = vec![if index == 0 {
                    marker.clone()
                } else {
                    Span::raw("  ")
                }];
                spans.extend(line.spans);
                lines.push(Line::from(spans));
            }
        }
    }
    lines
}

/// Maps the first visible row (`start`, in 0..=max_start) to ratatui's
/// scrollbar position domain 0..=content_length-1, where 0 puts the thumb
/// at the very top and content_length-1 at the very bottom. Passing the
/// raw row index leaves the thumb short of the bottom by one viewport.
fn scrollbar_position(start: usize, max_start: usize, content_length: usize) -> usize {
    if max_start == 0 {
        return 0;
    }
    let domain = content_length.saturating_sub(1);
    (start.saturating_mul(domain) / max_start).min(domain)
}

/// 权限对话框的参数字段摘要：对象时列出全部顶层键（截断到 10 个
/// 并标注剩余数），非对象（字符串/数字等）返回 None——摘要只在
/// 键存在时才有意义。危险目标（command/path/url 等）可能藏在长
/// JSON 深处，顶层键一览让批准前不可错过。
fn top_level_argument_keys(arguments: &serde_json::Value) -> Option<String> {
    let map = arguments.as_object()?;
    if map.is_empty() {
        return None;
    }
    let mut keys: Vec<&str> = map.keys().map(String::as_str).collect();
    keys.sort_unstable();
    let shown: Vec<&str> = keys.iter().take(10).copied().collect();
    let mut summary = shown.join(", ");
    if keys.len() > 10 {
        summary.push_str(&format!(" (+{} more)", keys.len() - 10));
    }
    Some(summary)
}

/// 扩展“从首行起连续看过”的区间。新视口与既有区间相接/重叠时
/// 才前进；跳过中间行（例如直接 End）不能伪造完整审阅。
fn advance_reviewed_through(reviewed_through: usize, start: usize, end: usize) -> usize {
    if start <= reviewed_through {
        reviewed_through.max(end)
    } else {
        reviewed_through
    }
}

fn wrap_text(text: &str, width: usize) -> Vec<String> {
    if text.is_empty() {
        return vec![String::new()];
    }
    let mut result = Vec::new();
    for source_line in text.split('\n') {
        if source_line.is_empty() {
            result.push(String::new());
            continue;
        }
        let mut current = String::new();
        let mut current_width = 0usize;
        for ch in source_line.chars() {
            let ch_width = UnicodeWidthChar::width(ch).unwrap_or(0);
            if current_width > 0 && current_width + ch_width > width {
                result.push(std::mem::take(&mut current));
                current_width = 0;
            }
            current.push(ch);
            current_width += ch_width;
        }
        result.push(current);
    }
    result
}

fn centered_rect(percent_x: u16, height: u16, area: Rect) -> Rect {
    let top = area.height.saturating_sub(height) / 2;
    let vertical = Rect::new(area.x, area.y + top, area.width, height.min(area.height));
    let horizontal = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - percent_x) / 2),
            Constraint::Percentage(percent_x),
            Constraint::Percentage((100 - percent_x) / 2),
        ])
        .split(vertical);
    horizontal[1]
}

/// 渲染项目确权对话框。独立于 App 以便用 TestBackend 验证边框完整。
fn render_trust_dialog(frame: &mut Frame, area: Rect, root: &Path) {
    // 文本按对话框 84% 宽的“最小合理终端”（约 80 列内宽）换行；
    // 更宽的终端留白更多，更窄的终端由 Paragraph 截断右侧。
    let inner_width = 84usize.saturating_sub(2);
    let mut lines = vec![
        Line::from(Span::styled(
            "Trust this project?",
            Style::default().add_modifier(Modifier::BOLD),
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
        Style::default().add_modifier(Modifier::BOLD),
    )));

    let height = (lines.len() as u16 + 2).min(area.height.saturating_sub(2));
    let dialog = centered_rect(84, height.max(10), area);
    frame.render_widget(Clear, dialog);
    frame.render_widget(
        Paragraph::new(lines).block(Block::default().borders(Borders::ALL).title(" Trust ")),
        dialog,
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    #[test]
    fn permission_review_requires_a_contiguous_path_to_the_last_line() {
        let reviewed = advance_reviewed_through(0, 0, 10);
        assert_eq!(reviewed, 10);
        // 直接跳到末页，中间 10..90 未进入视口，不能解锁。
        assert_eq!(advance_reviewed_through(reviewed, 90, 100), 10);
        // 逐页连续审阅会一直推进到末尾。
        let reviewed = advance_reviewed_through(reviewed, 10, 50);
        let reviewed = advance_reviewed_through(reviewed, 50, 100);
        assert_eq!(reviewed, 100);
    }

    /// 确权对话框四条边框必须完整渲染：定位左上角 `┌`，断言左列
    /// 全部为竖线、顶行/底行为横线，且左右均有留白（未被切出屏外）。
    /// 这是实机 bug（"左边竖线不见了"）的回归测试。
    #[test]
    fn trust_dialog_renders_all_four_borders() {
        for (width, height) in [(100u16, 30u16), (80, 24), (213, 55), (48, 20)] {
            let backend = TestBackend::new(width, height);
            let mut terminal = Terminal::new(backend).expect("terminal");
            terminal
                .draw(|frame| {
                    render_trust_dialog(
                        frame,
                        frame.area(),
                        Path::new("/Users/deng/Documents/GitHub/clat"),
                    );
                })
                .expect("draw");
            let buffer = terminal.backend().buffer();

            // 找左上角 ┌（圆角边框是 ratatui 默认 BorderType::Rounded）
            let mut corner = None;
            for y in 0..height {
                for x in 0..width {
                    if buffer[(x, y)].symbol() == "┌" {
                        corner = Some((x, y));
                        break;
                    }
                }
                if corner.is_some() {
                    break;
                }
            }
            let Some((left, top)) = corner else {
                panic!("top-left border corner not found at {width}x{height}");
            };

            // 左右边框：顶行之下、底行之上的每一行都是竖线。
            // 沿顶行走到 ┐ 定位右列，沿左列走到 └ 定位底行。
            let mut right = left + 1;
            while right < width && buffer[(right, top)].symbol() != "┐" {
                right += 1;
            }
            assert!(right < width, "right corner not found at {width}x{height}");
            let mut bottom = top + 1;
            while bottom < height && buffer[(left, bottom)].symbol() != "└" {
                bottom += 1;
            }
            assert!(
                bottom < height,
                "bottom corner not found at {width}x{height}"
            );
            for y in (top + 1)..bottom {
                assert_eq!(
                    buffer[(left, y)].symbol(),
                    "│",
                    "left border missing at x={left} y={y} ({width}x{height})"
                );
                assert_eq!(
                    buffer[(right, y)].symbol(),
                    "│",
                    "right border missing at x={right} y={y} ({width}x{height})"
                );
            }
            // 对话框完整在屏内：左侧与右侧都有留白。
            assert!(left > 0, "dialog touches the left edge at {width}x{height}");
            assert!(
                right + 1 < width,
                "dialog touches the right edge at {width}x{height}"
            );
        }
    }

    #[test]
    fn wraps_cjk_by_terminal_width() {
        assert_eq!(wrap_text("你好世界", 4), vec!["你好", "世界"]);
    }

    #[test]
    fn converts_stored_messages_to_chat() {
        let message = ChatMessage::from_stored(StoredMessage {
            role: "user".into(),
            content: "hello".into(),
        })
        .unwrap();
        assert_eq!(message.role, ChatRole::User);
        assert_eq!(message.content, "hello");
    }

    #[test]
    fn thinking_line_shimmers_a_soft_band_over_the_fixed_tone() {
        // Extract the (character, style) pairs of the text part; the first
        // two spans are the spinner frame and the separating space.
        fn text_spans(line: &Line<'static>) -> Vec<(char, Style)> {
            line.spans[2..]
                .iter()
                .map(|span| {
                    (
                        span.content.chars().next().expect("one char per span"),
                        span.style,
                    )
                })
                .collect()
        }

        let at_start = thinking_line(0, None);
        let text = text_spans(&at_start);
        // The spinner frame itself rotates and keeps the brand blue.
        assert_eq!(at_start.spans[0].content, SPINNER_FRAMES[0]);
        assert_eq!(at_start.spans[0].style.fg, Some(DEEPSEEK_500));

        // The band sits on the seam at the start: both ends glow while the
        // middle stays (approximately) in the fixed tone.
        let brightness = |style: &Style| match style.fg {
            Some(Color::Rgb(r, g, b)) => r as u32 + g as u32 + b as u32,
            _ => 0,
        };
        let close_to_base = |color: Color| match color {
            Color::Rgb(r, g, b) => {
                let (br, bg, bb) = (65u8, 118u8, 230u8);
                r.abs_diff(br) <= 2 && g.abs_diff(bg) <= 2 && b.abs_diff(bb) <= 2
            }
            _ => false,
        };
        assert!(brightness(&text[0].1) > brightness(&text[4].1));
        assert!(close_to_base(text[4].1.fg.unwrap_or_default()));

        // Mid-cycle the band has moved to the middle: the brightest
        // character is 'k', and its color approaches the light deepseek-200.
        let mid = thinking_line(11, None);
        let text = text_spans(&mid);
        let brightest = text
            .iter()
            .max_by_key(|(_, style)| brightness(style))
            .expect("text");
        assert_eq!(brightest.0, 'k');
        match brightest.1.fg {
            Some(Color::Rgb(r, g, b)) => {
                assert!(r > 190 && g > 210 && b > 240, "band color {r},{g},{b}");
            }
            other => panic!("expected RGB, got {other:?}"),
        }

        // Far from the band the text is (approximately) the base blue.
        assert!(close_to_base(text[0].1.fg.unwrap_or_default()));

        // The elapsed clock is appended when known.
        let with_clock = thinking_line(0, Some(Duration::from_secs(42)));
        let last = with_clock.spans.last().expect("clock span");
        assert!(last.content.contains("42s"));
    }

    #[test]
    fn scrollbar_position_reaches_both_ends() {
        // 100 content rows, 20 visible: start runs 0..=80.
        assert_eq!(scrollbar_position(0, 80, 100), 0);
        assert_eq!(scrollbar_position(80, 80, 100), 99);
        // Proportional in between.
        assert_eq!(scrollbar_position(40, 80, 100), 49);
        // Nothing to scroll: stays at the top.
        assert_eq!(scrollbar_position(0, 0, 5), 0);
    }

    #[test]
    fn abbreviates_home_prefix_and_keeps_other_paths() {
        let home = Path::new("/Users/deng");
        assert_eq!(
            abbreviate_with(Path::new("/Users/deng/Documents/GitHub/clat"), home),
            "~/Documents/GitHub/clat"
        );
        // 项目恰好就是 home 本身
        assert_eq!(abbreviate_with(Path::new("/Users/deng"), home), "~");
        // 非 home 路径原样保留
        assert_eq!(
            abbreviate_with(Path::new("/tmp/project"), home),
            "/tmp/project"
        );
        // 前缀相似但不同的目录不算 home
        assert_eq!(
            abbreviate_with(Path::new("/Users/dengger/app"), home),
            "/Users/dengger/app"
        );
    }

    #[test]
    fn base64_encode_matches_known_vectors() {
        assert_eq!(base64_encode(b""), "");
        assert_eq!(base64_encode(b"h"), "aA==");
        assert_eq!(base64_encode(b"hello"), "aGVsbG8=");
        // "你" 的 UTF-8 字节 E4 BD A0
        assert_eq!(base64_encode("你".as_bytes()), "5L2g");
    }

    #[test]
    fn slices_text_by_display_columns_including_wide_chars() {
        // 选中列 [1, 5)：你(0-2) 好(2-4) w(4) 入选，o(5) 不选
        assert_eq!(slice_by_columns("你好world", 1, 5), "你好w");
        assert_eq!(slice_by_columns("你好world", 0, usize::MAX), "你好world");
        assert_eq!(slice_by_columns("你好world", 6, 6), "");
    }

    #[test]
    fn highlight_line_splits_spans_at_the_selection_boundary() {
        let line = Line::from(vec![
            Span::raw("abc"),
            Span::styled("def", Style::default().fg(Color::Yellow)),
        ]);
        let highlighted = highlight_line(&line, 2, 4);
        // 选区跨 span 边界时按来源 span 切开：ab / c+d / ef
        let contents: Vec<&str> = highlighted
            .spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect();
        assert_eq!(contents, vec!["ab", "c", "d", "ef"]);
        assert!(
            !highlighted.spans[0]
                .style
                .add_modifier
                .contains(Modifier::REVERSED)
        );
        assert!(
            highlighted.spans[1]
                .style
                .add_modifier
                .contains(Modifier::REVERSED)
        );
        assert!(
            highlighted.spans[2]
                .style
                .add_modifier
                .contains(Modifier::REVERSED)
        );
        assert!(
            !highlighted.spans[3]
                .style
                .add_modifier
                .contains(Modifier::REVERSED)
        );
    }

    #[test]
    fn selection_order_and_emptiness_follow_content_coordinates() {
        let anchor = SelectionPos { row: 3, col: 5 };
        let head = SelectionPos { row: 1, col: 2 };
        let selection = TextSelection {
            kind: SelectionKind::Conversation,
            anchor,
            head,
            active: true,
        };
        // 反向拖拽时 ordered 归一化为 (小, 大)
        assert_eq!(selection.ordered(), (head, anchor));
        assert!(!selection.is_empty());
        assert!(
            TextSelection {
                kind: SelectionKind::Input,
                anchor,
                head: anchor,
                active: false,
            }
            .is_empty()
        );
    }

    #[test]
    fn mouse_coordinates_map_into_content_and_clamp_when_dragged_out() {
        let area = Rect::new(10, 20, 12, 6);
        // 内容区为 (11,21) 起 10x4
        assert_eq!(
            content_pos(area, 12, 22),
            Some(SelectionPos { row: 1, col: 1 })
        );
        // 边框和外部不算内容区
        assert_eq!(content_pos(area, 10, 22), None);
        assert_eq!(content_pos(area, 11, 20), None);
        // 拖出边界时钳制到内容区（内容 10x4，行数上限 8）
        assert_eq!(
            clamped_pos(area, 8, 12, 22),
            SelectionPos { row: 1, col: 1 }
        );
        assert_eq!(
            clamped_pos(area, 8, 100, 100),
            SelectionPos { row: 7, col: 10 }
        );
    }

    #[test]
    fn deepseek_prefix_combines_balance_thinking_and_effort() {
        let balance = Some("110.00".to_owned());
        // 无缓存数据：前缀不含百分比
        let no_cache = Usage::default();

        // 开启思考 + high：余额 · Thinking · High
        let mut config = ModelConfig {
            endpoint: "https://api.deepseek.com".into(),
            extra_body: serde_json::json!({
                "thinking": {"type": "enabled"},
                "reasoning_effort": "high",
            }),
            ..ModelConfig::default()
        };
        assert_eq!(
            deepseek_status_prefix(&config, &balance, &no_cache),
            "￥110.00 · Thinking · High"
        );

        // 有缓存命中：余额后插入百分比（无"缓存"字样）
        let cached = Usage {
            input_tokens: 1000,
            cached_input_tokens: Some(870),
            ..Usage::default()
        };
        assert_eq!(
            deepseek_status_prefix(&config, &balance, &cached),
            "￥110.00 · 87% · Thinking · High"
        );

        // max 强度显示为 Max
        config.extra_body = serde_json::json!({
            "thinking": {"type": "enabled"},
            "reasoning_effort": "max",
        });
        assert_eq!(
            deepseek_status_prefix(&config, &balance, &cached),
            "￥110.00 · 87% · Thinking · Max"
        );

        // 未显式声明 effort 时按官方默认 high 显示
        config.extra_body = serde_json::json!({"thinking": {"type": "enabled"}});
        assert_eq!(
            deepseek_status_prefix(&config, &balance, &no_cache),
            "￥110.00 · Thinking · High"
        );

        // 未显式声明 thinking 时按服务端默认开启处理
        config.extra_body = serde_json::json!({});
        assert_eq!(
            deepseek_status_prefix(&config, &balance, &no_cache),
            "￥110.00 · Thinking · High"
        );

        // 思考开关关闭：余额与百分比
        config.extra_body = serde_json::json!({
            "thinking": {"type": "disabled"},
            "reasoning_effort": "high",
        });
        assert_eq!(
            deepseek_status_prefix(&config, &balance, &cached),
            "￥110.00 · 87%"
        );

        // 非 DeepSeek/GLM 端点：无前缀
        config.endpoint = "https://api.openai.com/v1".into();
        assert_eq!(deepseek_status_prefix(&config, &balance, &cached), "");

        // DeepSeek 端点但余额未就绪（未配置 key 或查询失败）：无前缀
        config.endpoint = "https://api.deepseek.com".into();
        let empty = None;
        assert_eq!(deepseek_status_prefix(&config, &empty, &cached), "");

        // GLM Coding Plan 端点：槽位存 5 小时窗口剩余额度百分比，
        // 替代 DeepSeek 余额的位置，不再加货币符号。
        config.endpoint = "https://open.bigmodel.cn/api/coding/paas/v4".into();
        config.extra_body = serde_json::json!({"thinking": {"type": "enabled"}});
        let quota = Some("62%".to_owned());
        assert_eq!(
            deepseek_status_prefix(&config, &quota, &no_cache),
            "62% · Thinking · High"
        );
        assert_eq!(
            deepseek_status_prefix(&config, &quota, &cached),
            "62% · 87% · Thinking · High"
        );
        // 海外 z.ai 端点同样生效
        config.endpoint = "https://api.z.ai/api/coding/paas/v4".into();
        assert_eq!(
            deepseek_status_prefix(&config, &quota, &no_cache),
            "62% · Thinking · High"
        );
        // 槽位无值时不显示
        assert_eq!(deepseek_status_prefix(&config, &empty, &no_cache), "");

        // 缓存命中为零时不显示百分比（回到 DeepSeek 端点验证 ￥ 前缀）
        config.endpoint = "https://api.deepseek.com".into();
        config.extra_body = serde_json::json!({"thinking": {"type": "enabled"}});
        let zero_cache = Usage {
            input_tokens: 1000,
            cached_input_tokens: Some(0),
            ..Usage::default()
        };
        assert_eq!(
            deepseek_status_prefix(&config, &balance, &zero_cache),
            "￥110.00 · Thinking · High"
        );
    }

    #[test]
    fn flash_status_expires_after_ttl_but_persistent_status_stays() {
        let now = Instant::now();
        // 常驻状态（无过期时刻）永不到期。
        assert!(!status_expired(None, now));
        // 未到 TTL 保持显示。
        assert!(!status_expired(
            Some(now + STATUS_TTL - Duration::from_millis(100)),
            now
        ));
        // 恰好到达 TTL 或已过期：过期，回落常驻状态。
        assert!(status_expired(Some(now), now));
        assert!(status_expired(Some(now - Duration::from_secs(1)), now));
    }

    #[test]
    fn user_messages_render_as_a_full_width_background_block() {
        let message = ChatMessage::user("hello");
        let lines = message_lines(&message, 12);

        // One text row only: the block hugs the text exactly, with a
        // bright yellow "❯ " marker, padded to the full width.
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].spans[0].content, "❯ ");
        assert_eq!(lines[0].spans[0].style.bg, Some(USER_BG));
        assert_eq!(lines[0].spans[0].style.fg, Some(Color::Yellow));
        assert_eq!(lines[0].spans[1].content, "hello");
        assert_eq!(lines[0].spans[1].style.bg, Some(USER_BG));
        let total_width: usize = lines[0]
            .spans
            .iter()
            .map(|span| UnicodeWidthStr::width(span.content.as_ref()))
            .sum();
        assert_eq!(total_width, 12);
    }

    #[test]
    fn assistant_messages_render_markdown_without_background() {
        let message = ChatMessage::assistant("**bold** and `code`");
        let lines = message_lines(&message, 30);
        // The "⏺" marker prefixes the first line, no background.
        assert_eq!(lines[0].spans[0].content, "⏺ ");
        assert!(lines[0].spans[0].style.bg.is_none());
        // Bold and inline-code segments survive.
        assert!(
            lines[0]
                .spans
                .iter()
                .any(|span| span.content == "bold"
                    && span.style.add_modifier.contains(Modifier::BOLD))
        );
        assert!(
            lines[0]
                .spans
                .iter()
                .any(|span| span.content == "code" && span.style.bg.is_some())
        );
    }
}
