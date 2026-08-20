use crate::SessionId;
use crate::presets::preset_by_id;
use crate::tui_conversation::ToolCardVisibility;
use crate::tui_input::InputBuffer;
use crate::tui_model::{EditorAction, ModelEditor, ModelPicker, PickerAction};
use crate::tui_sessions::{ResumeAction, SessionPicker};
use crate::tui_theme;
use crate::tui_worker::{
    ChannelApprover, ChannelEventSink, ChannelUserAsker, UiEvent, WorkerMessage,
};
use crate::{
    ApplicationEvent, ApplicationRunRequest, BootstrapApplication, CompactHandle, CompactionStatus,
    McpStatusDto, ModelConfig, ModelEvent, ModelVendor, PermissionDecision, PermissionMode,
    PermissionRequest, Project, ProjectAuthorization, ProviderCredentials, ProviderDescriptor,
    RenameOutcome, RunEvent, RunHandle, SteerOutcome, ThinkingLevel, TrustedProjectApplication,
    Usage, apply_thinking_level, effective_thinking_level, escalation_targets, next_thinking_level,
    thinking_levels,
};
use crossterm::event::{
    self, DisableBracketedPaste, DisableMouseCapture, Event, KeyCode, KeyEvent, KeyEventKind,
    KeyModifiers, KeyboardEnhancementFlags, MouseButton, MouseEvent, MouseEventKind,
    PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
};
use crossterm::execute;
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Margin, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{
    Block, Borders, Clear, Padding, Paragraph, Scrollbar, ScrollbarOrientation, ScrollbarState,
    Wrap,
};
use ratatui::{DefaultTerminal, Frame};
use std::env;
use std::io::{self, Write, stdout};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;
use std::time::{Duration, Instant};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

/// Spinner frames for the "thinking" indicator（状态栏唯一旋转元素）。
/// 2026-08-19 起帧步进为每 [`SPINNER_STEP_TICKS`] 个渲染 tick（160ms/帧
/// @80ms 唤醒）：80ms 下盲文旋转快得看不清。
const SPINNER_FRAMES: [&str; 8] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧"];
/// spinner 帧步进（渲染 tick 数）。
const SPINNER_STEP_TICKS: u64 = 2;

/// 动画帧号换算（纯函数，供 [`App::animation_tick`] 与不变量测试）：
/// 帧号 = 流逝时间 / 帧周期。不变量 A-CLK：同一时刻任意次重绘得到
/// 同一帧；帧差只由时间差决定——与重绘次数、绘制耗时、内容长度
/// 全部无关（2026-08-19 用户三次反馈的根因是 draw() 自增帧号）。
fn animation_tick_for(elapsed: Duration) -> u64 {
    elapsed.as_millis() as u64 / SPINNER_FRAME.as_millis() as u64
}

/// 提醒铃（2026-08-19，AFK 场景：对话结束/需要批准时人可能不在屏
/// 前）。run 结束（用户主动取消除外）、权限与 ask 弹框打开时响一声。
///
/// 三种模式：
/// - `Terminal`（默认）：发终端铃 BEL（`\x07`）。**声音本身由终端模拟
///   器决定**——应用只能触发，换音效去终端设置改（iTerm2/Warp 等支持
///   自选铃声音效文件）；
/// - `Off`（`CLAT_NO_BELL=1`）：静音；
/// - `Command`（`CLAT_BELL_COMMAND="..."`）：任意 shell 命令（macOS
///   `afplay ~/Sounds/ding.aiff`、Linux `paplay ding.ogg`），完全自定
///   义。后台执行、stdio 全断开、失败静默——提醒是尽力而为，绝不影
///   响主流程。
#[derive(Clone, Debug, Eq, PartialEq)]
enum BellMode {
    Terminal,
    Off,
    Command(String),
}

/// 环境变量 → 模式（纯函数，测试从这里推导）。
fn bell_mode_from_env(no_bell: Option<String>, command: Option<String>) -> BellMode {
    let silenced = no_bell
        .map(|value| matches!(value.trim(), "1" | "true" | "yes" | "on"))
        .unwrap_or(false);
    if silenced {
        return BellMode::Off;
    }
    match command {
        Some(command) if !command.trim().is_empty() => BellMode::Command(command),
        _ => BellMode::Terminal,
    }
}

/// 响一声。BEL 直写 stdout：raw 模式只改输入侧，模拟器收到 BEL 按
/// 自己的铃设置发声（或视觉闪铃）。命令模式 detached spawn（不 wait、
/// 不接管 stdio）。
fn ring_bell(mode: &BellMode) {
    match mode {
        BellMode::Off => {}
        BellMode::Terminal => {
            let _ = write!(stdout(), "\x07");
            let _ = stdout().flush();
        }
        BellMode::Command(command) => {
            // Child drop 是 detach 不是 reap：不 wait 的话每次响铃留一个
            // 僵尸到进程退出（对抗审计 2026-08-19）。提醒命令都是短命
            // 进程，一个专属收割线程足够。spawn 失败静默（提醒尽力而为）。
            if let Ok(mut child) = std::process::Command::new("sh")
                .arg("-c")
                .arg(command)
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn()
            {
                std::thread::spawn(move || {
                    let _ = child.wait();
                });
            }
        }
    }
}

/// 粘贴的图片附件判定（M6，纯函数可测）：**整条**粘贴（trim 后）恰好
/// 是一个存在的图片文件**绝对路径**时返回它（`~` 展开；Windows 盘符
/// 路径同放行）。防误判优先：相对路径、含空白/换行、扩展名不认识、
/// 文件不存在、超过 4MB 一律 None——宁可漏判当文本插入，不可把用户
/// 的文字吞成附件。相对路径被排除是刻意的：存在性检查相对进程 cwd
/// 解析，裸文件名（"logo.png"）碰巧同名就会被误判。
fn pasted_image_path(text: &str) -> Option<std::path::PathBuf> {
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

/// 当前 spinner 帧字形（阶段行专用）。
fn spinner_frame(tick: u64) -> &'static str {
    SPINNER_FRAMES[((tick / SPINNER_STEP_TICKS) % SPINNER_FRAMES.len() as u64) as usize]
}

/// 会话区流式 assistant 前缀的"太阳"帧：四分圆旋转——保持圆形字形
///（用户要求：原来的点是圆的，替代品也应是圆的），灰色与落定 ⏺ 同色
/// 族、与状态栏的蓝色盲文 spinner 不同形不同色，不构成重复（2026-08-19
/// 第二轮反馈：两个盲文旋转并排不好看）。
const MARKER_FRAMES: [&str; 4] = ["◐", "◓", "◑", "◒"];

/// 当前流式前缀帧（会话区专用，与 spinner 同步进、不同字形）。
fn marker_frame(tick: u64) -> &'static str {
    MARKER_FRAMES[((tick / SPINNER_STEP_TICKS) % MARKER_FRAMES.len() as u64) as usize]
}

/// run 内当前派生阶段（phase-1 P1-5）：从既有事件流派生，非独立状态机
/// 输入；每个模型步（ModelRequested）重开 Waiting，步内只前进不回退。
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
enum Phase {
    WaitingFirstToken,
    Thinking,
    Responding,
    ExecutingTools,
}

impl Phase {
    fn label(self) -> &'static str {
        match self {
            Self::WaitingFirstToken => "Waiting first token",
            Self::Thinking => "Thinking…",
            Self::Responding => "Responding",
            Self::ExecutingTools => "Executing tools",
        }
    }
}

/// 阶段与双时钟的纯状态机（G6 可直接单测）。
#[derive(Default)]
struct PhaseTracker {
    phase: Option<Phase>,
    phase_started: Option<Instant>,
    run_started: Option<Instant>,
}

impl PhaseTracker {
    /// 新模型步：阶段重开为 Waiting（DSH ttft 语义），run 钟只启一次。
    fn model_requested(&mut self) {
        self.phase = Some(Phase::WaitingFirstToken);
        self.phase_started = Some(Instant::now());
        self.run_started.get_or_insert_with(Instant::now);
    }

    /// 步内只前进：Waiting→Thinking→Responding→ExecutingTools。
    fn advance(&mut self, target: Phase) {
        if self.phase.is_none() {
            return;
        }
        if self.phase.is_some_and(|current| target > current) {
            self.phase = Some(target);
            self.phase_started = Some(Instant::now());
        }
    }

    /// run 终态：全部计时状态清空，不留活计时器（G6）。
    fn finish(&mut self) {
        self.phase = None;
        self.phase_started = None;
        self.run_started = None;
    }
}

/// 双时钟格式：<1 分钟 `8s`，≥1 分钟 `1m05s`。
fn format_clock(duration: Duration) -> String {
    let secs = duration.as_secs();
    if secs < 60 {
        format!("{secs}s")
    } else {
        format!("{}m{:02}s", secs / 60, secs % 60)
    }
}
/// 探照灯单字符驻留（渲染 tick）：光带每 [`SWEEP_STEP_TICKS`] 个 tick
/// 前进**一个字符**（2026-08-19 第五轮：驻留减半提速一倍——160ms 太
/// 慢，人类体感拖沓）。锚点仍是单字符照亮时长，不锚定整圈周期：标签
/// 长则整圈按比例长，视觉速度恒定。转圈不再与光带同步：spinner 保持
/// 每 [`SPINNER_STEP_TICKS`]（=2）tick 一帧——即**每照亮 2 个字符转圈
/// 换一帧**（8 帧一圈 = 16 字符 × 80ms = 1.28s），转速既不随标签长度
/// 变，也不随探照灯提速变。旧两版各自的病根：整圈周期恒定 → 每字速
/// 度随字数变（v0.6.1 的 bug）；整词呼吸 → 移动消失成霓虹。tick 墙钟
/// 驱动（A-CLK），驻留与重绘频率无关。
const SWEEP_STEP_TICKS: u64 = 1;
/// 光带进出余量（字符）：出光后灯光完全离开尾字符、再从首字符进入，
/// 换圈处没有亮度跳变；余量足以把高斯尾压到熄灭阈之下。
const SWEEP_MARGIN_CHARS: u64 = 3;
/// 光带柔和度（字符）。
const SHIMMER_SIGMA: f64 = 1.2;
/// 熄灭阈：高斯尾低于此值按 0 处理——"灯光过去后回原色"是精确的
/// 基色，而不是差 1 个 RGB 值的残影。
const SHIMMER_UNLIT_FLOOR: f64 = 0.03;

