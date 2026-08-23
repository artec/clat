//! Terminal frontend: `run(project)` is the only public entry. `App`
//! owns the dialog state machine, event loop, and rendering; widgets,
//! input handling, and rendering pieces live in the `tui/` submodules.

use crate::SessionId;
use crate::presets::preset_by_id;
use crate::tui::conversation::ToolCardVisibility;
use crate::tui::input::InputBuffer;
use crate::tui::model_editor::{ModelEditor, ModelPicker, PickerSnapshot};
use crate::tui::session_picker::{ResumeAction, SessionPicker};

use crate::dsh::backend::DshEvent;
use crate::tui::worker::{
    ChannelApprover, ChannelEventSink, ChannelUserAsker, UiEvent, WorkerMessage,
};
pub(crate) mod conversation;
mod dsh_events;
mod input;
mod logo;
mod markdown;
mod model_editor;
mod permission_picker;
mod session_picker;
mod theme;
mod worker;

use crate::{
    ApplicationEvent, ApplicationRunRequest, BootstrapApplication, CommandError, CommandInfo,
    CommandOutcome, CompactHandle, CompactionStatus, McpStatusDto, ModelConfig, ModelEvent,
    ModelVendor, PermissionDecision, PermissionMode, PermissionRequest, Project,
    ProjectAuthorization, ProviderCredentials, ProviderDescriptor, RenameOutcome, RunEvent,
    RunHandle, SteerOutcome, ThinkingLevel, TrustedProjectApplication, Usage, apply_thinking_level,
    effective_thinking_level, escalation_targets, next_thinking_level, thinking_levels,
};
use crossterm::event::{
    self, DisableBracketedPaste, DisableFocusChange, DisableMouseCapture, Event, KeyCode, KeyEvent,
    KeyEventKind, KeyModifiers, KeyboardEnhancementFlags, MouseButton, MouseEvent, MouseEventKind,
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
use std::collections::BTreeMap;
use std::env;
use std::io::{self, Write, stdout};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;
use std::time::{Duration, Instant};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

mod actions;
mod bell;
mod dialogs;
mod keys;
mod popup;
mod render;
mod run_events;
mod selection;
mod status;

#[cfg(test)]
use keys::*;
#[cfg(test)]
use render::*;

use bell::*;
use dialogs::*;
use dsh_events::*;
use popup::*;
use selection::*;
use status::*;

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

pub fn run(project: Project) -> io::Result<()> {
    let app = match App::open_deferred(project, None) {
        Ok(app) => app,
        Err(error) => return Err(io::Error::other(error)),
    };
    run_frontend(app)
}

/// `clat dsh` 入口（D-2 §1.0）：构造 dsh 态 App（不走本地信任门/存储），
/// 连接期由后台 ensure_online 线程驱动（`dsh_connect` 占位 + loading 状态），
/// 复用同一终端生命周期与 App::run 主循环。
pub fn run_dsh_mode(args: &[String]) -> io::Result<()> {
    let port = parse_dsh_port(args).unwrap_or(crate::dsh::connect::DEFAULT_PORT);
    let app = App::open_dsh(port).map_err(io::Error::other)?;
    run_frontend(app)
}

/// `--port <n>` 解析（D-1 原样迁移）。
fn parse_dsh_port(args: &[String]) -> Option<u16> {
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        if arg == "--port" {
            return iter.next().and_then(|value| value.parse().ok());
        }
    }
    None
}

