use crate::presets::preset_by_id;
use crate::tui_input::InputBuffer;
use crate::tui_markdown::render_markdown;
use crate::tui_model::{EditorAction, ModelEditor, ModelPicker, PickerAction};
use crate::tui_sessions::{ResumeAction, SessionPicker};
use crate::tui_worker::{ChannelApprover, ChannelEventSink, UiEvent, WorkerMessage};
use crate::{
    ApplicationEvent, ApplicationRunRequest, BootstrapApplication, CompactHandle, CompactionStatus,
    ModelConfig, ModelEvent, ModelVendor, PermissionDecision, PermissionRequest, Project,
    ProjectAuthorization, ProviderCredentials, ProviderDescriptor, RunEvent, RunHandle,
    ThinkingLevel, TrustedProjectApplication, Usage, apply_thinking_level,
    effective_thinking_level, next_thinking_level, thinking_levels,
};
use crate::{SessionId, TranscriptLine};
use crossterm::event::{
    self, DisableBracketedPaste, DisableMouseCapture, Event, KeyCode, KeyEvent, KeyEventKind,
    KeyModifiers, KeyboardEnhancementFlags, MouseButton, MouseEvent, MouseEventKind,
    PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
};
use crossterm::execute;
use ratatui::layout::{Constraint, Direction, Layout, Margin, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{
    Block, Borders, Clear, Padding, Paragraph, Scrollbar, ScrollbarOrientation, ScrollbarState,
    Wrap,
};
use ratatui::{DefaultTerminal, Frame};
use std::collections::HashMap;
use std::env;
use std::io::{self, Write, stdout};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, Sender};
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

/// 会话累计的缓存命中百分比文本（如 "99.99%"，两位小数）。无输入
/// token 或服务端未上报缓存命中时不显示（返回 None）。
fn cache_hit_percent(usage: &Usage) -> Option<String> {
    let cached = usage.cached_input_tokens?;
    if usage.input_tokens == 0 || cached == 0 {
        return None;
    }
    let percent = cached as f64 / usage.input_tokens as f64 * 100.0;
    Some(format!("{percent:.2}%"))
}

