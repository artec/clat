//! 语义终端快照测试（phase-1 P0-1）。
//!
//! 快照对象是语义投影——每行 `(文本, 非默认样式区间)` 加光标/滚动状态
//! 头注记，不是像素也不是事件序列（G1）。动画确定性由 App 上的测试钩
//! （`test_freeze_tick` / `test_thinking_elapsed`）保证；同一输入序列连
//! 绘两次必须产生同一投影。期望文件在 `tests/fixtures/tui-snapshots/`，
//! 场景与文件一一对应（无孤儿文件）；刷新用
//! `CLAT_REFRESH_SNAPSHOTS=1 cargo test`，每次刷新必须逐一说明原因。

use super::App;
use crate::test_support::{TestBehavior, TestProviderPlugin, roots};
use crate::tui_conversation::{CardState, ConversationModel, ToolCardVisibility};
use crate::tui_worker::{UiEvent, WorkerMessage};
use crate::{BootstrapApplication, PermissionRequest, Project, RunEvent, ToolEffect};
use crossterm::event::{
    Event, KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use ratatui::Terminal;
use ratatui::backend::{Backend, TestBackend};
use serde_json::json;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::Duration;

/// 场景注册表：`snapshot_files_form_a_closed_set` 据此校验目录无孤儿文件。
const SCENARIOS: &[&str] = &[
    "idle-transcript-80",
    "idle-transcript-40",
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
    "steered-transcript",
    "ask-dialog-options",
    "ask-dialog-custom",
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
        App::open(Project::new(&project_root), Some(storage_root.clone())).expect("app opens");
    app.test_freeze_tick = true;
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

/// 临时目录路径在投影中归一化为占位符，保证快照可移植。两道防线：
/// 完整路径替换 + 裸纳秒数字串替换——路径在窄对话框里换行时，完整
/// 路径字符串被拆散，只有数字串归一仍能兜住（位数恒定→换行位置恒定）。
/// 临时目录路径在投影中归一化为占位符，保证快照可移植。两道防线：
/// 完整路径替换 + 投影级数字串归一——路径在窄对话框里换行时会把纳秒
/// 串从中间拆开（位数恒定 → 换行位置恒定，但裸数字串本身必须归一）。
fn normalize_paths(projection: &str, project_root: &Path) -> String {
    mask_long_digit_runs(&projection.replace(project_root.to_string_lossy().as_ref(), "<ROOT>"))
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
    let mut harness = Harness::trusted("snap-idle-80", 80, 24);
    harness.snapshot("idle-transcript-80");
}

#[test]
fn idle_transcript_narrow() {
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
    harness.snapshot("conversation-with-messages");
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
    harness.snapshot("selection-highlight");
}

#[test]
fn trust_dialog_snapshot() {
    let mut harness = Harness::untrusted("snap-trust");
    harness.snapshot("trust-dialog");
}

#[test]
fn permission_dialog_snapshot() {
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

#[test]
fn waiting_first_token_snapshot() {
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
    let mut harness = Harness::trusted("snap-model-picker", 80, 24);
    harness.type_text("/model");
    harness.key(KeyCode::Enter);
    harness.snapshot("model-picker");
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
        .snapshot("tool-card-collapsed");
}

#[test]
fn tool_card_expanded_snapshot() {
    card_harness("snap-card-expanded", ToolCardVisibility::Expanded).snapshot("tool-card-expanded");
}

#[test]
fn tool_card_hidden_snapshot() {
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
    harness.snapshot("markdown-cjk-wrap");
}

#[test]
fn steering_badge_snapshot() {
    // 运行中排队插话：状态行 phase 之后挂 `steering·N` 徽标，输入框
    // 标题提示 Enter 插话 / Esc 取消。
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
    harness.app.steering_queued = 2;
    harness.app.test_phase_elapsed = Some(Duration::from_secs(3));
    harness.app.test_run_elapsed = Some(Duration::from_secs(5));
    harness.snapshot("steer-badge");
}

#[test]
fn steered_transcript_snapshot() {
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
    // 末行自定义入口、脚注键位。
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

#[test]
fn ask_dialog_custom_snapshot() {
    // 自定义输入模式：`c` 进入、键入 canary、下划线标示输入位。
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
