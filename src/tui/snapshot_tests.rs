//! 语义终端快照测试（phase-1 P0-1）。
//!
//! 快照对象是语义投影——每行 `(文本, 非默认样式区间)` 加光标/滚动状态
//! 头注记，不是像素也不是事件序列（G1）。动画确定性由 App 上的测试钩
//! （`test_freeze_tick` / `test_thinking_elapsed`）保证；同一输入序列连
//! 绘两次必须产生同一投影。期望文件在 `tests/fixtures/tui-snapshots/`，
//! 场景与文件一一对应（无孤儿文件）；刷新用
//! `CLAT_REFRESH_SNAPSHOTS=1 cargo test`，每次刷新必须逐一说明原因。

use super::App;
use super::{conversation_wrap_width, slice_by_columns};
use crate::test_support::{TestBehavior, TestProviderPlugin, roots};
use crate::tui::conversation::{CardState, ConversationModel, ToolCardVisibility};
use crate::tui::worker::{UiEvent, WorkerMessage};
use crate::{BootstrapApplication, ModelEvent, PermissionRequest, Project, RunEvent, ToolEffect};
use crossterm::event::{
    Event, KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use ratatui::Terminal;
use ratatui::backend::{Backend, TestBackend};
use serde_json::json;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::Duration;
use unicode_width::UnicodeWidthStr;

/// 场景注册表：`snapshot_files_form_a_closed_set` 据此校验目录无孤儿文件。
///
/// 2026-08-19 批量刷新（24 个既有场景）：输入框 block 右上角新增权限
/// 档位名（默认档 `Project Write`，与左上角 Message 对称）——本批引入
/// 权限三档后的全局可见变化；help-dialog 额外因 /permission、/rename
/// 两行命令条目而变化。新增场景：permission-picker /
/// permission-confirm-full / permission-dialog-escalate / session-title /
/// rename-dialog / rename-not-named。
/// 2026-08-19 同日二次刷新：权限命令更名 `/permission` → `/perm`
///（长名保留为别名）——permission-picker / permission-confirm-full 的
/// 弹框标题与 help-dialog 的命令行随之变化。
const SCENARIOS: &[&str] = &[
    "idle-transcript-80",
    "idle-transcript-40",
    "startup-loading",
    "conversation-with-messages",
    "selection-highlight",
    "trust-dialog",
    "permission-dialog",
    "permission-dialog-reviewed",
    "phase-waiting",
    "phase-thinking",
    "phase-responding",
    "phase-executing",
    "model-picker",
    "tool-card-collapsed",
    "tool-card-expanded",
    "tool-card-hidden",
    "tool-card-denied",
    "turn-end-notice",
    "markdown-table",
    "markdown-cjk-wrap",
    "steer-badge",
    "steer-pending-recall",
    "steered-transcript",
    "ask-dialog-options",
    "ask-dialog-custom",
    "help-dialog",
    "mcp-dialog",
    "permission-picker",
    "permission-confirm-full",
    "permission-dialog-escalate",
    "session-title",
    "rename-dialog",
    "rename-not-named",
    "attachment-chip",
];

fn snapshot_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/tui-snapshots")
}

/// 真实 App + TestBackend 的驱动装置。生产事件循环之外逐条注入
/// UiEvent、以 TestBackend 绘制、投影为可 review 文本。
struct Harness {
    app: App,
    terminal: Terminal<TestBackend>,
    project_root: PathBuf,
    #[allow(dead_code)]
    storage_root: PathBuf,
}

impl Drop for Harness {
    fn drop(&mut self) {
        if let Some(application) = self.app.application.take() {
            let _ = application.close();
        }
    }
}

fn harness(tag: &str, width: u16, height: u16, trusted: bool) -> Harness {
    let (storage_root, project_root) = roots(tag);
    std::fs::create_dir_all(&project_root).expect("project dir");
    // 确权对话框逐行换行显示 project root：临时前缀长度随平台不同
    //（macOS $TMPDIR ~48 字符 vs Linux /tmp 4 字符），换行/裁剪发生在
    // 渲染期、先于任何投影归一化——跨平台行数都会不同。未确权路径不
    // 触碰任何文件系统（只显示），用固定虚拟路径钉住形状。
    let project_for_app = if trusted {
        project_root.clone()
    } else {
        std::path::PathBuf::from("/home/dev/example-project")
    };
    if trusted {
        // 预授权 storage root（挂载一次以写入信任行后立即关闭），再以
        // 生产构造路径打开 App。受信路径挂载完整生产插件目录——MCP
        // 配置来自 storage root，临时根下为空，不会拉起真实子进程。
        let bootstrap =
            BootstrapApplication::open(Project::new(&project_root), storage_root.clone())
                .expect("open bootstrap");
        let application = bootstrap
            .authorize_and_mount_with_provider(std::sync::Arc::new(TestProviderPlugin {
                behavior: TestBehavior::Success,
            }))
            .expect("authorize");
        application.close().expect("close authorizer");
    }
    let mut app =
        App::open(Project::new(&project_for_app), Some(storage_root.clone())).expect("app opens");
    app.test_freeze_tick = true;
    // 底部状态栏默认值是 storage root 绝对路径：同样的渲染期裁剪问题
    //（macOS 下 79 列处截断、Linux 下整条放得下）。以固定占位符替换
    // 显示值——"这一行渲染什么"的覆盖保留，环境依赖清零。
    app.default_status = "<STORAGE-ROOT>".into();
    app.status = "<STORAGE-ROOT>".into();
    Harness {
        app,
        terminal: Terminal::new(TestBackend::new(width, height)).expect("test terminal"),
        project_root,
        storage_root,
    }
}

impl Harness {
    fn trusted(tag: &str, width: u16, height: u16) -> Self {
        harness(tag, width, height, true)
    }

    fn untrusted(tag: &str) -> Self {
        harness(tag, 80, 24, false)
    }

    fn event(&mut self, event: UiEvent) {
        self.app.handle_ui_event(event);
    }

    fn key(&mut self, code: KeyCode) {
        self.event(UiEvent::Terminal(Event::Key(KeyEvent::new(
            code,
            KeyModifiers::NONE,
        ))));
    }

    fn type_text(&mut self, text: &str) {
        for character in text.chars() {
            self.key(KeyCode::Char(character));
        }
    }

    fn run_event(&mut self, event: RunEvent) {
        self.event(UiEvent::Worker(WorkerMessage::Event(event)));
    }

    fn drag_select(&mut self, x0: u16, y0: u16, x1: u16, y1: u16) {
        let mouse = |kind: MouseEventKind, column: u16, row: u16| {
            UiEvent::Terminal(Event::Mouse(MouseEvent {
                kind,
                column,
                row,
                modifiers: KeyModifiers::NONE,
            }))
        };
        self.event(mouse(MouseEventKind::Down(MouseButton::Left), x0, y0));
        self.event(mouse(MouseEventKind::Drag(MouseButton::Left), x1, y1));
        self.event(mouse(MouseEventKind::Up(MouseButton::Left), x1, y1));
    }

    /// 绘制并投影。连绘两次并断言一致（G1），返回投影文本。
    fn draw_projection(&mut self) -> String {
        let first = self.project();
        let second = self.project();
        assert_eq!(first, second, "G1: 同一输入序列必须产生同一投影");
        second
    }

    fn project(&mut self) -> String {
        self.app.expire_status();
        self.terminal
            .draw(|frame| self.app.draw(frame))
            .expect("draw");
        let cursor = self
            .terminal
            .backend_mut()
            .get_cursor_position()
            .map(|position| (position.x, position.y))
            .unwrap_or((0, 0));
        let projection = render_projection(
            self.terminal.backend().buffer(),
            cursor,
            self.app.conversation_scroll_from_bottom,
        );
        normalize_paths(&projection, &self.project_root)
    }

    fn snapshot(&mut self, name: &str) {
        let projection = self.draw_projection();
        check_or_refresh(name, &projection);
    }
}

/// 三道环境归一化，保证 fixture 与开发机/CI、crate 版本解耦：
/// 1. project root 整段替换；
/// 2. 临时目录前缀替换（macOS `$TMPDIR` vs Linux `/tmp`）——必须在
///    project root 之后做（project root 是临时目录的子串）；
/// 3. 版本号打码——发布提交 bump `CARGO_PKG_VERSION` 时，头部的
///    `CLAT v0.6.x` 不应令全部 fixture 集体失效；
///
/// 最后叠加裸纳秒数字串归一——路径在窄对话框里换行时会把纳秒串从
/// 中间拆开（位数恒定 → 换行位置恒定，但裸数字串本身必须归一）。
fn normalize_paths(projection: &str, project_root: &Path) -> String {
    let normalized = projection.replace(project_root.to_string_lossy().as_ref(), "<ROOT>");
    let tmp = std::env::temp_dir();
    let normalized = normalized.replace(tmp.to_string_lossy().trim_end_matches('/'), "<TMP>");
    let version = concat!("v", env!("CARGO_PKG_VERSION"));
    mask_long_digit_runs(&normalized.replace(version, "v<VERSION>"))
}

/// 投影中任何 ≥12 位的连续数字串（临时目录纳秒后缀）归一为固定占位
/// 符；版本号等短数字不受影响。
fn mask_long_digit_runs(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut run = String::new();
    for character in text.chars().chain(['\0']) {
        if character.is_ascii_digit() {
            run.push(character);
        } else {
            if run.len() >= 12 {
                out.push_str("<NANOS>");
            } else {
                out.push_str(&run);
            }
            run.clear();
            if character != '\0' {
                out.push(character);
            }
        }
    }
    out
}

