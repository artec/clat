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
use crate::dsh::backend::{DshEvent, DshTask, TaskReply};
use crate::test_support::{LiveGlmProviderPlugin, TestBehavior, TestProviderPlugin, roots};
use crate::tui::conversation::{CardState, ConversationModel, ToolCardVisibility};
use crate::tui::dialogs::RenameDialog;
use crate::tui::dsh_events::DshState;
use crate::tui::permission_picker::PermissionPicker;
use crate::tui::session_picker::SessionPicker;
use crate::tui::worker::{SteeringAdmissionFinished, UiEvent, WorkerMessage};
use crate::{BootstrapApplication, ModelEvent, PermissionRequest, Project, RunEvent, ToolEffect};
use crossterm::event::{
    Event, KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use ratatui::Terminal;
use ratatui::backend::{Backend, TestBackend};
use serde_json::json;
use std::path::{Path, PathBuf};
use std::sync::{Arc, mpsc};
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
/// 2026-08-23 批量刷新（34 个既有场景，D-2 闪光点吸收 · 设计档案 §4
/// 负责人拍板的三项风格改进）：① 标题栏标识加 ◆ 前缀且模型名段改
/// 主题蓝 `Role::ModelAccent`；② 全部边框 title 前后空一格
/// （popup_block 构造器内聚，含 Conversation/Message/权限档位角标）；
/// ③ 其余零变化（INV-U8 例外清单只此三项）。同日二次刷新：◆ 菱形
/// 前缀按负责人 dogfood 反馈撤下（"好丑"），local 标识回到裸 `CLAT`。
/// 同日三次刷新（1 个场景）：dsh-model-picker——标题栏模型标签从裸
/// id（`deepseek · deepseek-chat`）升级为展示名（`DeepSeek ·
/// DeepSeek Chat`，名字目录 prime 解析，负责人 dogfood 第三轮反馈）。
/// 同日四次刷新与新增（档位接入）：dsh-model-picker 再刷——fixture 补
/// reasoning.efforts + current.reasoningEffort，标题栏出现
/// `· Thinking · High` 档位段、当前模型行常显档位、footer 提示
/// ⇧Tab；新场景 dsh-model-effort——二级高亮行 Shift+Tab 后行内呈现
/// pending 档位（`fast general model · max ●`）。
/// 2026-09-02 批量刷新与新增（SC 组技能与命令面，A1/A2/A3 裁定）：
/// help-dialog 重钉——命令节按权威顺序表十五行重排（会话→上下文→
/// 模型→安全→扩展→实验→元；/mem /sub 成为主名，/skill 落扩展组），
/// 并新增前端本地 Composer 节（/attach /paste-image /attachments clear，
/// INV-SC-3）；context-dialog 重钉——新增 "Invoked skill" 估算行（SC-2，
/// 未武装时为 0）；新场景 skills-dialog——/skill 列表弹窗（bundled 五条
/// 含 grill-me，来源层与 requires-execution 呈现）。
/// 2026-09-02 三次刷新（TC 组 Tencent Hy 接入）：model-picker 与
/// model-editor-escape 两场景重钉——一级列表新增 Tencent 厂商行
/// （五厂商 + Custom = 6 行，hy4-preview 条目）。
/// 同日四次刷新（TC-2 口径修正）：model-picker 再钉——Tencent 一级
/// 行更名 "Hy Token Plan"（归队计划名命名模式，负责人二次裁定）。
/// 2026-09-02 五次刷新与新增（CP-2 帮助归位与命令短名，A4/A5/A6）：
/// 新场景 help-dialog-end——首页快照只锁命令节，尾页（滚动钳制位）
/// 补锁 Composer/Keys 节；help-dialog 本身零变化（命令节未动）。
/// Composer 节改三行短主名（/pi, /paste-image；/ac, /attach-clear,
/// /attachments clear），Ctrl+V 归位 Keys 节（A4 四组 11 行：输入与
/// 提交 / 运行控制 / 浏览与显示 / 选择与复制）。
/// 2026-09-02 六次刷新（CP-3 弹窗守卫收窄为仅横向）：21 个含弹窗
/// 场景重钉——clear_popup_with_guards 不再上下各扩一行，弹框上/下
/// 紧邻行恢复显示压暗底层 UI（此前被守卫清成空白；每场景恰两行、
/// 框内零变化）。CP-4 同批：help-dialog-end 尾页 Composer 行三名改
/// 两名（/ac, /attach-clear——退役全拼不再宣传）。
/// 2026-09-03 刷新（VP-2 内置矩阵终态）：model-picker 与
/// model-picker-vendor 两场景重钉——Qwen Token Plan 一级行 "1 models"
/// → "2 models"（增量预设 qwen3.8-flash），二级列表新增 flash 行
/// （Qwen3.8 Max + Qwen3.8 Flash 两模型）。
/// 2026-09-03 刷新（VP-3）：model-picker / model-picker-vendor 增加本地
/// `⧉` 图片能力标识与图例；model-picker、permission-picker、dsh-resume-
/// picker、dsh-model-picker、dsh-permission-picker 的导航行改为全内宽
/// REVERSED，当前项 `✓` 固定最右列。能力未知的 dsh 模型行不显示 `⧉`。
/// dsh-model-effort 同步当前项新几何；attachment-failure-restore 的本地
/// 已验证测试模型标题同步显示 `⧉`。
/// 2026-09-03 二次刷新（VP-3 返工二轮→四轮定稿，负责人三条）：①`✓`
/// **永远紧贴名称之前的固定列**——锚定名称、不绑定行首：有数字列
/// （model/session picker）为 `1 ✓ 名称`（数字列之后、名称列之前），
/// 无数字列（permission 等）为 `✓ 名称`；未选中该列留空；字形
/// U+2713 纯文本形。②名称列放宽为舒适定宽 40 列（名称+⧉ 单元整体
/// 省略号截断，内置名永不截断），hint 从固定列起排；③图例撤独立行
/// 并入说明行行尾（`· ⧉ images`），/help 不加图例。受影响快照：
/// model-picker、model-picker-vendor、dsh-resume-picker、
/// dsh-model-picker、dsh-model-effort（permission-picker 无数字列、
/// 几何不变未动）。
/// 2026-09-04 三次刷新（审计 F-1 修复 + 负责人五轮微调，审计
/// `docs/audit/2026-09-04-vp3-rework2-vp4-review.md`）：①local /model
/// 弹框高度回 rows+4——`chrome_height` 遗留返工一轮的 5 造成内容与
/// 说明行之间**双空行**（model-picker、model-picker-vendor 重钉为
/// 单空行）；②无数字列行改 ` ✓ 名称`——✓ 前一个空格不顶左缘、✓
/// 与名称之间恰一个空格（permission-picker、dsh-permission-picker
/// 重钉；名称仍恒定第 3 列）。
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
    "model-picker-vendor",
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
    "help-dialog-end",
    "context-dialog",
    "skills-dialog",
    "mcp-dialog",
    "permission-picker",
    "permission-confirm-full",
    "permission-dialog-escalate",
    "session-title",
    "rename-dialog",
    "rename-not-named",
    "attachment-chip",
    "attachment-multi",
    "attachment-steering",
    "attachment-failure-restore",
    // D-2 §7.2：dsh 快照族（App 单壳——同一 draw 管线，事件/状态注入）。
    "dsh-connecting",
    "dsh-idle",
    "dsh-running-phase",
    "dsh-approval-dialog",
    "dsh-ask-dialog",
    "dsh-resume-picker",
    "dsh-model-picker",
    "dsh-model-effort",
    "dsh-permission-picker",
    "dsh-rename-dialog",
    "dsh-disconnected",
    "dsh-unknown-events",
    "dsh-steer-badge",
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

// FIX-5/CA-08：测试记录 sink——单元/快照测试不写真实终端或系统
// 剪贴板；R5-2 以此断言「编码正确 + 调用时机」。
thread_local! {
    static CLIPBOARD_BYTES: std::cell::RefCell<Vec<u8>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

fn recording_clipboard_sink(bytes: &[u8]) -> bool {
    CLIPBOARD_BYTES.with(|cell| cell.borrow_mut().extend_from_slice(bytes));
    true
}

fn test_png(width: u32, height: u32) -> Vec<u8> {
    let image = image::RgbaImage::from_pixel(width, height, image::Rgba([4, 8, 15, 255]));
    let mut bytes = Vec::new();
    image::DynamicImage::ImageRgba8(image)
        .write_to(
            &mut std::io::Cursor::new(&mut bytes),
            image::ImageFormat::Png,
        )
        .expect("encode test png");
    bytes
}

#[test]
fn core_staged_clipboard_drafts_release_on_remove_clear_and_durable_claim_only() {
    let mut harness = Harness::trusted("clipboard-draft-lifecycle", 80, 24);
    let store = harness
        .app
        .application
        .as_ref()
        .expect("trusted application")
        .draft_image_store();
    let bytes = test_png(8, 6);

    let removed = store
        .stage_png(&bytes)
        .expect("stage removable clipboard image");
    harness
        .app
        .attachments
        .add_paths(&harness.project_root, [removed.clone()])
        .unwrap();
    assert!(harness.app.handle_attachment_command("/image remove 1"));
    assert!(!removed.exists(), "remove reclaims a core-staged raw image");

    let cleared = store
        .stage_png(&bytes)
        .expect("stage clearable clipboard image");
    harness
        .app
        .attachments
        .add_paths(&harness.project_root, [cleared.clone()])
        .unwrap();
    assert!(harness.app.handle_attachment_command("/ac"));
    assert!(!cleared.exists(), "clear reclaims a core-staged raw image");

    // CP-4：退役全拼在 App 面同样被拦截（返回 true = 消费掉输入、
    // 不落普通消息），草稿保持原样——迁移提示不是清空。
    let hinted = store
        .stage_png(&bytes)
        .expect("stage image for the retired-spelling hint leg");
    harness
        .app
        .attachments
        .add_paths(&harness.project_root, [hinted.clone()])
        .unwrap();
    assert!(
        harness.app.handle_attachment_command("/attachments clear"),
        "the retired spelling must be intercepted, never sent as a message"
    );
    assert_eq!(harness.app.attachments.len(), 1, "the hint does not clear");
    assert!(harness.app.handle_attachment_command("/ac"));
    assert!(!hinted.exists(), "/ac still clears after the hint");

    let user_source = harness.project_root.join("user-owned.png");
    std::fs::write(&user_source, &bytes).unwrap();
    harness
        .app
        .attachments
        .add_paths(&harness.project_root, [user_source.clone()])
        .unwrap();
    harness.app.clear_attachment_draft();
    assert!(
        user_source.exists(),
        "composer cleanup must never delete a user-selected /attach source"
    );

    let unclaimed = store
        .stage_png(&bytes)
        .expect("stage queued clipboard image");
    harness
        .app
        .remember_native_steering("not claimed".into(), vec![unclaimed.clone()]);
    assert!(
        unclaimed.exists(),
        "queue acknowledgement is not the durable claim point"
    );

    let claimed = store
        .stage_png(&bytes)
        .expect("stage claimed clipboard image");
    harness
        .app
        .remember_native_steering("claimed".into(), vec![claimed.clone()]);
    harness.run_event(RunEvent::SteeringApplied {
        message: crate::message::MessageContent::text("claimed"),
        client_message_id: None,
        request_digest: None,
        receipt: None,
    });
    assert!(
        !claimed.exists(),
        "durable SteeringApplied releases its raw clipboard source"
    );
    assert!(
        unclaimed.exists(),
        "a different unclaimed draft remains retryable"
    );

    // A claim can arrive while the admission worker temporarily owns the
    // Application facade. Presentation authority is removed immediately, but
    // physical cleanup waits for that exact owner to return.
    let deferred = store
        .stage_png(&bytes)
        .expect("stage deferred clipboard image");
    harness
        .app
        .remember_native_steering("deferred".into(), vec![deferred.clone()]);
    let application = harness.app.application.take().expect("move application");
    harness.run_event(RunEvent::SteeringApplied {
        message: crate::message::MessageContent::text("deferred"),
        client_message_id: None,
        request_digest: None,
        receipt: None,
    });
    assert!(deferred.exists());
    assert_eq!(
        harness.app.deferred_core_staged_releases.as_slice(),
        std::slice::from_ref(&deferred)
    );
    harness.app.application = Some(application);
    harness
        .app
        .release_core_staged_attachment_paths(std::iter::empty());
    assert!(!deferred.exists());
    assert!(harness.app.deferred_core_staged_releases.is_empty());
}

/// CP four-leg discriminator, image leg: Ctrl+V invokes the shared explicit
/// reader once, and the returned PNG is owned by the existing stage_png/H-17
/// registry rather than by a frontend-only path or cleanup lifecycle.
#[test]
fn ctrl_v_image_uses_existing_clipboard_staging_and_attachment_rail() {
    use crate::tui::attachments::{ClipboardPasteMode, PreparedClipboardPaste};
    use std::sync::atomic::{AtomicUsize, Ordering};

    let mut harness = Harness::trusted("ctrl-v-image", 80, 24);
    let probes = Arc::new(AtomicUsize::new(0));
    let staged = Arc::new(std::sync::Mutex::new(None));
    let probe_count = Arc::clone(&probes);
    let staged_path = Arc::clone(&staged);
    harness.app.clipboard_paste_reader = Arc::new(move |stager, mode| {
        probe_count.fetch_add(1, Ordering::SeqCst);
        assert_eq!(mode, ClipboardPasteMode::ImageOrText);
        let path = stager
            .expect("local Ctrl+V image uses the application draft store")
            .stage_png(&test_png(9, 7))?;
        *staged_path.lock().unwrap() = Some(path.clone());
        Ok(PreparedClipboardPaste::Image(path))
    });
    let (sender, events) = super::ui_event_channel();
    harness.app.event_sender = Some(sender);

    harness.key_with_modifiers(KeyCode::Char('v'), KeyModifiers::CONTROL);
    let prepared = events
        .recv_timeout(Duration::from_secs(2))
        .expect("Ctrl+V clipboard worker result");
    harness.event(prepared);

    assert_eq!(probes.load(Ordering::SeqCst), 1, "one explicit probe");
    assert_eq!(harness.app.attachments.len(), 1);
    assert!(
        harness
            .app
            .attachments
            .rows()
            .next()
            .is_some_and(|row| row.contains("clipboard-")),
        "the existing composer rail presents the core-staged image"
    );
    let path = staged.lock().unwrap().clone().expect("staged path");
    assert!(path.exists(), "the draft stays retryable before claim");
    assert!(harness.app.handle_attachment_command("/ac"));
    assert!(
        !path.exists(),
        "the existing clipboard registry reclaims Ctrl+V drafts"
    );
}

/// CP four-leg discriminator, text leg: the same explicit key probe falls
/// back to ordinary composer insertion without creating an attachment.
#[test]
fn ctrl_v_text_falls_back_to_ordinary_composer_insertion() {
    use crate::tui::attachments::{ClipboardPasteMode, PreparedClipboardPaste};
    use std::sync::atomic::{AtomicUsize, Ordering};

    let mut harness = Harness::trusted("ctrl-v-text", 80, 24);
    let probes = Arc::new(AtomicUsize::new(0));
    let probe_count = Arc::clone(&probes);
    harness.app.clipboard_paste_reader = Arc::new(move |_stager, mode| {
        probe_count.fetch_add(1, Ordering::SeqCst);
        assert_eq!(mode, ClipboardPasteMode::ImageOrText);
        Ok(PreparedClipboardPaste::Text("hello\nclipboard".into()))
    });
    let (sender, events) = super::ui_event_channel();
    harness.app.event_sender = Some(sender);

    harness.key_with_modifiers(KeyCode::Char('v'), KeyModifiers::CONTROL);
    harness.event(
        events
            .recv_timeout(Duration::from_secs(2))
            .expect("Ctrl+V text result"),
    );

    assert_eq!(probes.load(Ordering::SeqCst), 1);
    assert_eq!(harness.app.input.text(), "hello\nclipboard");
    assert!(harness.app.attachments.is_empty());
}

/// CP four-leg discriminator, empty leg: no modal is opened and the
/// unavailable content is explained on the transient status line.
#[test]
fn ctrl_v_empty_clipboard_flashes_status_without_mutating_composer() {
    use crate::tui::attachments::PreparedClipboardPaste;

    let mut harness = Harness::trusted("ctrl-v-empty", 80, 24);
    harness.app.clipboard_paste_reader =
        Arc::new(|_stager, _mode| Ok(PreparedClipboardPaste::Empty));
    let (sender, events) = super::ui_event_channel();
    harness.app.event_sender = Some(sender);

    harness.key_with_modifiers(KeyCode::Char('v'), KeyModifiers::CONTROL);
    harness.event(
        events
            .recv_timeout(Duration::from_secs(2))
            .expect("Ctrl+V empty result"),
    );

    assert_eq!(harness.app.status, "clipboard is empty or unreadable");
    assert!(harness.app.input.text().is_empty());
    assert!(harness.app.attachments.is_empty());
    assert!(harness.app.info_dialog.is_none());
}

/// CP-I1 discriminator: terminal bracketed paste is a separate event path
/// and must never invoke the system clipboard reader. Removing that routing
/// boundary makes this assertion red even when the pasted payload is text.
#[test]
fn bracketed_paste_remains_zero_clipboard_probe() {
    use crate::tui::attachments::PreparedClipboardPaste;
    use std::sync::atomic::{AtomicUsize, Ordering};

    let mut harness = Harness::trusted("bracketed-paste-zero-probe", 80, 24);
    let probes = Arc::new(AtomicUsize::new(0));
    let probe_count = Arc::clone(&probes);
    harness.app.clipboard_paste_reader = Arc::new(move |_stager, _mode| {
        probe_count.fetch_add(1, Ordering::SeqCst);
        Ok(PreparedClipboardPaste::Text("wrong source".into()))
    });

    harness.event(UiEvent::Terminal(Event::Paste("terminal payload".into())));

    assert_eq!(probes.load(Ordering::SeqCst), 0);
    assert_eq!(harness.app.input.text(), "terminal payload");
}

/// CP-I3: modal owners see Ctrl+V before the composer shortcut, matching the
/// existing `/paste-image` command's inability to bypass a dialog.
#[test]
fn ctrl_v_respects_modal_key_ownership() {
    use crate::tui::attachments::PreparedClipboardPaste;
    use std::sync::atomic::{AtomicUsize, Ordering};

    let mut harness = Harness::trusted("ctrl-v-modal-gate", 80, 24);
    let probes = Arc::new(AtomicUsize::new(0));
    let probe_count = Arc::clone(&probes);
    harness.app.clipboard_paste_reader = Arc::new(move |_stager, _mode| {
        probe_count.fetch_add(1, Ordering::SeqCst);
        Ok(PreparedClipboardPaste::Text("must not paste".into()))
    });
    harness.type_text("/help");
    harness.key(KeyCode::Enter);
    assert!(harness.app.info_dialog.is_some());

    harness.key_with_modifiers(KeyCode::Char('v'), KeyModifiers::CONTROL);

    assert_eq!(probes.load(Ordering::SeqCst), 0);
    assert!(harness.app.info_dialog.is_some());
    assert!(harness.app.input.text().is_empty());
}

/// CP-I3 run-state leg: an active run keeps the composer available for
/// steering, so Ctrl+V has the same reachability as `/paste-image` there.
#[test]
fn ctrl_v_remains_available_for_the_running_steering_composer() {
    use crate::tui::attachments::PreparedClipboardPaste;

    let mut harness = Harness::trusted("ctrl-v-running", 80, 24);
    harness.app.running = true;
    harness.app.clipboard_paste_reader =
        Arc::new(|_stager, _mode| Ok(PreparedClipboardPaste::Text("steering paste".into())));
    let (sender, events) = super::ui_event_channel();
    harness.app.event_sender = Some(sender);

    harness.key_with_modifiers(KeyCode::Char('v'), KeyModifiers::CONTROL);
    harness.event(
        events
            .recv_timeout(Duration::from_secs(2))
            .expect("running Ctrl+V result"),
    );

    assert_eq!(harness.app.input.text(), "steering paste");
    assert!(harness.app.attachments.is_empty());
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
    // FIX-5/CA-08：记录 sink——快照/单元测试不写真实终端或系统剪贴板
    //（鼠标释放自动复制路径因此零副作用）。
    app.clipboard_writer = recording_clipboard_sink;
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
        self.key_with_modifiers(code, KeyModifiers::NONE);
    }

    fn key_with_modifiers(&mut self, code: KeyCode, modifiers: KeyModifiers) {
        self.event(UiEvent::Terminal(Event::Key(KeyEvent::new(
            code, modifiers,
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
            receipt: None,
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
            receipt: None,
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
    // FIX-5/CA-08 / R5-2 判别：鼠标释放的自动复制经注入 sink——字节
    // 形如 `\x1b]52;c;<base64>\x1b\\` 且载荷 == 选区文本（编码正确 +
    // 调用时机一并钉住；真实 stdout 零控制序列）。判别（删修复即红）：
    // 调用点还原直写 stdout → sink 恒空 → 红。
    let recorded = CLIPBOARD_BYTES.with(|cell| std::mem::take(&mut *cell.borrow_mut()));
    assert!(
        !recorded.is_empty(),
        "mouse-up must route the copy through the injected sink"
    );
    assert!(
        recorded.starts_with(b"\x1b]52;c;") && recorded.ends_with(b"\x1b\\"),
        "OSC 52 frame shape: {recorded:?}"
    );
    assert_eq!(
        recorded,
        crate::tui::selection::osc52_copy_bytes(&copied).expect("non-empty selection encodes"),
        "the sink must receive exactly one OSC 52 frame carrying the selection text"
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
    // 刷新 2026-08-22：键位说明行统一弹窗规范——Faint 灰（fg=DarkGray）
    // + 钉在弹框内底行（此前 DIM 修饰符与其余弹窗不一致）。
    let mut harness = Harness::trusted("snap-model-picker", 80, 24);
    harness.type_text("/model");
    harness.key(KeyCode::Enter);
    let projection = harness.draw_projection();
    check_or_refresh("model-picker", &projection);
    // VP-3 返工二轮：图例只留说明行行尾一处（`· ⧉ images`）。
    assert!(projection.contains("· ⧉ images"));
    assert!(
        !projection.contains("DeepSeek ⧉"),
        "vendor rows have no capability icon"
    );
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

/// U1（INV-U1 原位返回，用户反馈 2026-08-22）：Custom 入口进入编辑器
///（零档案直进新建页）后 Esc 取消，picker 必须以进入前的层级与光标
/// 原位重建（Custom 行）——而不是整个选择链路消失。恢复逻辑删除
///（Cancel 不重建 picker）→ 本测试红。
#[test]
fn model_editor_escape_returns_to_the_picker_in_place() {
    let mut harness = Harness::trusted("snap-model-editor-back", 80, 24);
    harness.type_text("/model");
    harness.key(KeyCode::Enter);
    for _ in 0..5 {
        harness.key(KeyCode::Down);
    }
    harness.key(KeyCode::Enter); // Custom（零档案）→ 新建页
    assert!(
        harness.app.editor.is_some(),
        "editor opens from the Custom entry"
    );
    assert!(harness.app.picker.is_none(), "picker yields to the editor");

    harness.key(KeyCode::Esc); // 取消编辑 → 原位回到 picker 的 Custom 行
    assert!(harness.app.editor.is_none());
    let picker = harness
        .app
        .picker
        .as_ref()
        .expect("picker restored in place after editor cancel");
    assert_eq!(
        picker.selected_index(),
        5,
        "cursor returns to the Custom row we entered from"
    );
}

/// U1（弹窗规范统一，用户反馈 2026-08-22）：二级厂商列表（单模型）
/// 的键位说明行必须贴弹框底、Faint 灰（fg=DarkGray）、与内容恰好隔
/// 一空行——此前高度公式 `.max(8)` 兜底把小列表撑高，说明行悬空、
/// 各级弹框观感不一。快照钉住紧凑高度（1 行内容 + 空行 + 说明行 +
/// 双边框 = 5 行）。
#[test]
fn model_picker_vendor_level_snapshot() {
    let mut harness = Harness::trusted("snap-model-picker-vendor", 80, 24);
    harness.type_text("/model");
    harness.key(KeyCode::Enter);
    for _ in 0..2 {
        harness.key(KeyCode::Down);
    }
    harness.key(KeyCode::Enter); // 第 3 行 Qwen（单模型二级）
    let projection = harness.draw_projection();
    check_or_refresh("model-picker-vendor", &projection);
    assert!(projection.contains("Qwen3.8 Max ⧉"));
    assert!(projection.contains("Qwen3.8 Flash ⧉"));
    assert!(projection.contains("· ⧉ images"));
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
        message: crate::message::MessageContent::text("也讲讲投影 checkpoint"),
        client_message_id: None,
        request_digest: None,
        receipt: None,
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
    // CP-2：尾页（钳制位）补钉 help-dialog-end——首页快照只见命令节，
    // Composer/Keys 节（A4 四组 11 行 + 短主名）由尾页锁定。
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
    harness.snapshot("help-dialog-end");
    harness.key(KeyCode::Esc);
    assert!(harness.app.info_dialog.is_none(), "Esc closes the dialog");
    harness.type_text("hi");
    assert!(
        harness.app.input.visual_rows(60).join("").contains("hi"),
        "input unlocks after the dialog closes"
    );
}

/// `/context` 走真实 core command → 前端中立 DTO → TUI modal；
/// snapshot 锁定分项/工具/skills 的只读呈现，并验证模态输入门控。
#[test]
fn context_dialog_snapshot_and_modal_gate() {
    let mut harness = Harness::trusted("snap-context", 80, 24);
    harness.type_text("/context");
    harness.key(KeyCode::Enter);
    assert!(
        harness
            .app
            .info_dialog
            .as_ref()
            .is_some_and(|dialog| dialog.kind == super::InfoDialogKind::Context),
        "the /context command opens the Context dialog"
    );
    let view = harness.app.context_view.as_ref().expect("context view");
    assert_eq!(
        view.input_estimate + view.output_reserve_estimate,
        view.total_estimate
    );
    harness.project();
    harness.snapshot("context-dialog");
    harness.type_text("x");
    assert!(!harness.app.input.visual_rows(60).join("").contains('x'));
    harness.key(KeyCode::Esc);
    assert!(harness.app.info_dialog.is_none(), "Esc closes the dialog");
}

/// SC-3/CP 判别：帮助弹窗在前端本地呈现 Composer 节，命令与别名来自
/// TUI 自有交互面；附件命令与 attachments.rs 的本地拦截一一对应
///（不可列出死条目——W1-07 同精神）；删 Composer 节此测试红。CP-2
///（A5/A6）后主名是 `/pi`、`/ac`，别名 `/paste-image`、`/attach-clear`
/// 全部可达；CP-4 退役 `/attachments` 全拼（迁移提示，不再宣传）；
/// Ctrl+V 归位 Keys 节（A4），不再列在 Composer。帮助快照停在第一页，
/// Composer/Keys 在滚动区后方，由尾页快照（help-dialog-end）与本测试
/// 补齐可见性判别。
#[test]
fn help_dialog_lines_carry_the_frontend_local_composer_section() {
    let harness = Harness::trusted("snap-help-composer", 80, 24);
    let commands = harness.app.application.as_ref().unwrap().command_catalog();
    let mut text = String::new();
    for line in super::dialogs::help_dialog_lines(76, &commands) {
        for span in line.spans {
            text.push_str(&span.content);
        }
        text.push('\n');
    }
    assert!(text.contains("Composer"), "the Composer section exists");
    for entry in ["/attach PATH", "/pi, /paste-image", "/ac, /attach-clear"] {
        assert!(text.contains(entry), "Composer lists {entry}");
    }
    // CP-4：退役全拼不再出现在帮助表。
    assert!(
        !text.contains("/attachments clear"),
        "the retired spelling must not be advertised"
    );
    // A4：Ctrl+V 归位 Keys 节——Composer 节不再出现按键条目。
    assert!(
        text.contains("Keys"),
        "the Keys section exists for the Ctrl+V entry"
    );
    assert!(
        text.contains("Ctrl+V — paste clipboard image or text"),
        "Ctrl+V is listed in the Keys section"
    );
    // 三条命令真实存在于 TUI 本地拦截（attachments.rs），不是死条目；
    // 短主名与旧名（A5 旧名纪律）全部可达。
    for dispatchable in ["/pi", "/paste-image", "/ac", "/attach-clear"] {
        assert!(
            crate::tui::attachments::parse_attachment_command(dispatchable).is_some(),
            "{dispatchable} must be intercepted by the real TUI composer path"
        );
    }
    // `/attach` 的解析器吃前缀（带参形态），同样真实可达。
    assert!(crate::tui::attachments::parse_attachment_command("/attach a.png").is_some());
}

/// `/skill`（SC-2）：真实 core command → `ShowSkills` DTO → TUI modal；
/// snapshot 锁定五条 bundled 技能（含 grill-me）的名称/来源层呈现与模
/// 态输入门控。
#[test]
fn skills_dialog_snapshot_and_modal_gate() {
    let mut harness = Harness::trusted("snap-skills", 80, 24);
    harness.type_text("/skill");
    harness.key(KeyCode::Enter);
    assert!(
        harness
            .app
            .info_dialog
            .as_ref()
            .is_some_and(|dialog| dialog.kind == super::InfoDialogKind::Skills),
        "the /skill command opens the Skills dialog"
    );
    let view = harness.app.skills_view.as_ref().expect("skills view");
    let names: Vec<&str> = view
        .entries
        .iter()
        .map(|entry| entry.name.as_str())
        .collect();
    assert_eq!(
        names,
        vec![
            "bug-diagnosis",
            "change-verification",
            "code-review",
            "docs-sync",
            "grill-me",
        ]
    );
    harness.project();
    harness.snapshot("skills-dialog");
    harness.type_text("x");
    assert!(!harness.app.input.visual_rows(60).join("").contains('x'));
    harness.key(KeyCode::Esc);
    assert!(harness.app.info_dialog.is_none(), "Esc closes the dialog");
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

/// MM-3 结构化附件 rail：绝对路径粘贴生成稳定 Image #N、元数据与总
/// 预算，不进文本；Esc 连同输入一起清空。
#[test]
fn attachment_chip_snapshot() {
    let mut harness = Harness::trusted("snap-attach", 80, 24);
    let image = harness.project_root.join("probe-shot.png");
    std::fs::write(&image, test_png(32, 24)).unwrap();
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

/// MM-3 keyboard-only multi-select/reorder/remove path. Quoted project-relative
/// paths are parsed literally; stable ids survive reordering.
#[test]
fn attachment_multi_reorder_remove_snapshot_and_behavior() {
    let mut harness = Harness::trusted("snap-attach-multi", 100, 28);
    std::fs::write(
        harness.project_root.join("first shot.png"),
        test_png(64, 32),
    )
    .unwrap();
    std::fs::write(harness.project_root.join("second.png"), test_png(16, 48)).unwrap();

    harness.type_text("/attach \"first shot.png\" second.png");
    harness.key(KeyCode::Enter);
    assert_eq!(harness.app.attachments.len(), 2);
    harness.type_text("/image move 2 1");
    harness.key(KeyCode::Enter);
    let rows = harness.app.attachments.rows().collect::<Vec<_>>();
    assert!(rows[0].starts_with("[Image #2]"));
    assert!(rows[1].starts_with("[Image #1]"));
    harness.type_text("describe both");
    harness.snapshot("attachment-multi");

    // Remove by stable id, not current visual position.
    harness.app.input.clear();
    harness.type_text("/image remove #1");
    harness.key(KeyCode::Enter);
    let rows = harness.app.attachments.rows().collect::<Vec<_>>();
    assert_eq!(rows.len(), 1);
    assert!(rows[0].starts_with("[Image #2]"));

    // The default test route is text-only/unconfigured. A send attempt keeps
    // both text and the remaining structured image draft.
    harness.type_text("please inspect");
    harness.key(KeyCode::Enter);
    assert!(harness.app.input.text().contains("please inspect"));
    assert_eq!(harness.app.attachments.len(), 1);
    assert!(harness.app.status.contains("cannot send images"));
}

/// The pending steering zone must make an image-bearing follow-up visible
/// without exposing its source path. Core/application tests separately lock
/// Reserved→Committed admission and provider delivery; this snapshot owns the
/// TUI presentation boundary.
#[test]
fn attachment_steering_pending_snapshot_is_path_free() {
    let mut harness = Harness::trusted("snap-attach-steering", 90, 25);
    harness
        .app
        .conversation
        .push_user("first request is still running".into());
    harness
        .app
        .conversation
        .push_pending_steering("inspect the new screenshots\n[2 image(s)]".into());
    harness.app.running = true;
    let projection = harness.draw_projection();
    check_or_refresh("attachment-steering", &projection);
    assert!(projection.contains("[2 image(s)]"));
    assert!(!projection.contains("/tmp/"));
}

/// An asynchronous pre-commit startup failure must leave both the text and
/// ordered structured image draft visible for a lossless retry.
#[test]
fn attachment_start_failure_restores_the_complete_draft_snapshot() {
    let mut harness = Harness::trusted("snap-attach-failure", 90, 25);
    let image = harness.project_root.join("retry.png");
    std::fs::write(&image, test_png(20, 10)).unwrap();
    harness.event(UiEvent::Terminal(Event::Paste(image.display().to_string())));

    let application = harness.app.application.as_mut().expect("application");
    crate::test_support::configure_test_model(application);
    let (config, credentials) = application.model_state().unwrap();
    harness.app.config = config;
    harness.app.credentials = credentials;
    application.fail_next_run_spawn_for_test();
    let (sender, events) = super::ui_event_channel();
    harness.app.event_sender = Some(sender);

    harness.type_text("retry this exact draft");
    harness.key(KeyCode::Enter);
    assert!(harness.app.run_start_pending);
    assert!(harness.app.application.is_none());
    let finished = events
        .recv_timeout(std::time::Duration::from_secs(2))
        .expect("background admission failure");
    harness.event(finished);
    assert_eq!(harness.app.input.text(), "retry this exact draft");
    assert_eq!(harness.app.attachments.len(), 1);
    assert!(harness.app.status.contains("failed to start run"));
    harness.snapshot("attachment-failure-restore");
}

#[test]
fn failed_initial_admission_keeps_core_staged_clipboard_source_retryable() {
    let mut harness = Harness::trusted("clipboard-start-failure", 90, 25);
    let image = harness
        .app
        .application
        .as_ref()
        .expect("application")
        .draft_image_store()
        .stage_png(&test_png(20, 10))
        .expect("core-staged retry image");
    harness.event(UiEvent::Terminal(Event::Paste(image.display().to_string())));

    let application = harness.app.application.as_mut().expect("application");
    crate::test_support::configure_test_model(application);
    let (config, credentials) = application.model_state().unwrap();
    harness.app.config = config;
    harness.app.credentials = credentials;
    application.fail_next_run_spawn_for_test();
    let (sender, events) = super::ui_event_channel();
    harness.app.event_sender = Some(sender);

    harness.type_text("retry this staged clipboard image");
    harness.key(KeyCode::Enter);
    let finished = events
        .recv_timeout(std::time::Duration::from_secs(2))
        .expect("background admission failure");
    harness.event(finished);

    assert_eq!(harness.app.attachments.len(), 1);
    assert!(
        image.exists(),
        "failed admission keeps the core-staged raw image retryable"
    );
}

#[test]
fn attachment_admission_handoff_restores_app_before_first_run_event() {
    let mut harness = Harness::trusted("attach-admission-handoff", 90, 25);
    let image = harness
        .app
        .application
        .as_ref()
        .expect("application")
        .draft_image_store()
        .stage_png(&test_png(40, 30))
        .expect("core-staged handoff image");
    harness.event(UiEvent::Terminal(Event::Paste(image.display().to_string())));

    let application = harness.app.application.as_ref().expect("application");
    crate::test_support::configure_test_model(application);
    let (config, credentials) = application.model_state().unwrap();
    harness.app.config = config;
    harness.app.credentials = credentials;
    let (sender, events) = super::ui_event_channel();
    harness.app.event_sender = Some(sender);

    harness.type_text("inspect without blocking input");
    harness.key(KeyCode::Enter);
    assert!(harness.app.run_start_pending);
    assert!(harness.app.application.is_none());
    // Terminal edits are gated while the worker owns the application.
    harness.key(KeyCode::Char('x'));
    assert!(harness.app.input.text().is_empty());
    assert_eq!(harness.app.attachments.len(), 1);
    assert!(
        image.exists(),
        "pre-commit handoff cannot reclaim the retry source"
    );

    let prepared = events
        .recv_timeout(std::time::Duration::from_secs(2))
        .expect("background admission success");
    assert!(matches!(
        prepared,
        UiEvent::Worker(WorkerMessage::RunStartFinished(_))
    ));
    harness.event(prepared);
    assert!(harness.app.application.is_some());
    assert!(!harness.app.run_start_pending);
    assert!(harness.app.running);
    assert!(harness.app.attachments.is_empty());
    assert!(
        !image.exists(),
        "successful durable admission reclaims the core-staged raw source"
    );
    harness
        .app
        .run_handle
        .as_ref()
        .expect("run handle")
        .cancel();

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
    while harness.app.running && std::time::Instant::now() < deadline {
        match events.recv_timeout(std::time::Duration::from_millis(200)) {
            Ok(event) => harness.event(event),
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                panic!("run event channel disconnected after handoff")
            }
        }
    }
    assert!(
        !harness.app.running,
        "test run must finish after gate opens"
    );
}

/// Image-bearing steering must not decode/normalize on the terminal thread.
/// While that admission worker owns the facade, the already-running model
/// keeps its event stream and no second input can observe the handoff. Once
/// restored, the ordinary DSH-style pending queue still reaches the next
/// model boundary with the original image draft committed exactly once.
#[test]
fn attachment_steering_admission_handoff_keeps_active_run_live() {
    let mut harness = Harness::trusted("attach-steering-admission", 90, 25);
    // This test needs a deliberately blocked first model call so `steer()`
    // is guaranteed to target an active run rather than exercise the
    // NotRunning fallback. Replace the harness's default deterministic
    // provider while preserving its trusted storage/project pair.
    harness
        .app
        .application
        .take()
        .expect("default application")
        .close()
        .expect("close default application");
    let gate = Arc::new(crate::test_support::SteerGate::default());
    let application = BootstrapApplication::open(
        Project::new(&harness.project_root),
        harness.storage_root.clone(),
    )
    .expect("reopen trusted bootstrap")
    .into_trusted_with_provider(Arc::new(TestProviderPlugin {
        behavior: TestBehavior::Steer(Arc::clone(&gate)),
    }))
    .expect("mount steering provider");
    crate::test_support::configure_test_model(&application);
    let (config, credentials) = application.model_state().expect("model state");
    harness.app.config = config;
    harness.app.credentials = credentials;
    harness.app.application = Some(application);

    let (sender, events) = super::ui_event_channel();
    harness.app.event_sender = Some(sender);
    harness.type_text("start work");
    harness.key(KeyCode::Enter);
    gate.wait_entered();
    assert!(harness.app.running);

    let image = harness.project_root.join("steering-handoff.png");
    std::fs::write(&image, test_png(40, 30)).unwrap();
    harness.event(UiEvent::Terminal(Event::Paste(image.display().to_string())));
    harness.type_text("also run the tests");
    harness.key(KeyCode::Enter);
    assert!(harness.app.run_start_pending);
    assert!(harness.app.application.is_none());
    // The existing run stays active, but the temporary owner handoff gates
    // all interactive mutation until admission returns.
    assert!(harness.app.running);
    harness.key(KeyCode::Char('x'));
    assert!(harness.app.input.text().is_empty());

    let admitted = loop {
        let event = events
            .recv_timeout(std::time::Duration::from_secs(2))
            .expect("steering admission result");
        if matches!(
            event,
            UiEvent::Worker(WorkerMessage::SteeringAdmissionFinished(_))
        ) {
            break event;
        }
        harness.event(event);
    };
    harness.event(admitted);
    assert!(harness.app.application.is_some());
    assert!(!harness.app.run_start_pending);
    assert!(harness.app.running);
    assert!(harness.app.attachments.is_empty());
    assert_eq!(harness.app.conversation.pending_steering_count(), 1);

    gate.release();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
    while harness.app.running && std::time::Instant::now() < deadline {
        match events.recv_timeout(std::time::Duration::from_millis(200)) {
            Ok(event) => harness.event(event),
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                panic!("run event channel disconnected after steering handoff")
            }
        }
    }
    assert!(!harness.app.running, "steered test run must finish");
    assert!(gate.saw_steering.load(std::sync::atomic::Ordering::Acquire));
}

/// INV-MM-I11/TUI: accepting a steering draft is not ownership transfer to
/// durable history. If cancellation seals the run before `SteeringApplied`,
/// the exact text and ordered source images return to the composer. This test
/// is red on the old `discard_pending_steering()`-only finish path.
#[test]
fn cancelled_unclaimed_image_steering_restores_the_exact_retry_draft() {
    let mut harness = Harness::trusted("attach-steering-cancel-retry", 90, 25);
    harness
        .app
        .application
        .take()
        .expect("default application")
        .close()
        .expect("close default application");
    let gate = Arc::new(crate::test_support::SteerGate::default());
    let application = BootstrapApplication::open(
        Project::new(&harness.project_root),
        harness.storage_root.clone(),
    )
    .expect("reopen trusted bootstrap")
    .into_trusted_with_provider(Arc::new(TestProviderPlugin {
        behavior: TestBehavior::Steer(Arc::clone(&gate)),
    }))
    .expect("mount steering provider");
    crate::test_support::configure_test_model(&application);
    let (config, credentials) = application.model_state().expect("model state");
    harness.app.config = config;
    harness.app.credentials = credentials;
    harness.app.application = Some(application);

    let (sender, events) = super::ui_event_channel();
    harness.app.event_sender = Some(sender);
    harness.type_text("start cancellable work");
    harness.key(KeyCode::Enter);
    gate.wait_entered();

    let first = harness.project_root.join("cancel-retry-first.png");
    let second = harness.project_root.join("cancel-retry-second.png");
    std::fs::write(&first, test_png(40, 30)).unwrap();
    std::fs::write(&second, test_png(30, 40)).unwrap();
    harness.event(UiEvent::Terminal(Event::Paste(first.display().to_string())));
    // Add the second file through the same validated composer surface.
    harness
        .app
        .attachments
        .add_paths(&harness.project_root, [second.clone()])
        .unwrap();
    harness.type_text("retry these images after cancellation");
    harness.key(KeyCode::Enter);

    let admitted = loop {
        let event = events
            .recv_timeout(Duration::from_secs(2))
            .expect("steering admission result");
        if matches!(
            event,
            UiEvent::Worker(WorkerMessage::SteeringAdmissionFinished(_))
        ) {
            break event;
        }
        harness.event(event);
    };
    harness.event(admitted);
    assert_eq!(harness.app.pending_native_steering.len(), 1);
    assert!(harness.app.attachments.is_empty());

    harness
        .app
        .run_handle
        .as_ref()
        .expect("active run")
        .cancel();
    gate.release();
    let deadline = std::time::Instant::now() + Duration::from_secs(3);
    while harness.app.running && std::time::Instant::now() < deadline {
        match events.recv_timeout(Duration::from_millis(200)) {
            Ok(event) => harness.event(event),
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                panic!("run event channel disconnected before cancellation settled")
            }
        }
    }

    assert!(!harness.app.running, "cancelled run must settle");
    assert_eq!(
        harness.app.input.text(),
        "retry these images after cancellation"
    );
    assert_eq!(
        harness.app.attachments.paths(),
        vec![
            first.canonicalize().unwrap(),
            second.canonicalize().unwrap()
        ]
    );
    assert!(harness.app.pending_native_steering.is_empty());
    assert!(harness.app.recovered_native_steering.is_empty());
    assert!(harness.app.status.contains("restored"));
    assert_eq!(harness.app.conversation.pending_steering_count(), 0);
    assert!(!gate.saw_steering.load(std::sync::atomic::Ordering::Acquire));
}

/// The run worker and the admission worker are distinct UI-event producers.
/// A very fast claim may therefore arrive before the frontend receives the
/// `Queued` acknowledgement. Pairing that early claim must not leave a stale
/// draft that is resurrected when the run later finishes.
#[test]
fn steering_claim_racing_ahead_of_admission_ack_consumes_the_local_draft() {
    let mut harness = Harness::trusted("attach-steering-early-claim", 80, 24);
    harness.app.run_start_pending = true;
    harness.app.steering_admission_pending = true;
    harness.run_event(RunEvent::SteeringApplied {
        message: crate::message::MessageContent::text("claimed before ack"),
        client_message_id: None,
        request_digest: None,
        receipt: None,
    });
    assert_eq!(harness.app.native_steering_claim_credits.len(), 1);
    assert_eq!(
        harness.app.native_steering_claim_credits.front(),
        Some(&"claimed before ack".to_owned())
    );

    harness.app.remember_native_steering(
        "claimed before ack".into(),
        vec![harness.project_root.join("already-claimed.png")],
    );
    harness.app.run_start_pending = false;
    harness.app.steering_admission_pending = false;
    assert!(harness.app.native_steering_claim_credits.is_empty());
    assert!(harness.app.pending_native_steering.is_empty());

    harness
        .app
        .remember_native_steering("current run draft".into(), Vec::new());
    harness.app.run_start_pending = true;
    harness.app.steering_admission_pending = false;
    harness.run_event(RunEvent::SteeringApplied {
        message: crate::message::MessageContent::text("stale previous run claim"),
        client_message_id: None,
        request_digest: None,
        receipt: None,
    });
    assert_eq!(harness.app.pending_native_steering.len(), 1);
    assert_eq!(
        harness.app.pending_native_steering.front().unwrap().prompt,
        "current run draft"
    );
    assert!(harness.app.native_steering_claim_credits.is_empty());
}

/// MM-5 paid TUI product gate: crossterm paste/input handlers → TUI async
/// admission handoff → real Application/provider → rendered transcript. The
/// credential remains process-local through `LiveGlmProviderPlugin`.
#[test]
#[ignore = "paid GLM TUI attachment/history check; set CLAT_GLM_CODING_PLAN_KEY explicitly"]
fn live_glm_tui_multi_image_and_image_only_history() {
    if std::env::var_os("CLAT_GLM_CODING_PLAN_KEY").is_none() {
        eprintln!("live GLM TUI image gate not armed; skipping");
        return;
    }
    let mut harness = Harness::trusted("mm5-live-tui-images", 100, 30);
    harness
        .app
        .application
        .take()
        .expect("default application")
        .close()
        .expect("close default application");
    let application = BootstrapApplication::open(
        Project::new(&harness.project_root),
        harness.storage_root.clone(),
    )
    .expect("reopen trusted bootstrap")
    .into_trusted_with_provider(Arc::new(LiveGlmProviderPlugin))
    .expect("mount live GLM provider");
    let config = crate::ModelConfig {
        preset: Some("glm-5.3-flash".into()),
        overrides: crate::model::ModelOverrides {
            // This campaign accumulates several thinking/tool/image turns.
            // Pin a real override version and leave enough room for reasoning
            // before the exact-answer sentinel; 512 can end at `length` with
            // an empty visible answer even though the request is valid.
            output_limit: crate::Override::Set(4_096),
            ..crate::model::ModelOverrides::default()
        },
        overrides_version: Some(1),
        ..crate::ModelConfig::default()
    };
    application
        .save_model_state(
            &config,
            &crate::ProviderCredentials::for_protocol(config.protocol),
        )
        .expect("save GLM preset without persisted key");
    let (config, credentials) = application.model_state().expect("effective model state");
    harness.app.config = config;
    harness.app.credentials = credentials;
    harness.app.application = Some(application);
    let (sender, events) = super::ui_event_channel();
    harness.app.event_sender = Some(sender);

    let green = harness.project_root.join("tui-live-green.png");
    let yellow = harness.project_root.join("tui-live-yellow.png");
    image::DynamicImage::ImageRgb8(image::RgbImage::from_pixel(
        128,
        128,
        image::Rgb([0, 200, 0]),
    ))
    .save_with_format(&green, image::ImageFormat::Png)
    .unwrap();
    image::DynamicImage::ImageRgb8(image::RgbImage::from_pixel(
        128,
        128,
        image::Rgb([250, 220, 0]),
    ))
    .save_with_format(&yellow, image::ImageFormat::Png)
    .unwrap();

    let wait_until_idle = |harness: &mut Harness| {
        let deadline = std::time::Instant::now() + Duration::from_secs(180);
        let mut steering_applied = 0usize;
        while (harness.app.run_start_pending || harness.app.running)
            && std::time::Instant::now() < deadline
        {
            match events.recv_timeout(Duration::from_millis(500)) {
                Ok(event) => {
                    if matches!(
                        &event,
                        UiEvent::Worker(WorkerMessage::Event(RunEvent::SteeringApplied { .. }))
                    ) {
                        steering_applied += 1;
                    }
                    harness.event(event);
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    panic!("live TUI event channel disconnected")
                }
            }
        }
        assert!(
            !harness.app.run_start_pending && !harness.app.running,
            "live TUI run must settle within the campaign deadline"
        );
        steering_applied
    };

    harness.event(UiEvent::Terminal(Event::Paste(green.display().to_string())));
    harness
        .app
        .attachments
        .add_paths(&harness.project_root, [yellow.clone()])
        .unwrap();
    harness
        .type_text("Two solid-color images are attached in order. Reply exactly 1=green;2=yellow.");
    harness.key(KeyCode::Enter);
    let _ = wait_until_idle(&mut harness);
    let projection = harness.draw_projection().to_ascii_lowercase();
    assert!(projection.contains("1=green"), "projection: {projection}");
    assert!(projection.contains("2=yellow"), "projection: {projection}");

    harness.event(UiEvent::Terminal(Event::Paste(green.display().to_string())));
    harness.key(KeyCode::Enter);
    let _ = wait_until_idle(&mut harness);
    harness.type_text(
        "What solid color filled my immediately previous image-only message? Reply exactly HISTORY_OK_GREEN.",
    );
    harness.key(KeyCode::Enter);
    let _ = wait_until_idle(&mut harness);
    let projection = harness.draw_projection().to_ascii_uppercase();
    assert!(
        projection.contains("HISTORY_OK_GREEN"),
        "image-only history must survive the TUI pipeline: {projection}"
    );

    harness.type_text(
        "Call view_image exactly once with project_relative_path tui-live-green.png. A queued steering image will arrive before the next model step. After observing both, reply exactly TUI_STEER_OK_GREEN_YELLOW.",
    );
    harness.key(KeyCode::Enter);
    assert!(harness.app.running);
    harness.event(UiEvent::Terminal(Event::Paste(
        yellow.display().to_string(),
    )));
    harness.type_text(
        "The queued steering image is yellow. Incorporate it with the green view_image result and reply exactly TUI_STEER_OK_GREEN_YELLOW.",
    );
    harness.key(KeyCode::Enter);
    let steering_applied = wait_until_idle(&mut harness);
    assert_eq!(
        steering_applied, 1,
        "steering must cross the durable claim point"
    );
    let projection = harness.draw_projection().to_ascii_uppercase();
    assert!(
        projection.contains("TUI_STEER_OK_GREEN_YELLOW"),
        "live TUI steering/tool loop must complete: {projection}"
    );

    harness.type_text(
        "Call view_image with project_relative_path tui-live-green.png, then produce a long analysis. Do not finish before using the tool.",
    );
    harness.key(KeyCode::Enter);
    assert!(harness.app.running);
    harness.event(UiEvent::Terminal(Event::Paste(green.display().to_string())));
    harness.type_text(
        "If cancellation returns this green image draft, retry it and reply exactly TUI_CANCEL_RETRY_GREEN.",
    );
    harness.key(KeyCode::Enter);
    let admission_deadline = std::time::Instant::now() + Duration::from_secs(10);
    while harness.app.run_start_pending && std::time::Instant::now() < admission_deadline {
        match events.recv_timeout(Duration::from_millis(250)) {
            Ok(event) => harness.event(event),
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                panic!("live TUI event channel disconnected during steering admission")
            }
        }
    }
    assert!(
        !harness.app.run_start_pending,
        "steering admission must settle"
    );
    assert_eq!(
        harness.app.pending_native_steering.len(),
        1,
        "the draft must still be unclaimed at the cancellation point"
    );
    harness
        .app
        .run_handle
        .as_ref()
        .expect("live cancellable run")
        .cancel();
    let _ = wait_until_idle(&mut harness);
    assert_eq!(
        harness.app.input.text(),
        "If cancellation returns this green image draft, retry it and reply exactly TUI_CANCEL_RETRY_GREEN."
    );
    assert_eq!(harness.app.attachments.len(), 1);
    assert!(harness.app.status.contains("restored"));

    harness.key(KeyCode::Enter);
    let _ = wait_until_idle(&mut harness);
    let assistant = harness
        .app
        .conversation
        .last_assistant_text()
        .unwrap_or_default()
        .to_ascii_uppercase();
    assert!(
        assistant.contains("TUI_CANCEL_RETRY_GREEN"),
        "retried image draft must complete through the real model: {assistant}"
    );
}

/// MM-5 manual physical-terminal gate. Unlike the TestBackend campaigns, this
/// enters the real ratatui/crossterm lifecycle, reads keys from the process
/// PTY, enables raw/bracketed-paste/mouse modes, and restores the terminal on
/// exit. Run with `--ignored --exact --nocapture` in a real terminal, type
/// `/attach physical.png`, visually confirm the `[Image #1]` rail, then press
/// Ctrl+C. It deliberately uses a generated non-sensitive fixture and an
/// isolated storage root; it neither reads the OS clipboard nor persists a
/// provider credential.
#[test]
#[ignore = "manual physical PTY TUI attachment composer gate"]
fn physical_pty_tui_attachment_composer_smoke() {
    use std::io::IsTerminal as _;

    if std::env::var_os("CLAT_PHYSICAL_PTY").as_deref() != Some(std::ffi::OsStr::new("1")) {
        eprintln!("physical PTY gate not armed; set CLAT_PHYSICAL_PTY=1");
        return;
    }
    assert!(
        std::io::stdin().is_terminal() && std::io::stdout().is_terminal(),
        "run this ignored gate from a real terminal/PTY with --nocapture"
    );
    let (storage_root, project_root) = roots("mm5-physical-pty");
    std::fs::create_dir_all(&project_root).expect("project dir");
    image::DynamicImage::ImageRgb8(image::RgbImage::from_pixel(
        96,
        64,
        image::Rgb([20, 180, 80]),
    ))
    .save_with_format(project_root.join("physical.png"), image::ImageFormat::Png)
    .expect("physical PTY fixture");

    let project = Project::new(&project_root);
    BootstrapApplication::open(project.clone(), storage_root.clone())
        .expect("bootstrap")
        .authorize_and_mount_with_provider(std::sync::Arc::new(TestProviderPlugin {
            behavior: TestBehavior::Success,
        }))
        .expect("authorize isolated project")
        .close()
        .expect("close authorizer");
    let mut app = App::open(project, Some(storage_root.clone())).expect("open physical PTY app");
    app.test_freeze_tick = false;

    eprintln!("MM5_PHYSICAL_PTY: type /attach physical.png, verify [Image #1], then Ctrl+C");
    let result = super::run_frontend(app);
    crate::test_support::cleanup_tree(storage_root.parent().expect("isolated base"));
    result.expect("physical PTY frontend lifecycle");
}

/// Paid TUI compaction leg kept separate from the larger image campaign so a
/// transient visual-model miss does not hide whether the `/compact` command,
/// application notice lane, durable replay, and cold-remount continuation work.
/// Seed turns use the deterministic provider; only summary + post-reopen run
/// consume the process-local live credential.
#[test]
#[ignore = "paid GLM TUI compaction/cold-reopen check; set CLAT_GLM_CODING_PLAN_KEY explicitly"]
fn live_glm_tui_manual_compaction_cold_reopen_and_continue() {
    if std::env::var_os("CLAT_GLM_CODING_PLAN_KEY").is_none() {
        eprintln!("live GLM TUI compaction gate not armed; skipping");
        return;
    }
    let mut harness = Harness::trusted("mm5-live-tui-compact", 100, 30);
    let application = harness.app.application.as_ref().expect("application");
    crate::test_support::configure_test_model(application);
    let (config, credentials) = application.model_state().expect("test model state");
    harness.app.config = config;
    harness.app.credentials = credentials;
    let (sender, events) = super::ui_event_channel();
    harness.app.event_sender = Some(sender);
    harness.app.wire_application_events();

    let wait_until_idle = |harness: &mut Harness| {
        let deadline = std::time::Instant::now() + Duration::from_secs(30);
        while (harness.app.run_start_pending || harness.app.running)
            && std::time::Instant::now() < deadline
        {
            match events.recv_timeout(Duration::from_millis(250)) {
                Ok(event) => harness.event(event),
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    panic!("TUI event channel disconnected")
                }
            }
        }
        assert!(!harness.app.run_start_pending && !harness.app.running);
    };

    for turn in 0..5 {
        harness.type_text(&format!("deterministic history seed {turn}"));
        harness.key(KeyCode::Enter);
        wait_until_idle(&mut harness);
    }
    harness
        .app
        .application
        .take()
        .expect("deterministic application")
        .close()
        .expect("close deterministic seed application");

    let application = BootstrapApplication::open(
        Project::new(&harness.project_root),
        harness.storage_root.clone(),
    )
    .expect("reopen live bootstrap")
    .into_trusted_with_provider(Arc::new(LiveGlmProviderPlugin))
    .expect("mount live GLM provider");
    let config = crate::ModelConfig {
        preset: Some("glm-5.3-flash".into()),
        overrides: crate::model::ModelOverrides {
            output_limit: crate::Override::Set(512),
            ..crate::model::ModelOverrides::default()
        },
        overrides_version: Some(1),
        ..crate::ModelConfig::default()
    };
    application
        .save_model_state(
            &config,
            &crate::ProviderCredentials::for_protocol(config.protocol),
        )
        .expect("save live model state without a key");
    harness.app.application = Some(application);
    harness.app.wire_application_events();
    harness
        .app
        .adopt_snapshot()
        .expect("adopt seeded session before compact");

    harness.type_text("/compact");
    harness.key(KeyCode::Enter);
    let compact = harness
        .app
        .compact_handle
        .as_ref()
        .expect("/compact returns a cancellable handle")
        .clone();
    let deadline = std::time::Instant::now() + Duration::from_secs(180);
    while !compact.is_finished() && std::time::Instant::now() < deadline {
        match events.recv_timeout(Duration::from_millis(500)) {
            Ok(event) => harness.event(event),
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                panic!("TUI event channel disconnected during live compaction")
            }
        }
    }
    let report = compact
        .join_report()
        .expect("join live compaction")
        .expect("live GLM compaction succeeds");
    assert!(
        report.shadowed_count > 0,
        "manual compaction shrinks history"
    );
    while let Ok(event) = events.try_recv() {
        harness.event(event);
    }

    harness
        .app
        .application
        .take()
        .expect("live application before cold remount")
        .close()
        .expect("close after live compaction");
    harness.app.compact_handle = None;
    let mut reopened = BootstrapApplication::open(
        Project::new(&harness.project_root),
        harness.storage_root.clone(),
    )
    .expect("cold reopen trusted bootstrap")
    .into_trusted_with_provider(Arc::new(LiveGlmProviderPlugin))
    .expect("cold remount live GLM provider");
    let snapshot = reopened.snapshot().expect("cold replay snapshot");
    assert!(
        snapshot.replay.iter().any(|event| matches!(
            event,
            crate::session::replay::ReplayEvent::Compaction { .. }
        )),
        "cold TUI replay contains the durable compaction summary"
    );
    assert!(
        snapshot
            .credentials
            .values()
            .iter()
            .all(|value| value.is_empty()),
        "the process-local live key never enters persisted credentials"
    );
    harness.app.application = Some(reopened);
    harness.app.wire_application_events();
    harness
        .app
        .adopt_snapshot()
        .expect("adopt cold TUI snapshot");

    harness.type_text("Reply exactly TUI_COMPACT_REOPEN_OK.");
    harness.key(KeyCode::Enter);
    wait_until_idle(&mut harness);
    let assistant = harness
        .app
        .conversation
        .last_assistant_text()
        .unwrap_or_default()
        .to_ascii_uppercase();
    assert!(
        assistant.contains("TUI_COMPACT_REOPEN_OK"),
        "cold-remounted compacted TUI session continues: {assistant}; status={}",
        harness.app.status
    );
}

/// The asynchronous steering worker must preserve the exact structured draft
/// when core refuses admission. This is a distinct outcome from an ended run:
/// no ordinary fallback may consume the images, and the user gets a lossless
/// retry in the composer.
#[test]
fn refused_image_steering_admission_restores_text_and_draft() {
    let mut harness = Harness::trusted("attach-steering-refused", 90, 25);
    let image = harness.project_root.join("refused.png");
    std::fs::write(&image, test_png(12, 12)).unwrap();
    harness.event(UiEvent::Terminal(Event::Paste(image.display().to_string())));
    let application = harness.app.application.take().expect("application");
    harness.app.run_start_pending = true;

    harness.event(UiEvent::Worker(WorkerMessage::SteeringAdmissionFinished(
        Box::new(SteeringAdmissionFinished {
            application,
            prompt: "keep this exact image draft".into(),
            outcome: crate::SteerOutcome::Refused {
                reason: "image is no longer admissible".into(),
                receipt: None,
            },
        }),
    )));

    assert!(harness.app.application.is_some());
    assert!(!harness.app.run_start_pending);
    assert_eq!(harness.app.input.text(), "keep this exact image draft");
    assert_eq!(harness.app.attachments.len(), 1);
    assert!(harness.app.status.contains("steering refused"));
}

/// A run can seal while an image steering worker is decoding. `NotRunning`
/// must therefore restore the owner and start an ordinary image run, rather
/// than dropping the draft or leaving the UI permanently admission-gated.
#[test]
fn sealed_image_steering_admission_falls_back_to_an_ordinary_run() {
    let mut harness = Harness::trusted("attach-steering-sealed", 90, 25);
    let image = harness.project_root.join("sealed.png");
    std::fs::write(&image, test_png(24, 16)).unwrap();
    harness.event(UiEvent::Terminal(Event::Paste(image.display().to_string())));
    let application = harness.app.application.as_ref().expect("application");
    crate::test_support::configure_test_model(application);
    let (config, credentials) = application.model_state().expect("model state");
    harness.app.config = config;
    harness.app.credentials = credentials;
    let application = harness.app.application.take().expect("application");
    harness.app.run_start_pending = true;
    let (sender, events) = super::ui_event_channel();
    harness.app.event_sender = Some(sender);

    harness.event(UiEvent::Worker(WorkerMessage::SteeringAdmissionFinished(
        Box::new(SteeringAdmissionFinished {
            application,
            prompt: "send after the previous run sealed".into(),
            outcome: crate::SteerOutcome::NotRunning { receipt: None },
        }),
    )));
    // Image-bearing ordinary fallback itself uses the bounded admission path.
    assert!(harness.app.run_start_pending);
    assert!(harness.app.application.is_none());
    assert_eq!(harness.app.attachments.len(), 1);

    let prepared = events
        .recv_timeout(std::time::Duration::from_secs(2))
        .expect("fallback admission result");
    assert!(matches!(
        prepared,
        UiEvent::Worker(WorkerMessage::RunStartFinished(_))
    ));
    harness.event(prepared);
    assert!(harness.app.application.is_some());
    assert!(!harness.app.run_start_pending);
    assert!(harness.app.running);
    assert!(harness.app.attachments.is_empty());
    harness
        .app
        .run_handle
        .as_ref()
        .expect("fallback run handle")
        .cancel();

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
    while harness.app.running && std::time::Instant::now() < deadline {
        match events.recv_timeout(std::time::Duration::from_millis(200)) {
            Ok(event) => harness.event(event),
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                panic!("run event channel disconnected after fallback admission")
            }
        }
    }
    assert!(
        !harness.app.running,
        "fallback run must finish after cancellation"
    );
}

#[test]
fn ctrl_c_during_admission_waits_to_recover_application_before_exit() {
    let mut harness = Harness::trusted("attach-admission-exit", 80, 24);
    let image = harness.project_root.join("exit.png");
    std::fs::write(&image, test_png(12, 12)).unwrap();
    harness.event(UiEvent::Terminal(Event::Paste(image.display().to_string())));
    let application = harness.app.application.as_mut().expect("application");
    crate::test_support::configure_test_model(application);
    let (config, credentials) = application.model_state().unwrap();
    harness.app.config = config;
    harness.app.credentials = credentials;
    application.fail_next_run_spawn_for_test();
    let (sender, events) = super::ui_event_channel();
    harness.app.event_sender = Some(sender);

    harness.type_text("recover owner before exit");
    harness.key(KeyCode::Enter);
    harness.event(UiEvent::Terminal(Event::Key(KeyEvent::new(
        KeyCode::Char('c'),
        KeyModifiers::CONTROL,
    ))));
    assert!(!harness.app.should_quit);
    assert!(harness.app.quit_after_run_start);

    let finished = events
        .recv_timeout(std::time::Duration::from_secs(2))
        .expect("background admission result");
    harness.event(finished);
    assert!(harness.app.application.is_some());
    assert!(harness.app.should_quit);
    assert!(!harness.app.quit_after_run_start);
}

// ---- D-2：dsh 快照族（§7.2） ----
//
// 构造原则：不走信任流、不触网——DshState 直接预置（任务/事件通道两端
// 都握在测试手里，发送静默入队），状态一律经**真实事件通路**注入
//（Reply/Frame），快照锁的是 App 单壳的完整归约渲染管线（INV-U2）。
// describe fixture 的 cwd 是固定虚拟路径：第二行 project 与状态栏
// 零环境依赖。TTL flash 有跨时钟漂移，场景收尾统一 settle 状态行。

/// describe fixture（钉靶形状：version/cwd/provider/model/
/// attachedSessions/home，client.rs looks_like_dsh 同款）。
fn dsh_describe_fixture() -> serde_json::Value {
    json!({
        "version": "0.1.1-rc.2",
        "cwd": "/home/dev/dsh-project",
        "provider": "deepseek",
        "model": "deepseek-chat",
        "attachedSessions": 1,
        "home": "/home/dev/.dsh"
    })
}

const DSH_SNAP_SESSION: &str = "session-dsh-snap-0001";

/// dsh 已连接态 harness：恢复最近活跃会话 + 空历史（各场景再叠加）。
fn harness_dsh(tag: &str, width: u16, height: u16) -> Harness {
    let (storage_root, project_root) = roots(tag);
    let mut app = App::open_dsh(3080).expect("dsh app opens");
    app.test_freeze_tick = true;
    app.clipboard_writer = recording_clipboard_sink;
    let (task_tx, _task_rx) = mpsc::channel::<DshTask>();
    let (events_tx, _events_rx) = crate::dsh::backend::event_channel();
    let mut state = DshState::new(3080, dsh_describe_fixture(), task_tx, events_tx);
    state.test_mark_ws_open();
    app.dsh = Some(state);
    app.dsh_connect = None;
    app.dsh_connect_rx = None;
    // 记忆文件改道临时路径——快照不得触碰真实 ~/.clat。
    app.dsh_memory_path = std::env::temp_dir().join(format!(
        "clat-dsh-memo-snap-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    app.default_status = "<DSH-ROOT>".into();
    app.status = "<DSH-ROOT>".into();
    let mut harness = Harness {
        app,
        terminal: Terminal::new(TestBackend::new(width, height)).expect("test terminal"),
        project_root,
        storage_root,
    };
    harness.event(UiEvent::Dsh(DshEvent::Reply(TaskReply::Restored {
        session: Some(DSH_SNAP_SESSION.into()),
        // 快照基线不携带会话 cwd：第二行保持 describe.cwd fixture
        //（workspace 显示的判别在 dsh_events 单测）。
        cwd: None,
    })));
    harness.event(UiEvent::Dsh(DshEvent::Reply(TaskReply::History {
        session: DSH_SNAP_SESSION.into(),
        events: Vec::new(),
    })));
    harness.settle_dsh_status();
    harness
}

impl Harness {
    /// 状态行去漂移：TTL flash 换回常驻占位（快照只锁布局与内容语义）。
    fn settle_dsh_status(&mut self) {
        self.app.status = "<DSH-ROOT>".into();
        self.app.status_until = None;
    }

    fn dsh_frame(&mut self, frame: crate::dsh::frames::DshFrame) {
        // 以 App 当前代际注入（快照态的 downlink 是预置的，代际恒 0）。
        let generation = self.app.dsh.as_ref().map(|dsh| dsh.generation).unwrap_or(0);
        self.event(UiEvent::Dsh(DshEvent::Frame { generation, frame }));
    }

    fn dsh_session_event(&mut self, event: crate::session::event::SessionEvent) {
        self.dsh_frame(crate::dsh::frames::DshFrame::SessionEvent {
            session_id: DSH_SNAP_SESSION.into(),
            event,
        });
    }
}

/// 连接期形态（§1.0）：dsh=None + dsh_connect 占位 + status 文案 +
/// 标题栏 loading / ○。
#[test]
fn dsh_connecting_snapshot() {
    let (storage_root, project_root) = roots("snap-dsh-connecting");
    let mut app = App::open_dsh(3080).expect("dsh app opens");
    app.test_freeze_tick = true;
    app.clipboard_writer = recording_clipboard_sink;
    assert!(app.dsh.is_none() && app.dsh_connect.is_some());
    let mut harness = Harness {
        app,
        terminal: Terminal::new(TestBackend::new(80, 24)).expect("test terminal"),
        project_root,
        storage_root,
    };
    harness.snapshot("dsh-connecting");
}

/// 空闲已连接（§4 闪光点吸收后的本体外观）：● 在线点 + 模型蓝名 +
/// title 空格 + 第二行宿主 cwd。
#[test]
fn dsh_idle_snapshot() {
    let mut harness = harness_dsh("snap-dsh-idle", 80, 24);
    assert!(harness.app.dsh.as_ref().is_some_and(|dsh| dsh.connected));
    harness.snapshot("dsh-idle");
}

/// 运行态：turn/start + text-delta chunk → App.running + phase 派生
///（Responding）+ 输入框 Running 标题（无召回段）。
#[test]
fn dsh_running_phase_snapshot() {
    let mut harness = harness_dsh("snap-dsh-running", 80, 24);
    harness.dsh_session_event(crate::session::event::SessionEvent::new(
        "turn/start",
        10,
        1_700_000_040_000,
        json!({"turn": 1}),
    ));
    harness.dsh_session_event(crate::session::event::SessionEvent::new(
        "assistant/chunk",
        11,
        1_700_000_041_000,
        json!({"turn": 1, "step": 1, "chunk": {"type": "text-delta", "index": 0, "text": "think"}}),
    ));
    assert!(harness.app.running, "turn/start sets running");
    harness.settle_dsh_status();
    harness.snapshot("dsh-running-phase");
}

/// 宿主审批帧 → CLAT PendingPermission 弹框（与 local
/// permission-dialog 形态对拍——同一 draw 管线）。
#[test]
fn dsh_approval_dialog_snapshot() {
    let mut harness = harness_dsh("snap-dsh-approval", 80, 24);
    harness.dsh_frame(crate::dsh::frames::DshFrame::ApprovalRequested {
        rpc_id: "rpc-approval-1".into(),
        session_id: DSH_SNAP_SESSION.into(),
        approval_id: "approval-1".into(),
        tool_name: "write_file".into(),
        call_id: Some("call-7".into()),
        reason: Some("writes outside the workspace".into()),
    });
    assert!(harness.app.pending_permission.is_some());
    harness.settle_dsh_status();
    harness.snapshot("dsh-approval-dialog");
}

/// 宿主问答帧 → PendingAskUser 弹框（多题：(1/2) 前缀融入题面）。
#[test]
fn dsh_ask_dialog_snapshot() {
    let mut harness = harness_dsh("snap-dsh-ask", 80, 24);
    harness.dsh_frame(crate::dsh::frames::DshFrame::QuestionRequested {
        rpc_id: "rpc-question-1".into(),
        session_id: DSH_SNAP_SESSION.into(),
        questions: json!([
            {"id": "q1", "question": "Which database?", "options": [
                {"label": "Postgres"}, {"label": "SQLite"}
            ]},
            {"id": "q2", "question": "Any migrations?", "options": []}
        ]),
    });
    assert!(harness.app.pending_ask_user.is_some());
    let question = &harness.app.pending_ask_user.as_ref().unwrap().question;
    assert!(question.question.contains("(1/2)"), "{}", question.question);
    harness.settle_dsh_status();
    harness.snapshot("dsh-ask-dialog");
}

/// /resume 选择器（返工终版·单一分组列表）：全部工作区常显、分组
/// 头 Faint 行、行内无工作区标签；打开定位当前工作区组（活跃会话
/// session-dsh-snap-0001 属 alpha 组——光标落 alpha 首行）。
#[test]
fn dsh_resume_picker_snapshot() {
    let mut harness = harness_dsh("snap-dsh-resume", 80, 24);
    let rows = dsh_resume_rows_fixture();
    harness.app.session_picker = Some(SessionPicker::new_dsh(rows, Some(DSH_SNAP_SESSION.into())));
    harness.snapshot("dsh-resume-picker");
}

fn dsh_resume_rows_fixture() -> Vec<crate::dsh::files::DshSessionRow> {
    // 活跃降序（files::read_sessions 的产出序）：alpha 组两行夹着
    // beta 组一行——分组归并后 alpha 组在前（最近活跃组）。
    vec![
        crate::dsh::files::DshSessionRow {
            session_id: DSH_SNAP_SESSION.into(),
            workspace_title: "alpha".into(),
            workspace_path: "/w/alpha".into(),
            title: Some("Fix the flaky test".into()),
            created_at_ms: 1_787_400_000_000,
            activity_ms: 1_787_493_900_000,
        },
        crate::dsh::files::DshSessionRow {
            session_id: "session-dsh-snap-0002".into(),
            workspace_title: "beta".into(),
            workspace_path: "/w/beta".into(),
            title: None,
            created_at_ms: 1_787_400_000_000,
            activity_ms: 1_787_400_000_000,
        },
        crate::dsh::files::DshSessionRow {
            session_id: "session-dsh-snap-0003".into(),
            workspace_title: "alpha".into(),
            workspace_path: "/w/alpha".into(),
            title: Some("Older alpha work".into()),
            created_at_ms: 1_787_300_000_000,
            activity_ms: 1_787_300_000_000,
        },
    ]
}

/// /model 选择器（宿主动态组）：组行 + current ● + 失败组灰行 +
/// 当前档位呈现（档位接入 2026-08-23：当前模型行常显其档位）。
#[test]
fn dsh_model_picker_snapshot() {
    let mut harness = harness_dsh("snap-dsh-model", 80, 24);
    harness.event(UiEvent::Dsh(DshEvent::Reply(TaskReply::Models(json!({
        "groups": [
            {"id": "deepseek", "name": "DeepSeek", "models": [
                {"id": "deepseek-chat", "name": "DeepSeek Chat",
                 "description": "fast general model",
                 "reasoning": {"efforts": [
                    {"id": "off", "name": "Off"},
                    {"id": "low", "name": "Low"},
                    {"id": "high", "name": "High"}
                 ]}},
                {"id": "deepseek-reasoner", "name": "DeepSeek Reasoner"}
            ]},
            {"id": "custom-ollama", "name": "Ollama (custom)", "models": [
                {"id": "llama-local", "name": "Llama Local"}
            ]}
        ],
        "failures": [
            {"id": "broken", "name": "Broken Provider", "message": "connect ECONNREFUSED"}
        ],
        "current": {"provider": "deepseek", "model": "deepseek-chat",
                    "reasoningEffort": "high"}
    })))));
    assert!(
        harness.app.picker.is_some(),
        "models reply opens the picker"
    );
    harness.settle_dsh_status();
    harness.snapshot("dsh-model-picker");
}

/// 档位循环（档位接入 2026-08-23）：二级高亮模型行 Shift+Tab 后行内
/// 呈现 pending 档位、footer 提示 ⇧Tab、标题栏档位段（当前档位）。
#[test]
fn dsh_model_effort_snapshot() {
    let mut harness = harness_dsh("snap-dsh-effort", 80, 24);
    harness.event(UiEvent::Dsh(DshEvent::Reply(TaskReply::Models(json!({
        "groups": [
            {"id": "deepseek", "name": "DeepSeek", "models": [
                {"id": "deepseek-chat", "name": "DeepSeek Chat",
                 "description": "fast general model",
                 "reasoning": {"efforts": [
                    {"id": "off", "name": "Off"},
                    {"id": "low", "name": "Low"},
                    {"id": "high", "name": "High"},
                    {"id": "max", "name": "Max"}
                 ]}}
            ]}
        ],
        "current": {"provider": "deepseek", "model": "deepseek-chat",
                    "reasoningEffort": "high"}
    })))));
    // 进二级（Enter）→ Shift+Tab：当前 high → pending max。
    harness.key(KeyCode::Enter);
    harness.key(KeyCode::BackTab);
    harness.settle_dsh_status();
    harness.snapshot("dsh-model-effort");
}

/// /perm 选择器（DSH 三档词汇：Read Only / Workspace Write /
/// Full Access + 宿主 description 原文）。
#[test]
fn dsh_permission_picker_snapshot() {
    let mut harness = harness_dsh("snap-dsh-perm", 80, 24);
    harness.dsh_session_event(crate::session::event::SessionEvent::new(
        "sandbox/mode",
        10,
        1_700_000_040_000,
        json!({"mode": "workspace-write"}),
    ));
    assert_eq!(
        harness.app.dsh.as_ref().unwrap().preset.as_deref(),
        Some("workspace-write"),
        "sandbox/mode folds into the preset projection"
    );
    harness.app.permission_picker = Some(PermissionPicker::new_dsh(
        crate::PermissionMode::ProjectWrite,
    ));
    harness.settle_dsh_status();
    harness.snapshot("dsh-permission-picker");
}

/// /rename 弹框（RenameDialog 复用，预填当前标题）。
#[test]
fn dsh_rename_dialog_snapshot() {
    let mut harness = harness_dsh("snap-dsh-rename", 80, 24);
    harness.app.session_title = Some("Fix the flaky test".into());
    harness.app.rename_dialog = Some(RenameDialog::new("Fix the flaky test"));
    harness.snapshot("dsh-rename-dialog");
}

/// 断线（自动重连拍板）：○ 空心 + 会话区顶部通知条（Error 角色）。
#[test]
fn dsh_disconnected_snapshot() {
    let mut harness = harness_dsh("snap-dsh-disconnect", 80, 24);
    harness.event(UiEvent::Dsh(DshEvent::LinkDown {
        generation: 0,
        reason: "connection closed by peer".into(),
    }));
    let dsh = harness.app.dsh.as_ref().unwrap();
    assert!(!dsh.connected);
    assert!(dsh.banner.is_some(), "the notice banner is up");
    harness.settle_dsh_status();
    harness.snapshot("dsh-disconnected");
}

/// 词汇违规（INV-D8 呈现）：未知非 ignorable 类型 → 标题栏 ⚠ N 徽标。
#[test]
fn dsh_unknown_events_snapshot() {
    let mut harness = harness_dsh("snap-dsh-unknown", 80, 24);
    harness.dsh_session_event(crate::session::event::SessionEvent::new(
        "mystery/kind",
        10,
        1_700_000_040_000,
        json!({"payload": true}),
    ));
    assert_eq!(harness.app.dsh.as_ref().unwrap().unknown_events, 1);
    harness.settle_dsh_status();
    harness.snapshot("dsh-unknown-events");
}

/// 插话回显徽标：running 态 Enter steer → pending 回显区 → 状态栏
/// phase 行 steering·N（与 local 同族样式）。
#[test]
fn dsh_steer_badge_snapshot() {
    let mut harness = harness_dsh("snap-dsh-steer", 80, 24);
    harness.dsh_session_event(crate::session::event::SessionEvent::new(
        "turn/start",
        10,
        1_700_000_040_000,
        json!({"turn": 1}),
    ));
    harness
        .app
        .conversation
        .push_pending_steering("while you are at it…".into());
    assert_eq!(harness.app.conversation.pending_steering_count(), 1);
    harness.settle_dsh_status();
    harness.snapshot("dsh-steer-badge");
}