/// 派生阶段状态行（phase-1 P1-5）：spinner + 探照灯阶段标签 + 双时钟
/// `<phase> <phase-elapsed> · total <run-elapsed>`；Waiting 只报总计。
fn phase_line(
    tick: u64,
    phase: Phase,
    phase_elapsed: Option<Duration>,
    run_elapsed: Option<Duration>,
    steering_queued: usize,
) -> Line<'static> {
    let frame = spinner_frame(tick);
    let base = tui_theme::style(tui_theme::Role::ThinkingGlyph);

    let mut spans = vec![
        Span::styled(frame, base.add_modifier(Modifier::BOLD)),
        Span::styled(" ", base),
    ];

    // 探照灯：光带中心每 SWEEP_STEP_TICKS（1 tick）前进一个字符，
    // 范围 [-margin, len+margin)。先照亮的字符在灯光走过后回基色；
    // 高斯尾给出柔和的边缘。转圈独立节律（每 2 字符一帧，见常量注
    // 释）。
    let label = phase.label();
    let len = label.chars().count() as u64;
    let cycle = len + 2 * SWEEP_MARGIN_CHARS;
    let center = ((tick / SWEEP_STEP_TICKS) % cycle) as f64 - SWEEP_MARGIN_CHARS as f64;
    for (index, ch) in label.chars().enumerate() {
        let distance = (index as f64 + 0.5 - center).abs();
        let intensity = (-(distance * distance) / (2.0 * SHIMMER_SIGMA * SHIMMER_SIGMA)).exp();
        let intensity = if intensity < SHIMMER_UNLIT_FLOOR {
            0.0
        } else {
            intensity
        };
        spans.push(Span::styled(
            ch.to_string(),
            Style::default().fg(tui_theme::blend(
                tui_theme::BRAND_SHIMMER_LOW,
                tui_theme::BRAND_SHIMMER_HIGH,
                intensity,
            )),
        ));
    }

    if let Some(run_elapsed) = run_elapsed {
        let clocks = match (phase, phase_elapsed) {
            (Phase::WaitingFirstToken, _) => format!(" {}", format_clock(run_elapsed)),
            (_, Some(phase_elapsed)) => format!(
                " {} · total {}",
                format_clock(phase_elapsed),
                format_clock(run_elapsed)
            ),
            (_, None) => format!(" · total {}", format_clock(run_elapsed)),
        };
        spans.push(Span::styled(
            clocks,
            tui_theme::style(tui_theme::Role::Faint),
        ));
    }
    if steering_queued > 0 {
        // DSH `N queued` 徽标：advisory 实时状态，claim 后随
        // SteeringApplied 回收。
        spans.push(Span::styled(
            format!(" · steering·{steering_queued}"),
            tui_theme::style(tui_theme::Role::Warning),
        ));
    }
    Line::from(spans)
}