/// token 数的紧凑展示：`1M` / `1.5M` / `120k` / `999`。千位以上四舍
/// 五入到 k，百万以上保留一位小数（整数则省略小数部分）。
fn format_tokens(tokens: u64) -> String {
    if tokens >= 1_000_000 {
        // 一位小数，整数值省略小数部分（1.0M → 1M）。
        let millions = format!("{:.1}", tokens as f64 / 1_000_000.0);
        format!("{}M", millions.trim_end_matches(".0"))
    } else if tokens >= 1_000 {
        format!("{}k", (tokens + 500) / 1_000)
    } else {
        tokens.to_string()
    }
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

/// 状态栏右侧遥测段，按优先级降序（额度 > Cache > Context）。各段
/// 无值即不产生；仅 DeepSeek/GLM 端点有遥测。渲染时
/// `fit_status_suffix` 在窄终端从尾部（最低优先）开始让位。
///
/// - DeepSeek：`Wallet: ￥89.35 · Cache: 99.99% · Context: 120k/1M`
/// - GLM Coding Plan：`Token: 87% · Cache: 99.99% · Context: 120k/1M`
///   （Token 段是 5 小时窗口剩余额度）
///
/// 思考档位不在这里——它属于标题栏（`compose_header_rest`）。
fn status_suffix_segments(
    config: &ModelConfig,
    balance: &Option<String>,
    session_usage: &Usage,
    last_turn_usage: Option<&Usage>,
) -> Vec<String> {
    let mut parts = Vec::new();
    if config.vendor() == ModelVendor::Other {
        return parts;
    }
    // DeepSeek 槽位存余额文本，加 Wallet 标签与货币符号；GLM 槽位存
    // 5 小时窗口剩余额度百分比（如 "87%"），加 Token 标签。
    if let Some(balance) = balance {
        if config.vendor() == ModelVendor::DeepSeek {
            parts.push(format!("Wallet: ￥{balance}"));
        } else {
            parts.push(format!("Token: {balance}"));
        }
    }
    if let Some(percent) = cache_hit_percent(session_usage) {
        parts.push(format!("Cache: {percent}"));
    }
    // Context 当前值 ≈ 最近一次模型请求的 input+output（下一次请求
    // 的近似起点）；分母是预设的官方上下文窗口，自定义端点未知则
    // 省略整段。
    let window = config
        .preset
        .as_deref()
        .and_then(preset_by_id)
        .map(|preset| preset.context_window);
    if let (Some(usage), Some(window)) = (last_turn_usage, window) {
        let current = usage.input_tokens + usage.output_tokens;
        parts.push(format!(
            "Context: {}/{}",
            format_tokens(current),
            format_tokens(window as u64)
        ));
    }
    parts
}

/// 左侧常规状态（错误/取消/权限提示）的最小保留宽度（TUI-L02）：
/// 右侧遥测宁可整段省略也不挤掉左侧。
const MIN_STATUS_LEFT: u16 = 20;

/// 在 `budget` 显示宽度内按优先级保留遥测段：装不下低优先段时，它
/// 及其后继全部省略；首段都装不下则整体让位（左侧状态优先）。
fn fit_status_suffix(segments: &[String], budget: usize) -> String {
    let mut kept: Vec<&str> = Vec::new();
    for segment in segments {
        let width = kept
            .iter()
            .map(|text| UnicodeWidthStr::width(*text))
            .chain(std::iter::once(UnicodeWidthStr::width(segment.as_str())))
            .sum::<usize>()
            + 3 * kept.len();
        if width > budget {
            break;
        }
        kept.push(segment.as_str());
    }
    kept.join(" · ")
}

/// 标题栏首行在 "CLAT" 之后的内容，按可用显示宽度逐级退化（TUI-L02），
/// 保证档位在窄终端仍可见：
///
/// 1. 完整：` v0.5.1  ready  ·  {model} · Thinking · {level}`
/// 2. 紧凑：` v0.5.1 ready · {model} · {level}`（压缩间距、省略
///    "Thinking · " 文案）
/// 3. 最小：` v0.5.1 ready · Thinking · {level}`（省略模型名）
///
/// 模型+思考+强度是一个整体：组内分隔符统一窄间距 ` · `，与主分段
/// 的宽间距 `  ·  ` 区分。无档位（未配置 / 非 DeepSeek/GLM / 手工
/// 关闭）时各层级不含档位片段；三级都放不下交由终端截断。
fn compose_header_rest(
    version: &str,
    state: &str,
    model: &str,
    level: Option<&str>,
    width: usize,
) -> String {
    let full_suffix = level
        .map(|level| format!(" · Thinking · {level}"))
        .unwrap_or_default();
    let full = format!(" v{version}  {state}  ·  {model}{full_suffix}");
    if UnicodeWidthStr::width(full.as_str()) <= width {
        return full;
    }
    let compact = match level {
        Some(level) => format!(" v{version} {state} · {model} · {level}"),
        None => format!(" v{version} {state} · {model}"),
    };
    if UnicodeWidthStr::width(compact.as_str()) <= width {
        return compact;
    }
    match level {
        Some(level) => format!(" v{version} {state} · Thinking · {level}"),
        None => format!(" v{version} {state}"),
    }
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

    fn from_transcript(line: TranscriptLine) -> Option<Self> {
        let role = match line.kind.as_str() {
            "user" | "compaction" => ChatRole::User,
            "assistant" => ChatRole::Assistant,
            _ => return None,
        };
        Some(Self {
            role,
            content: line.text,
        })
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

    let (result, close_error) = match App::new(project) {
        Err(error) => (Err(io::Error::other(error)), None),
        Ok(mut app) => {
            let run_result = app.run(&mut terminal);
            (run_result, app.take_close_error())
        }
    };

    let _ = execute!(stdout(), PopKeyboardEnhancementFlags);
    let paste_result = execute!(stdout(), DisableBracketedPaste);
    let mouse_result = execute!(stdout(), DisableMouseCapture);
    ratatui::restore();
    // 显式 shutdown 的失败在终端恢复后可见地报告（plan §16 阶段5）。
    if let Some(error) = close_error {
        let _ = writeln!(io::stderr(), "clat: application close failed: {error}");
    }
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
    bootstrap: Option<BootstrapApplication>,
    application: Option<TrustedProjectApplication>,
    /// 当前会话 id；`None` 表示项目尚未受信（延迟初始化前不可对话）。
    /// 确权门保证所有用到它的路径只在 Some 时可达。
    session_id: Option<SessionId>,
    /// shutdown 时 Application close() 的错误（终端恢复后展示）。
    close_error: Option<String>,
    config: ModelConfig,
    credentials: ProviderCredentials,
    provider_descriptors: Vec<ProviderDescriptor>,
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
    run_handle: Option<RunHandle>,
    /// `/compact` 进行中的句柄；Esc 取消。
    compact_handle: Option<CompactHandle>,
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
    /// 余额/额度当前值：核心 Monitor 插件经 ApplicationEvent 写回，状态栏读取。
    balance: Option<String>,
    /// 本会话累计 token 用量，用于状态栏缓存命中百分比。
    session_usage: Usage,
    /// 最近一次模型请求的用量（INV-F：随会话切换/新建重置），用于
    /// 状态栏 `Context: 120k/1M` 的当前值近似。
    last_turn_usage: Option<Usage>,
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
        let bootstrap = BootstrapApplication::open_default(project.clone())
            .map_err(|error| error.to_string())?;
        let trusted = bootstrap.is_trusted().map_err(|error| error.to_string())?;
        let config = ModelConfig::default();
        let credentials = ProviderCredentials::for_protocol(config.protocol);

        // 状态栏初始显示当前打开的项目目录（home 缩写为 ~）。
        let status = abbreviate_home(project.root());
        let mut app = Self {
            project,
            bootstrap: Some(bootstrap),
            application: None,
            session_id: None,
            close_error: None,
            config,
            credentials,
            provider_descriptors: Vec::new(),
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
            run_handle: None,
            compact_handle: None,
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
            session_usage: Usage::default(),
            last_turn_usage: None,
        };
        if trusted {
            app.initialize_project()?;
        }
        Ok(app)
    }

    /// 项目级资源初始化：挂载 Trusted Project（已信任路径）并采纳
    /// 快照。任何失败向上报告——确权流程据此保持阻断。
    fn initialize_project(&mut self) -> Result<(), String> {
        let bootstrap = self
            .bootstrap
            .take()
            .ok_or_else(|| "bootstrap scope is unavailable".to_owned())?;
        let application = match bootstrap.into_trusted() {
            Ok(application) => application,
            Err(error) => {
                self.bootstrap = Some(
                    BootstrapApplication::open_default(self.project.clone())
                        .map_err(|open_error| open_error.to_string())?,
                );
                return Err(error.to_string());
            }
        };
        self.application = Some(application);
        self.adopt_snapshot()
    }

    /// 从已挂载的 application 读取项目快照并重置前端状态。
    fn adopt_snapshot(&mut self) -> Result<(), String> {
        let snapshot = match self.application.as_ref().map(|app| app.snapshot()) {
            Some(Ok(snapshot)) => snapshot,
            Some(Err(error)) => return Err(error.to_string()),
            None => return Err("project application is unavailable".into()),
        };
        self.session_id = snapshot.session_id;
        self.messages = snapshot
            .transcript
            .into_iter()
            .filter_map(ChatMessage::from_transcript)
            .collect();
        self.input = InputBuffer::new(snapshot.input_history);
        self.markdown_cache.clear();
        self.config = snapshot.config;
        self.credentials = snapshot.credentials;
        self.provider_descriptors = snapshot.provider_descriptors;
        if snapshot.mcp.configured != 0 {
            if snapshot.mcp.failures.is_empty() {
                self.flash_status(format!(
                    "mcp: {} server(s) connected",
                    snapshot.mcp.connected
                ));
            } else {
                self.flash_status(format!("mcp: {}", snapshot.mcp.failures.join("; ")));
            }
        }
        // 挂载期诊断（如 workspace 指针指向不可加载的会话）：订阅尚未
        // 建立，只能经访问器在此时取走。
        if let Some(application) = &self.application
            && let Some(diagnostic) = application.startup_diagnostic()
        {
            self.flash_status(diagnostic.to_owned());
        }
        self.refresh_balance_now();
        Ok(())
    }

    fn run(&mut self, terminal: &mut DefaultTerminal) -> io::Result<()> {
        // 统一事件通道：主循环从此只与消息打交道。
        //
        // - 输入线程阻塞读终端事件（event::read），按键到达即刻转发，
        //   零轮询、零节流——不再有 60ms/16ms 的自适应间隔；
        // - 核心 Monitor 插件的 ApplicationEvent 由前端桥接线程转发；
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

        if let Some(application) = &self.application {
            let (application_sender, application_events) = mpsc::channel();
            application.subscribe(application_sender);
            let ui = event_sender.clone();
            thread::spawn(move || {
                while let Ok(event) = application_events.recv() {
                    if ui.send(UiEvent::Application(event)).is_err() {
                        break;
                    }
                }
            });
            application.refresh_monitor();
        }

        let events = self
            .events
            .take()
            .expect("events channel was just installed");
        // 首帧无条件先行：循环内的绘制发生在每次 wait 之后，而无重绘
        // 需求时 wait 会无限挂起——未受信目录没有任何启动事件（无
        // monitor 订阅、无 worker），首绘绝不能以"事件先到达"为前提，
        // 否则用户看到的是空备用屏 + 左上角光标（实机 pty 复现）。
        self.expire_status();
        terminal.draw(|frame| self.draw(frame))?;
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
        // 显式 shutdown：flush 会话与 checkpoint、join 全部 worker，
        // 消费 close 错误（Drop 只兜底，不算成功关闭）。
        if let Some(application) = self.application.take()
            && let Err(error) = application.close()
        {
            self.close_error = Some(error.to_string());
        }
        Ok(())
    }

    fn take_close_error(&mut self) -> Option<String> {
        self.close_error.take()
    }

    fn handle_ui_event(&mut self, event: UiEvent) {
        match event {
            UiEvent::Terminal(event) => self.handle_terminal_event(event),
            UiEvent::Worker(message) => self.handle_worker_message(message),
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
                // authorize_and_mount 消费 bootstrap（lease → preflight →
                // 控制面提交 → 挂载）；失败时重开 bootstrap 保持阻断。
                let bootstrap = self
                    .bootstrap
                    .take()
                    .ok_or_else(|| "bootstrap scope is unavailable".to_owned());
                let trusted = bootstrap.and_then(|bootstrap| {
                    bootstrap
                        .authorize_and_mount(ProjectAuthorization::grant())
                        .map_err(|error| error.to_string())
                });
                match trusted {
                    Ok(application) => {
                        self.application = Some(application);
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
            // Shift+Tab 循环思考档位（Low→High→Max→Low）。不 gate
            // running：配置每次 run 重读，对下一次 run 生效；当前 run
            // 不受影响。
            KeyCode::BackTab => self.cycle_thinking_level(),
            KeyCode::Esc => {
                if self.running {
                    if let Some(handle) = &self.run_handle {
                        handle.cancel();
                        self.flash_status("cancelling…");
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

    /// 松开鼠标：空选区时单击定位输入光标；非空选区保持高亮，等用户
    /// 显式复制（Cmd+C / Ctrl+Shift+C）。选中即复制会静默覆盖系统
    /// 剪贴板，且 OSC 52 在不支持的终端上假报成功，已移除。
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

    /// 请求核心 Monitor 插件立即重新查询一次。用于配置变更与模型
    /// 运行结束（额度刚被消耗）。
    fn refresh_balance_now(&mut self) {
        if let Some(application) = &self.application {
            application.refresh_monitor();
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

    /// 切换到指定会话（/resume 确认时）：workspace 选择 CAS → 冷恢
    /// 复目标会话（原始事件永不删除，随时可再次 resume）→ 重置视图。
    fn switch_session(&mut self, session_id: SessionId) -> Result<(), String> {
        let snapshot = self
            .application
            .as_mut()
            .ok_or_else(|| "project application is unavailable".to_owned())?
            .switch_session(session_id.clone())
            .map_err(|error| error.to_string())?;
        self.messages = snapshot
            .transcript
            .into_iter()
            .filter_map(ChatMessage::from_transcript)
            .collect();
        self.session_id = Some(session_id);
        // 输入历史随会话切换：恢复目标会话自己的历史（含内存中
        // 未持久化的导航状态一并重置）。
        self.input = InputBuffer::new(snapshot.input_history);
        self.markdown_cache.clear();
        self.conversation_scroll_from_bottom = 0;
        self.assistant_message_index = None;
        // 用量指标归属会话（TUI-L04）：缓存命中率与上下文水位都随切换
        // 清零，目标会话首次 run 后重新累计/上报。
        self.session_usage = Usage::default();
        self.last_turn_usage = None;
        Ok(())
    }

    /// Shift+Tab：循环思考档位并随模型配置持久化（INV-D）。生效于
    /// 下一次 run（`start_run` 每次重读 `model_state`）；标题栏即时
    /// 同步（`self.config` 原地更新，重绘即见）。保存失败整体回滚，
    /// 内存与库不出现半套配置。
    fn cycle_thinking_level(&mut self) {
        let vendor = self.config.vendor();
        if thinking_levels(vendor).is_empty() {
            self.flash_status("thinking levels apply to DeepSeek and GLM models");
            return;
        }
        // 当前生效档位：一等字段优先，其次解析 extra_body；手工编辑成
        // disabled 视为关闭，从 High 起步一键恢复。
        let current = effective_thinking_level(&self.config).unwrap_or(ThinkingLevel::High);
        let Some(next) = next_thinking_level(vendor, current) else {
            return;
        };
        let previous = self.config.clone();
        self.config.thinking_level = Some(next);
        apply_thinking_level(&mut self.config.extra_body, next);
        let saved = match self.application.as_ref() {
            Some(application) => application
                .save_model_state(&self.config, &self.credentials)
                .map_err(|error| error.to_string()),
            // 未确权阶段没有项目应用：只改内存配置，确权后落盘。
            None => Ok(()),
        };
        match saved {
            Ok(()) => self.flash_status(format!("Thinking · {}", next.label())),
            Err(error) => {
                self.config = previous;
                self.flash_status(format!("failed to save thinking level: {error}"));
            }
        }
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
                self.editor = Some(ModelEditor::new_with_descriptors(
                    &self.config,
                    self.credentials.clone(),
                    self.provider_descriptors.clone(),
                ));
                self.flash_status("editing model configuration");
            }
            PickerAction::SelectPreset(preset) => {
                let mut config = self.config.clone();
                preset.apply(&mut config);
                // 换模型不携带旧档位：归位 None，新模型跟随预设默认
                // （与编辑器 cycle_preset 同一不变量）。
                config.thinking_level = None;
                let same_endpoint = self.config.endpoint.trim_end_matches('/')
                    == preset.endpoint.trim_end_matches('/');
                let key_present = self
                    .credentials
                    .value(0)
                    .is_some_and(|value| !value.trim().is_empty());
                if same_endpoint && key_present {
                    match self
                        .application
                        .as_ref()
                        .ok_or_else(|| "project application is unavailable".to_owned())
                        .and_then(|application| {
                            application
                                .save_model_state(&config, &self.credentials)
                                .map_err(|error| error.to_string())
                        }) {
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
                    let mut editor = ModelEditor::new_with_descriptors(
                        &config,
                        self.credentials.clone(),
                        self.provider_descriptors.clone(),
                    );
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
                let (config, credentials) = *saved;
                match self
                    .application
                    .as_ref()
                    .ok_or_else(|| "project application is unavailable".to_owned())
                    .and_then(|application| {
                        application
                            .save_model_state(&config, &credentials)
                            .map_err(|error| error.to_string())
                    }) {
                    Ok(()) => {
                        self.config = config;
                        self.credentials = credentials;
                        if let Some(application) = &self.application {
                            self.provider_descriptors =
                                application.provider_descriptors(&self.credentials);
                        }
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
        // 输入历史是进程内的（↑/↓ 召回）；跨重启的回忆来自会话的
        // transcript 投影（recent_inputs），命令输入永不落盘。
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
                self.status = "/model · /new · /clear · /compact · /resume · /quit · ↑/↓ input history · PgUp/PgDn chat · Shift+Tab thinking level · Cmd+C copy selection"
                    .into();
            }
            "/compact" => {
                // 异步：立即返回 handle，状态经 CompactionUpdated 事件回流
                // （启动时 "compacting…"，完成/失败时结果文本）；Esc 取消。
                match self.application.as_mut() {
                    Some(application) => match application.compact_session() {
                        Ok(handle) => {
                            self.compact_handle = Some(handle);
                        }
                        Err(error) => {
                            self.flash_status(format!("compaction unavailable: {error}"));
                        }
                    },
                    None => self.flash_status("project application is unavailable"),
                }
            }
            "/resume" => match self
                .application
                .as_ref()
                .ok_or_else(|| "project application is unavailable".to_owned())
                .and_then(|application| {
                    application
                        .list_sessions()
                        .map_err(|error| error.to_string())
                }) {
                Ok(sessions) => {
                    let current = self.session_id.clone();
                    self.session_picker = Some(SessionPicker::new(sessions, current));
                }
                Err(error) => self.flash_status(format!("failed to list conversations: {error}")),
            },
            "/new" | "/clear" => {
                // 纯内存切换：session_id 置 None，首条内容写入时才
                // 落盘建会话（/new 十次不产生任何库行）。活动 Run/
                // 压缩期间拒绝（INV-T3）。
                let switched = match &mut self.application {
                    Some(application) => match application.new_session() {
                        Ok(()) => true,
                        Err(error) => {
                            self.flash_status(format!("{error}"));
                            false
                        }
                    },
                    None => true,
                };
                if !switched {
                    return;
                }
                self.session_id = None;
                self.messages.clear();
                self.markdown_cache.clear();
                self.conversation_scroll_from_bottom = 0;
                self.assistant_message_index = None;
                self.input = InputBuffer::new(Vec::new());
                // 用量指标归属会话（TUI-L04）：新会话从零累计。
                self.session_usage = Usage::default();
                self.last_turn_usage = None;
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
        let sender = self
            .event_sender
            .clone()
            .expect("event channel is installed by run()");
        let (completion, completed) = mpsc::channel();
        let request = ApplicationRunRequest {
            prompt: prompt.clone(),
            approver: Arc::new(ChannelApprover::new(sender.clone())),
            events: Box::new(ChannelEventSink(sender.clone())),
            completion,
        };
        let handle = match self
            .application
            .as_mut()
            .ok_or_else(|| "project application is unavailable".to_owned())
            .and_then(|application| {
                application
                    .start_run(request)
                    .map_err(|error| error.to_string())
            }) {
            Ok(handle) => handle,
            Err(error) => {
                self.flash_status(format!("failed to start run: {error}"));
                return;
            }
        };
        self.messages.push(ChatMessage::user(prompt));
        self.conversation_scroll_from_bottom = 0;
        self.run_handle = Some(handle);
        self.running = true;
        self.assistant_message_index = None;
        self.flash_status("starting model…");

        // Completion is already post-persistence and post-scope-cleanup; this
        // tiny frontend bridge only multiplexes it into the terminal channel.
        thread::spawn(move || {
            if let Ok(result) = completed.recv() {
                let _ = sender.send(UiEvent::Worker(WorkerMessage::Done(result)));
            }
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
            // 流式 usage（DeepSeek 经 stream_options.include_usage，GLM
            // 默认携带）只取最近一次：input+output 近似当前上下文水位，
            // 供状态栏 Context 段使用。多轮 run 每轮覆盖前一轮。
            RunEvent::ModelStream {
                event: ModelEvent::Usage(usage),
                ..
            } => {
                self.last_turn_usage = Some(usage);
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

    fn finish_run(&mut self, result: crate::ApplicationRunResult) {
        self.running = false;
        if let Some(handle) = self.run_handle.take() {
            let _ = handle.join();
        }
        // 首 run 可能刚物化会话（Fresh→Session）：同步本地镜像，/resume
        // 的 current 标记与后续写路径立即正确。
        if let Some(application) = &self.application {
            self.session_id = application.current_session_id();
        }
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
                if done.cancelled {
                    self.flash_status(format!("cancelled · {} model turns", done.turns));
                } else {
                    self.flash_status(format!("completed · {} model turns", done.turns));
                }
            }
            Err(failure) => {
                self.session_usage.add_assign(&failure.usage);
                self.flash_status(format!(
                    "run failed after {} model turns: {}",
                    failure.turns, failure.error
                ));
            }
        }
        self.assistant_message_index = None;
        self.conversation_scroll_from_bottom = 0;
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
        // 状态栏：左边是 storage 等常规状态，最右边是模型遥测
        // （Wallet/Token · Cache% · Context current/total）。窄终端时
        // 左侧保底 MIN_STATUS_LEFT，右侧按优先级让位（TUI-L02）。
        // 左右各留 1 列边距，文字不贴终端边缘。
        let bar = chunks[3].inner(Margin::new(1, 0));
        let segments = status_suffix_segments(
            &self.config,
            &self.balance,
            &self.session_usage,
            self.last_turn_usage.as_ref(),
        );
        let budget = (bar.width.saturating_sub(MIN_STATUS_LEFT + 2)) as usize;
        let suffix = fit_status_suffix(&segments, budget);
        let status_line = if self.thinking {
            let elapsed = self.thinking_since.map(|since| since.elapsed());
            thinking_line(self.spinner_tick, elapsed)
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
        // `centered_rect(84, ..)` 的 84 是百分比，不是 84 列。预览
        // 必须使用本次真实矩形宽度换行，否则在 80 列终端里会按
        // 78 列排版、再被约 65 列的框裁掉危险命令尾部。
        let argument_width = permission_argument_width(area);
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
        // 写/执行类工具的专用预览：JSON 转义长串对内容审阅不友好
        // （write_file 的 content 会变成一行带 \n 的转义串）。改为
        // 人类可读形式：edit_file 显示 old→new 的迷你 diff，write_file
        // 显示目标与内容，run_command 突出命令与执行环境。其余工具
        // 回退完整 pretty JSON。两种形态共享同一滚动/强制审阅机制
        // ——预览行就是被审阅的参数。
        let argument_lines = match write_tool_preview(
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
        let max_dialog_height = area.height.saturating_sub(2);
        let reserved = lines.len() + 5; // 状态 + 空行 + 快捷键 + 边框
        let available_for_arguments = (max_dialog_height as usize).saturating_sub(reserved);
        if available_for_arguments == 0 || argument_width < 8 {
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
                Paragraph::new(compact).block(popup_block(" Permission ")),
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
            Paragraph::new(lines).block(popup_block(" Permission ")),
            dialog,
        );
    }

    fn draw_header(&self, frame: &mut Frame, area: Rect) {
        let (model, level) = if self.config.is_configured() {
            let name = match self.config.preset.as_deref().and_then(preset_by_id) {
                // 预设模型的 name 与 model id 重复（仅大小写不同），只展示名称。
                Some(preset) => preset.name.to_owned(),
                None => format!("{} · {}", self.config.protocol, self.config.model),
            };
            (
                name,
                effective_thinking_level(&self.config).map(|level| level.label()),
            )
        } else {
            ("not configured — /model".into(), None)
        };
        let state = if self.running { "running" } else { "ready" };
        // 首行内容预算：总宽减边框 2 列、水平内边距 2 列与 "CLAT " 前缀
        // 5 列；宽度不足时逐级退化（TUI-L02），档位优先于模型名保留。
        let rest_budget = area.width.saturating_sub(2 + 2 + 5) as usize;
        let rest =
            compose_header_rest(env!("CARGO_PKG_VERSION"), state, &model, level, rest_budget);
        frame.render_widget(
            Paragraph::new(vec![
                Line::from(vec![
                    Span::styled("CLAT", Style::default().add_modifier(Modifier::BOLD)),
                    Span::raw(rest),
                ]),
                Line::from(format!("project: {}", self.project.root().display())),
            ])
            // 水平内边距 1 列：文字与边框字符之间留空，不贴框。
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .padding(Padding::horizontal(1)),
            ),
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

/// 权限对话框的写/执行工具专用预览。返回 None 表示该工具不适用
/// （回退完整 JSON）。预览行就是被审阅的参数：强制滚动与批准解锁
/// 逻辑对它一视同仁——渲染形式变了，审阅义务不变。
///
/// NWE-04：命令/路径/内容一律**逻辑行拆分 + wrap_text 换行**——
/// 未换行的超长命令尾部会被水平裁掉，而审阅计数只有 1 行，批准
/// 在用户没看到命令尾部时就解锁。控制字符（\n、\t 之外）转成
/// 可见的 ^X 记法，不可再藏。
fn write_tool_preview(
    tool: &str,
    arguments: &serde_json::Value,
    width: usize,
) -> Option<Vec<Line<'static>>> {
    let object = arguments.as_object()?;
    let mut lines: Vec<Line<'static>> = Vec::new();
    // 控制字符可见化：\r、\0、ESC 等 shell 语义字符在预览里显形
    // 为 ^M、^@、^[，无法借零宽度隐身。
    fn visible(text: &str) -> String {
        text.chars()
            .map(|ch| match ch {
                '\t' => "    ".to_owned(),
                '\n' => ch.to_string(),
                '\x00'..='\x1f' => format!("^{}", (b'@' + ch as u8) as char),
                '\x7f' => "^?".to_owned(),
                _ => ch.to_string(),
            })
            .collect()
    }
    // 标题（edit/write 路径、$ 命令）也换行并可见化控制字符——
    // 路径和命令同样可能长于对话框宽度，藏尾部即藏目标。
    fn push_header(lines: &mut Vec<Line<'static>>, title: String, width: usize) {
        for logical in visible(&title).split('\n') {
            for wrapped in wrap_text(logical, width.saturating_sub(2)) {
                lines.push(Line::from(Span::styled(
                    format!("  {wrapped}"),
                    Style::default().add_modifier(Modifier::BOLD),
                )));
            }
        }
    }
    // 多行文本先按逻辑行拆分再换行：wrap_text 视 \n 为零宽字符，
    // 直接喂多行会把内容挤成一坨，审阅时无法分清结构。
    let push_wrapped = |lines: &mut Vec<Line<'static>>, prefix: &str, text: &str| {
        for logical in visible(text).split('\n') {
            for wrapped in wrap_text(logical, width.saturating_sub(prefix.len() + 2)) {
                lines.push(Line::from(format!("{prefix} {wrapped}")));
            }
        }
    };
    match tool {
        "edit_file" => {
            let path = object.get("path")?.as_str()?;
            let old_str = object.get("old_str")?.as_str()?;
            let new_str = object
                .get("new_str")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("");
            push_header(&mut lines, format!("edit {path}"), width);
            lines.push(Line::from(Span::styled(
                "- old_str (must match the file exactly, once):",
                Style::default().add_modifier(Modifier::DIM),
            )));
            push_wrapped(&mut lines, "-", old_str);
            lines.push(Line::from(Span::styled(
                "+ new_str:",
                Style::default().add_modifier(Modifier::DIM),
            )));
            push_wrapped(&mut lines, "+", new_str);
        }
        "write_file" => {
            let path = object.get("path")?.as_str()?;
            let content = object.get("content")?.as_str()?;
            push_header(
                &mut lines,
                format!("write {path} ({} bytes)", content.len()),
                width,
            );
            for logical in visible(content).split('\n') {
                for wrapped in wrap_text(logical, width.saturating_sub(2)) {
                    lines.push(Line::from(format!("  {wrapped}")));
                }
            }
        }
        "run_command" => {
            let command = object.get("command")?.as_str()?;
            let timeout = object
                .get("timeout_seconds")
                .and_then(serde_json::Value::as_u64)
                .unwrap_or(120);
            push_header(&mut lines, format!("$ {command}"), width);
            lines.push(Line::from(Span::styled(
                format!("  in the project root · timeout {timeout}s"),
                Style::default().add_modifier(Modifier::DIM),
            )));
        }
        _ => return None,
    }
    Some(lines)
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

/// 弹出窗与屏幕左右边缘的最小留白（列）。纯百分比布局在窄终端/
/// 分屏下会退化：94% 在 60 列下每侧仅 1 列、40 列下为 0，对话框
/// 直接贴住左右墙。所有弹出窗共用这一下限，宽度不够时收缩对话框
/// 并保持居中，而不是牺牲边距。
pub(crate) const POPUP_H_MARGIN: u16 = 4;

/// 钳制生效所需的最低对话框宽度：更窄的终端连"边距 + 可用宽度"
/// 都放不下，保留百分比行为，不把对话框挤没。
const MIN_POPUP_WIDTH: u16 = 16;

/// 弹出窗内容的水平内边距（列）。文字与边框字符之间留空，不贴框；
/// 手工换行/截断的宽度计算必须同步扣除 `2 × POPUP_TEXT_PADDING`。
pub(crate) const POPUP_TEXT_PADDING: u16 = 1;

/// 弹出窗统一的边框块：全边框 + 标题 + 1 列水平内边距。标题原样
/// 使用，调用方自带前后空格（如 `" Permission "`）。
pub(crate) fn popup_block(title: &str) -> Block<'_> {
    Block::default()
        .borders(Borders::ALL)
        .title(title)
        .padding(Padding::horizontal(POPUP_TEXT_PADDING))
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
    let mut rect = horizontal[1];
    let bounded = area.width.saturating_sub(2 * POPUP_H_MARGIN);
    if rect.width > bounded && bounded >= MIN_POPUP_WIDTH {
        rect.x = area.x + (area.width - bounded) / 2;
        rect.width = bounded;
    }
    rect
}

/// 权限参数内容在给定终端区域内实际可用的列数。与
/// `draw_permission_dialog` 共用同一矩形和边距计算，避免测试或
/// 预览再次把百分比误当成固定列数。边框 2 列与弹窗内边距
/// 2×POPUP_TEXT_PADDING 列必须先扣掉，预览行才不会贴框或右侧被裁。
fn permission_argument_width(area: Rect) -> usize {
    centered_rect(84, 1, area)
        .width
        .saturating_sub(2) // 边框
        .saturating_sub(2 * POPUP_TEXT_PADDING) // 弹窗内边距
        .saturating_sub(4) as usize // 参数缩进/留白
}

/// 渲染项目确权对话框。独立于 App 以便用 TestBackend 验证边框完整。
fn render_trust_dialog(frame: &mut Frame, area: Rect, root: &Path) {
    // 文本按对话框 84% 宽的“最小合理终端”（约 80 列内宽）换行；
    // 更宽的终端留白更多，更窄的终端由 Paragraph 截断右侧。
    // 边框 2 列与弹窗内边距 2×POPUP_TEXT_PADDING 列先扣掉。
    let inner_width = 84usize.saturating_sub(2 + 2 * POPUP_TEXT_PADDING as usize);
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
    frame.render_widget(Paragraph::new(lines).block(popup_block(" Trust ")), dialog);
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

    /// 实机回归：窄终端/分屏下百分比边距退化（94% 在 50 列下每侧仅
    /// 1 列），弹出窗贴住屏幕左右墙。所有 centered_rect 调用方
    /// （/model、编辑器、/resume、权限与确权对话框）必须保住
    /// POPUP_H_MARGIN 的最小左右留白。
    #[test]
    fn popup_rects_keep_a_minimum_horizontal_margin() {
        for width in [50u16, 60, 66, 80, 120] {
            for percent in [84u16, 94] {
                let area = Rect::new(0, 0, width, 24);
                let rect = centered_rect(percent, 10, area);
                assert!(
                    rect.x >= POPUP_H_MARGIN,
                    "width={width} percent={percent}: left margin {} < {POPUP_H_MARGIN}",
                    rect.x
                );
                assert!(
                    rect.x + rect.width <= width - POPUP_H_MARGIN,
                    "width={width} percent={percent}: right edge {} exceeds {}",
                    rect.x + rect.width,
                    width - POPUP_H_MARGIN
                );
            }
        }
        // 钳制保持居中：收缩量两侧均分。
        let rect = centered_rect(94, 10, Rect::new(0, 0, 50, 24));
        assert_eq!(rect.x, POPUP_H_MARGIN);
        assert_eq!(rect.width, 50 - 2 * POPUP_H_MARGIN);
    }

    /// 极窄终端（放不下边距 + MIN_POPUP_WIDTH）保留百分比行为，
    /// 钳制不得把对话框挤没。
    #[test]
    fn popup_margin_never_squeezes_tiny_terminals() {
        let area = Rect::new(0, 0, 20, 10);
        let rect = centered_rect(84, 6, area);
        assert!(rect.width > 0, "tiny terminals still get a usable popup");
    }

    /// NWE-04：超长命令必须换行成多个审阅行——危险尾部（藏在第
    /// 200 列的 `; shred`）要成为**可滚动到的独立行**，而不是被
    /// 水平裁掉后永不可见。旧实现单行预览 + 1 行计数 = 尾部不可见
    /// 即解锁批准。
    #[test]
    fn long_command_previews_as_multiple_reviewable_lines() {
        let command = format!(
            "cargo test --features long-boring-prefix-{} ; shred /tmp/victim",
            "x".repeat(200)
        );
        let lines = write_tool_preview(
            "run_command",
            &serde_json::json!({"command": command, "timeout_seconds": 30}),
            60,
        )
        .expect("preview");
        assert!(
            lines.len() > 2,
            "a 200+ column command must wrap into many review lines, got {}",
            lines.len()
        );
        let rendered: String = lines
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|span| span.content.to_string())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            rendered.contains("shred"),
            "the dangerous tail must be present in reviewable lines"
        );
        // 尾部落在自己的审阅行上：含 shred/victim 的行存在，且
        // 任何单行宽度不超过可用宽度（没有被水平裁掉的内容）。
        assert!(
            rendered.split('\n').any(|line| line.contains("victim")),
            "the dangerous tail must land on its own reviewable line"
        );
    }

    /// 审计回归：80 列终端的权限框只有约 67 列，参数区更窄。
    /// 换行宽度必须来自真实矩形；旧实现固定传 78，命令尾部会在
    /// Paragraph 渲染时被水平裁掉，但一行审阅计数仍可解锁批准。
    #[test]
    fn permission_preview_uses_the_actual_dialog_width() {
        let area = Rect::new(0, 0, 80, 24);
        let width = permission_argument_width(area);
        assert!(width < 78, "84% must not be treated as 84 columns");
        let command = format!("printf safe-{}; rm -rf ./victim", "x".repeat(70));
        let lines = write_tool_preview(
            "run_command",
            &serde_json::json!({"command": command, "timeout_seconds": 30}),
            width,
        )
        .expect("preview");
        let rendered = lines
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|span| span.content.to_string())
                    .collect::<String>()
            })
            .collect::<Vec<_>>();
        assert!(
            rendered
                .iter()
                .all(|line| UnicodeWidthStr::width(line.as_str()) <= width),
            "no review line may be clipped by the actual dialog"
        );
        assert!(
            rendered.iter().any(|line| line.contains("rm -rf ./victim")),
            "the dangerous command tail must be a visible review line"
        );
    }

    /// NWE-04：命令内嵌换行拆成独立审阅行（shell 语义上它们就是
    /// 两条命令）；控制字符显形为 ^X 记法，不可借零宽度隐身。
    #[test]
    fn multiline_and_control_characters_are_visible_in_previews() {
        let lines = write_tool_preview(
            "run_command",
            &serde_json::json!({"command": "echo a\necho b\rc\x1b[2J tail"}),
            60,
        )
        .expect("preview");
        let rendered: Vec<String> = lines
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|span| span.content.to_string())
                    .collect::<String>()
            })
            .collect();
        let joined = rendered.join("\n");
        assert!(
            rendered.iter().any(|line| line.contains("echo b")),
            "embedded newlines must split into separate review lines"
        );
        assert!(joined.contains("^M"), "CR must be visible as ^M");
        assert!(joined.contains("^["), "ESC must be visible as ^[");

        // edit_file 的多行 old_str 保持行结构。
        let lines = write_tool_preview(
            "edit_file",
            &serde_json::json!({
                "path": "src/main.rs",
                "old_str": "fn a() {\n    body\n}",
                "new_str": "fn a() {\n    body2\n}"
            }),
            60,
        )
        .expect("preview");
        assert!(
            lines.len() >= 8,
            "three-line old_str and new_str must render as distinct lines"
        );
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

    /// 实机回归：弹出窗**内部**的文字左右贴着弹窗自己的边框（用户
    /// 反馈"文字左右贴着弹出窗"）。所有文本行与左右边框之间必须
    /// 保留 POPUP_TEXT_PADDING 列空白。
    #[test]
    fn popup_text_never_touches_its_own_borders() {
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
            let mut right = left + 1;
            while right < width && buffer[(right, top)].symbol() != "┐" {
                right += 1;
            }
            let mut bottom = top + 1;
            while bottom < height && buffer[(left, bottom)].symbol() != "└" {
                bottom += 1;
            }
            assert!(right < width && bottom < height, "borders not found");

            // 修复前文本直接从边框下一列开始（如 'T' of "Trust this
            // project?"）；修复后该列必须是内边距空白。
            for y in (top + 1)..bottom {
                assert_eq!(
                    buffer[(left + 1, y)].symbol(),
                    " ",
                    "text touches the left border at x={} y={y} ({width}x{height})",
                    left + 1
                );
                assert_eq!(
                    buffer[(right - 1, y)].symbol(),
                    " ",
                    "text touches the right border at x={} y={y} ({width}x{height})",
                    right - 1
                );
            }
        }
    }

    /// `popup_block` 是所有弹出窗共用的边框块：文字与边框之间固定
    /// 保留 POPUP_TEXT_PADDING 列。
    #[test]
    fn popup_block_pads_text_away_from_the_borders() {
        let backend = TestBackend::new(40, 7);
        let mut terminal = Terminal::new(backend).expect("terminal");
        terminal
            .draw(|frame| {
                frame.render_widget(
                    Paragraph::new("X").block(popup_block(" T ")),
                    Rect::new(0, 0, 40, 7),
                );
            })
            .expect("draw");
        let buffer = terminal.backend().buffer();
        // 内容首行在顶边框之下（y=1）：边框、内边距、文字、内边距。
        assert_eq!(buffer[(0, 1)].symbol(), "│");
        assert_eq!(
            buffer[(1, 1)].symbol(),
            " ",
            "no padding inside the left border"
        );
        assert_eq!(buffer[(2, 1)].symbol(), "X");
        assert_eq!(buffer[(3, 1)].symbol(), " ");
    }

    #[test]
    fn wraps_cjk_by_terminal_width() {
        assert_eq!(wrap_text("你好世界", 4), vec!["你好", "世界"]);
    }

    #[test]
    fn converts_transcript_lines_to_chat() {
        let message = ChatMessage::from_transcript(TranscriptLine {
            kind: "user".into(),
            text: "hello".into(),
            is_error: false,
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

    /// INV-C：状态栏后缀只有 Wallet/Token、Cache、Context 三段，思考
    /// 档位不在这里（属标题栏）；三段各自缺值即省略。
    #[test]
    fn status_suffix_combines_wallet_cache_and_context() {
        // 全宽拼接（生产路径按宽度经 fit_status_suffix 裁剪，另测）。
        fn full_suffix(
            config: &ModelConfig,
            balance: &Option<String>,
            session_usage: &Usage,
            last_turn_usage: Option<&Usage>,
        ) -> String {
            status_suffix_segments(config, balance, session_usage, last_turn_usage).join(" · ")
        }

        let balance = Some("110.00".to_owned());
        let no_data = Usage::default();
        let cached = Usage {
            input_tokens: 1000,
            cached_input_tokens: Some(870),
            ..Usage::default()
        };
        let turn = Usage {
            input_tokens: 115_000,
            output_tokens: 5_000,
            ..Usage::default()
        };
        let mut config = ModelConfig {
            preset: Some("deepseek-v4-pro".into()),
            endpoint: "https://api.deepseek.com".into(),
            ..ModelConfig::default()
        };

        // 三段齐全（DeepSeek）。
        assert_eq!(
            full_suffix(&config, &balance, &cached, Some(&turn)),
            "Wallet: ￥110.00 · Cache: 87.00% · Context: 120k/1M"
        );
        // 无任何数据：空串，状态栏保持原样。
        assert_eq!(full_suffix(&config, &None, &no_data, None), "");
        // 余额未就绪：Cache/Context 照常显示（不再整条消失）。
        assert_eq!(
            full_suffix(&config, &None, &cached, Some(&turn)),
            "Cache: 87.00% · Context: 120k/1M"
        );
        // 尚无上下文样本：省略 Context 段。
        assert_eq!(
            full_suffix(&config, &balance, &cached, None),
            "Wallet: ￥110.00 · Cache: 87.00%"
        );
        // 缓存命中为零：省略 Cache 段。
        let zero_cache = Usage {
            input_tokens: 1000,
            cached_input_tokens: Some(0),
            ..Usage::default()
        };
        assert_eq!(
            full_suffix(&config, &balance, &zero_cache, Some(&turn)),
            "Wallet: ￥110.00 · Context: 120k/1M"
        );

        // GLM Coding Plan：Token 前缀替代 Wallet，不加货币符号。
        config.preset = Some("glm-5.3".into());
        config.endpoint = "https://open.bigmodel.cn/api/coding/paas/v4".into();
        let quota = Some("87%".to_owned());
        assert_eq!(
            full_suffix(&config, &quota, &cached, Some(&turn)),
            "Token: 87% · Cache: 87.00% · Context: 120k/1M"
        );
        // 海外 z.ai 端点同样生效。
        config.endpoint = "https://api.z.ai/api/coding/paas/v4".into();
        assert_eq!(
            full_suffix(&config, &quota, &cached, Some(&turn)),
            "Token: 87% · Cache: 87.00% · Context: 120k/1M"
        );

        // 自定义端点（无预设）：Context 分母未知，省略整段。
        config.preset = None;
        config.endpoint = "https://api.deepseek.com".into();
        assert_eq!(
            full_suffix(&config, &balance, &cached, Some(&turn)),
            "Wallet: ￥110.00 · Cache: 87.00%"
        );

        // 非 DeepSeek/GLM 端点：无后缀。
        config.endpoint = "https://api.openai.com/v1".into();
        assert_eq!(full_suffix(&config, &balance, &cached, Some(&turn)), "");
    }

    #[test]
    fn format_tokens_uses_compact_units() {
        assert_eq!(format_tokens(0), "0");
        assert_eq!(format_tokens(999), "999");
        // 千位就近取整。
        assert_eq!(format_tokens(1_000), "1k");
        assert_eq!(format_tokens(120_000), "120k");
        assert_eq!(format_tokens(120_499), "120k");
        assert_eq!(format_tokens(120_500), "121k");
        // 百万位保留一位小数，整数省略小数部分。
        assert_eq!(format_tokens(1_000_000), "1M");
        assert_eq!(format_tokens(1_048_576), "1M");
        assert_eq!(format_tokens(1_500_000), "1.5M");
        assert_eq!(format_tokens(2_000_000), "2M");
    }

    /// INV-C + TUI-L02：标题栏首行按宽度三级退化，档位优先于模型名
    /// 保留；无档位时各层级不含档位片段。模型+思考+强度是一体：
    /// 组内分隔符统一窄间距（模型↔Thinking 与 Thinking↔强度一致），
    /// 与主分段宽间距区分。
    #[test]
    fn header_rest_degrades_by_width_keeping_the_level_visible() {
        let full = compose_header_rest("0.5.1", "ready", "DeepSeek V4.0 Flash", Some("High"), 200);
        assert_eq!(
            full,
            " v0.5.1  ready  ·  DeepSeek V4.0 Flash · Thinking · High"
        );
        // 宽度恰好：完整。
        let fit = UnicodeWidthStr::width(full.as_str());
        assert_eq!(
            compose_header_rest("0.5.1", "ready", "DeepSeek V4.0 Flash", Some("High"), fit),
            full
        );
        // 差一列 → 紧凑（保留模型与档位、省略 "Thinking · " 文案）。
        let compact = " v0.5.1 ready · DeepSeek V4.0 Flash · High";
        assert_eq!(
            compose_header_rest(
                "0.5.1",
                "ready",
                "DeepSeek V4.0 Flash",
                Some("High"),
                fit - 1
            ),
            compact
        );
        // 紧凑也放不下 → 最小（省略模型名，档位仍在）。
        let compact_fit = UnicodeWidthStr::width(compact);
        assert_eq!(
            compose_header_rest(
                "0.5.1",
                "ready",
                "DeepSeek V4.0 Flash",
                Some("High"),
                compact_fit - 1
            ),
            " v0.5.1 ready · Thinking · High"
        );
        // 60 列终端（预算 60-7=53）：紧凑。紧凑层级宽 42 列：预算 42
        // 仍完整，41（48 列终端）即降到最小——档位保留、模型名省略。
        assert_eq!(
            compose_header_rest("0.5.1", "ready", "DeepSeek V4.0 Flash", Some("High"), 53),
            compact
        );
        assert_eq!(
            compose_header_rest("0.5.1", "ready", "DeepSeek V4.0 Flash", Some("High"), 42),
            compact
        );
        assert_eq!(
            compose_header_rest("0.5.1", "ready", "DeepSeek V4.0 Flash", Some("High"), 41),
            " v0.5.1 ready · Thinking · High"
        );
        assert_eq!(
            compose_header_rest("0.5.1", "ready", "DeepSeek V4.0 Flash", Some("High"), 40),
            " v0.5.1 ready · Thinking · High"
        );
        // 无档位（未配置 / 其它厂商 / 手工 disabled 由调用方归为 None）：
        // 各层级不出现档位片段，最小层级只剩版本与状态。
        assert_eq!(
            compose_header_rest("0.5.1", "ready", "DeepSeek V4.0 Flash", None, 200),
            " v0.5.1  ready  ·  DeepSeek V4.0 Flash"
        );
        assert_eq!(
            compose_header_rest("0.5.1", "ready", "DeepSeek V4.0 Flash", None, 35),
            " v0.5.1 ready · DeepSeek V4.0 Flash"
        );
        assert_eq!(
            compose_header_rest("0.5.1", "ready", "DeepSeek V4.0 Flash", None, 34),
            " v0.5.1 ready"
        );
    }

    /// TUI-L02：窄终端下右侧遥测按优先级让位（Context 先弃，Cache 次
    /// 之，余额最后），左侧常规状态保底不小于 MIN_STATUS_LEFT。
    #[test]
    fn status_suffix_yields_to_left_status_by_priority() {
        let segments = vec![
            "Wallet: ￥89.35".to_owned(),
            "Cache: 99.99%".to_owned(),
            "Context: 120k/1M".to_owned(),
        ];
        // 120/80 列终端：预算充足，三段齐全。
        assert_eq!(
            fit_status_suffix(&segments, 52),
            "Wallet: ￥89.35 · Cache: 99.99% · Context: 120k/1M"
        );
        // 60 列（预算 60-22=38）：放弃 Context。
        assert_eq!(
            fit_status_suffix(&segments, 38),
            "Wallet: ￥89.35 · Cache: 99.99%"
        );
        // 48 列（预算 26）：仅余额。
        assert_eq!(fit_status_suffix(&segments, 26), "Wallet: ￥89.35");
        // 首段都装不下：整体让位，左侧状态独占整行。
        assert_eq!(fit_status_suffix(&segments, 14), "");
        // 恰好等于段宽时保留（含全角 ￥ 的宽度计入）。
        assert_eq!(fit_status_suffix(&segments, 15), "Wallet: ￥89.35");
        assert_eq!(fit_status_suffix(&[], 100), "");
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