/// 两模式共用的终端生命周期：初始化（鼠标/粘贴/kitty + panic hook）→
/// App::run 主循环 → 恢复 + 告别 LOGO + close 错误报告。
fn run_frontend(mut app: App) -> io::Result<()> {
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
            DisableFocusChange,
            PopKeyboardEnhancementFlags
        );
        let _ = stdout().write_all(b"\x1b[?1006l");
        let _ = stdout().flush();
        default_hook(info);
    }));

    // Enable mouse reporting without any-event tracking (1003): CLAT only
    // needs clicks (1000), drags (1002), and the wheel via SGR coordinates
    // (1006). crossterm's EnableMouseCapture also turns on 1003, and in
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
    // Focus Reporting（DECSET 1004，B-1 前后台感知）：终端焦点变化上报
    // 为 FocusGained/FocusLost——提醒铃据此区分「人在屏前看得见」与
    // AFK。不支持的终端（含 tmux 默认不转发）静默忽略，App 保持
    // None 未知态、铃铛保守响。
    let focus_result = execute!(stdout(), crossterm::event::EnableFocusChange);

    let (result, close_error) = {
        let run_result = app.run(&mut terminal);
        (run_result, app.take_close_error())
    };

    let _ = execute!(stdout(), PopKeyboardEnhancementFlags);
    let paste_result = execute!(stdout(), DisableBracketedPaste);
    let _ = execute!(stdout(), DisableFocusChange);
    let mouse_result = execute!(stdout(), DisableMouseCapture);
    ratatui::restore();
    // 告别 LOGO：主屏恢复后打印，与启动欢迎页成对（TTY 守卫 + 静默
    // 失败，纯装饰不影响退出码与管道输出）。
    crate::tui::logo::print_farewell();
    // 显式 shutdown 的失败在终端恢复后可见地报告（plan §16 阶段5）。
    if let Some(error) = close_error {
        let _ = writeln!(io::stderr(), "clat: {error}");
    }
    result.and(mouse_result).and(paste_result).and(focus_result)
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
    conversation: crate::tui::conversation::ConversationModel,
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
    /// INV-U1（原位返回）：进入编辑器前的 picker 导航态快照；编辑器
    /// 取消后据此原位重建 picker，保存/切换/删除等完结路径清空。
    picker_return: Option<PickerSnapshot>,
    /// /resume 会话选择器；打开期间独占按键与鼠标。
    session_picker: Option<SessionPicker>,
    running: bool,
    /// 统一事件通道：输入线程、余额监控、worker 的消息都汇到这里。
    /// `None` 表示尚未启动（run() 建立通道后填充）。
    events: Option<Receiver<UiEvent>>,
    /// 统一事件通道的发送端克隆：start_run 移交 worker，刷新触发用。
    event_sender: Option<Sender<UiEvent>>,
    pending_permission: Option<PendingPermission>,
    pending_ask_user: Option<PendingAskUser>,
    /// `/perm` 权限三档选择器（冷切换/降级入口；`/permission` 为别名）。
    permission_picker: Option<crate::tui::permission_picker::PermissionPicker>,
    /// `/rename` 会话改名弹框（显式标题存在时才可打开，N4）。
    rename_dialog: Option<RenameDialog>,
    /// 待随下一条消息发送的图片附件（用户路径；提交时复制进会话附件
    /// 目录，见 M4）。仅空闲态可附加；Esc 清空输入时一并清空。
    attachments: Vec<std::path::PathBuf>,
    run_handle: Option<RunHandle>,
    /// 当前 run 的本地纪元（W1-13）：start_run 成功即自增；完成消息
    /// 携带启动时的纪元，失配 = 上一 run 的陈旧完成，收尾动作跳过。
    run_epoch: u64,
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
    /// /help 打开时缓存的命令目录（`ShowHelp` 载荷，INV-C4）：帮助表
    /// 行从它派生，新增命令不改前端。
    help_commands: Vec<CommandInfo>,
    /// 余额/额度当前值：核心 Monitor 插件经 ApplicationEvent 写回，状态栏读取。
    balance: Option<String>,
    /// 本会话累计 token 用量，用于状态栏缓存命中百分比。journal 还原
    /// （挂载/切换）+ 运行中流式实时累计 + run 结束以结果权威覆盖。
    session_usage: Usage,
    /// INV-C1（Cache 按路由分桶）：每条模型路由（`model_route_key`，
    /// journal `source {provider, model}` 同口径）保留自己的累计——
    /// 切换模型不混合、不清零（服务端缓存跨往返存活，口径也必须存活）。
    /// 状态栏 Cache 段取"当前配置路由"的桶；没用过的路由显示 `--%`。
    usage_routes: BTreeMap<String, Usage>,
    /// 本次 run 开始时的路由桶快照：与 run_usage_base 同律——流式实时
    /// 重建、run 结束以 RunOutput 权威覆盖（只动本次 run 的路由桶）。
    run_routes_base: Option<BTreeMap<String, Usage>>,
    /// 本次 run 实际运行的模型路由（ModelRequested 事件指定，早于任何
    /// usage；run 中途切配置不影响入账归属）。
    run_route: Option<String>,
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
    /// 终端焦点三态（B-1 前后台感知，DECSET 1004）：`Some(true)` = 收
    /// 到过 FocusGained（人在屏前看得见，铃静音）；`Some(false)` = 失
    /// 焦（AFK，响）；`None` = 终端不支持/不转发 1004（tmux 默认不转
    /// 发）——未知保守响，漏报比多响伤害大（用户已有 NO_BELL/
    /// BELL_COMMAND 出口）。local 与 dsh 共用（铃铛门禁在 App 层）。
    focused: Option<bool>,
    /// dsh 会话状态（D-2 §1.1；local 模式恒 None）。running 不在此——
    /// App.running 是唯一事实源。
    dsh: Option<DshState>,
    /// 连接期占位（D-2 §1.0：Some = 正在连接宿主，`dsh` 为 None）。
    /// 元组 = (preferred_port, DshEvent 通道发送端——连接线程、后续
    /// worker 与 WS 线程共用一条通道，App 侧一根转发线程汇入 UiEvent)。
    dsh_connect: Option<(u16, Sender<DshEvent>)>,
    /// 连接期 DshEvent 通道接收端（`run` 起转发线程时消费）。
    dsh_connect_rx: Option<Receiver<DshEvent>>,
    /// dsh 客户端的「最后打开会话」记忆文件（拍板 A，2026-08-24：
    /// web localStorage 的 CLAT 同款；读写归 core 的
    /// control_storage::dsh_last_session，测试注入临时路径）。
    dsh_memory_path: PathBuf,
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
            conversation: crate::tui::conversation::ConversationModel::new(),
            card_visibility: ToolCardVisibility::default(),
            input: InputBuffer::new(Vec::new()),
            trust_prompt: !trusted,
            default_status: status.clone(),
            status,
            status_until: None,
            editor: None,
            picker: None,
            picker_return: None,
            session_picker: None,
            running: false,
            events: None,
            event_sender: None,
            pending_permission: None,
            pending_ask_user: None,
            permission_picker: None,
            rename_dialog: None,
            attachments: Vec::new(),
            run_handle: None,
            run_epoch: 0,
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
            help_commands: Vec::new(),
            balance: None,
            session_usage: Usage::default(),
            usage_routes: BTreeMap::new(),
            run_routes_base: None,
            run_route: None,
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
            focused: None,
            dsh: None,
            dsh_connect: None,
            dsh_connect_rx: None,
            dsh_memory_path: crate::control_storage::dsh_last_session::last_session_path(),
        };
        Ok(app)
    }

    /// dsh 态构造器（D-2 §1.0 字段初值表）：**不走本地信任门**——本地
    /// run 全家（application/bootstrap/run_handle/compact_handle）不激
    /// 活、无信任交互、不读写本地存储；模型与余额来自宿主侧，相关
    /// refresh 调用点不接线。连接期形态：`dsh=None` + `dsh_connect`
    /// 占位 + status "connecting to dsh…"（快照 dsh-connecting 断言此态）。
    fn open_dsh(preferred_port: u16) -> Result<Self, String> {
        let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        let project = Project::new(&cwd);
        let (dsh_tx, dsh_rx) = mpsc::channel::<DshEvent>();
        let status = "connecting to dsh…".to_owned();
        let config = ModelConfig::default();
        let credentials = ProviderCredentials::for_protocol(config.protocol);
        Ok(Self {
            project,
            bootstrap: None,
            application: None,
            session_id: None,
            session_title: None,
            close_error: None,
            config,
            credentials,
            provider_descriptors: Vec::new(),
            conversation: crate::tui::conversation::ConversationModel::new(),
            card_visibility: ToolCardVisibility::default(),
            input: InputBuffer::new(Vec::new()),
            trust_prompt: false,
            default_status: status.clone(),
            status,
            status_until: None,
            editor: None,
            picker: None,
            picker_return: None,
            session_picker: None,
            running: false,
            events: None,
            event_sender: None,
            pending_permission: None,
            pending_ask_user: None,
            permission_picker: None,
            rename_dialog: None,
            attachments: Vec::new(),
            run_handle: None,
            run_epoch: 0,
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
            help_commands: Vec::new(),
            balance: None,
            session_usage: Usage::default(),
            usage_routes: BTreeMap::new(),
            run_routes_base: None,
            run_route: None,
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
            focused: None,
            dsh: None,
            dsh_connect: Some((preferred_port, dsh_tx)),
            dsh_connect_rx: Some(dsh_rx),
            dsh_memory_path: crate::control_storage::dsh_last_session::last_session_path(),
        })
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
            crate::tui::conversation::ConversationModel::from_replay(&snapshot.replay);
        self.input = InputBuffer::new(snapshot.input_history);
        self.config = snapshot.config;
        self.credentials = snapshot.credentials;
        self.provider_descriptors = snapshot.provider_descriptors;
        // journal 用量统计（DSH assistant/message.usage）：状态栏的
        // Cache/Context 启动即有值，不必等首次 run 上报。路由桶随
        // journal 折叠还原（INV-C1/C2 平价：重开会话后切换回来的
        // 模型仍能看到自己的历史口径）。
        self.session_usage = snapshot.session_usage;
        self.usage_routes = snapshot.usage_routes;
        self.last_turn_usage = snapshot.last_request_usage;
        if snapshot.mcp.configured != 0 {
            if snapshot.mcp.connecting > 0 {
                self.flash_status(format!(
                    "mcp: {} server(s) connected · {} connecting",
                    snapshot.mcp.connected, snapshot.mcp.connecting
                ));
            } else if snapshot.mcp.failures.is_empty() {
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

        // dsh 连接期启动（D-2 §1.0）：DshEvent 通道 + 转发线程（汇入
        // UiEvent::Dsh）+ ensure_online 连接线程。
        if let Some((port, dsh_tx)) = self.dsh_connect.clone()
            && let Some(dsh_rx) = self.dsh_connect_rx.take()
        {
            let ui = event_sender.clone();
            thread::spawn(move || {
                while let Ok(event) = dsh_rx.recv() {
                    if ui.send(UiEvent::Dsh(event)).is_err() {
                        break;
                    }
                }
            });
            crate::dsh::backend::spawn_connect(port, dsh_tx);
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
            // dsh：自动重连排程 + 审批/问答应答排水（每帧轮询）。
            self.poll_dsh();
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

    /// 响一声提醒铃（触发点与模式见 [`BellMode`]）。B-1 前后台感知：
    /// 焦点三态在此单点判——local 与 dsh 的全部触发点（run 结束/弹框）
    /// 都走这一扇门，两模式同判。
    fn notify(&self) {
        if self.should_ring() {
            ring_bell(&self.bell);
        }
    }

    /// 铃铛三态判定：`Some(true)`（屏前看得见）不响；失焦响；
    /// `None`（终端无 Focus Reporting，如 tmux 默认不转发 1004）
    /// 保守响——漏报比多响伤害大，静音出口已有
    /// （CLAT_NO_BELL/CLAT_BELL_COMMAND）。
    fn should_ring(&self) -> bool {
        self.focused != Some(true)
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

    /// 提醒铃模式解析（规格推导）：默认系统声音（2026-08-21 起，替换
    /// BEL——BEL 在多数终端上不足以起到提醒作用）；CLAT_NO_BELL 压过
    /// 一切；CLAT_BELL_COMMAND 非空即自定义命令；NO_BELL 的真值形态
    /// 宽限（1/true/yes/on，大小写与空白不敏感由 trim+小写集合决定
    /// ——这里只锁小写形态，多余宽容不加）。
    #[test]
    fn bell_mode_resolves_from_env() {
        assert_eq!(bell_mode_from_env(None, None), BellMode::Sound);
        assert_eq!(
            bell_mode_from_env(Some("1".into()), None),
            BellMode::Off,
            "the silencer wins over everything"
        );
        assert_eq!(
            bell_mode_from_env(Some("1".into()), Some("afplay x".into())),
            BellMode::Off
        );
        assert_eq!(bell_mode_from_env(Some("0".into()), None), BellMode::Sound);
        assert_eq!(
            bell_mode_from_env(None, Some("afplay ~/ding.aiff".into())),
            BellMode::Command("afplay ~/ding.aiff".into())
        );
        // 空白命令视为未设置，回落系统声音。
        assert_eq!(
            bell_mode_from_env(None, Some("   ".into())),
            BellMode::Sound
        );
    }

    /// 平台默认声音的形状（macOS：afplay 放 Funk.aiff，`-v 2.0` 略放
    /// 大；声音文件存在才返回 Some——不存在时回落 BEL）。
    #[test]
    #[cfg(target_os = "macos")]
    fn system_sound_uses_funk_with_modest_volume_boost() {
        let (program, args) = system_sound_command().expect("macOS ships afplay and Funk.aiff");
        assert_eq!(program, std::path::PathBuf::from("afplay"));
        assert_eq!(
            args,
            vec![
                "-v".to_owned(),
                "2.0".to_owned(),
                "/System/Library/Sounds/Funk.aiff".to_owned()
            ]
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

    /// B-1 前后台感知（DECSET 1004）四条验收的判定面：
    /// ③无焦点事件的终端（None 未知态——不支持 1004 / tmux 默认不
    /// 转发）保守响；①FocusGained 后（屏前看得见）零声音；②
    /// FocusLost 后响；④local 与 dsh 两模式同判（门禁与状态机都在
    /// App 层单点）。判别：删掉 focused 记录或 should_ring 门禁即红。
    #[test]
    fn bell_respects_the_focus_tri_state_in_both_modes() {
        fn assert_tri_state(app: &mut App) {
            assert_eq!(app.focused, None);
            assert!(
                app.should_ring(),
                "unknown focus rings conservatively (B-1 acceptance 3)"
            );
            app.handle_ui_event(UiEvent::Terminal(Event::FocusGained));
            assert_eq!(app.focused, Some(true));
            assert!(
                !app.should_ring(),
                "foreground silences the bell (B-1 acceptance 1)"
            );
            app.handle_ui_event(UiEvent::Terminal(Event::FocusLost));
            assert_eq!(app.focused, Some(false));
            assert!(app.should_ring(), "lost focus rings (B-1 acceptance 2)");
        }
        // local 态（未受信虚拟路径——不触碰文件系统，快照测试同款）。
        let storage = std::env::temp_dir().join(format!(
            "clat-bell-focus-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let mut local = App::open(
            Project::new(std::path::Path::new("/home/dev/example-project")),
            Some(storage.clone()),
        )
        .expect("local app opens");
        local.test_freeze_tick = true;
        assert_tri_state(&mut local);
        // dsh 态（④：同一门禁、同一事件路径）。
        let mut dsh = App::open_dsh(3080).expect("dsh app opens");
        dsh.test_freeze_tick = true;
        assert_tri_state(&mut dsh);
        let _ = std::fs::remove_dir_all(&storage);
    }

    /// B-1 端到端腿：门禁真的拦住声音——前台态 `notify()` 不执行铃
    /// 命令（marker 不出现），失焦态执行。判别：删掉 notify 的
    /// should_ring 门禁即红（前台 marker 也会出现）。
    #[test]
    fn bell_gate_actually_blocks_the_sound_path_in_foreground() {
        let marker = std::env::temp_dir().join(format!(
            "clat-bell-gate-{}.marker",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let mut app = App::open_dsh(3080).expect("dsh app opens");
        app.bell = BellMode::Command(format!("printf ok > {:?}", marker));
        app.handle_ui_event(UiEvent::Terminal(Event::FocusGained));
        app.notify();
        std::thread::sleep(Duration::from_millis(300));
        assert!(
            !marker.exists(),
            "foreground: the bell command must not run"
        );
        app.handle_ui_event(UiEvent::Terminal(Event::FocusLost));
        app.notify();
        let mut appeared = false;
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while std::time::Instant::now() < deadline {
            if marker.exists() {
                appeared = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(appeared, "lost focus: the bell command runs");
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
                    Paragraph::new("X").block(popup_block("T")),
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
        assert_eq!(at_start.spans[0].style.fg, Some(theme::BRAND_SHIMMER_LOW));
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
        let base = Some(theme::BRAND_SHIMMER_LOW);
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
        // 短内容：高度 = 行数 + 边框 2 + 空行 1 + 脚注 1，居中后上下
        // 各 ≥ 边距（2026-08-21 起脚注上方有统一的空行分隔）。
        let height = content_dialog_height(5, area);
        assert_eq!(height, 9, "5 content lines + border + blank + footer");
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
        assert_eq!(content_dialog_height(0, area), 4);
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
            route_usage: Option<&Usage>,
            last_turn_usage: Option<&Usage>,
        ) -> String {
            status_suffix_segments(config, balance, route_usage, last_turn_usage).join(" · ")
        }

        let balance = Some("110.00".to_owned());
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
            full_suffix(&config, &balance, Some(&cached), Some(&turn)),
            "Wallet: ￥110.00 · Cache: 87.00% · Context: 120k/1M"
        );
        // 无任何数据（全新会话/当前路由无桶）：Cache/Context 常驻兜底
        // （--% 与 0），三段布局自启动起稳定（2026-08-19 用户反馈）。
        assert_eq!(
            full_suffix(&config, &None, None, None),
            "Cache: --% · Context: 0/1M"
        );
        // 余额未就绪：Cache/Context 照常显示（不再整条消失）。
        assert_eq!(
            full_suffix(&config, &None, Some(&cached), Some(&turn)),
            "Cache: 87.00% · Context: 120k/1M"
        );
        // 尚无上下文样本：Context 按 0 计，段落仍在。
        assert_eq!(
            full_suffix(&config, &balance, Some(&cached), None),
            "Wallet: ￥110.00 · Cache: 87.00% · Context: 0/1M"
        );
        // 缓存命中为零（服务端上报零命中）：真实的 0.00%，不是未知。
        let zero_cache = Usage {
            input_tokens: 1000,
            cached_input_tokens: Some(0),
            ..Usage::default()
        };
        assert_eq!(
            full_suffix(&config, &balance, Some(&zero_cache), Some(&turn)),
            "Wallet: ￥110.00 · Cache: 0.00% · Context: 120k/1M"
        );

        // GLM Coding Plan：Token 前缀替代 Wallet，不加货币符号。
        config.preset = Some("glm-5.3".into());
        config.endpoint = "https://open.bigmodel.cn/api/coding/paas/v4".into();
        let quota = Some("87%".to_owned());
        assert_eq!(
            full_suffix(&config, &quota, Some(&cached), Some(&turn)),
            "Token: 87% · Cache: 87.00% · Context: 120k/1M"
        );
        // 海外 z.ai 端点同样生效。
        config.endpoint = "https://api.z.ai/api/coding/paas/v4".into();
        assert_eq!(
            full_suffix(&config, &quota, Some(&cached), Some(&turn)),
            "Token: 87% · Cache: 87.00% · Context: 120k/1M"
        );

        // 自定义端点（无预设）：Context 分母未知，省略整段。
        config.preset = None;
        config.endpoint = "https://api.deepseek.com".into();
        assert_eq!(
            full_suffix(&config, &balance, Some(&cached), Some(&turn)),
            "Wallet: ￥110.00 · Cache: 87.00%"
        );

        // 非 DeepSeek/GLM 端点：无后缀。
        config.endpoint = "https://api.openai.com/v1".into();
        assert_eq!(
            full_suffix(&config, &balance, Some(&cached), Some(&turn)),
            ""
        );
    }

    /// INV-C1：Cache 按路由分桶显示——只有当前配置路由的桶上屏；切到
    /// 没跑过的路由显示 `--%`；切回来数字仍在（来回切换不清零，服务端
    /// 缓存跨往返存活，口径也跨往返保留）。修复前：单一会话累计让
    /// GLM→DeepSeek 切换后 Cache 残留旧模型的命中率（用户报告）。
    #[test]
    fn cache_scopes_to_the_current_model_route() {
        let mut routes = BTreeMap::new();
        routes.insert(
            crate::model::model_route_key("OpenAI Compatible", "glm-5.3"),
            Usage {
                input_tokens: 1000,
                cached_input_tokens: Some(870),
                ..Usage::default()
            },
        );
        let glm = ModelConfig {
            model: "glm-5.3".into(),
            ..ModelConfig::default()
        };
        let deepseek = ModelConfig {
            model: "deepseek-v4-flash".into(),
            ..ModelConfig::default()
        };
        // 当前 = GLM：自己的 87%。
        assert_eq!(
            cache_hit_percent(current_route_usage(&routes, &glm).expect("glm bucket")),
            Some("87.00%".to_owned())
        );
        // 切到没跑过的 DeepSeek：无桶 → `--%`（诚实值，不借 GLM 的数）。
        assert!(current_route_usage(&routes, &deepseek).is_none());
        assert_eq!(
            cache_hit_percent(current_route_usage(&routes, &deepseek).unwrap_or(&Usage::default())),
            None
        );
        // 切回 GLM：数字仍在。
        assert_eq!(
            cache_hit_percent(current_route_usage(&routes, &glm).expect("glm bucket")),
            Some("87.00%".to_owned())
        );
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
    /// 与主分段宽间距区分。模型名段带 `Role::ModelAccent`（D-2 闪光点
    /// b：两模式同色的主题蓝），其余段原色。
    #[test]
    fn header_rest_degrades_by_width_keeping_the_level_visible() {
        fn header<'a>(
            version: &'a str,
            state: &'a str,
            model: &'a str,
            level: Option<&'a str>,
        ) -> HeaderModel<'a> {
            HeaderModel {
                version,
                state,
                model,
                level,
            }
        }
        fn text(spans: Vec<Span<'static>>) -> String {
            spans
                .into_iter()
                .map(|span| span.content.to_string())
                .collect()
        }
        let levelled = header("0.5.1", "ready", "DeepSeek V4.0 Flash", Some("High"));
        let full = " v0.5.1  ready  ·  DeepSeek V4.0 Flash · Thinking · High";
        let full_spans = compose_header_rest(&levelled, 200);
        assert_eq!(text(full_spans.clone()), full);
        // 模型名段是唯一的着色段（ModelAccent），前后为原色段。
        let styled: Vec<&Span<'static>> = full_spans
            .iter()
            .filter(|span| span.style.fg.is_some())
            .collect();
        assert_eq!(styled.len(), 1, "{full_spans:?}");
        assert_eq!(styled[0].content, "DeepSeek V4.0 Flash");
        assert_eq!(
            styled[0].style.fg,
            theme::style(theme::Role::ModelAccent).fg
        );
        // 宽度恰好：完整。
        let fit = UnicodeWidthStr::width(full);
        assert_eq!(text(compose_header_rest(&levelled, fit)), full);
        // 差一列 → 紧凑（保留模型与档位、省略 "Thinking · " 文案）。
        let compact = " v0.5.1 ready · DeepSeek V4.0 Flash · High";
        assert_eq!(text(compose_header_rest(&levelled, fit - 1)), compact);
        // 紧凑也放不下 → 最小（省略模型名，档位仍在）。
        let compact_fit = UnicodeWidthStr::width(compact);
        assert_eq!(
            text(compose_header_rest(&levelled, compact_fit - 1)),
            " v0.5.1 ready · Thinking · High"
        );
        // 60 列终端（预算 60-7=53）：紧凑。紧凑层级宽 42 列：预算 42
        // 仍完整，41（48 列终端）即降到最小——档位保留、模型名省略。
        assert_eq!(text(compose_header_rest(&levelled, 53)), compact);
        assert_eq!(text(compose_header_rest(&levelled, 42)), compact);
        assert_eq!(
            text(compose_header_rest(&levelled, 41)),
            " v0.5.1 ready · Thinking · High"
        );
        assert_eq!(
            text(compose_header_rest(&levelled, 40)),
            " v0.5.1 ready · Thinking · High"
        );
        // 无档位（未配置 / 其它厂商 / 手工 disabled 由调用方归为 None /
        // dsh 模式恒 None）：各层级不出现档位片段，最小层级只剩版本与
        // 状态。
        let unlevelled = header("0.5.1", "ready", "DeepSeek V4.0 Flash", None);
        assert_eq!(
            text(compose_header_rest(&unlevelled, 200)),
            " v0.5.1  ready  ·  DeepSeek V4.0 Flash"
        );
        assert_eq!(
            text(compose_header_rest(&unlevelled, 35)),
            " v0.5.1 ready · DeepSeek V4.0 Flash"
        );
        assert_eq!(text(compose_header_rest(&unlevelled, 34)), " v0.5.1 ready");
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