/// 会话累计的缓存命中百分比文本（如 "99.99%"，两位小数）。无输入
/// token 或服务端未上报缓存命中时不显示（返回 None）。
fn cache_hit_percent(usage: &Usage) -> Option<String> {
    let cached = usage.cached_input_tokens?;
    if usage.input_tokens == 0 {
        return None;
    }
    // Some(0) 是"服务端上报了零命中"——真实的 0.00%，不是未知。
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

/// 会话文本的折行宽度：inner（边框内 `width - 2`）再为右侧滚动条列
/// 预留 1 列——行尾宽字符（CJK/emoji 占 2 列）的字形不得铺进滚动条
/// 列（ratatui diff 的 `to_skip` 会吞掉被字形覆盖的滚动条符号补发，
/// 用户实测：纯 ASCII 行不遮挡，被遮挡的行必含非英文字符）。渲染与
/// 选区复制必须共用本函数——复制旁路宽度会让拷出的行与显示的折行
/// 错位（2026-08-19 审计发现的回归）。
fn conversation_wrap_width(area: Rect) -> usize {
    area.width.saturating_sub(3).max(1) as usize
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

/// 状态栏右侧遥测段，按优先级降序（额度 > Cache > Context）。Wallet/
/// Token 段随余额查询就绪；Cache/Context 对 DeepSeek/GLM **常驻**——
/// 无数据时显示 `--%` / `0`（2026-08-19 用户反馈：启动/首跑中途三段
/// 必须齐全，布局不跳变）。journal 还原 + 流式实时累计让真实值尽早
/// 出现。渲染时 `fit_status_suffix` 在窄终端从尾部（最低优先）开始
/// 让位。
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
    // Cache 段对 DeepSeek/GLM 常驻：无数据时显示 `--%`（新会话尚未
    // 首跑、或适配器未上报），三段布局自启动起稳定。
    let cache = cache_hit_percent(session_usage).unwrap_or_else(|| "--%".into());
    parts.push(format!("Cache: {cache}"));
    // Context 当前值 ≈ 最近一次模型请求的 input+output（下一次请求
    // 的近似起点）；分母是预设的官方上下文窗口，自定义端点未知则
    // 省略整段。新会话无请求历史时按 0 计。
    let window = config
        .preset
        .as_deref()
        .and_then(preset_by_id)
        .map(|preset| preset.context_window);
    if let Some(window) = window {
        let current = last_turn_usage
            .map(|usage| usage.input_tokens + usage.output_tokens)
            .unwrap_or(0);
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

    let (result, close_error) = match App::open_deferred(project, None) {
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
    // 告别 LOGO：主屏恢复后打印，与启动欢迎页成对（TTY 守卫 + 静默
    // 失败，纯装饰不影响退出码与管道输出）。
    crate::tui_logo::print_farewell();
    // 显式 shutdown 的失败在终端恢复后可见地报告（plan §16 阶段5）。
    if let Some(error) = close_error {
        let _ = writeln!(io::stderr(), "clat: {error}");
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
    /// 升级选项（P5）：能让本次调用直接放行的更宽档位（宽度升序）。
    /// 弹框打开时由 `escalation_targets(当前档, effect)` 算出；键位
    /// `w`/`f` 按此集合生效——先 `set_permission_mode` 再回 Allow，
    /// approver 契约零改动。
    escalations: Vec<PermissionMode>,
}

/// ask-user 对话框状态：选项模式（`custom = None`，selection 游标含
/// 末尾的"自定义输入"行）或自定义输入模式（`custom = Some`）。无选项时
/// 直接进入输入模式；Esc 拒绝（Declined → isError 结果，run 继续）。
struct PendingAskUser {
    question: crate::interaction::AskQuestion,
    answer_tx: Sender<crate::interaction::AskAnswer>,
    selection: usize,
    custom: Option<String>,
}

/// 信息弹窗（/help、/mcp）的种类。两类共用滚动/翻页/绘制骨架；键位
/// 差异只有 /mcp 多一个 `r` 刷新。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum InfoDialogKind {
    Help,
    Mcp,
}

/// 打开中的信息弹窗：种类 + 当前滚动位（绘制期钳制在
/// `App::info_scroll_max`）。
struct InfoDialog {
    kind: InfoDialogKind,
    offset: usize,
}

impl InfoDialog {
    fn new(kind: InfoDialogKind) -> Self {
        Self { kind, offset: 0 }
    }
}

/// `/rename` 弹框：内嵌完整 `InputBuffer`（真实光标编辑，比 model
/// editor 的追加式 `EditPopup` 强一档），预填当前标题。Enter 提交
/// （空文本 flash 拒绝、不关框）、Esc 取消。门槛（2026-08-19 放宽）：
/// 有活动会话即可——不再要求 LLM 已起名。
struct RenameDialog {
    buffer: InputBuffer,
}

impl RenameDialog {
    fn new(prefill: &str) -> Self {
        let mut buffer = InputBuffer::new(Vec::new());
        buffer.insert_str(prefill);
        Self { buffer }
    }
}

struct App {
    project: Project,
    bootstrap: Option<BootstrapApplication>,
    application: Option<TrustedProjectApplication>,
    /// 当前会话 id；`None` 表示项目尚未受信（延迟初始化前不可对话）。
    /// 确权门保证所有用到它的路径只在 Some 时可达。
    session_id: Option<SessionId>,
    /// 会话右标题（对话框 block 右上角，与左上角 Conversation 对称）：
    /// effective 标题。快照路径（挂载/resume//new）+ `TitleUpdated` 事件
    /// 两路维护，显示与 /resume 列表同源。
    session_title: Option<String>,
    /// shutdown 时 Application close() 的错误（终端恢复后展示）。
    close_error: Option<String>,
    config: ModelConfig,
    credentials: ProviderCredentials,
    provider_descriptors: Vec<ProviderDescriptor>,
    /// 转录模型：live 与回放的单一装配（G2/G8），渲染缓存内建（G3）。
    conversation: crate::tui_conversation::ConversationModel,
    /// 工具卡三态（Ctrl+O 循环）。纯呈现状态：不持久化（G5）。
    card_visibility: ToolCardVisibility,
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
    /// 已入队、尚未被 claim 的 steering 条数（advisory 实时状态：不进
    /// 会话日志，resume 后从零开始——DSH TUI 同款）。SteeringApplied
    /// 回流减一；run 结束清零并提示丢弃。
    steering_queued: usize,
    /// 统一事件通道：输入线程、余额监控、worker 的消息都汇到这里。
    /// `None` 表示尚未启动（run() 建立通道后填充）。
    events: Option<Receiver<UiEvent>>,
    /// 统一事件通道的发送端克隆：start_run 移交 worker，刷新触发用。
    event_sender: Option<Sender<UiEvent>>,
    pending_permission: Option<PendingPermission>,
    pending_ask_user: Option<PendingAskUser>,
    /// `/perm` 权限三档选择器（冷切换/降级入口；`/permission` 为别名）。
    permission_picker: Option<crate::tui_permission::PermissionPicker>,
    /// `/rename` 会话改名弹框（显式标题存在时才可打开，N4）。
    rename_dialog: Option<RenameDialog>,
    /// 待随下一条消息发送的图片附件（用户路径；提交时复制进会话附件
    /// 目录，见 M4）。仅空闲态可附加；Esc 清空输入时一并清空。
    attachments: Vec<std::path::PathBuf>,
    run_handle: Option<RunHandle>,
    /// `/compact` 进行中的句柄；Esc 取消。
    compact_handle: Option<CompactHandle>,
    phases: PhaseTracker,
    /// 动画时钟起点：帧号 = 流逝时间 / [`SPINNER_FRAME`]（见
    /// [`App::animation_tick`]）。2026-08-19 第三轮反馈：旧实现每个
    /// draw() 自增帧号——重绘频率（流式事件洪峰）把旋转/呼吸加速成
    /// 频闪，单帧绘制耗时长（长转录）又拖成慢动作，速度永远随"这一
    /// 帧画了多少东西"漂移。真实时间驱动的帧号对重绘次数彻底不敏感。
    animation_epoch: Instant,
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
    /// 信息弹窗（/help、/mcp）：`Some` = 打开。两类弹窗共用一套滚动/
    /// 翻页/绘制骨架（内容驱动高度，2026-08-19 第三轮反馈：旧 /help
    /// 恒取满额高度，内容再少也是整屏框、上下边距形同虚设）。
    info_dialog: Option<InfoDialog>,
    /// 绘制期计算的最大滚动位（info 弹窗每帧刷新，按键翻页用它钳制）。
    /// 首帧绘制先于任何按键，键处理器读到的总是有效值。
    info_scroll_max: usize,
    /// 绘制期记录的弹窗可视行数（PgUp/PgDn 的翻页步长）。
    info_page: usize,
    /// /mcp 打开时缓存的 MCP 状态（DTO）；弹窗内 `r` 向 Application 重取。
    mcp_view: Option<McpStatusDto>,
    /// 余额/额度当前值：核心 Monitor 插件经 ApplicationEvent 写回，状态栏读取。
    balance: Option<String>,
    /// 本会话累计 token 用量，用于状态栏缓存命中百分比。journal 还原
    /// （挂载/切换）+ 运行中流式实时累计 + run 结束以结果权威覆盖。
    session_usage: Usage,
    /// 最近一次模型请求的用量（INV-F：随会话切换/新建重置），用于
    /// 状态栏 `Context: 120k/1M` 的当前值近似。
    last_turn_usage: Option<Usage>,
    /// 本次 run 开始时的会话用量基线：流式 Usage 事件在其上累加出
    /// 实时值，run 结束以 RunOutput 的全量结果替换（权威、不重复计）。
    run_usage_base: Option<Usage>,
    /// 本次 run 内流式 Usage 事件的累计（每请求一份）。
    run_usage_acc: Usage,
    /// 后台挂载（TUI 先行启动，2026-08-19 用户反馈）：Some = 会话仍在
    /// 加载（LOGO 欢迎页 + loading 状态 + 禁输入），接收端交出挂载
    /// 完成的 TrustedProjectApplication。未受信路径无重活，不进入该态。
    loading: Option<mpsc::Receiver<Result<TrustedProjectApplication, String>>>,
    /// 快照测试确定性钩子：冻结动画帧号，同一输入序列永远同一画面。
    #[cfg(test)]
    test_freeze_tick: bool,
    /// 快照测试确定性钩子：阶段/run 计时固定值（Instant 不可移植构造）。
    #[cfg(test)]
    test_phase_elapsed: Option<Duration>,
    #[cfg(test)]
    test_run_elapsed: Option<Duration>,
    /// 提醒铃模式（构造期从环境变量解析一次；见 [`BellMode`]）。
    bell: BellMode,
}

impl App {
    /// 快照测试用可注入 storage root 的同步构造入口（生产路径是
    /// [`Self::open_deferred`]：TUI 先行、会话后台加载；测试需要构
    /// 造即就绪的 App）。
    #[cfg(test)]
    fn open(project: Project, storage_root: Option<PathBuf>) -> Result<Self, String> {
        let mut app = Self::open_minimal(project, storage_root)?;
        if !app.trust_prompt {
            app.initialize_project()?;
        }
        Ok(app)
    }

    /// 生产构造（2026-08-19 用户反馈：大会话启动等待一个黑窗口）：
    /// 最小阶段同步完成即返回，TUI 先行上屏；重活（挂载 + 大会话
    /// journal 回放）挪到后台线程，加载画面（LOGO 欢迎页 + loading
    /// 状态 + 禁输入）接管，完成后经 [`Self::poll_loading`] 交接。
    /// 未受信路径无重活（无会话可载），走同步确权流程。
    fn open_deferred(project: Project, storage_root: Option<PathBuf>) -> Result<Self, String> {
        let mut app = Self::open_minimal(project, storage_root)?;
        if app.trust_prompt {
            return Ok(app);
        }
        let bootstrap = app
            .bootstrap
            .take()
            .expect("trusted path holds the bootstrap scope");
        let (loaded, loading) = mpsc::channel();
        thread::spawn(move || {
            let _ = loaded.send(
                bootstrap
                    .with_permission_modes()
                    .into_trusted()
                    .map_err(|error| error.to_string()),
            );
        });
        app.loading = Some(loading);
        app.status = "loading conversation…".into();
        app.status_until = None;
        Ok(app)
    }

    /// 最小构造（两阶段构造 A-02 的第一阶段）：打开全局存储、查询信
    /// 任表；不挂载项目、不读会话、不启动 MCP。
    fn open_minimal(project: Project, storage_root: Option<PathBuf>) -> Result<Self, String> {
        let bootstrap = match storage_root {
            Some(root) => BootstrapApplication::open(project.clone(), root),
            None => BootstrapApplication::open_default(project.clone()),
        }
        .map_err(|error| error.to_string())?;
        let trusted = bootstrap.is_trusted().map_err(|error| error.to_string())?;
        let config = ModelConfig::default();
        let credentials = ProviderCredentials::for_protocol(config.protocol);

        // 状态栏初始显示当前打开的项目目录（home 缩写为 ~）。
        let status = abbreviate_home(project.root());
        let app = Self {
            project,
            bootstrap: Some(bootstrap),
            application: None,
            session_id: None,
            session_title: None,
            close_error: None,
            config,
            credentials,
            provider_descriptors: Vec::new(),
            conversation: crate::tui_conversation::ConversationModel::new(),
            card_visibility: ToolCardVisibility::default(),
            input: InputBuffer::new(Vec::new()),
            trust_prompt: !trusted,
            default_status: status.clone(),
            status,
            status_until: None,
            editor: None,
            picker: None,
            session_picker: None,
            running: false,
            steering_queued: 0,
            events: None,
            event_sender: None,
            pending_permission: None,
            pending_ask_user: None,
            permission_picker: None,
            rename_dialog: None,
            attachments: Vec::new(),
            run_handle: None,
            compact_handle: None,
            phases: PhaseTracker::default(),
            animation_epoch: Instant::now(),
            conversation_scroll_from_bottom: 0,
            input_area: Rect::default(),
            editor_area: None,
            conversation_area: Rect::default(),
            conversation_start: 0,
            conversation_rows: 0,
            selection: None,
            should_quit: false,
            info_dialog: None,
            info_scroll_max: 0,
            info_page: 1,
            mcp_view: None,
            balance: None,
            session_usage: Usage::default(),
            last_turn_usage: None,
            run_usage_base: None,
            run_usage_acc: Usage::default(),
            loading: None,
            #[cfg(test)]
            test_freeze_tick: false,
            #[cfg(test)]
            test_phase_elapsed: None,
            #[cfg(test)]
            test_run_elapsed: None,
            bell: bell_mode_from_env(
                env::var("CLAT_NO_BELL").ok(),
                env::var("CLAT_BELL_COMMAND").ok(),
            ),
        };
        Ok(app)
    }

    /// 项目级资源初始化：挂载 Trusted Project（已信任路径）并采纳
    /// 快照。同步构造（测试路径）使用；确权流程走
    /// `authorize_and_mount`（见 handle_key 的确权分支）。
    #[cfg(test)]
    fn initialize_project(&mut self) -> Result<(), String> {
        let bootstrap = self
            .bootstrap
            .take()
            .ok_or_else(|| "bootstrap scope is unavailable".to_owned())?;
        let application = match bootstrap.with_permission_modes().into_trusted() {
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

    /// 后台挂载交接（每帧轮询）：完成则订阅 application 事件流、采纳
    /// 快照、恢复常驻状态栏、解锁输入。失败与同步路径同款语义——
    /// 报错退出（用户视角与"启动失败"一致，不留在加载死屏）。
    fn poll_loading(&mut self) {
        let Some(loading) = self.loading.take() else {
            return;
        };
        let outcome = match loading.try_recv() {
            Ok(outcome) => outcome,
            Err(mpsc::TryRecvError::Empty) => {
                self.loading = Some(loading);
                return;
            }
            Err(mpsc::TryRecvError::Disconnected) => {
                // 线程未发消息即终止（panic）：按加载失败处理。
                Err("background mount thread terminated".into())
            }
        };
        match outcome {
            Ok(application) => {
                self.application = Some(application);
                self.wire_application_events();
                // 先复位常驻状态栏、再采纳快照（对抗审计 2026-08-19 修
                // 复：adopt_snapshot 里的 MCP 状态提示是 flash——复位放
                // 在后面会把它立即覆盖，用户永远看不到）。
                self.status = self.default_status.clone();
                self.status_until = None;
                if let Err(error) = self.adopt_snapshot() {
                    self.close_error = Some(format!("project initialization failed: {error}"));
                    self.should_quit = true;
                }
            }
            Err(error) => {
                self.close_error = Some(format!("project initialization failed: {error}"));
                self.should_quit = true;
            }
        }
    }

    /// 订阅 application 事件流（余额/压缩）并触发首次余额查询。启动
    /// 即挂载与后台交接两个路径共用——订阅晚于挂载不会丢余额事件
    ///（refresh_monitor 立即拉一次）。
    fn wire_application_events(&mut self) {
        let Some(application) = &self.application else {
            return;
        };
        let Some(ui) = self.event_sender.clone() else {
            return;
        };
        let (application_sender, application_events) = mpsc::channel();
        application.subscribe(application_sender);
        thread::spawn(move || {
            while let Ok(event) = application_events.recv() {
                if ui.send(UiEvent::Application(event)).is_err() {
                    break;
                }
            }
        });
        application.refresh_monitor();
    }

    /// 从已挂载的 application 读取项目快照并重置前端状态。
    fn adopt_snapshot(&mut self) -> Result<(), String> {
        let snapshot = match self.application.as_mut().map(|app| app.snapshot()) {
            Some(Ok(snapshot)) => snapshot,
            Some(Err(error)) => return Err(error.to_string()),
            None => return Err("project application is unavailable".into()),
        };
        self.session_id = snapshot.session_id;
        self.session_title = snapshot.session_title;
        // 转录一律从 journal 回放构造（G2/G8）：事件日志是唯一权威，
        // 前端不再维护独立的 TranscriptLine 派生视图。
        self.conversation =
            crate::tui_conversation::ConversationModel::from_replay(&snapshot.replay);
        self.input = InputBuffer::new(snapshot.input_history);
        self.config = snapshot.config;
        self.credentials = snapshot.credentials;
        self.provider_descriptors = snapshot.provider_descriptors;
        // journal 用量统计（DSH assistant/message.usage）：状态栏的
        // Cache/Context 启动即有值，不必等首次 run 上报。
        self.session_usage = snapshot.session_usage;
        self.last_turn_usage = snapshot.last_request_usage;
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

        // 启动即挂载（同步 open，测试路径）在这里订阅；后台交接路径
        // 在 poll_loading 完成时订阅——两者共用 wire_application_events。
        self.wire_application_events();

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
                    // 权限审阅、问答对话框与帮助弹窗期间每个导航键后都
                    // 要先绘制一帧（维持滚动/高亮的即时性）。
                    if self.pending_permission.is_none()
                        && self.pending_ask_user.is_none()
                        && self.info_dialog.is_none()
                    {
                        while let Ok(event) = events.try_recv() {
                            self.handle_ui_event(event);
                            if self.pending_permission.is_some()
                                || self.pending_ask_user.is_some()
                                || self.info_dialog.is_some()
                            {
                                break;
                            }
                        }
                    }
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            }
            self.expire_status();
            // 后台挂载完成则交接（每帧轮询；loading 态自身保证有唤醒
            // deadline，不会死等）。
            self.poll_loading();
            terminal.draw(|frame| self.draw(frame))?;
        }
        // 显式 shutdown：flush 会话与 checkpoint、join 全部 worker，
        // 消费 close 错误（Drop 只兜底，不算成功关闭）。
        if let Some(application) = self.application.take()
            && let Err(error) = application.close()
        {
            self.close_error = Some(format!("application close failed: {error}"));
        }
        Ok(())
    }

    fn take_close_error(&mut self) -> Option<String> {
        self.close_error.take()
    }

    /// 动画帧号：只由真实流逝时间决定（80ms/帧）。事件洪峰里的额外
    /// 重绘拿到同一帧号，重绘慢时帧号按真实时间推进——旋转/呼吸速度
    /// 与"画了多少次、每次画多重"彻底解耦。快照测试钩
    /// （`test_freeze_tick`）冻结为 0，保证同一输入序列永远同一画面。
    fn animation_tick(&self) -> u64 {
        #[cfg(test)]
        if self.test_freeze_tick {
            return 0;
        }
        animation_tick_for(self.animation_epoch.elapsed())
    }

    /// 阶段耗时；测试可注入固定值（见 `test_phase_elapsed`）。
    #[cfg(test)]
    fn phase_elapsed(&self) -> Option<Duration> {
        self.test_phase_elapsed
            .or_else(|| self.phases.phase_started.map(|since| since.elapsed()))
    }

    /// 响一声提醒铃（触发点与模式见 [`BellMode`]）。
    fn notify(&self) {
        ring_bell(&self.bell);
    }

    #[cfg(not(test))]
    fn phase_elapsed(&self) -> Option<Duration> {
        self.phases.phase_started.map(|since| since.elapsed())
    }

    /// run 总耗时；测试可注入固定值（见 `test_run_elapsed`）。
    #[cfg(test)]
    fn run_elapsed(&self) -> Option<Duration> {
        self.test_run_elapsed
            .or_else(|| self.phases.run_started.map(|since| since.elapsed()))
    }

    #[cfg(not(test))]
    fn run_elapsed(&self) -> Option<Duration> {
        self.phases.run_started.map(|since| since.elapsed())
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
            // N2：自动命名/改名落盘成功——右标题即时更新，无需重拉快照。
            UiEvent::Application(ApplicationEvent::TitleUpdated { title }) => {
                self.session_title = Some(title);
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
    fn next_repaint_deadline(&self) -> Option<Instant> {
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
            let current = self
                .application
                .as_ref()
                .map(|application| application.permission_mode())
                .unwrap_or_default();
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
        action: crate::tui_permission::PermissionPickerAction,
    ) {
        use crate::tui_permission::PermissionPickerAction;
        match action {
            PermissionPickerAction::Continue => {}
            PermissionPickerAction::Cancel => {
                self.permission_picker = None;
                self.flash_status("permission mode unchanged");
            }
            PermissionPickerAction::Apply(mode) => {
                self.permission_picker = None;
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
            if copy_to_clipboard(&text) {
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
    fn selection_text(&mut self) -> Option<String> {
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
        self.session_id = Some(session_id);
        self.session_title = snapshot.session_title;
        // 转录从回放重建（G2/G8）；输入历史随会话切换：恢复目标会话
        // 自己的历史（含内存中未持久化的导航状态一并重置）。
        self.conversation =
            crate::tui_conversation::ConversationModel::from_replay(&snapshot.replay);
        self.input = InputBuffer::new(snapshot.input_history);
        self.conversation_scroll_from_bottom = 0;
        // 用量指标归属会话（TUI-L04）：恢复目标会话的 journal 统计
        // （与挂载路径同源），Cache/Context 切换即有值。
        self.session_usage = snapshot.session_usage;
        self.last_turn_usage = snapshot.last_request_usage;
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
        let vendor = self.config.vendor();
        apply_thinking_level(&mut self.config.extra_body, vendor, next);
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
        // 附件只随对话消息走：slash 命令不携带、也不清空（留待下一
        // 条消息）；纯附件（空文本）允许提交。
        let is_command = value.starts_with('/');
        if value.is_empty() && (self.attachments.is_empty() || is_command) {
            return;
        }
        let attachments = if is_command {
            Vec::new()
        } else {
            std::mem::take(&mut self.attachments)
        };
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
                // 帮助改弹窗（2026-08-19）：原先是状态栏一行长文本，
                // 已长到与右侧遥测段重叠。
                self.info_dialog = Some(InfoDialog::new(InfoDialogKind::Help));
            }
            "/mcp" => {
                // MCP 状态弹窗：数据来自挂载期的 McpStatus（Application
                // DTO），弹窗内 `r` 重取。前端不接触会话/注册表本体。
                match self.application.as_ref() {
                    Some(application) => {
                        self.mcp_view = Some(application.mcp_status());
                        self.info_dialog = Some(InfoDialog::new(InfoDialogKind::Mcp));
                    }
                    None => self.flash_status("project application is unavailable"),
                }
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
                self.session_title = None;
                self.conversation = crate::tui_conversation::ConversationModel::new();
                self.conversation_scroll_from_bottom = 0;
                self.input = InputBuffer::new(Vec::new());
                // 用量指标归属会话（TUI-L04）：新会话从零累计。
                self.session_usage = Usage::default();
                self.last_turn_usage = None;
                self.run_usage_base = None;
                self.run_usage_acc = Usage::default();
                self.flash_status("new conversation");
            }
            // `/perm` 是主命令（短、好记）；`/permission` 保留为别名——
            // 与 /new|/clear、/quit|/exit 同款双臂惯例。
            "/perm" | "/permission" => {
                // 冷切换/降级入口（权限三档）。升权到 Full Access 有
                // 确认子态（P4）；运行中切档对下一次权限检查生效（P3）。
                match self.application.as_ref() {
                    Some(application) => {
                        self.permission_picker =
                            Some(crate::tui_permission::PermissionPicker::new(
                                application.permission_mode(),
                            ));
                    }
                    None => self.flash_status("project application is unavailable"),
                }
            }
            "/rename" => {
                // 门槛（2026-08-19 放宽）：有活动会话即可改，不再要求
                // LLM 已起名——原门槛把"首轮自动命名失败/早于命名功能
                // 的旧会话"永久挡在门外（用户实测几百轮的会话被拒），
                // 而 CAS 本就保证改名压制迟到的自动命名。空会话 flash。
                match self.session_id.as_ref() {
                    Some(_) => {
                        let prefill = self.session_title.clone().unwrap_or_default();
                        self.rename_dialog = Some(RenameDialog::new(&prefill));
                    }
                    None => self.flash_status("no active conversation to rename"),
                }
            }
            "/quit" | "/exit" => self.should_quit = true,
            command if command.starts_with('/') => {
                self.flash_status(format!("unknown command: {command}"));
            }
            prompt => self.start_run(prompt.to_owned(), attachments),
        }
    }

    /// 运行中提交 = 插话（DSH `steer()`）：消息入队，在下一次模型请求
    /// 边界并入；转录在 `SteeringApplied` 回流时才出现该消息，徽标计数
    /// 提示排队中。run 恰好收尾的竞争窗口（NotRunning）回退为普通提交。
    fn steer_input(&mut self) {
        let value = self.input.take();
        let value = value.trim().to_owned();
        if value.is_empty() {
            return;
        }
        if value.starts_with('/') {
            // slash 命令只作用于空闲态；退还输入，避免用户丢字。
            self.input.insert_str(&value);
            self.flash_status("slash commands run when idle — steering sends plain text");
            return;
        }
        self.input.remember(value.clone());
        let outcome = self
            .application
            .as_ref()
            .map(|application| application.steer(value.clone()));
        match outcome {
            Some(SteerOutcome::Queued) => {
                self.steering_queued += 1;
                self.flash_status("steering queued — applies at the next model step");
            }
            // run 恰好收尾的竞争窗口回退为普通提交——steering 不携带
            // 附件（M6：附件只能随空闲态的新消息走）。
            _ => self.start_run(value, Vec::new()),
        }
    }

    fn start_run(&mut self, prompt: String, attachments: Vec<std::path::PathBuf>) {
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
            attachments,
            asker: Some(Arc::new(ChannelUserAsker::new(sender.clone()))),
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
        self.conversation.push_user(prompt);
        self.conversation_scroll_from_bottom = 0;
        self.run_handle = Some(handle);
        self.running = true;
        // 实时用量基线：流式 Usage 在其上累加，结束以 RunOutput 权威替换。
        self.run_usage_base = Some(self.session_usage.clone());
        self.run_usage_acc = Usage::default();
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
                self.phases.finish();
                let escalations = self
                    .application
                    .as_ref()
                    .map(|application| {
                        escalation_targets(application.permission_mode(), request.effect)
                    })
                    .unwrap_or_default();
                self.pending_permission = Some(PendingPermission {
                    request,
                    decision_tx,
                    argument_scroll: 0,
                    argument_page_size: 1,
                    argument_line_count: 0,
                    reviewed_through: 0,
                    reviewed_to_end: false,
                    escalations,
                });
                // AFK 提醒：run 正卡在等你批准——不响铃它可能静默卡到
                // 天荒地老。
                self.notify();
                self.flash_status("permission required — review arguments, then allow or deny");
            }
            WorkerMessage::AskUserRequest {
                question,
                answer_tx,
            } => {
                self.phases.finish();
                // 无选项时直接进入自定义输入模式（无可选内容）。
                let custom = question.options.is_empty().then(String::new);
                self.pending_ask_user = Some(PendingAskUser {
                    question,
                    answer_tx,
                    selection: 0,
                    custom,
                });
                // AFK 提醒：同权限框——run 在等你的回答。
                self.notify();
                self.flash_status("the model asks a question — answer or Esc to decline");
            }
            WorkerMessage::Done(result) => {
                self.finish_run(result);
            }
        }
    }

    fn handle_run_event(&mut self, event: RunEvent) {
        // 转录装配的唯一 live 入口（G8）：状态行闪烁等呈现逻辑之外，
        // 事件先喂会话模型。
        self.conversation.apply_run_event(&event);
        // 派生阶段（G6）：新模型步重开 Waiting；步内按事件推进、只进
        // 不退；未知事件落 `_ => {}` 保持现状、永不 panic。
        match &event {
            RunEvent::ModelRequested { .. } => self.phases.model_requested(),
            RunEvent::ModelStream {
                event: ModelEvent::ReasoningDelta { .. } | ModelEvent::ReasoningSummaryDelta { .. },
                ..
            } => self.phases.advance(Phase::Thinking),
            RunEvent::ModelStream {
                event: ModelEvent::TextDelta { .. } | ModelEvent::RefusalDelta { .. },
                ..
            } => self.phases.advance(Phase::Responding),
            RunEvent::ToolRequested { .. } => self.phases.advance(Phase::ExecutingTools),
            _ => {}
        }
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
                event: ModelEvent::TextDelta { .. },
            }
            | RunEvent::ModelStream {
                turn,
                event: ModelEvent::RefusalDelta { .. },
            } => {
                // 流式追加与贴底滚动由模型负责；这里只管状态行。
                self.conversation_scroll_from_bottom = 0;
                self.flash_status(format!("answering · turn {turn}"));
            }
            RunEvent::ModelStream {
                event: ModelEvent::ReasoningDelta { .. },
                ..
            } => {}
            // 流式 usage（DeepSeek 经 stream_options.include_usage，GLM
            // 默认携带）只取最近一次：input+output 近似当前上下文水位，
            // 供状态栏 Context 段使用。多轮 run 每轮覆盖前一轮。同时
            // 在 run 基线上实时累计会话用量——Cache 段首跑中途即有值。
            RunEvent::ModelStream {
                event: ModelEvent::Usage(usage),
                ..
            } => {
                self.last_turn_usage = Some(usage.clone());
                self.run_usage_acc.add_assign(&usage);
                if let Some(base) = self.run_usage_base.clone() {
                    let mut live = base;
                    live.add_assign(&self.run_usage_acc);
                    self.session_usage = live;
                }
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
            RunEvent::SteeringApplied { .. } => {
                // 转录用户块由会话模型负责（apply_run_event 已推入）；
                // 这里只回收排队徽标。
                self.steering_queued = self.steering_queued.saturating_sub(1);
            }
            _ => {}
        }
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
            // 会话右标题即时刷新：首条消息的 fallback 标题不产生事件
            //（投影派生），run 结束是它出现的第一个时机。
            self.session_title = application.session_title();
        }
        self.phases.finish();
        // run 刚消耗了额度：触发监控线程立即重新查询一次（计划外，
        // 不影响 5 分钟巡查周期）。
        self.refresh_balance_now();
        match result {
            Ok(done) => {
                // 会话用量以 run 结果权威覆盖：RunOutput.usage 是全 run
                // 总量，替换"基线 + 流式累计"的实时近似（不重复计）。
                match self.run_usage_base.take() {
                    Some(base) => {
                        self.session_usage = base;
                        self.session_usage.add_assign(&done.usage);
                    }
                    None => self.session_usage.add_assign(&done.usage),
                }
                // 非流式 provider 兜底：本轮无任何 delta 时以最终输出
                // 回填 assistant（与 journal 的 settled 文本对拍一致）。
                self.conversation.settle_streamed_output(&done.output);
                // 终态通知进转录（G7）：与回放 TurnEnded 同源文本。
                if done.cancelled {
                    self.conversation.push_turn_end("cancelled".into());
                    self.flash_status(format!("cancelled · {} model turns", done.turns));
                } else {
                    self.conversation.push_turn_end("completed".into());
                    self.flash_status(format!("completed · {} model turns", done.turns));
                    // AFK 提醒：对话结束响铃。用户主动取消不响——人就在
                    // 键盘前（Esc 是他按的）。
                    self.notify();
                }
            }
            Err(failure) => {
                match self.run_usage_base.take() {
                    Some(base) => {
                        self.session_usage = base;
                        self.session_usage.add_assign(&failure.usage);
                    }
                    None => self.session_usage.add_assign(&failure.usage),
                }
                self.conversation
                    .push_turn_end(format!("error: {}", failure.error));
                self.flash_status(format!(
                    "run failed after {} model turns: {}",
                    failure.turns, failure.error
                ));
                // 失败也是"对话结束"——AFK 下同样需要知道。
                self.notify();
            }
        }
        if self.steering_queued > 0 {
            // 未经 claim 的插话不落盘（S4）；显式告知而不是静默吞掉。
            self.flash_status(format!(
                "{} steering discarded — run ended before it applied",
                self.steering_queued
            ));
            self.steering_queued = 0;
        }
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
        let segments = status_suffix_segments(
            &self.config,
            &self.balance,
            &self.session_usage,
            self.last_turn_usage.as_ref(),
        );
        let budget = (bar.width.saturating_sub(MIN_STATUS_LEFT + 2)) as usize;
        let suffix = fit_status_suffix(&segments, budget);
        let status_line = if let Some(phase) = self.phases.phase {
            phase_line(
                tick,
                phase,
                self.phase_elapsed(),
                self.run_elapsed(),
                self.steering_queued,
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
            let height = (picker.row_count() as u16 + 4).min(popup_height_cap(area));
            // 84%：与其余弹窗统一（弹窗规范 2026-08-19）。94% 在宽终端
            // 上每边仅留 3%，视觉上与贴墙无异（用户实测报告撞墙）。
            let picker_area = centered_rect(84, height.max(8), area);
            self.editor_area = Some(picker_area);
            picker.draw(frame, picker_area);
        } else if let Some(editor) = &self.editor {
            let height = (editor.row_count() as u16 + 4).min(popup_height_cap(area));
            // 84%：同上——选择器与编辑器是仅有的两个 94% 弹窗，统一后
            // 全部弹窗同宽族、同最小边距（POPUP_H_MARGIN 钳制兜底）。
            let editor_area = centered_rect(84, height.max(8), area);
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
    fn refresh_mcp_view(&mut self) {
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
    fn draw_help_dialog(&mut self, frame: &mut Frame) {
        let area = frame.area();
        let inner_width = popup_inner_width(84, area);
        let lines = help_dialog_lines(inner_width);
        let dialog = centered_rect(84, content_dialog_height(lines.len(), area), area);
        // 可视行数：内框（去边框）减脚注一行。
        let visible = (dialog.height.saturating_sub(2 + 1)) as usize;
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
        body.push(Line::from(Span::styled(
            footer.trim(),
            tui_theme::style(tui_theme::Role::Faint),
        )));
        clear_popup_with_guards(frame, dialog);
        frame.render_widget(Paragraph::new(body).block(popup_block(" /help ")), dialog);
    }

    /// /mcp 状态弹窗：连接概览 + 每服务器一行（名称 · 传输 · 协议 ·
    /// 版本 · 工具数）+ 失败条目（含 stderr 尾部，按内宽折行——它们是
    /// 用户来排查的正文）。骨架与 /help 相同：内容驱动高度、超预算滚
    /// 动、脚注键位（多一个 `r` 刷新）。数据是打开/刷新时缓存的
    /// `McpStatusDto`，弹窗自身不触碰会话与注册表。
    fn draw_mcp_dialog(&mut self, frame: &mut Frame) {
        let area = frame.area();
        let inner_width = popup_inner_width(84, area);
        let view = self.mcp_view.clone().unwrap_or_default();
        let lines = mcp_dialog_lines(&view, inner_width);
        let dialog = centered_rect(84, content_dialog_height(lines.len(), area), area);
        let visible = (dialog.height.saturating_sub(2 + 1)) as usize;
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
        body.push(Line::from(Span::styled(
            footer.trim(),
            tui_theme::style(tui_theme::Role::Faint),
        )));
        clear_popup_with_guards(frame, dialog);
        frame.render_widget(Paragraph::new(body).block(popup_block(" /mcp ")), dialog);
    }

    /// ask-user 对话框：问题原文（按实际宽度换行）+ 选项列表（选择行
    /// 高亮，描述 dim）+ 自定义行 / 输入回显 + 键位脚注。窄屏降级为
    /// 问题 + 脚注（选项照常可按 ↑↓ 选中）。
    fn draw_ask_dialog(&mut self, frame: &mut Frame) {
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
                Span::styled("❯ ", tui_theme::style(tui_theme::Role::UserMarker)),
                Span::raw(text.clone()),
                Span::styled("_", tui_theme::style(tui_theme::Role::Faint)),
            ]));
        } else {
            for (index, option) in pending.question.options.iter().enumerate() {
                let selected = index == pending.selection;
                let marker = if selected { "● " } else { "○ " };
                let mut spans = vec![Span::styled(
                    marker,
                    tui_theme::style(if selected {
                        tui_theme::Role::Selected
                    } else {
                        tui_theme::Role::Faint
                    }),
                )];
                spans.push(if selected {
                    Span::styled(
                        option.label.clone(),
                        tui_theme::style(tui_theme::Role::Selected),
                    )
                } else {
                    Span::raw(option.label.clone())
                });
                lines.push(Line::from(spans));
                if let Some(description) = &option.description {
                    for wrapped in wrap_text(description, inner_width.saturating_sub(2)) {
                        lines.push(Line::from(Span::styled(
                            format!("   {wrapped}"),
                            tui_theme::style(tui_theme::Role::Faint),
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
                        tui_theme::style(if selected {
                            tui_theme::Role::Selected
                        } else {
                            tui_theme::Role::Faint
                        }),
                    ),
                    Span::styled(
                        "type a custom answer…",
                        tui_theme::style(tui_theme::Role::Italic),
                    ),
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
            tui_theme::style(tui_theme::Role::Faint),
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
        frame.render_widget(
            Paragraph::new(spans).block(popup_block(" Question ")),
            dialog,
        );
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
                tui_theme::style(tui_theme::Role::Bold),
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
                    tui_theme::style(tui_theme::Role::Bold),
                )),
                Line::from("Terminal is too small to review arguments."),
                Line::from("Maximize to continue · Esc / n — deny"),
            ];
            let height = (compact.len() as u16 + 2).min(max_dialog_height);
            let dialog = centered_rect(84, height, area);
            clear_popup_with_guards(frame, dialog);
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
            tui_theme::style(tui_theme::Role::Bold),
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
                tui_theme::style(tui_theme::Role::Bold),
            )));
        }

        let height = (lines.len() as u16 + 2).min(max_dialog_height);
        let dialog = centered_rect(84, height.max(10), area);
        clear_popup_with_guards(frame, dialog);
        frame.render_widget(
            Paragraph::new(lines).block(popup_block(" Permission ")),
            dialog,
        );
    }

    /// /rename 弹框：预填的 InputBuffer + 真实光标（与主输入框同一套
    /// 换行/光标算法，坐标天然一致）。行由 `visual_rows` 预折——不用
    /// Paragraph 的 wrap，保证光标列与显示列可换算。
    fn draw_rename_dialog(&self, frame: &mut Frame) {
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
            tui_theme::style(tui_theme::Role::Faint),
        )));
        let height = (lines.len() as u16 + 2).min(popup_height_cap(area));
        let dialog_area = centered_rect(72, height.max(6), area);
        clear_popup_with_guards(frame, dialog_area);
        frame.render_widget(
            Paragraph::new(lines).block(popup_block(" /rename ")),
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
        let state = if self.loading.is_some() {
            "loading"
        } else if self.running {
            "running"
        } else {
            "ready"
        };
        // 首行内容预算：总宽减边框 2 列、水平内边距 2 列与 "CLAT " 前缀
        // 5 列；宽度不足时逐级退化（TUI-L02），档位优先于模型名保留。
        let rest_budget = area.width.saturating_sub(2 + 2 + 5) as usize;
        let rest =
            compose_header_rest(env!("CARGO_PKG_VERSION"), state, &model, level, rest_budget);
        frame.render_widget(
            Paragraph::new(vec![
                Line::from(vec![
                    Span::styled("CLAT", tui_theme::style(tui_theme::Role::Bold)),
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
        // 会话右标题（用户指定布局）：左上角 Conversation、右上角对称
        // 放当前会话名（effective：LLM/用户标题，否则首条消息派生）。
        // 超宽截断保头（标题语义在头部），留出左标题与边框的余量。
        let mut block = Block::default().title("Conversation").borders(Borders::ALL);
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
                    shown,
                    tui_theme::style(tui_theme::Role::Faint),
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
                .style(tui_theme::style(tui_theme::Role::ScrollTrack))
                .thumb_style(tui_theme::style(tui_theme::Role::ScrollThumb)),
            block.inner(area),
            &mut scrollbar_state,
        );
    }

    /// 内容总行数（含分隔空行）；模型内建逐 item 渲染缓存（G3）。
    fn conversation_total_lines(&mut self, width: usize) -> usize {
        self.conversation.ensure_rendered(width);
        self.conversation.total_lines(self.card_visibility)
    }

    fn draw_input(&self, frame: &mut Frame, area: Rect) {
        // 标题只有两态：空闲 Message / 运行插话提示。loading 不进输入框
        // 标题——头部状态与底部状态栏已在报 loading，第三处是画蛇添足
        // （2026-08-19 用户反馈；输入禁用本身由 loading 门保证）。
        let title = if self.running {
            "Running — Enter steers · Esc cancels"
        } else {
            "Message"
        };
        // 权限档位名放右上角（用户指定布局，与左上角 Message 对称；
        // DSH composer Access 徽标对应物）。Full Access 用警示黄——
        // 它是"不再有任何弹窗"的档位，颜色是唯一的风险暗示。Application
        // 缺席（未确权）时无档位可示。直接读 cell（单一数据源，无前端
        // 镜像）。
        let mut block = Block::default().title(title).borders(Borders::ALL);
        if let Some(mode) = self
            .application
            .as_ref()
            .map(|application| application.permission_mode())
        {
            let style = if mode == PermissionMode::FullAccess {
                tui_theme::style(tui_theme::Role::Warning)
            } else {
                tui_theme::style(tui_theme::Role::Faint)
            };
            block = block.title(Line::from(mode.to_string()).style(style).right_aligned());
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
                Line::from(Span::styled(
                    chips,
                    tui_theme::style(tui_theme::Role::Faint),
                )),
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
/// 工具参数的结构化呈现（edit_file 迷你 diff / write_file 全文 /
/// run_command `$ cmd`）。权限对话框审阅与转录工具卡共用同一渲染器
/// （phase-1 P1-4：预览即卡片正文）。
pub(crate) fn tool_argument_lines(
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
                    tui_theme::style(tui_theme::Role::Bold),
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
                tui_theme::style(tui_theme::Role::Dim),
            )));
            push_wrapped(&mut lines, "-", old_str);
            lines.push(Line::from(Span::styled(
                "+ new_str:",
                tui_theme::style(tui_theme::Role::Dim),
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
                tui_theme::style(tui_theme::Role::Dim),
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

pub(crate) fn wrap_text(text: &str, width: usize) -> Vec<String> {
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

/// 弹窗上下不贴屏幕上下沿的最少行数（与 `POPUP_H_MARGIN` 对称的垂直
/// 边距；弹窗规范 2026-08-19：四边间距）。终端高度放不下时不硬挤
/// 没——按旧钳制退化（见 `centered_rect`）。
pub(crate) const POPUP_V_MARGIN: u16 = 2;

/// 垂直边距钳制生效所需的最低弹窗高度：更矮的终端保留旧行为。
const MIN_POPUP_HEIGHT: u16 = 6;

/// 钳制生效所需的最低对话框宽度：更窄的终端连"边距 + 可用宽度"
/// 都放不下，保留百分比行为，不把对话框挤没。
const MIN_POPUP_WIDTH: u16 = 16;

/// 弹出窗内容的水平内边距（列）。文字与边框字符之间留空，不贴框；
/// 手工换行/截断的宽度计算必须同步扣除 `2 × POPUP_TEXT_PADDING`。
pub(crate) const POPUP_TEXT_PADDING: u16 = 1;

/// 弹出窗统一的边框块：全边框 + 标题 + 1 列水平内边距 + Warning 黄
/// 边框/标题（弹窗规范 2026-08-19：所有弹窗同一样式——黄边框、背景
/// 压暗、四边间距；黄 = 需要注意/决策的模态语义，与主题 Role::Warning
/// 一致）。标题原样使用，调用方自带前后空格（如 `" Permission "`）。
pub(crate) fn popup_block(title: &str) -> Block<'_> {
    let warning = tui_theme::style(tui_theme::Role::Warning);
    Block::default()
        .borders(Borders::ALL)
        .title(title)
        .title_style(warning.add_modifier(Modifier::BOLD))
        .border_style(warning)
        .padding(Padding::horizontal(POPUP_TEXT_PADDING))
}

/// 弹窗清屏垫边（wide-glyph guard）：Clear 范围左右各扩一列（钳制在
/// 终端区域内）。跨在弹窗左边框起点上的宽字符（CJK/emoji 占 2 列，
/// 起点格在 Clear 范围之外）会让 ratatui diff 的 `to_skip` 吞掉边框
/// 列的更新——上一帧的字形铺进边框列，本帧 │ 不再补发，左边线被吃
/// 掉（用户实测：仅左边线受损，弹窗内部不受影响；右侧因起点格在
/// Clear 范围内天然安全，扩列是对称保险）。起点格被一并清掉后，
/// diff 正常发出边框更新。
pub(crate) fn clear_popup_with_guards(frame: &mut Frame, rect: Rect) {
    let area = frame.area();
    let left = rect.x.saturating_sub(1).max(area.x);
    let right = rect.right().saturating_add(1).min(area.right());
    let top = rect.y.saturating_sub(1).max(area.y);
    let bottom = rect.bottom().saturating_add(1).min(area.bottom());
    if right <= left || bottom <= top {
        frame.render_widget(Clear, rect);
        return;
    }
    frame.render_widget(
        Clear,
        Rect {
            x: left,
            width: right - left,
            y: top,
            height: bottom - top,
        },
    );
}

/// 空会话欢迎页：LOGO + 版本行 + 起步提示，双向居中于会话区内框。
/// 窄到放不下 LOGO 的终端退化为单行提示（ASCII 字形无法有意义地缩放）。
fn draw_welcome(frame: &mut Frame, inner: Rect) {
    if inner.width == 0 || inner.height == 0 {
        return;
    }
    let welcome = crate::tui_logo::welcome_lines();
    let content_width = welcome.iter().map(Line::width).max().unwrap_or(0) as u16;
    if inner.width < content_width {
        let hint = Line::from(Span::styled(
            format!(
                "clat v{} · type a message to begin",
                env!("CARGO_PKG_VERSION")
            ),
            tui_theme::style(tui_theme::Role::Dim),
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

/// 弹窗在给定终端内的最大可用高度（垂直边距感知）：放得下边距时为
/// `area.height - 2×POPUP_V_MARGIN`，终端过矮时退化为整屏高。这是
/// 弹窗高度的唯一预算来源——`centered_rect` 的钳制与所有弹窗内容方
/// 的分页/行数预算必须共用同一函数，否则预算与实际渲染高度错位
/// （真实回归：权限弹窗分页按 `area.height - 2` 旧预算计算，而
/// centered_rect 钳到 `area.height - 4`——End 翻到底时页底两行渲染
/// 在框外，永远看不到最后两行）。
pub(crate) fn popup_height_cap(area: Rect) -> u16 {
    let bounded = area.height.saturating_sub(2 * POPUP_V_MARGIN);
    if bounded >= MIN_POPUP_HEIGHT {
        bounded.min(area.height)
    } else {
        area.height
    }
}

/// 弹窗水平切分的实际宽度（列）：与 [`centered_rect`] 同一 Layout 与
/// `POPUP_H_MARGIN` 钳制，供"行数依赖内宽、而矩形高度又依赖行数"的
/// 内容驱动弹窗先行取宽（一致性由
/// `popup_width_matches_centered_rect` 锁定，不共代码路径是为了不动
/// 既有渲染的百分比取整行为）。
pub(crate) fn popup_width(percent_x: u16, area: Rect) -> u16 {
    let horizontal = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - percent_x) / 2),
            Constraint::Percentage(percent_x),
            Constraint::Percentage((100 - percent_x) / 2),
        ])
        .split(area);
    let mut width = horizontal[1].width;
    let bounded = area.width.saturating_sub(2 * POPUP_H_MARGIN);
    if width > bounded && bounded >= MIN_POPUP_WIDTH {
        width = bounded;
    }
    width
}

/// 内容驱动弹窗的内宽（列）：弹窗宽减边框 2 列与内边距
/// 2×POPUP_TEXT_PADDING。内容折行宽度与实际渲染矩形必须同源，否则
/// 预算行数与真实可放行数错位（权限弹窗分页曾因此翻不到底）。
pub(crate) fn popup_inner_width(percent_x: u16, area: Rect) -> usize {
    popup_width(percent_x, area).saturating_sub(2 + 2 * POPUP_TEXT_PADDING) as usize
}

/// 内容驱动弹窗高度：内容行数 + 边框 2 行 + 脚注 1 行，钳在
/// [`popup_height_cap`] 预算内。短内容得到小框（上下留出真实边距），
/// 长内容恰好贴满预算继续滚动（2026-08-19 第三轮反馈：/help 恒取满额
/// 高度，内容再少也是整屏框、边距形同虚设）。
pub(crate) fn content_dialog_height(content_lines: usize, area: Rect) -> u16 {
    (content_lines as u16)
        .saturating_add(3)
        .min(popup_height_cap(area))
}

/// /help 弹窗内容：命令与键位两节，逐条 `命令 — 说明`，按弹窗内宽
/// 折行（wrap_text）。节标题 Bold，条目默认色。
fn help_dialog_lines(width: usize) -> Vec<Line<'static>> {
    let sections: &[(&str, &[(&str, &str)])] = &[
        (
            "Commands",
            &[
                ("/model", "configure the active model/provider"),
                ("/new, /clear", "start a new conversation"),
                ("/compact", "summarize earlier turns into a compact context"),
                ("/resume", "pick a previous conversation to continue"),
                ("/mcp", "inspect MCP servers, tools, and failures"),
                (
                    "/perm",
                    "switch the permission mode (Read Only / Project Write / Full Access)",
                ),
                ("/rename", "rename the current conversation"),
                ("/help", "this help"),
                ("/quit", "exit"),
            ],
        ),
        (
            "Keys",
            &[
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
            ],
        ),
    ];
    let mut lines = Vec::new();
    for (title, entries) in sections {
        if !lines.is_empty() {
            lines.push(Line::from(""));
        }
        lines.push(Line::from(Span::styled(
            *title,
            tui_theme::style(tui_theme::Role::Bold),
        )));
        for (name, description) in *entries {
            for wrapped in wrap_text(&format!("  {name} — {description}"), width) {
                lines.push(Line::from(wrapped));
            }
        }
    }
    lines
}

/// /mcp 弹窗内容行。结构：概览行（`connected/configured`）→ 空行 →
/// 每服务器一行 `● name  transport · protocol · v版本 · N tools`
///（名称默认色，其余 dim；这些字段短，保持单行不折）→ 空行 →
/// `Failures` 节（失败消息按内宽折行，dim——含挂载失败时的 stderr
/// 尾部，是排查的主要正文）。
fn mcp_dialog_lines(view: &McpStatusDto, width: usize) -> Vec<Line<'static>> {
    let mut lines = vec![Line::from(vec![Span::styled(
        format!(
            "MCP servers: {}/{} connected",
            view.connected, view.configured
        ),
        tui_theme::style(tui_theme::Role::Bold),
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
                    tui_theme::style(tui_theme::Role::Dim),
                ),
            ]));
        }
    }
    if !view.failures.is_empty() {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            "Failures",
            tui_theme::style(tui_theme::Role::Bold),
        )));
        for failure in &view.failures {
            for wrapped in wrap_text(&format!("  {failure}"), width) {
                lines.push(Line::from(Span::styled(
                    wrapped,
                    tui_theme::style(tui_theme::Role::Dim),
                )));
            }
        }
    }
    lines
}

pub(crate) fn centered_rect(percent_x: u16, height: u16, area: Rect) -> Rect {
    let height = height.min(popup_height_cap(area));
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
            tui_theme::style(tui_theme::Role::Bold),
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
        tui_theme::style(tui_theme::Role::Bold),
    )));

    let height = (lines.len() as u16 + 2).min(popup_height_cap(area));
    let dialog = centered_rect(84, height.max(10), area);
    clear_popup_with_guards(frame, dialog);
    frame.render_widget(Paragraph::new(lines).block(popup_block(" Trust ")), dialog);
}