/// 语义投影：`size`/`cursor`/`scroll` 头注记 + 每行文本与非默认样式
/// 区间。行尾默认样式的空白裁剪；**带样式的行尾空白**（用户块的满宽
/// 背景填充——2026-08-19 恢复的设计内视觉）以显式 `PAD:n` 段保留，
/// 使横贯效果在快照中可见可审。
fn render_projection(
    buffer: &ratatui::buffer::Buffer,
    cursor: (u16, u16),
    scroll_from_bottom: usize,
) -> String {
    let area = buffer.area;
    let mut out = format!(
        "size {}x{}\ncursor {},{}\nscroll {}\n",
        area.width, area.height, cursor.0, cursor.1, scroll_from_bottom
    );
    for y in 0..area.height {
        out.push_str(&row_projection(buffer, area.width, y));
        out.push('\n');
    }
    out
}

fn row_projection(buffer: &ratatui::buffer::Buffer, width: u16, y: u16) -> String {
    // 逐列收集 (符号, 样式描述)，样式描述为空串表示完全默认。
    let mut symbols: Vec<&str> = Vec::with_capacity(width as usize);
    let mut descs: Vec<String> = Vec::with_capacity(width as usize);
    for x in 0..width {
        let cell = &buffer[(x, y)];
        symbols.push(cell.symbol());
        descs.push(style_desc(cell));
    }
    // 行尾空白：先确定可见列数，再决定裁剪或 PAD 标记。
    let mut visible = width as usize;
    while visible > 0 && symbols[visible - 1] == " " {
        visible -= 1;
    }
    let trailing = width as usize - visible;
    let pad_styled = trailing > 0 && descs[visible..].iter().any(|desc| !desc.is_empty());
    let mut text = String::new();
    for symbol in &symbols[..visible] {
        text.push_str(symbol);
    }
    // 非默认样式段合并（列区间 [start, end] 闭区间，仅限可见列）。
    let mut segments: Vec<(usize, usize, String)> = Vec::new();
    let mut start = 0;
    for x in 1..=visible {
        if x == visible || descs[x] != descs[start] {
            if !descs[start].is_empty() {
                segments.push((start, x - 1, descs[start].clone()));
            }
            start = x;
        }
    }
    if pad_styled {
        let pad_desc = descs[visible..]
            .iter()
            .find(|desc| !desc.is_empty())
            .cloned()
            .unwrap_or_default();
        segments.push((
            visible,
            width as usize - 1,
            format!("PAD:{trailing} {pad_desc}"),
        ));
    }
    if text.is_empty() && segments.is_empty() {
        return format!("{y:03}| <blank>");
    }
    if segments.is_empty() {
        return format!("{y:03}| {text}");
    }
    let joined = segments
        .iter()
        .map(|(start, end, description)| format!("{start}..{end} {description}"))
        .collect::<Vec<_>>()
        .join(" · ");
    format!("{y:03}| {text}  ⟦{joined}⟧")
}

fn style_desc(cell: &ratatui::buffer::Cell) -> String {
    let mut parts = Vec::new();
    if cell.fg != ratatui::prelude::Color::Reset {
        parts.push(format!("fg={:?}", cell.fg));
    }
    if cell.bg != ratatui::prelude::Color::Reset {
        parts.push(format!("bg={:?}", cell.bg));
    }
    let modifier = cell.modifier;
    if !modifier.is_empty() {
        let names = [
            (ratatui::style::Modifier::BOLD, "BOLD"),
            (ratatui::style::Modifier::DIM, "DIM"),
            (ratatui::style::Modifier::ITALIC, "ITALIC"),
            (ratatui::style::Modifier::UNDERLINED, "UNDERLINED"),
            (ratatui::style::Modifier::REVERSED, "REVERSED"),
        ];
        for (flag, name) in names {
            if modifier.contains(flag) {
                parts.push(name.to_string());
            }
        }
    }
    parts.join("+")
}

fn check_or_refresh(name: &str, projection: &str) {
    let path = snapshot_dir().join(format!("{name}.txt"));
    if std::env::var("CLAT_REFRESH_SNAPSHOTS").as_deref() == Ok("1") {
        std::fs::create_dir_all(snapshot_dir()).expect("snapshot dir");
        std::fs::write(&path, projection).expect("write snapshot");
        return;
    }
    match std::fs::read_to_string(&path) {
        Ok(expected) => assert_eq!(
            expected, projection,
            "snapshot `{name}` changed; if intentional: CLAT_REFRESH_SNAPSHOTS=1 cargo test tui::snapshot_tests (and justify)"
        ),
        Err(_) => panic!(
            "snapshot `{name}` missing; generate with CLAT_REFRESH_SNAPSHOTS=1 cargo test tui::snapshot_tests"
        ),
    }
}

/// G1 封闭集：目录里的期望文件必须全部对应注册场景。
#[test]
fn snapshot_files_form_a_closed_set() {
    let dir = snapshot_dir();
    if !dir.is_dir() {
        return; // 尚未生成任何快照
    }
    let mut files: Vec<String> = std::fs::read_dir(&dir)
        .expect("read snapshot dir")
        .filter_map(|entry| {
            let path = entry.expect("dir entry").path();
            path.extension()
                .is_some_and(|extension| extension == "txt")
                .then(|| {
                    path.file_stem()
                        .expect("stem")
                        .to_string_lossy()
                        .into_owned()
                })
        })
        .collect();
    files.sort();
    let mut expected: Vec<&str> = SCENARIOS.to_vec();
    expected.sort();
    assert_eq!(
        files, expected,
        "孤儿快照文件或缺失场景注册（SCENARIOS 与目录必须一一对应）"
    );
}

#[test]
fn idle_transcript_wide() {
    // 刷新 2026-08-19：空会话占位 "No messages yet." 升级为 LOGO 欢迎页
    //（tui_logo + Role::Logo 品牌蓝）。
    let mut harness = Harness::trusted("snap-idle-80", 80, 24);
    harness.snapshot("idle-transcript-80");
}

#[test]
fn idle_transcript_narrow() {
    // 刷新 2026-08-19：40 列放不下 LOGO（34 列），退化为单行提示。
    let mut harness = Harness::trusted("snap-idle-40", 40, 24);
    harness.snapshot("idle-transcript-40");
}

#[test]
fn conversation_with_messages_snapshot() {
    let mut harness = Harness::trusted("snap-conversation", 80, 24);
    let mut conversation = ConversationModel::new();
    conversation.push_user("please fix the login crash on Safari".into());
    conversation.push_assistant_for_test(
        "I'll inspect the relevant files first.\n\n- `src/login.rs` holds the bug\n- patched in **two** places",
    );
    conversation.push_user("thanks".into());
    harness.app.conversation = conversation;
    harness.app.conversation_scroll_from_bottom = 0;
    // 刷新 2026-08-19：会话折行宽度 -1（滚动条列专属，宽字符不再铺
    // 进滚动条列）——长行换行点前移一列。
    harness.snapshot("conversation-with-messages");
}

/// 回归（真实事故，用户实测确认规律）：行尾为宽字符（CJK/emoji，占
/// 2 列）的行会遮挡滚动条，纯 ASCII 行不受影响。机制：文本按完整
/// inner 宽度折行时，行尾宽字符的字形铺进滚动条列；ratatui 的 diff
/// 会跳过宽字符右侧单元格的更新（to_skip），滚动条符号被字形覆盖且
/// 不再补发。不变量：会话文本换行宽度必须比 inner 少一列（滚动条
/// 专属列），任何宽字符字形都不得延伸进滚动条列——等价于"滚动条列
/// 左侧一格不得起始双宽字形"。修复前本测试失败。
#[test]
fn wide_glyphs_never_bleed_into_the_scrollbar_column() {
    let mut harness = Harness::trusted("snap-cjk-scrollbar", 80, 24);
    let mut conversation = ConversationModel::new();
    conversation.push_user("cjk wrap probe".into());
    // 助手文本按完整 inner 宽度折行（用户块因 `width - 4` 折行 + 尾部
    // 填充天然碰不到边缘列）。200 个全角字符必然产生恰好铺满 78 列
    // 的行——行尾宽字符起始于第 77 列，字形铺进第 78 列（滚动条列）。
    conversation.push_assistant_for_test(&"试".repeat(200));
    harness.app.conversation = conversation;
    harness.app.conversation_scroll_from_bottom = 0;
    harness.project();
    let area = harness.app.conversation_area;
    let buffer = harness.terminal.backend().buffer();
    // block.inner 的最右列是滚动条（VerticalRight）；其左一格若起始
    // 双宽字形，字形必然占据滚动条列。
    let scrollbar_column = area.x + area.width - 2;
    for y in area.y..area.y + area.height {
        let cell = &buffer[(scrollbar_column - 1, y)];
        let symbol = cell.symbol();
        assert!(
            UnicodeWidthStr::width(symbol) < 2,
            "wide glyph {symbol:?} at column {} row {y} bleeds into scrollbar column {scrollbar_column}",
            scrollbar_column - 1,
        );
    }
}

/// 回归（用户实测：非英文字符只破坏弹窗最左边的边线，内部不受影
/// 响）：跨在弹窗左边框起点上的宽字符（其起点格在 Clear 范围之外）
/// 会让 ratatui diff 的 to_skip 吞掉边框列的更新——上一帧字形铺进
/// 边框列，本帧 │ 不再补发。右边框因起点格在 Clear 范围内天然安全，
/// 故以右边框为基准推导弹窗行跨度，在跨度内断言左边框完好。修复前
/// 本测试失败。
#[test]
fn popup_left_border_survives_wide_glyphs_from_the_layer_below() {
    let mut harness = Harness::trusted("snap-cjk-popup", 80, 24);
    let mut conversation = ConversationModel::new();
    // 前导短消息把宽字符行推进弹窗跨度中央（弹窗高度随参数变化，
    // 固定几何会随内容漂移；跨中放置对任意高度稳定）。
    for seed in ["one", "two", "three"] {
        conversation.push_user(seed.into());
        conversation.push_assistant_for_test("ok");
    }
    // "❯ " 标记 2 列 + 全角字符连排：起点必然覆盖第 5 列（弹窗左边框
    // 6 的左邻），字形铺进边框列。20 个全角共 40 列，不触发换行。
    conversation.push_user("测".repeat(20));
    harness.app.conversation = conversation;
    harness.app.conversation_scroll_from_bottom = 0;
    harness.project(); // 第 1 帧：无弹窗，底层宽字符已上屏
    let (decision_tx, _decision_rx) = mpsc::channel();
    harness.event(UiEvent::Worker(WorkerMessage::PermissionRequest {
        request: PermissionRequest {
            tool: "write_file".into(),
            effect: ToolEffect::Write,
            reason: "writes a file".into(),
            arguments: json!({"path": "src/lib.rs", "content": "fn main() {}\n"}),
            call_id: "call-cjk-1".into(),
        },
        decision_tx,
    }));
    harness.project(); // 第 2 帧：弹窗出现，边框更新必须穿透 diff
    let buffer = harness.terminal.backend().buffer();
    // 80 列终端、84% 弹窗（快照几何）：左边框 6，右边框 73。
    let (left, right) = (6u16, 73u16);
    // 从右边框（本 bug 不影响的一侧）推导弹窗行跨度。
    let span: Vec<u16> = (0..24u16)
        .filter(|&y| matches!(buffer[(right, y)].symbol(), "│" | "┐" | "┘"))
        .collect();
    assert!(!span.is_empty(), "dialog right border not found");
    for y in span {
        let guard = buffer[(left - 1, y)].symbol();
        assert!(
            UnicodeWidthStr::width(guard) < 2,
            "wide glyph {guard:?} at column {} row {y} straddles the popup border column",
            left - 1,
        );
        let left_border = buffer[(left, y)].symbol();
        assert!(
            matches!(left_border, "│" | "┌" | "└"),
            "left border at row {y} was eaten by a wide glyph: {left_border:?}"
        );
    }
}

#[test]
fn selection_highlight_snapshot() {
    let mut harness = Harness::trusted("snap-selection", 80, 24);
    let mut conversation = ConversationModel::new();
    conversation.push_user("select me: the quick brown fox jumps over".into());
    conversation.push_assistant_for_test("ok");
    harness.app.conversation = conversation;
    // 先绘一帧让 App 记录会话区矩形（选区鼠标坐标映射依赖它）。
    harness.project();
    let area = harness.app.conversation_area;
    harness.drag_select(area.x + 2, area.y + 1, area.x + 14, area.y + 1);
    // 刷新 2026-08-19：会话折行宽度 -1（同 conversation-with-messages）。
    harness.snapshot("selection-highlight");
}

/// 运行中实时累计（2026-08-19 用户反馈）：首跑中途 Cache 段即有值，
/// 不必等 run 结束才三段齐全。不变量：流式 Usage 事件在 run 起点基线
/// 上累加出实时会话用量（多请求累计、水位取最近一次）；run 结束以
/// RunOutput 的全量结果权威覆盖——流式值只是近似，不得重复相加。
#[test]
fn session_usage_accumulates_live_during_a_run() {
    let mut harness = Harness::trusted("snap-live-usage", 80, 24);
    let usage = |input: u64, cached: Option<u64>| crate::model::Usage {
        input_tokens: input,
        output_tokens: input / 10,
        cached_input_tokens: cached,
        reasoning_tokens: None,
    };
    harness.app.running = true;
    harness.app.run_usage_base = Some(harness.app.session_usage.clone());
    harness.run_event(RunEvent::ModelStream {
        turn: 1,
        event: ModelEvent::Usage(usage(100, Some(90))),
    });
    assert_eq!(harness.app.session_usage.input_tokens, 100);
    assert_eq!(harness.app.session_usage.cached_input_tokens, Some(90));
    assert_eq!(
        harness.app.last_turn_usage.as_ref().map(|u| u.input_tokens),
        Some(100),
        "the context watermark is the most recent request"
    );
    // 第二个请求累计入会话用量，水位换新。
    harness.run_event(RunEvent::ModelStream {
        turn: 2,
        event: ModelEvent::Usage(usage(50, Some(40))),
    });
    assert_eq!(harness.app.session_usage.input_tokens, 150);
    assert_eq!(
        harness.app.last_turn_usage.as_ref().map(|u| u.input_tokens),
        Some(50)
    );
    // 结束权威覆盖：基线 + RunOutput 全量（流式近似被替换，不重复计）。
    harness.event(UiEvent::Worker(WorkerMessage::Done {
        epoch: harness.app.run_epoch,
        result: Ok(crate::ApplicationRunDone {
            output: "done".into(),
            turns: 2,
            usage: usage(200, Some(170)),
            cancelled: false,
        }),
    }));
    assert_eq!(harness.app.session_usage.input_tokens, 200);
    assert_eq!(harness.app.session_usage.cached_input_tokens, Some(170));
    assert!(!harness.app.running, "the run finished");
}

/// W1-13：收尾窗口竞态——run1 封口（busy=false、completion 在途）时用户
/// 走 W1-04 回退路径启动 run2，run1 的完成消息随后送达。不变量：陈旧
/// 完成（纪元失配）不得触碰**新** run 的任何收尾状态——不 take/join 新
/// 句柄、不置 running=false、不做用量对账。若删除纪元守卫回到无身份
/// Done，本测试在新句柄为 None 的构造下即以 `running` 误置 false 而红
/// （真实场景则表现为 join 新 run 冻结 UI + 产出按新 run 记账）。
#[test]
fn stale_run_completion_does_not_finalize_the_newer_run() {
    let mut harness = Harness::trusted("snap-stale-done", 80, 24);
    // 构造：run2 已启动（纪元 2、running），其句柄此处以 None 代位——
    // 判别只依赖收尾对 self 状态的触碰，不依赖句柄本身。
    harness.app.run_epoch = 2;
    harness.app.running = true;
    harness.app.run_usage_base = Some(harness.app.session_usage.clone());
    harness.event(UiEvent::Worker(WorkerMessage::Done {
        epoch: 1,
        result: Ok(crate::ApplicationRunDone {
            output: "run-1 output".into(),
            turns: 1,
            usage: crate::model::Usage {
                input_tokens: 999,
                output_tokens: 1,
                cached_input_tokens: None,
                reasoning_tokens: None,
            },
            cancelled: false,
        }),
    }));
    assert!(harness.app.running, "the newer run stays active");
    assert!(
        harness.app.run_usage_base.is_some(),
        "the newer run's accounting baseline is untouched"
    );
    assert_eq!(
        harness.app.run_epoch, 2,
        "no state transition happens for the stale completion"
    );
}

/// 回归（2026-08-19 审计）：滚动条列预留把渲染折行宽度改为
/// inner-1（conversation_wrap_width），但选区复制仍按 inner 取行——
/// 复制跨行长文本时，各行来自错误的折行点，拷出内容与显示错位。
/// 不变量：选区拷贝的每一行必须等于同一宽度（渲染的唯一来源）下的
/// row_plain_text。预修复代码上本测试失败（折行点差一列）。
#[test]
fn copying_a_selection_uses_the_rendered_wrap_width() {
    let mut harness = Harness::trusted("snap-copy-wrap", 80, 24);
    let mut conversation = ConversationModel::new();
    conversation.push_user("copy probe".into());
    // 循环字母表的长文本：任何折行宽度下都必然跨多行，且各行内容
    // 各不相同（同质文本会掩盖错位）。
    let text: String = (0..200)
        .map(|index| char::from(b'a' + (index % 26) as u8))
        .collect();
    conversation.push_assistant_for_test(&text);
    harness.app.conversation = conversation;
    harness.app.conversation_scroll_from_bottom = 0;
    harness.project();
    let area = harness.app.conversation_area;
    let width = conversation_wrap_width(area);
    let assistant_rows = 200usize.div_ceil(width);
    // 内容行 0 是用户块、1 是分隔空行，助手行从 2 起；拖满列宽选中
    // 全部助手行（指针落在最后一列 = 行尾）。
    let first_row = 2usize;
    let last_row = first_row + assistant_rows - 1;
    harness.drag_select(
        area.x + 1,
        area.y + 1 + first_row as u16,
        area.x + 1 + width as u16,
        area.y + 1 + last_row as u16,
    );
    let copied = harness.app.selection_text().expect("selection text");
    let expected: Vec<String> = (first_row..=last_row)
        .map(|row| {
            let plain =
                harness
                    .app
                    .conversation
                    .row_plain_text(row, width, ToolCardVisibility::Collapsed);
            slice_by_columns(&plain, 0, width)
        })
        .collect();
    assert_eq!(
        copied,
        expected.join("\n"),
        "copied rows must match the rendered wrap width"
    );
}