#[cfg(test)]
mod snapshot_tests;

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui::style::Color;

    /// M6：粘贴的图片附件判定。防误判优先：整条 == 存在的图片路径才
    /// 附加；混合文本、不存在、非图片扩展名、超 4MB 一律当文本。
    #[test]
    fn pasted_image_path_only_matches_whole_existing_image_paths() {
        let dir = std::env::temp_dir().join(format!(
            "clat-paste-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let image = dir.join("shot.png");
        std::fs::write(&image, b"png").unwrap();

        // 整条路径：命中。
        assert_eq!(
            pasted_image_path(&image.display().to_string()),
            Some(image.clone())
        );
        // 前后空白：trim 后仍命中（终端粘贴常带尾换行）。
        assert_eq!(
            pasted_image_path(&format!("  {}\n", image.display())),
            Some(image.clone())
        );
        // 混合文本：不误判（宁可当文本插入）。
        assert_eq!(
            pasted_image_path(&format!("look at {} please", image.display())),
            None
        );
        // 相对路径：永远不当附件——前缀守卫在 fs 访问之前就拒绝，
        // 与文件是否存在、cwd 在哪都无关（2026-08-19 收紧，pre-fix
        // 上 cwd 下碰巧同名的裸文件名会被误判成附件）。
        assert_eq!(pasted_image_path("logo.png"), None);
        assert_eq!(pasted_image_path("docs/diagram.png"), None);

        // 不存在 / 非图片扩展名：None。
        assert_eq!(
            pasted_image_path(&dir.join("missing.png").display().to_string()),
            None
        );
        let text = dir.join("notes.txt");
        std::fs::write(&text, b"txt").unwrap();
        assert_eq!(pasted_image_path(&text.display().to_string()), None);

        // 超过 4MB：None（入口封顶）。
        let big = dir.join("big.png");
        let sparse = std::fs::File::create(&big).unwrap();
        sparse
            .set_len(crate::media::MAX_ATTACHMENT_BYTES + 1)
            .unwrap();
        drop(sparse);
        assert_eq!(pasted_image_path(&big.display().to_string()), None);

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 提醒铃模式解析（规格推导）：默认终端铃；CLAT_NO_BELL 压过一切；
    /// CLAT_BELL_COMMAND 非空即自定义命令；NO_BELL 的真值形态宽限
    /// （1/true/yes/on，大小写与空白不敏感由 trim+小写集合决定——这里
    /// 只锁小写形态，多余宽容不加）。
    #[test]
    fn bell_mode_resolves_from_env() {
        assert_eq!(bell_mode_from_env(None, None), BellMode::Terminal);
        assert_eq!(
            bell_mode_from_env(Some("1".into()), None),
            BellMode::Off,
            "the silencer wins over everything"
        );
        assert_eq!(
            bell_mode_from_env(Some("1".into()), Some("afplay x".into())),
            BellMode::Off
        );
        assert_eq!(
            bell_mode_from_env(Some("0".into()), None),
            BellMode::Terminal
        );
        assert_eq!(
            bell_mode_from_env(None, Some("afplay ~/ding.aiff".into())),
            BellMode::Command("afplay ~/ding.aiff".into())
        );
        // 空白命令视为未设置，回落终端铃。
        assert_eq!(
            bell_mode_from_env(None, Some("   ".into())),
            BellMode::Terminal
        );
    }

    /// 自定义命令模式端到端：命令真的被执行（marker 文件出现），且
    /// ring_bell 立即返回（detached，不等命令结束）。
    #[test]
    fn bell_command_mode_runs_the_command_detached() {
        let marker = std::env::temp_dir().join(format!(
            "clat-bell-{}.marker",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let command = format!("printf ok > {:?}", marker);
        let started = std::time::Instant::now();
        ring_bell(&BellMode::Command(command));
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "ring_bell returns without waiting for the command"
        );
        let mut appeared = false;
        for _ in 0..200 {
            if marker.exists() {
                appeared = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(appeared, "the custom bell command actually ran");
        let _ = std::fs::remove_file(&marker);
    }

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
        let lines = tool_argument_lines(
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
        let lines = tool_argument_lines(
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
        let lines = tool_argument_lines(
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
        let lines = tool_argument_lines(
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

    /// G6：阶段步内只前进不回退；未知事件由调用侧 `_ => {}` 分支天然
    /// 保持现状；run 终态清空全部计时状态，不留活计时器。
    #[test]
    fn phases_only_advance_and_finish_clears() {
        let mut phases = PhaseTracker::default();
        assert_eq!(phases.phase, None);
        // 未开始 run 时的流事件不创建阶段（防御乱序）。
        phases.advance(Phase::Thinking);
        assert_eq!(phases.phase, None);
        phases.model_requested();
        assert_eq!(phases.phase, Some(Phase::WaitingFirstToken));
        phases.advance(Phase::Thinking);
        assert_eq!(phases.phase, Some(Phase::Thinking));
        // 回退拒绝：thinking 之后的 text 已到 Responding，再来 reasoning
        // 不回退。
        phases.advance(Phase::Responding);
        phases.advance(Phase::Thinking);
        assert_eq!(phases.phase, Some(Phase::Responding));
        phases.advance(Phase::ExecutingTools);
        assert_eq!(phases.phase, Some(Phase::ExecutingTools));
        // 新模型步重开 Waiting，run 钟不重置。
        let run_started = phases.run_started;
        phases.model_requested();
        assert_eq!(phases.phase, Some(Phase::WaitingFirstToken));
        assert_eq!(phases.run_started, run_started);
        phases.finish();
        assert_eq!(phases.phase, None);
        assert_eq!(phases.phase_started, None);
        assert_eq!(phases.run_started, None);
    }

    #[test]
    fn phase_line_searchlight_sweeps_at_a_constant_per_character_dwell() {
        let at_start = phase_line(0, Phase::WaitingFirstToken, None, None, 0);
        // The spinner frame itself rotates and keeps the brand blue.
        assert_eq!(at_start.spans[0].content, SPINNER_FRAMES[0]);
        assert_eq!(
            at_start.spans[0].style.fg,
            Some(tui_theme::BRAND_SHIMMER_LOW)
        );
        // 标签是逐字 span（探照灯按字符打亮），拼接还原原文。
        let joined: String = at_start.spans[2..2 + "Waiting first token".chars().count()]
            .iter()
            .map(|span| span.content.as_ref())
            .collect();
        assert_eq!(joined, "Waiting first token");

        // 不变量（2026-08-19 第四轮设计 + 第五轮提速）：单字符驻留恒定。
        // 光带每 SWEEP_STEP_TICKS（1 tick）前进恰一个字符；整圈 =
        // (字数 + 双侧余量) 步回到同态，长标签整圈长是设计使然。
        // pre-fix 两版各自必红：整圈周期恒定版（v0.6.1）每字速度随字数
        // 变；整词呼吸版没有任何字符差异。
        let styles = |tick: u64, phase: Phase| -> Vec<Option<Color>> {
            phase_line(tick, phase, None, None, 0)
                .spans
                .into_iter()
                .skip(2)
                .map(|span| span.style.fg)
                .collect()
        };
        let short_len = Phase::Thinking.label().chars().count() as u64;
        let long_len = Phase::WaitingFirstToken.label().chars().count() as u64;
        let full_circle = |len: u64| SWEEP_STEP_TICKS * (len + 2 * SWEEP_MARGIN_CHARS);

        // 光带每个 tick 都在移动（驻留 1 tick，提速一倍）。
        assert_ne!(
            styles(0, Phase::Thinking),
            styles(SWEEP_STEP_TICKS, Phase::Thinking),
            "the beam advances one character per dwell"
        );
        // 整圈回到同态（短/长标签各自的圈长，互不相同——圈随字数伸缩）。
        assert_eq!(
            styles(0, Phase::Thinking),
            styles(full_circle(short_len), Phase::Thinking),
            "short label completes its sweep in (len + margins) steps"
        );
        assert_eq!(
            styles(0, Phase::WaitingFirstToken),
            styles(full_circle(long_len), Phase::WaitingFirstToken),
            "long label completes its own sweep at its own length"
        );
        assert_ne!(full_circle(short_len), full_circle(long_len));

        // 转圈独立节律（第五轮）：探照灯每 tick 一字符，转圈每
        // SPINNER_STEP_TICKS（2）tick 一帧——即每 2 个字符换一帧，
        // 转速不随探照灯提速或标签长度变化（8 帧一圈 = 16 字符）。
        assert_eq!(spinner_frame(0), spinner_frame(1));
        assert_ne!(spinner_frame(0), spinner_frame(2));
        assert_eq!(marker_frame(0), MARKER_FRAMES[0]);
        assert_eq!(marker_frame(2), MARKER_FRAMES[1]);
        // 会话区太阳帧与转圈同节律（每 2 tick 一帧）。
        assert_eq!(marker_frame(1), MARKER_FRAMES[0]);

        // The elapsed clock is appended when known.
        let with_clock = phase_line(
            0,
            Phase::Responding,
            Some(Duration::from_secs(42)),
            Some(Duration::from_secs(61)),
            0,
        );
        let last = with_clock.spans.last().expect("clock span");
        assert!(last.content.contains("42s"));

        // The steering badge rides after the clocks and hides at zero.
        let with_steering = phase_line(
            0,
            Phase::Responding,
            Some(Duration::from_secs(4)),
            Some(Duration::from_secs(8)),
            2,
        );
        let last = with_steering.spans.last().expect("badge span");
        assert_eq!(last.content, " · steering·2");
    }

    /// 探照灯的"过去后回原色"（用户对效果的描述）：首字符在灯光到达
    /// 前是基色，被照亮时亮于基色，灯光走过后**精确**回到基色（熄灭阈
    /// 兜底，无 RGB 残影）。整词呼吸版（霓虹）上"只有首字符亮"断言必红。
    #[test]
    fn phase_line_searchlight_char_lights_then_returns_to_base() {
        let base = Some(tui_theme::BRAND_SHIMMER_LOW);
        let char_fg = |tick: u64, index: usize| {
            phase_line(tick, Phase::Thinking, None, None, 0).spans[2 + index]
                .style
                .fg
        };
        // 起始（center = -margin）：灯光尚未进入，全基色。
        assert_eq!(char_fg(0, 0), base, "dark before the beam enters");

        // center = 0（step = margin）：首字符正被打亮——亮于基色，且
        // 邻字符也各不相同（高斯边缘，非全词同亮）。
        let lit_tick = SWEEP_STEP_TICKS * SWEEP_MARGIN_CHARS;
        assert_ne!(char_fg(lit_tick, 0), base, "lit while the beam is on it");
        assert_ne!(
            char_fg(lit_tick, 0),
            char_fg(lit_tick, 2),
            "the beam is a gradient, not a whole-label flash"
        );

        // 灯光走远（center ≥ len + margin - 0.5 的余量）：首字符回基色。
        let passed_tick = SWEEP_STEP_TICKS
            * (Phase::Thinking.label().chars().count() as u64 + SWEEP_MARGIN_CHARS);
        assert_eq!(
            char_fg(passed_tick, 0),
            base,
            "returns to the exact base color after the beam passes"
        );
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
    fn popup_height_cap_reserves_vertical_margins() {
        // 弹窗高度预算的唯一来源：放得下边距时减 2×POPUP_V_MARGIN，
        // 过矮终端（减后低于 MIN_POPUP_HEIGHT）退化为整屏高，不硬挤
        // 没。分页预算与 centered_rect 的钳制必须同源，否则页底渲染
        // 在框外（见 permission_dialog_scrolling_reaches_the_last_argument_line）。
        let area = |height| Rect::new(0, 0, 80, height);
        assert_eq!(popup_height_cap(area(24)), 20);
        assert_eq!(
            popup_height_cap(area(10)),
            6,
            "bounded=6 exactly meets MIN_POPUP_HEIGHT"
        );
        assert_eq!(
            popup_height_cap(area(9)),
            9,
            "bounded=5 keeps the legacy full-height behavior"
        );
        assert_eq!(popup_height_cap(area(0)), 0);
    }

    #[test]
    fn info_dialogs_are_content_sized_with_real_margins() {
        // 不变量（2026-08-19 第三轮反馈）：信息弹窗高度由内容行数决定，
        // 不恒取满额高度——短内容必须留出上下真实边距，长内容恰好
        // 钳满预算（继续靠滚动读完）。
        let area = Rect::new(0, 0, 80, 24);
        // 短内容：高度 = 行数 + 边框 2 + 脚注 1，居中后上下各 ≥ 边距。
        let height = content_dialog_height(5, area);
        assert_eq!(height, 8, "5 content lines + border + footer");
        let dialog = centered_rect(84, height, area);
        assert!(
            dialog.y >= POPUP_V_MARGIN && dialog.bottom() + POPUP_V_MARGIN <= area.bottom(),
            "short content keeps real top and bottom margins, got y={} bottom={}",
            dialog.y,
            dialog.bottom()
        );
        // 长内容：钳在预算上，不再增长，边距仍由预算保证。
        let capped = content_dialog_height(10_000, area);
        assert_eq!(capped, popup_height_cap(area));
        // 0 行内容也给得出最小可用框（不塌缩成负数/零）。
        assert_eq!(content_dialog_height(0, area), 3);
    }

    #[test]
    fn popup_width_matches_centered_rect() {
        // 不变量：popup_width 是 centered_rect 水平切分的取宽版（含
        // POPUP_H_MARGIN 钳制），两者在任何终端宽度下必须一致——内容
        // 折行宽度按它预算，错一列就会出现"预算的行数放不进框"。
        for width in 10..200u16 {
            let area = Rect::new(0, 0, width, 24);
            assert_eq!(
                popup_width(84, area),
                centered_rect(84, 6, area).width,
                "width mismatch at terminal width {width}"
            );
        }
    }

    #[test]
    fn animation_frames_advance_with_wall_time_not_draw_count() {
        // 不变量 A-CLK：帧号只由真实时间决定。draw() 自增的旧实现里，
        // 流式事件洪峰（每秒几十次重绘）把旋转加速成频闪、单帧绘制
        // 耗时长（长转录）又拖成慢动作——速度随"画了多少"漂移，永远
        // 修不对。时间驱动的帧号对重绘次数彻底不敏感。
        let frame = SPINNER_FRAME;
        assert_eq!(animation_tick_for(Duration::from_millis(0)), 0);
        assert_eq!(animation_tick_for(frame - Duration::from_millis(1)), 0);
        assert_eq!(animation_tick_for(frame), 1);
        assert_eq!(animation_tick_for(2 * frame), 2);
        // 一帧周期内重绘 100 次，帧号纹丝不动。
        let epoch = Instant::now();
        let first = animation_tick_for(epoch.elapsed());
        for _ in 0..100 {
            std::thread::sleep(Duration::from_millis(0));
        }
        assert_eq!(
            animation_tick_for(epoch.elapsed()),
            first,
            "redraw count cannot advance the animation clock"
        );
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
    /// 档位不在这里（属标题栏）；Cache/Context 常驻（缺值兜底 --%/0），
    /// Wallet/Token 段仍随余额查询就绪。
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
        // 无任何数据（全新会话）：Cache/Context 常驻兜底（--% 与 0），
        // 三段布局自启动起稳定（2026-08-19 用户反馈）。
        assert_eq!(
            full_suffix(&config, &None, &no_data, None),
            "Cache: --% · Context: 0/1M"
        );
        // 余额未就绪：Cache/Context 照常显示（不再整条消失）。
        assert_eq!(
            full_suffix(&config, &None, &cached, Some(&turn)),
            "Cache: 87.00% · Context: 120k/1M"
        );
        // 尚无上下文样本：Context 按 0 计，段落仍在。
        assert_eq!(
            full_suffix(&config, &balance, &cached, None),
            "Wallet: ￥110.00 · Cache: 87.00% · Context: 0/1M"
        );
        // 缓存命中为零（服务端上报零命中）：真实的 0.00%，不是未知。
        let zero_cache = Usage {
            input_tokens: 1000,
            cached_input_tokens: Some(0),
            ..Usage::default()
        };
        assert_eq!(
            full_suffix(&config, &balance, &zero_cache, Some(&turn)),
            "Wallet: ￥110.00 · Cache: 0.00% · Context: 120k/1M"
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
}