/// 启动加载画面（2026-08-19 用户反馈：大会话启动不该等在黑窗口）：
/// TUI 先行上屏——LOGO 欢迎页 + loading 状态 + 输入禁用。不 poll
/// 接收端，状态确定性停在加载态（交接只发生在 poll 时）。
/// 刷新 2026-08-19（同日第四轮反馈）：输入框标题不再报 loading——
/// 头部状态与底部状态栏两处已足够，第三处画蛇添足；标题保持
/// `Message`（loading 期间输入禁用由 loading 门保证，不靠标题）。
#[test]
fn startup_loading_snapshot_and_handover() {
    let (storage_root, project_root) = roots("snap-loading");
    std::fs::create_dir_all(&project_root).unwrap();
    // 预信任（同 harness() 受信分支：写入信任行后立即关闭）。
    let bootstrap =
        BootstrapApplication::open(Project::new(&project_root), storage_root.clone()).unwrap();
    let application = bootstrap
        .authorize_and_mount_with_provider(std::sync::Arc::new(TestProviderPlugin {
            behavior: TestBehavior::Success,
        }))
        .unwrap();
    application.close().unwrap();

    let mut app =
        App::open_deferred(Project::new(&project_root), Some(storage_root.clone())).unwrap();
    assert!(
        app.loading.is_some(),
        "the session mounts in the background"
    );
    app.test_freeze_tick = true;
    let mut harness = Harness {
        app,
        terminal: Terminal::new(TestBackend::new(80, 24)).expect("test terminal"),
        project_root,
        storage_root,
    };
    harness.snapshot("startup-loading");

    // 加载门：普通按键被吞，唯一出口是 Ctrl+C。
    harness.key(KeyCode::Char('h'));
    assert!(
        !harness.app.input.visual_rows(60).join("").contains('h'),
        "typing is blocked while the session loads"
    );
    harness.event(UiEvent::Terminal(Event::Key(KeyEvent::new(
        KeyCode::Char('c'),
        KeyModifiers::CONTROL,
    ))));
    assert!(harness.app.should_quit, "Ctrl+C remains the exit");
    harness.app.should_quit = false;

    // 后台挂载完成：轮询交接后 loading 解除、状态栏复位、输入解锁。
    for _ in 0..500 {
        harness.app.poll_loading();
        if harness.app.loading.is_none() {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(
        harness.app.loading.is_none(),
        "the background mount hands over"
    );
    assert_eq!(
        harness.app.status, harness.app.default_status,
        "the status line returns to the resident directory"
    );
    harness.key(KeyCode::Char('h'));
    assert!(
        harness.app.input.visual_rows(60).join("").contains('h'),
        "input unlocks after the handover"
    );
}

#[test]
fn trust_dialog_snapshot() {
    // 刷新 2026-08-19：弹窗规范统一——黄边框/标题 + 上下垂直边距
    //（确权对话框是安全决策，与权限弹窗同款警示样式）。
    let mut harness = Harness::untrusted("snap-trust");
    harness.snapshot("trust-dialog");
}

#[test]
fn permission_dialog_snapshot() {
    // 刷新 2026-08-19：底层空会话改画欢迎页，弹窗左缘露出的
    // "No me…" 前缀随之消失。
    // 刷新 2026-08-19：权限弹窗加 Warning 黄边框/标题 + 全屏 DIM
    // 压暗层——异步弹窗此前与背景同亮度，被用户当成背景忽略。
    // 刷新 2026-08-19：弹窗 Clear 左右各扩一列垫边（宽字符不再跨在
    // 边框起点上吃掉左边线）。
    // 刷新 2026-08-19：弹窗规范统一——黄框下沉到通用 popup_block，
    // 垫边扩展到上下（四边间距，垂直边距 POPUP_V_MARGIN）。
    let mut harness = Harness::trusted("snap-permission", 80, 24);
    let (decision_tx, _decision_rx) = mpsc::channel();
    harness.event(UiEvent::Worker(WorkerMessage::PermissionRequest {
        request: PermissionRequest {
            tool: "write_file".into(),
            effect: ToolEffect::Write,
            reason: "writes a file".into(),
            arguments: json!({
                "path": "src/lib.rs",
                "content": "fn main() {\n    println!(\"hello\");\n}\n"
            }),
            call_id: "call-snap-1".into(),
        },
        decision_tx,
    }));
    harness.snapshot("permission-dialog");
}

#[test]
fn permission_dialog_reviewed_snapshot() {
    // 刷新 2026-08-19：同 permission-dialog——空会话底层换欢迎页。
    // 刷新 2026-08-19：同 permission-dialog——Warning 边框 + 全屏
    // DIM 压暗层。
    // 刷新 2026-08-19：同 permission-dialog——Clear 垫边。
    // 刷新 2026-08-19：弹窗规范统一（黄框下沉 + 四边垫边）。
    let mut harness = Harness::trusted("snap-permission-reviewed", 80, 24);
    let (decision_tx, _decision_rx) = mpsc::channel();
    harness.event(UiEvent::Worker(WorkerMessage::PermissionRequest {
        request: PermissionRequest {
            tool: "edit_file".into(),
            effect: ToolEffect::Write,
            reason: "edits a file".into(),
            arguments: json!({
                "path": "src/lib.rs",
                "old_str": "fn main() {\n    println!(\"old\");\n}",
                "new_str": "fn main() {\n    println!(\"new\");\n}"
            }),
            call_id: "call-snap-2".into(),
        },
        decision_tx,
    }));
    // 连续下翻审阅参数区（只累计从头连续进入视口的行）。
    for _ in 0..6 {
        harness.key(KeyCode::Down);
    }
    harness.snapshot("permission-dialog-reviewed");
}

/// 回归（垂直边距引入后的分页错位）：分页预算按 `area.height - 2`
/// 计算，而 centered_rect 的垂直边距钳制实际只给 `area.height - 4`
/// ——弹窗比预算矮 2 行，页底两行渲染在框外，End 也翻不到底（用户
/// 实测：加上下边距前分页正确，加了就翻不到底）。不变量：分页预算
/// 必须与弹窗实际可用高度一致——End 跳到底后，最后一条参数行必须
/// 出现在屏幕上。预修复代码上本测试失败（末行被裁出框外）。
#[test]
fn permission_dialog_scrolling_reaches_the_last_argument_line() {
    let mut harness = Harness::trusted("snap-perm-scroll", 80, 24);
    let (decision_tx, _decision_rx) = mpsc::channel();
    // 27 行参数（write 预览头 1 行 + 内容 26 行）撑爆 24 行终端的
    // 弹窗预算，触发分页与钳制的交界。
    let mut content = String::new();
    for i in 0..25 {
        content.push_str(&format!("filler-{i:02}\n"));
    }
    content.push_str("LAST-LINE-XYZ\n");
    harness.event(UiEvent::Worker(WorkerMessage::PermissionRequest {
        request: PermissionRequest {
            tool: "write_file".into(),
            effect: ToolEffect::Write,
            reason: "writes a file".into(),
            arguments: json!({"path": "src/lib.rs", "content": content}),
            call_id: "call-scroll-1".into(),
        },
        decision_tx,
    }));
    // 先绘一帧让分页预算落地（argument_page_size/line_count 在绘制期
    // 计算），再连续下翻——审阅解锁按"逐帧连续进入视口"累计（真实
    // TUI 每个按键后必绘一帧），因此每个 Down 之间插入绘制。
    harness.project();
    for _ in 0..40 {
        harness.key(KeyCode::Down);
        harness.project();
    }
    let buffer = harness.terminal.backend().buffer();
    let mut screen = String::new();
    for y in 0..24u16 {
        for x in 0..80u16 {
            screen.push_str(buffer[(x, y)].symbol());
        }
    }
    assert!(
        screen.contains("LAST-LINE-XYZ"),
        "paging to the bottom must bring the final argument line into view"
    );
    // 操作行（Enter/Esc）在段尾：预算错位时被裁出框外——翻到底也
    // 看不到批准键，即用户报告的"翻不到底"。
    assert!(
        screen.contains("Enter / y — allow"),
        "paging to the bottom must expose the actions footer inside the dialog"
    );
}

#[test]
fn waiting_first_token_snapshot() {
    // 刷新 2026-08-19：阶段标签改整词呼吸（单一 span，无逐字光带）。

    let mut harness = Harness::trusted("snap-waiting", 80, 24);
    harness.run_event(RunEvent::ModelRequested {
        turn: 1,
        provider: "application-test".into(),
        model: "deterministic".into(),
    });
    harness.app.test_run_elapsed = Some(Duration::from_secs(3));
    harness.snapshot("phase-waiting");
}

#[test]
fn thinking_phase_snapshot() {
    // 刷新 2026-08-19：同 waiting——整词呼吸。

    let mut harness = Harness::trusted("snap-thinking", 80, 24);
    harness.run_event(RunEvent::ModelRequested {
        turn: 1,
        provider: "application-test".into(),
        model: "deterministic".into(),
    });
    harness.run_event(RunEvent::ModelStream {
        turn: 1,
        event: crate::model::ModelEvent::ReasoningDelta {
            delta: "hmm".into(),
        },
    });
    harness.app.test_phase_elapsed = Some(Duration::from_secs(5));
    harness.app.test_run_elapsed = Some(Duration::from_secs(8));
    harness.snapshot("phase-thinking");
}

#[test]
fn responding_phase_snapshot() {
    // 刷新 2026-08-19：同 waiting——整词呼吸。

    let mut harness = Harness::trusted("snap-responding", 80, 24);
    harness.run_event(RunEvent::ModelRequested {
        turn: 1,
        provider: "application-test".into(),
        model: "deterministic".into(),
    });
    harness.run_event(RunEvent::ModelStream {
        turn: 1,
        event: crate::model::ModelEvent::TextDelta {
            delta: "answer".into(),
        },
    });
    harness.app.test_phase_elapsed = Some(Duration::from_secs(42));
    harness.app.test_run_elapsed = Some(Duration::from_secs(61));
    harness.snapshot("phase-responding");
}

#[test]
fn executing_tools_phase_snapshot() {
    // 刷新 2026-08-19：同 waiting——整词呼吸。

    let mut harness = Harness::trusted("snap-executing", 80, 24);
    harness.run_event(RunEvent::ModelRequested {
        turn: 1,
        provider: "application-test".into(),
        model: "deterministic".into(),
    });
    harness.run_event(RunEvent::ToolRequested {
        call: crate::ToolCall {
            id: "call-1".into(),
            name: "read_file".into(),
            arguments: serde_json::json!({"path": "src/lib.rs"}),
        },
    });
    harness.app.test_phase_elapsed = Some(Duration::from_secs(2));
    harness.app.test_run_elapsed = Some(Duration::from_secs(65));
    harness.snapshot("phase-executing");
}

#[test]
fn model_picker_snapshot() {
    // 刷新 2026-08-19：空会话底层改画 LOGO 欢迎页（选择器后方）。
    // 刷新 2026-08-19：弹窗规范统一——黄边框 + 全屏 DIM 压暗（选择器
    // 此前无压暗，与背景同亮度）+ Clear 垫边。
    // 刷新 2026-08-19：宽度 94% → 84%，与其余弹窗统一（94% 在宽终端
    // 上每边仅 3% 边距，用户观感为贴墙）。
    // 刷新 2026-08-19：新增 Qwen Token Plan / Kimi Coding Plan 两个
    // 厂商（一级 4 厂商 + Custom）。
    // 刷新 2026-08-21：DeepSeek 二级新增第三款 deepseek-v4-flash-vision-exp
    //（官方 2026-08 上架的实验性多模态模型，参数见 presets.rs）。
    let mut harness = Harness::trusted("snap-model-picker", 80, 24);
    harness.type_text("/model");
    harness.key(KeyCode::Enter);
    harness.snapshot("model-picker");
}

/// 弹窗规范不变量：/model 选择器在任何终端宽度下都不得贴屏幕左右
/// 墙（每边至少 2 列）。起因：选择器/编辑器是仅有的 94% 宽弹窗，
/// 宽终端上每边仅留 3%，视觉上与贴墙无异（用户实测报告"撞墙"），
/// 与其余 84% 弹窗的观感不一致。弹窗角标按"Yellow 色 ┌/┐"识别
/// （底层会话面板边框无前景色，不会误判）。
#[test]
fn model_picker_never_touches_screen_edges() {
    for width in [80u16, 120, 200] {
        let mut harness = Harness::trusted("snap-picker-edges", width, 24);
        harness.type_text("/model");
        harness.key(KeyCode::Enter);
        harness.project();
        let buffer = harness.terminal.backend().buffer();
        let mut left = None;
        let mut right = None;
        for y in 0..24u16 {
            for x in 0..width {
                let cell = &buffer[(x, y)];
                let is_corner = matches!(cell.symbol(), "┌" | "┐")
                    && matches!(cell.style().fg, Some(ratatui::style::Color::Yellow));
                if is_corner {
                    left = Some(left.map_or(x, |l: u16| l.min(x)));
                    right = Some(right.map_or(x, |r: u16| r.max(x)));
                }
            }
        }
        let (left, right) = (
            left.expect("picker top-left corner"),
            right.expect("picker top-right corner"),
        );
        assert!(
            left >= 2 && width - right >= 3,
            "picker touches the screen edges at width {width}: left={left}, right={right}"
        );
    }
}

/// 工具卡三态（B5）：同一张已落定的 write_file 卡，内容行数超过折叠
/// 预算（6）以触发计数标记；被拒卡单独一景。
fn card_harness(tag: &str, visibility: ToolCardVisibility) -> Harness {
    let mut harness = Harness::trusted(tag, 80, 24);
    let mut conversation = ConversationModel::new();
    conversation.push_user("please write the file".into());
    conversation.open_tool_card(
        "call-1".into(),
        "write_file".into(),
        serde_json::json!({
            "path": "src/lib.rs",
            "content": "fn main() {\n    println!(\"a\");\n    println!(\"b\");\n    println!(\"c\");\n    println!(\"d\");\n    println!(\"e\");\n    println!(\"f\");\n    println!(\"g\");\n}\n"
        }),
    );
    conversation.settle_tool_card(
        "call-1",
        CardState::Settled {
            output: serde_json::json!("written 1 file"),
            is_error: false,
        },
    );
    conversation.push_assistant_for_test("write attempted");
    harness.app.conversation = conversation;
    harness.app.card_visibility = visibility;
    harness
}

#[test]
fn tool_card_collapsed_snapshot() {
    card_harness("snap-card-collapsed", ToolCardVisibility::Collapsed)
        // 刷新 2026-08-19：会话折行宽度 -1（滚动条列专属）。
        .snapshot("tool-card-collapsed");
}

#[test]
fn tool_card_expanded_snapshot() {
    card_harness("snap-card-expanded", ToolCardVisibility::Expanded).snapshot("tool-card-expanded");
}

#[test]
fn tool_card_hidden_snapshot() {
    // 刷新 2026-08-19：会话折行宽度 -1（同 tool-card-collapsed）。
    card_harness("snap-card-hidden", ToolCardVisibility::Hidden).snapshot("tool-card-hidden");
}

#[test]
fn tool_card_denied_snapshot() {
    let mut harness = Harness::trusted("snap-card-denied", 80, 24);
    let mut conversation = ConversationModel::new();
    conversation.push_user("please write the file".into());
    conversation.open_tool_card(
        "call-2".into(),
        "write_file".into(),
        serde_json::json!({"path": "src/lib.rs", "content": "fn main() {}"}),
    );
    conversation.settle_tool_card(
        "call-2",
        CardState::Denied {
            reason: "not allowed".into(),
        },
    );
    conversation.push_assistant_for_test("I could not write the file.");
    harness.app.conversation = conversation;
    // 刷新 2026-08-19：会话折行宽度 -1（同 tool-card-collapsed）。
    harness.snapshot("tool-card-denied");
}

#[test]
fn turn_end_notice_snapshot() {
    let mut harness = Harness::trusted("snap-turn-end", 80, 24);
    let mut conversation = ConversationModel::new();
    conversation.push_user("please write the file".into());
    conversation.push_assistant_for_test("write attempted");
    conversation.push_turn_end("completed".into());
    harness.app.conversation = conversation;
    // 刷新 2026-08-19：会话折行宽度 -1（同 tool-card-collapsed）。
    harness.snapshot("turn-end-notice");
}

#[test]
fn markdown_table_snapshot() {
    let mut harness = Harness::trusted("snap-table", 80, 24);
    let mut conversation = ConversationModel::new();
    conversation.push_user("summarize the files as a table".into());
    conversation.push_assistant_for_test(
        "Here is the breakdown:\n\n| file | lines | state |\n| --- | :---: | ---: |\n| a.rs | 12 | ok |\n| b.rs | 345 | **failing** |\n| c.md | 7 | ok |",
    );
    harness.app.conversation = conversation;
    // 刷新 2026-08-19：会话折行宽度 -1（同 tool-card-collapsed）。
    harness.snapshot("markdown-table");
}

#[test]
fn markdown_cjk_wrap_snapshot() {
    // 用户 2026-08-19 实测反馈的原文：无空格中文段在旧按空格分词的
    // wrap_styled 下被整段甩行，第一行提前断在 ~/.clat/sessions/ 后。
    // 钉住 CJK 断行 + 禁则的视觉基线。
    let mut harness = Harness::trusted("snap-cjk", 80, 24);
    let mut conversation = ConversationModel::new();
    conversation.push_user("介绍一下会话与持久化".into());
    conversation.push_assistant_for_test(
        "### 会话与持久化（挺有特色的部分）\n\n- 会话是 DSH 兼容的 append-only 日志：每段对话一个 zstd 分帧 JSONL 文件，在 ~/.clat/sessions/ 下；先写后做（第一条用户消息在调模型之前已落盘），中途崩溃恢复到上一个完整批次\n- 投影 checkpoint 放在日志旁边，重开很快；SQLite 只存控制面状态（模型、profile、信任、当前会话指针）\n- CLAT 日志和 DSH 工具互相可读——docs/research/ 里一堆 dsh-* 映射文档表明这个项目是从 DSH 迁移/对标演化来的",
    );
    harness.app.conversation = conversation;
    // 刷新 2026-08-19：会话折行宽度 -1（CJK 换行点整体前移一列，
    // 滚动条列自此不受宽字符字形铺入）。
    harness.snapshot("markdown-cjk-wrap");
}

#[test]
fn steering_badge_snapshot() {
    // 刷新 2026-08-19：流式前缀改太阳帧（◐ 灰色圆形，不再与状态栏
    // 盲文 spinner 重复）+ 标签整词呼吸。

    // 运行中排队插话：状态行 phase 之后挂 `steering·N` 徽标，输入框
    // 标题提示 Enter 插话 / Esc 取消；插话即刻以 dim pending 块出现在
    // 转录尾部（2026-08-21 可视化）。
    let mut harness = Harness::trusted("snap-steer-badge", 80, 24);
    harness.run_event(RunEvent::ModelRequested {
        turn: 1,
        provider: "application-test".into(),
        model: "deterministic".into(),
    });
    harness.run_event(RunEvent::ModelStream {
        turn: 1,
        event: crate::model::ModelEvent::TextDelta {
            delta: "working".into(),
        },
    });
    harness.app.running = true;
    harness
        .app
        .conversation
        .push_pending_steering("等一下，先别跑测试".into());
    harness
        .app
        .conversation
        .push_pending_steering("改跑 clippy".into());
    harness.app.test_phase_elapsed = Some(Duration::from_secs(3));
    harness.app.test_run_elapsed = Some(Duration::from_secs(5));
    harness.snapshot("steer-badge");
}

/// pending 插话可见 + ESC 栈式召回（2026-08-21）：插话以 dim 块 +
/// `· queued` 标记出现在转录尾部（流式 assistant 之后）；连按两次 ESC
/// 逐条召回（LIFO），回填按**发送顺序**换行排列（先发的想法靠前，
/// 见 `prepend_recalled_line`）——快照里输入框带回两行文本、徽标归零。
#[test]
fn steer_pending_and_recall_snapshot() {
    let mut harness = Harness::trusted("snap-steer-pending", 80, 24);
    harness.run_event(RunEvent::ModelRequested {
        turn: 1,
        provider: "application-test".into(),
        model: "deterministic".into(),
    });
    harness.run_event(RunEvent::ModelStream {
        turn: 1,
        event: crate::model::ModelEvent::TextDelta {
            delta: "working".into(),
        },
    });
    harness.app.running = true;
    harness
        .app
        .conversation
        .push_pending_steering("等一下，先别跑测试".into());
    harness
        .app
        .conversation
        .push_pending_steering("改跑 clippy 再跑测试".into());
    // 两次 ESC 栈式召回（core 侧召回语义由 application 测试钉住；快照
    // 钉呈现——区侧弹出 + prepend 回填与生产 ESC 分支同一段代码）。
    if let Some(text) = harness.app.conversation.recall_pending_steering() {
        harness.app.input.prepend_recalled_line(&text);
    }
    if let Some(text) = harness.app.conversation.recall_pending_steering() {
        harness.app.input.prepend_recalled_line(&text);
    }
    harness.snapshot("steer-pending-recall");
}

#[test]
fn steered_transcript_snapshot() {
    // 刷新 2026-08-19：标签整词呼吸。

    // 插话被 claim 后进转录（SteeringApplied → 用户块，与回放 UserMessage
    // 同一位点），模型继续回应。
    let mut harness = Harness::trusted("snap-steered", 80, 24);
    let mut conversation = ConversationModel::new();
    conversation.push_user("介绍一下会话设计".into());
    conversation
        .push_assistant_for_test("会话是 append-only 日志：先写后做，崩溃恢复到上一个完整批次。");
    harness.app.conversation = conversation;
    harness.run_event(RunEvent::SteeringApplied {
        text: "也讲讲投影 checkpoint".into(),
    });
    harness.run_event(RunEvent::ModelRequested {
        turn: 2,
        provider: "application-test".into(),
        model: "deterministic".into(),
    });
    harness.run_event(RunEvent::ModelStream {
        turn: 2,
        event: crate::model::ModelEvent::TextDelta {
            delta: "checkpoint 放在日志旁边，重开很快。".into(),
        },
    });
    // 刷新 2026-08-19：会话折行宽度 -1（同 tool-card-collapsed）。
    harness.snapshot("steered-transcript");
}

fn ask_question_fixture() -> crate::AskQuestion {
    crate::AskQuestion {
        question: "Which release channel should we ship?".into(),
        options: vec![
            crate::AskOption {
                label: "stable".into(),
                description: Some("recommended for production".into()),
            },
            crate::AskOption {
                label: "beta".into(),
                description: None,
            },
        ],
        allow_custom: true,
    }
}

#[test]
fn ask_dialog_options_snapshot() {
    // 选项模式：游标落在 beta（按过一次 ↓），选中行高亮、描述 dim、
    // 末尾自定义入口、脚注键位。刷新 2026-08-19：底层空会话改画
    // LOGO 欢迎页（弹窗覆盖其上）。
    // 刷新 2026-08-19：ask 弹窗与权限弹窗同享全屏 DIM 压暗层（同为
    // 异步模态，同因可被忽略）。
    // 刷新 2026-08-19：弹窗 Clear 垫边（同 permission-dialog）。
    // 刷新 2026-08-19：弹窗规范统一——黄边框 + 四边垫边（同
    // permission-dialog）。
    let mut harness = Harness::trusted("snap-ask-options", 80, 24);
    let (answer_tx, _answer_rx) = mpsc::channel();
    harness.event(UiEvent::Worker(WorkerMessage::AskUserRequest {
        question: ask_question_fixture(),
        answer_tx,
    }));
    harness.event(UiEvent::Terminal(Event::Key(KeyEvent::new(
        KeyCode::Down,
        KeyModifiers::NONE,
    ))));
    harness.snapshot("ask-dialog-options");
}

/// /help 帮助弹窗（2026-08-19：原先是状态栏一行长文本，已长到与
/// 右侧 Token/Cache/Context 遥测重叠）。弹窗规范同其余弹窗：黄框 +
/// 背景压暗 + 四边边距；内容超可视高度时滚动，脚注提示还有下文。
/// 本场景同时验证模态门控（按键被吞）与翻页（Down 推进滚动位）。
#[test]
fn help_dialog_snapshot_and_paging() {
    let mut harness = Harness::trusted("snap-help", 80, 24);
    harness.type_text("/help");
    harness.key(KeyCode::Enter);
    assert!(harness.app.info_dialog.is_some(), "the help dialog opens");
    harness.project();
    assert!(
        harness.app.info_scroll_max > 0,
        "the help content overflows one page at 80x24 (paging is real)"
    );

    // 模态门控：弹窗期间普通按键进不了输入框。
    harness.type_text("abc");
    assert!(!harness.app.input.visual_rows(60).join("").contains('a'));

    // 翻页：Down 推进滚动位并钳制在最大值；Esc 关闭并交还输入。
    harness.key(KeyCode::Down);
    harness.snapshot("help-dialog");
    let max = harness.app.info_scroll_max;
    for _ in 0..max + 5 {
        harness.key(KeyCode::Down);
    }
    assert_eq!(
        harness.app.info_dialog.as_ref().map(|dialog| dialog.offset),
        Some(max),
        "scroll clamps at the end"
    );
    harness.key(KeyCode::Esc);
    assert!(harness.app.info_dialog.is_none(), "Esc closes the dialog");
    harness.type_text("hi");
    assert!(
        harness.app.input.visual_rows(60).join("").contains("hi"),
        "input unlocks after the dialog closes"
    );
}

/// /mcp 状态弹窗（2026-08-19）：命令打开 → 弹窗渲染挂载期的
/// `McpStatusDto`。场景先走真实命令路径（空 MCP 配置 → 0/0 概览），
/// 再注入富 DTO（两台服务器 + 一条失败含 stderr 尾部）锁定排版：
/// 快照不拉真实子进程，注入的是 Application 层同型的纯数据。同时
/// 验证 `r` 刷新复位滚动与模态门控。
#[test]
fn mcp_dialog_snapshot_and_refresh() {
    let mut harness = Harness::trusted("snap-mcp", 80, 24);
    harness.type_text("/mcp");
    harness.key(KeyCode::Enter);
    assert!(
        harness
            .app
            .info_dialog
            .as_ref()
            .is_some_and(|dialog| dialog.kind == super::InfoDialogKind::Mcp),
        "the /mcp command opens the MCP dialog"
    );
    // 未配置任何服务器：概览行 0/0，无服务器/失败节，高度内容驱动。
    assert_eq!(
        harness.app.mcp_view.as_ref().map(|view| view.configured),
        Some(0)
    );

    // 注入富视图锁定排版（服务器行 + 折行的失败条目）。
    harness.app.mcp_view = Some(fake_mcp_view());
    harness.project();
    harness.snapshot("mcp-dialog");

    // 模态门控：字母进不了输入框（r 例外——它刷新而不是输入）。
    harness.type_text("x");
    assert!(!harness.app.input.visual_rows(60).join("").contains('x'));

    // 翻页后 `r` 刷新：滚动复位、视图被重取（回到真实的空状态）。
    for _ in 0..5 {
        harness.key(KeyCode::Down);
    }
    harness.key(KeyCode::Char('r'));
    assert!(
        harness
            .app
            .info_dialog
            .as_ref()
            .is_some_and(|dialog| dialog.offset == 0),
        "refresh resets the scroll offset"
    );
    assert_eq!(
        harness.app.mcp_view.as_ref().map(|view| view.servers.len()),
        Some(0),
        "refresh pulls the live (empty) status back over the injected view"
    );
    harness.key(KeyCode::Esc);
    assert!(harness.app.info_dialog.is_none(), "Esc closes the dialog");
}

/// 快照专用富 MCP 视图：与 Application DTO 同型的纯数据（不拉子进程）。
fn fake_mcp_view() -> crate::McpStatusDto {
    crate::McpStatusDto {
        configured: 3,
        connected: 2,
        connecting: 0,
        failures: vec![
            "mcp `broken-server`: MCP negotiation failed: modern discover: timed out; legacy initialize: timed out | npx ERR! code E404 | last lines of stderr kept here".to_owned(),
        ],
        servers: vec![
            crate::McpServerInfoDto {
                name: "glm-web-search".to_owned(),
                server_version: "1.9.0".to_owned(),
                protocol_version: "2025-06-18".to_owned(),
                tools: 1,
                transport: "http".to_owned(),
            },
            crate::McpServerInfoDto {
                name: "glm-vision".to_owned(),
                server_version: "0.4.2".to_owned(),
                protocol_version: "2024-11-05".to_owned(),
                tools: 3,
                transport: "stdio".to_owned(),
            },
        ],
    }
}

/// `/perm` 选择器：三档列表 + 当前档标记 + 输入框右标题的模式名。
/// `/permission` 长名是同义别名（同臂断言）。确认子态见
/// `permission_confirm_full_snapshot`。
#[test]
fn permission_picker_snapshot() {
    let mut harness = Harness::trusted("snap-perm-picker", 80, 24);
    harness.type_text("/perm");
    harness.key(KeyCode::Enter);
    assert!(
        harness.app.permission_picker.is_some(),
        "the /perm command opens the picker"
    );
    // 模态门控：字母进不了输入框。
    harness.type_text("x");
    assert!(!harness.app.input.visual_rows(60).join("").contains('x'));
    harness.snapshot("permission-picker");
    harness.key(KeyCode::Esc);
    assert!(harness.app.permission_picker.is_none(), "Esc closes it");
    // 长名别名同样打开。
    harness.type_text("/permission");
    harness.key(KeyCode::Enter);
    assert!(
        harness.app.permission_picker.is_some(),
        "the long alias /permission opens the same picker"
    );
}

/// 选 Full Access 的确认子态（P4）：风险文案 + 二次 Enter。
#[test]
fn permission_confirm_full_snapshot() {
    let mut harness = Harness::trusted("snap-perm-full", 80, 24);
    harness.type_text("/perm");
    harness.key(KeyCode::Enter);
    harness.key(KeyCode::Down);
    harness.key(KeyCode::Down);
    harness.key(KeyCode::Enter);
    assert!(
        harness.app.permission_picker.is_some(),
        "the first Enter only arms the confirmation, not an apply"
    );
    harness.snapshot("permission-confirm-full");
    harness.key(KeyCode::Enter);
    assert!(
        harness.app.permission_picker.is_none(),
        "the second Enter applies Full Access"
    );
    assert_eq!(
        harness
            .app
            .application
            .as_ref()
            .map(|application| application.permission_mode()),
        Some(crate::PermissionMode::FullAccess)
    );
}

/// Read Only 下的权限弹框（P5）：Write 类调用 offered `w`（切 Project
/// Write）与 `f`（切 Full Access）两个升级键。审阅到尾后动作行显示
/// 两个升级提示。
#[test]
fn permission_dialog_escalate_snapshot() {
    let mut harness = Harness::trusted("snap-perm-escalate", 80, 24);
    harness
        .app
        .application
        .as_ref()
        .expect("application")
        .set_permission_mode(crate::PermissionMode::ReadOnly)
        .expect("persist mode");
    let (decision_tx, _decision_rx) = mpsc::channel();
    harness.event(UiEvent::Worker(WorkerMessage::PermissionRequest {
        request: PermissionRequest {
            tool: "write_file".into(),
            effect: ToolEffect::Write,
            reason: "writes a file".into(),
            arguments: json!({
                "path": "src/lib.rs",
                "content": "fn main() {}\n"
            }),
            call_id: "call-snap-esc".into(),
        },
        decision_tx,
    }));
    // 审阅到最后一行解锁动作行（Write 预览很短，几次 Down 即到底）。
    for _ in 0..6 {
        harness.key(KeyCode::Down);
    }
    harness.snapshot("permission-dialog-escalate");
}

/// 升级键的端到端行为（P5）：RO 下审阅完按 `w` = 档位已切 + 本次调用
/// 收到 Allow。pre-fix（无升级键）上 'w' 不产生任何效果，断言必红。
#[test]
fn permission_dialog_escalation_key_switches_mode_and_allows() {
    let mut harness = Harness::trusted("perm-escalate-key", 80, 24);
    harness
        .app
        .application
        .as_ref()
        .expect("application")
        .set_permission_mode(crate::PermissionMode::ReadOnly)
        .expect("persist mode");
    let (decision_tx, decision_rx) = mpsc::channel();
    harness.event(UiEvent::Worker(WorkerMessage::PermissionRequest {
        request: PermissionRequest {
            tool: "write_file".into(),
            effect: ToolEffect::Write,
            reason: "writes a file".into(),
            arguments: json!({"path": "src/lib.rs", "content": "fn main() {}\n"}),
            call_id: "call-esc-key".into(),
        },
        decision_tx,
    }));
    // 审阅到尾（reviewed_to_end 在绘制期计算，先画一帧）再按升级键。
    for _ in 0..6 {
        harness.key(KeyCode::Down);
    }
    harness.project();
    assert!(
        harness
            .app
            .pending_permission
            .as_ref()
            .is_some_and(|pending| pending.reviewed_to_end),
        "the review gate is open after scrolling through"
    );
    harness.key(KeyCode::Char('w'));
    assert!(
        harness.app.pending_permission.is_none(),
        "the escalation key resolves the dialog"
    );
    assert_eq!(
        decision_rx.recv().expect("decision"),
        crate::PermissionDecision::Allow,
        "escalating answers the pending call with Allow"
    );
    assert_eq!(
        harness
            .app
            .application
            .as_ref()
            .expect("application")
            .permission_mode(),
        crate::PermissionMode::ProjectWrite,
        "the mode cell switched before the call was allowed"
    );
}

/// 对抗审计（2026-08-19）：权限弹框的决策键必须是裸键。raw 模式下
/// Ctrl+W / Ctrl+Y / Alt+N 以 `Char(..)` + 修饰位到达——不挡住它们，
/// Ctrl+W 就成了"切档并放行"。pre-fix（无修饰守卫）上本测试必红：
/// 对话框被 Ctrl+W 解决且档位被切换。
#[test]
fn permission_dialog_decision_keys_require_plain_modifiers() {
    let mut harness = Harness::trusted("perm-plain-keys", 80, 24);
    harness
        .app
        .application
        .as_ref()
        .expect("application")
        .set_permission_mode(crate::PermissionMode::ReadOnly)
        .expect("persist mode");
    let (decision_tx, decision_rx) = mpsc::channel();
    harness.event(UiEvent::Worker(WorkerMessage::PermissionRequest {
        request: PermissionRequest {
            tool: "write_file".into(),
            effect: ToolEffect::Write,
            reason: "writes a file".into(),
            arguments: json!({"path": "src/lib.rs", "content": "fn main() {}\n"}),
            call_id: "call-plain-keys".into(),
        },
        decision_tx,
    }));
    // 审阅到尾解锁动作行。
    for _ in 0..6 {
        harness.key(KeyCode::Down);
    }
    harness.project();
    // Ctrl+W：不切档、不解决对话框、无决策。
    harness.event(UiEvent::Terminal(Event::Key(KeyEvent::new(
        KeyCode::Char('w'),
        KeyModifiers::CONTROL,
    ))));
    assert!(
        harness.app.pending_permission.is_some(),
        "Ctrl+W must not escalate"
    );
    assert!(decision_rx.try_recv().is_err(), "Ctrl+W sends no decision");
    assert_eq!(
        harness
            .app
            .application
            .as_ref()
            .expect("application")
            .permission_mode(),
        crate::PermissionMode::ReadOnly,
        "Ctrl+W must not switch the mode"
    );
    // Ctrl+Y（修饰版 allow）与 Alt+N（修饰版 deny）同样无效。
    harness.event(UiEvent::Terminal(Event::Key(KeyEvent::new(
        KeyCode::Char('y'),
        KeyModifiers::CONTROL,
    ))));
    assert!(
        harness.app.pending_permission.is_some(),
        "Ctrl+Y must not allow"
    );
    harness.event(UiEvent::Terminal(Event::Key(KeyEvent::new(
        KeyCode::Char('n'),
        KeyModifiers::ALT,
    ))));
    assert!(
        harness.app.pending_permission.is_some(),
        "Alt+N must not deny"
    );
    // 对照组：裸 'w' 照常工作。
    harness.key(KeyCode::Char('w'));
    assert!(
        harness.app.pending_permission.is_none(),
        "the plain key still works"
    );
    assert_eq!(
        decision_rx.recv().expect("decision"),
        crate::PermissionDecision::Allow
    );
}

/// 对抗审计（2026-08-19）：选择器的 Enter/Esc 只认裸键。CLAT 开着
/// keyboard-enhancement，Shift+Enter 独立到达——主输入里它是换行肌肉
/// 记忆，在选择器里不得套用选择（更不得进入 FA 确认）。判定探针：
/// 若 Shift+Enter 错误地进入了确认子态，随后的裸 Esc 只会退出确认、
/// 选择器仍开着；正确行为下 Esc 直接关掉选择器。
#[test]
fn permission_picker_enter_requires_a_plain_key() {
    let mut harness = Harness::trusted("perm-picker-plain", 80, 24);
    harness.type_text("/perm");
    harness.key(KeyCode::Enter);
    assert!(harness.app.permission_picker.is_some());
    // 选中 Full Access 行（第三行）。
    harness.key(KeyCode::Down);
    harness.key(KeyCode::Down);
    // Shift+Enter：不进确认子态、不套用。
    harness.event(UiEvent::Terminal(Event::Key(KeyEvent::new(
        KeyCode::Enter,
        KeyModifiers::SHIFT,
    ))));
    assert!(
        harness.app.permission_picker.is_some(),
        "Shift+Enter does not apply the selection"
    );
    assert_eq!(
        harness
            .app
            .application
            .as_ref()
            .expect("application")
            .permission_mode(),
        crate::PermissionMode::ProjectWrite,
        "Shift+Enter changed nothing"
    );
    // 探针：若 Shift+Enter 已错误进入确认子态，Esc 只退确认、选择器
    // 仍开；正确行为下 Esc 关掉整个选择器。
    harness.key(KeyCode::Esc);
    assert!(
        harness.app.permission_picker.is_none(),
        "Shift+Enter must not have armed the Full Access confirm state"
    );
    // 重开：裸 Enter 才进入 FA 确认子态，再按一次生效（P4）。
    harness.type_text("/perm");
    harness.key(KeyCode::Enter);
    harness.key(KeyCode::Down);
    harness.key(KeyCode::Down);
    harness.key(KeyCode::Enter);
    harness.key(KeyCode::Enter);
    assert!(
        harness.app.permission_picker.is_none(),
        "plain Enter twice confirms Full Access"
    );
    assert_eq!(
        harness
            .app
            .application
            .as_ref()
            .expect("application")
            .permission_mode(),
        crate::PermissionMode::FullAccess
    );
}

/// 会话右标题（N1/N2）：TitleUpdated 事件到达 → 对话框 block 右上角
/// 显示标题，左上角 Conversation 保持。
#[test]
fn session_title_snapshot() {
    let mut harness = Harness::trusted("snap-session-title", 80, 24);
    harness.event(UiEvent::Application(
        crate::ApplicationEvent::TitleUpdated {
            title: "Fix the Safari login crash".to_owned(),
        },
    ));
    harness.snapshot("session-title");
    assert_eq!(
        harness.app.session_title.as_deref(),
        Some("Fix the Safari login crash"),
        "the title event updates the display state without a snapshot pull"
    );
}

/// /rename 弹框（渲染快照）：预填当前标题 + 真实光标 + 页脚键位。
/// 门槛与提交行为在 tui.rs 内联测试覆盖；这里直接置入弹框状态锁定
/// 排版（与 fake_mcp_view 同款手法）。
#[test]
fn rename_dialog_snapshot() {
    let mut harness = Harness::trusted("snap-rename", 80, 24);
    harness.app.session_title = Some("Fix the Safari login crash".to_owned());
    harness.app.rename_dialog = Some(super::RenameDialog::new("Fix the Safari login crash"));
    harness.project();
    // 模态门控：字母进不了主输入框（进了弹框的编辑器）。
    harness.type_text("X");
    assert!(!harness.app.input.visual_rows(60).join("").contains('X'));
    assert!(
        harness
            .app
            .rename_dialog
            .as_ref()
            .is_some_and(|dialog| dialog.buffer.text().ends_with('X')),
        "typing edits the rename buffer"
    );
    harness.snapshot("rename-dialog");
}

/// /rename 门槛拒绝（N4）：模型尚未命名（无显式标题事件）时 flash 提示，
/// 不开弹框。
#[test]
fn rename_not_named_snapshot() {
    let mut harness = Harness::trusted("snap-rename-gate", 80, 24);
    harness.type_text("/rename");
    harness.key(KeyCode::Enter);
    assert!(
        harness.app.rename_dialog.is_none(),
        "the gate refuses to open the dialog before the model names the session"
    );
    harness.snapshot("rename-not-named");
}

#[test]
fn ask_dialog_custom_snapshot() {
    // 自定义输入模式：`c` 进入、键入 canary、下划线标示输入位。
    // 刷新 2026-08-19：底层空会话改画 LOGO 欢迎页。
    // 刷新 2026-08-19：全屏 DIM 压暗层（同 ask-dialog-options）。
    // 刷新 2026-08-19：弹窗 Clear 垫边（同 permission-dialog）。
    // 刷新 2026-08-19：弹窗规范统一（黄边框 + 四边垫边）。
    let mut harness = Harness::trusted("snap-ask-custom", 80, 24);
    let (answer_tx, _answer_rx) = mpsc::channel();
    harness.event(UiEvent::Worker(WorkerMessage::AskUserRequest {
        question: ask_question_fixture(),
        answer_tx,
    }));
    harness.event(UiEvent::Terminal(Event::Key(KeyEvent::new(
        KeyCode::Char('c'),
        KeyModifiers::NONE,
    ))));
    for ch in "canary".chars() {
        harness.event(UiEvent::Terminal(Event::Key(KeyEvent::new(
            KeyCode::Char(ch),
            KeyModifiers::NONE,
        ))));
    }
    harness.snapshot("ask-dialog-custom");
}

#[test]
fn ask_dialog_answers_selection_custom_and_decline() {
    // S9 端到端：Enter 回传所选标签；c+输入+Enter 回传自定义；Esc 回传
    // 拒绝；对话框独占期间按键不落进主输入框。
    let mut harness = Harness::trusted("ask-dialog-flow", 80, 24);

    let (answer_tx, answer_rx) = mpsc::channel();
    harness.event(UiEvent::Worker(WorkerMessage::AskUserRequest {
        question: ask_question_fixture(),
        answer_tx,
    }));
    // 对话框打开时输入的字符不得进入主输入框。
    harness.event(UiEvent::Terminal(Event::Key(KeyEvent::new(
        KeyCode::Char('x'),
        KeyModifiers::NONE,
    ))));
    harness.event(UiEvent::Terminal(Event::Key(KeyEvent::new(
        KeyCode::Enter,
        KeyModifiers::NONE,
    ))));
    assert!(matches!(answer_rx.recv(), Ok(crate::AskAnswer::Selected(label)) if label == "stable"));
    assert!(harness.app.pending_ask_user.is_none());
    assert_eq!(harness.app.input.take(), "");

    let (answer_tx, answer_rx) = mpsc::channel();
    harness.event(UiEvent::Worker(WorkerMessage::AskUserRequest {
        question: ask_question_fixture(),
        answer_tx,
    }));
    harness.event(UiEvent::Terminal(Event::Key(KeyEvent::new(
        KeyCode::Char('c'),
        KeyModifiers::NONE,
    ))));
    for ch in "ship friday".chars() {
        harness.event(UiEvent::Terminal(Event::Key(KeyEvent::new(
            KeyCode::Char(ch),
            KeyModifiers::NONE,
        ))));
    }
    harness.event(UiEvent::Terminal(Event::Key(KeyEvent::new(
        KeyCode::Enter,
        KeyModifiers::NONE,
    ))));
    assert!(
        matches!(answer_rx.recv(), Ok(crate::AskAnswer::Custom(text)) if text == "ship friday")
    );

    let (answer_tx, answer_rx) = mpsc::channel();
    harness.event(UiEvent::Worker(WorkerMessage::AskUserRequest {
        question: ask_question_fixture(),
        answer_tx,
    }));
    harness.event(UiEvent::Terminal(Event::Key(KeyEvent::new(
        KeyCode::Esc,
        KeyModifiers::NONE,
    ))));
    assert!(matches!(answer_rx.recv(), Ok(crate::AskAnswer::Declined)));
    assert!(harness.app.pending_ask_user.is_none());
}

#[test]
fn ctrl_c_copies_a_selection_instead_of_quitting() {
    let mut harness = Harness::trusted("ctrl-c-copy", 80, 24);
    let mut conversation = ConversationModel::new();
    conversation.push_user("select me: the quick brown fox".into());
    harness.app.conversation = conversation;
    // 先绘一帧记录会话区矩形，再拖选。
    harness.project();
    let area = harness.app.conversation_area;
    harness.drag_select(area.x + 2, area.y + 1, area.x + 14, area.y + 1);
    harness.event(UiEvent::Terminal(Event::Key(KeyEvent::new(
        KeyCode::Char('c'),
        KeyModifiers::CONTROL,
    ))));
    assert!(
        !harness.app.should_quit,
        "Ctrl+C with a live selection must copy, not quit"
    );
    assert!(
        harness.app.status.contains("copied"),
        "status should confirm the copy: {}",
        harness.app.status
    );
    // 无选区时 Ctrl+C 仍是退出。
    harness.app.selection = None;
    harness.event(UiEvent::Terminal(Event::Key(KeyEvent::new(
        KeyCode::Char('c'),
        KeyModifiers::CONTROL,
    ))));
    assert!(harness.app.should_quit, "Ctrl+C without a selection quits");
}

#[test]
fn finishing_a_drag_copies_the_selection_immediately() {
    // 选中即复制（2026-08-19 按用户决策恢复）：拖选松开即写剪贴板并
    // 闪现 copied 提示；高亮保留，Ctrl+C 仍是显式重试路径。
    let mut harness = Harness::trusted("drag-copy", 80, 24);
    let mut conversation = ConversationModel::new();
    conversation.push_user("select me: the quick brown fox".into());
    harness.app.conversation = conversation;
    harness.project();
    let area = harness.app.conversation_area;
    harness.drag_select(area.x + 2, area.y + 1, area.x + 14, area.y + 1);
    assert!(
        harness.app.status.contains("copied"),
        "drag completion must copy immediately: {}",
        harness.app.status
    );
    assert!(
        harness.app.selection.is_some(),
        "the highlight stays for the Ctrl+C retry path"
    );
}

/// 图片附件徽标（M6）：拖图进终端 = 粘贴整条绝对路径 → 输入框顶部
/// 出现附件行（📷 文件名），不进文本；Esc 连同输入一起清空。
#[test]
fn attachment_chip_snapshot() {
    let mut harness = Harness::trusted("snap-attach", 80, 24);
    let image = harness.project_root.join("probe-shot.png");
    std::fs::write(&image, b"png").unwrap();
    harness.event(UiEvent::Terminal(Event::Paste(image.display().to_string())));
    assert_eq!(
        harness.app.attachments.len(),
        1,
        "the pasted path became an attachment"
    );
    assert!(
        harness.app.input.visual_rows(60).join("").is_empty(),
        "the path itself never lands in the text buffer"
    );
    harness.type_text("what is in this picture");
    harness.snapshot("attachment-chip");
    // Esc 清空输入连同附件。
    harness.key(KeyCode::Esc);
    assert!(
        harness.app.attachments.is_empty(),
        "Esc drops the attachment"
    );
}
