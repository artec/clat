//! AG-DOGFOOD-4（PASTE-1 + RUNERR-1）TUI 输入与终态保真测试。
//!
//! PASTE-1：真实终端粘贴（bracketed paste 的 `Event::Paste` 与 Ctrl+V 系统
//! 剪贴板两条入口）在 Message 草稿中必须保留段落硬换行——终端把粘贴换行
//! 发送为 `\r`/`\r\n`，`InputBuffer` 的行模型只认 `\n`，未归一化时多段文本
//! 视觉塌成一行（用户现场：ChatGPT/PWA 对照正常，仅 TUI 异常）。
//!
//! RUNERR-1：attached TUI 的 `prompt.settled` 处理只取 `outcome.type`，丢弃
//! `outcome.error`——失败只剩 `failed`。standalone（run_events.rs）与回放
//! （turn_end_notice_text）均已显示 `error: …`，本文件锁定 attached 实时路径
//! 与它们同源。

use super::snapshot_tests::{Harness, harness_native};
use crossterm::event::{Event, KeyCode};

// ---------------------------------------------------------------------------
// PASTE-1
// ---------------------------------------------------------------------------

/// 真实 bracketed paste 事件（终端把换行发为 \r / \r\n）进入 Message 草稿后，
/// 内容语义必须保留为 `\n` 硬换行，渲染按多行呈现。
#[test]
fn paste_event_keeps_paragraph_breaks_in_draft() {
    let mut harness = Harness::trusted("ag4-paste-paragraphs", 80, 24);
    let pasted = "AG-DOGFOOD-4 首段任务说明\r\n第二段引用内容\r\r末段验收标准";
    harness.event(super::UiEvent::Terminal(Event::Paste(pasted.into())));
    assert_eq!(
        harness.app.input.text(),
        "AG-DOGFOOD-4 首段任务说明\n第二段引用内容\n\n末段验收标准",
        "paste must normalize terminal CR/CRLF breaks to LF in the draft"
    );
    assert_eq!(
        harness.app.input.line_count(80),
        4,
        "three paragraphs plus one blank line must occupy four visual rows"
    );
    let projection = harness.draw_projection();
    // 投影把 CJK 宽字符渲染为带空格形式；断言前统一去空格。
    let squeeze = |line: &str| line.replace(' ', "");
    let first = projection
        .lines()
        .find(|line| squeeze(line).contains("首段任务说明"))
        .unwrap_or_else(|| panic!("first paragraph missing from: {projection}"));
    assert!(
        !squeeze(first).contains("末段验收标准"),
        "paragraphs must render on separate rows, not collapse into one: {projection}"
    );
    assert!(
        projection
            .lines()
            .any(|line| squeeze(line).contains("末段验收标准"))
    );
}

/// Ctrl+V 系统剪贴板路径（clipboard worker → apply_clipboard_result）与
/// bracketed paste 同一条保真契约。
#[test]
fn clipboard_text_paste_keeps_paragraph_breaks() {
    let mut harness = Harness::trusted("ag4-paste-clipboard", 80, 24);
    harness.event(super::UiEvent::Worker(
        super::worker::WorkerMessage::ClipboardPastePrepared(Ok(
            super::attachments::PreparedClipboardPaste::Text(
                "clipboard first\r\nclipboard second\r\rclipboard last".into(),
            ),
        )),
    ));
    assert_eq!(
        harness.app.input.text(),
        "clipboard first\nclipboard second\n\nclipboard last",
        "Ctrl+V clipboard text must preserve paragraph breaks as LF"
    );
}

/// P4 显式输入变体：LF / CRLF / 仅 CR / Unicode 行-段分隔符 / 无尾换行 /
/// Markdown 引用与代码缩进 / 较长文本，统一归一为 LF 且不提前发送。
#[test]
fn paste_variants_normalize_without_early_submit() {
    let cases: &[(&str, &str)] = &[
        ("A\nB\n\nC", "A\nB\n\nC"),
        ("A\r\nB\r\n\r\nC", "A\nB\n\nC"),
        (
            "only-cr-first\rsecond\r\rthird",
            "only-cr-first\nsecond\n\nthird",
        ),
        ("no-trailing", "no-trailing"),
        ("uni-l\u{2028}para\u{2029}graph", "uni-l\npara\ngraph"),
        (
            "> 引用行一\r\r    indented code()",
            "> 引用行一\n\n    indented code()",
        ),
    ];
    for (index, (input, expected)) in cases.iter().enumerate() {
        let mut harness = Harness::trusted(&format!("ag4-paste-variant-{index}"), 80, 24);
        harness.event(super::UiEvent::Terminal(Event::Paste((*input).into())));
        assert_eq!(
            harness.app.input.text(),
            *expected,
            "variant {index}: {input:?}"
        );
        assert!(
            harness.app.conversation.is_empty(),
            "paste must never submit: variant {index}"
        );
    }

    // 较长多段文本：保持响应与有界（单次归一化不改资源界，段落计数正确）。
    let long = "段落内容０１２３４５６７８９\r\r".repeat(60);
    let mut harness = Harness::trusted("ag4-paste-long", 80, 24);
    harness.event(super::UiEvent::Terminal(Event::Paste(long)));
    let text = harness.app.input.text().to_owned();
    assert_eq!(text.matches('\r').count(), 0, "no CR may survive paste");
    assert_eq!(
        text.split('\n').filter(|line| !line.is_empty()).count(),
        60,
        "all 60 paragraphs must survive"
    );
}

/// P2 负控：粘贴含换行的文本不触发发送；Shift+Enter 原契约不受影响
/// （真实提交面由 J1 联合用例在真实宿主链上复核）。
#[test]
fn paste_only_inserts_draft_and_editing_keys_survive() {
    let mut harness = Harness::trusted("ag4-paste-submit", 80, 24);
    harness.event(super::UiEvent::Terminal(Event::Paste(
        "first paragraph\r\n\r\nsecond paragraph".into(),
    )));
    assert!(
        harness.app.conversation.is_empty(),
        "pasted newlines must never submit"
    );
    assert_eq!(
        harness.app.input.text(),
        "first paragraph\n\nsecond paragraph"
    );
    // 原 Shift+Enter 契约：显式插入换行（不发送）。
    harness.key_with_modifiers(KeyCode::Enter, crossterm::event::KeyModifiers::SHIFT);
    assert!(harness.app.input.text().ends_with('\n'));
    assert!(harness.app.conversation.is_empty());
    // 追加粘贴与已有内容衔接，段落仍保留。
    harness.event(super::UiEvent::Terminal(Event::Paste(
        "tail-a\rtail-b".into(),
    )));
    assert!(harness.app.input.text().ends_with("tail-a\ntail-b"));
}

// ---------------------------------------------------------------------------
// RUNERR-1（attached TUI 实时路径；帧形状与 serve/shapes.rs 同源）
// ---------------------------------------------------------------------------

use serde_json::json;

fn settled(harness: &mut Harness, payload: serde_json::Value) -> String {
    let epoch = harness.app.native.as_ref().expect("native state").epoch;
    harness
        .app
        .handle_ui_event(super::UiEvent::Native(super::native::NativeEvent::Frame(
            epoch,
            crate::host::HostEvent::Control {
                kind: "prompt.settled".into(),
                payload,
            },
        )));
    harness.draw_projection()
}

/// E1：失败 settled 帧的真实原因必须进入转录，不能只剩 `failed`。
#[test]
fn attached_settled_failed_shows_real_reason() {
    let mut harness = harness_native("ag4-settled-failed-reason", true);
    let projection = settled(
        &mut harness,
        json!({"outcome": {"type": "failed", "error": "intentional failure (ag4 fixture)"}}),
    );
    assert!(
        projection.contains("intentional failure (ag4 fixture)"),
        "the real failure reason must be visible in the attached transcript: {projection}"
    );
    assert!(
        projection.contains("error:"),
        "the notice must identify itself as an error (TUI 与 standalone/回放同词汇): {projection}"
    );
    assert!(!harness.app.running, "settled ends the run");
}

/// E1/E3：失败但缺 error 字段（旧版/异常宿主）须给出明确的无原因标注，
/// 不得伪装成有原因或静默。
#[test]
fn attached_settled_failed_without_error_shows_explicit_fallback() {
    let mut harness = harness_native("ag4-settled-failed-noerror", true);
    let projection = settled(&mut harness, json!({"outcome": {"type": "failed"}}));
    assert!(
        projection.contains("no reason"),
        "a missing error field must produce an explicit no-reason notice: {projection}"
    );
}

/// E3 负控：cancelled 不被误标为 failed/error；completed 保持纯文案。
#[test]
fn attached_settled_cancelled_and_completed_stay_truthful() {
    let mut harness = harness_native("ag4-settled-cancelled", true);
    let projection = settled(
        &mut harness,
        json!({"outcome": {"type": "cancelled", "turns": 1, "usage": {}}}),
    );
    assert!(projection.contains("cancelled"));
    assert!(
        !projection.contains("error"),
        "cancel must not be labeled a failure"
    );

    let mut harness = harness_native("ag4-settled-completed", true);
    let projection = settled(
        &mut harness,
        json!({"outcome": {"type": "completed", "output": "done", "turns": 1, "usage": {}}}),
    );
    assert!(projection.contains("completed"));
    assert!(!projection.contains("error"));
}

/// E4：敌意错误字符串（换行/制表/ANSI 转义/超长/伪凭据形态）须有界、单行化
/// 处理，不得撕开转录或注入控制序列。
#[test]
fn attached_settled_error_is_bounded_and_sanitized() {
    let mut harness = harness_native("ag4-settled-hostile", true);
    let hostile = format!(
        "line1\nline2\ttabbed \x1b[31mred\x1b[0m {}",
        "sk-FAKE-KEY-MARKER-0123456789".repeat(300)
    );
    let projection = settled(
        &mut harness,
        json!({"outcome": {"type": "failed", "error": hostile}}),
    );
    // 转录项被压缩为有界单行：换行/控制字符不进入投影，超长文本被截断。
    let notice_line = projection
        .lines()
        .find(|line| line.contains("error:"))
        .expect("a failure notice line");
    assert!(
        !notice_line.contains('\x1b'),
        "ANSI escapes must not reach the terminal: {notice_line:?}"
    );
    assert!(
        !notice_line.contains("sk-FAKE-KEY-MARKER"),
        "credential-shaped values must never be visible verbatim (E4): {notice_line}"
    );
    assert!(
        notice_line.contains("[REDACTED]"),
        "the credential is replaced by the shared redaction marker: {notice_line}"
    );
    assert!(
        notice_line.chars().count() <= 300,
        "the notice must stay bounded, saw {} chars: {notice_line}",
        notice_line.chars().count()
    );
}

/// E2 对拍：断线重连/回放路径（journal → TurnEnded::Error）已显示原因——
/// 本用例钉住该行为，保证实时与回放两条腿一致。
#[test]
fn attached_replay_keeps_failure_reason() {
    let mut harness = harness_native("ag4-settled-replay-parity", true);
    let epoch = harness.app.native.as_ref().expect("native state").epoch;
    let frame = |app: &mut super::App, kind: &str, payload: serde_json::Value| {
        app.handle_ui_event(super::UiEvent::Native(super::native::NativeEvent::Frame(
            epoch,
            crate::host::HostEvent::Control {
                kind: kind.to_owned(),
                payload,
            },
        )));
    };
    frame(
        &mut harness.app,
        "replay.begin",
        serde_json::json!({"has_more": false}),
    );
    harness
        .app
        .handle_ui_event(super::UiEvent::Native(super::native::NativeEvent::Frame(
            epoch,
            crate::host::HostEvent::Replay(crate::session::replay::ReplayEvent::TurnEnded {
                seq: 7,
                turn: 2,
                time_ms: 0,
                reason: crate::session::replay::ReplayTurnEnd::Error {
                    message: "intentional failure (ag4 fixture)".into(),
                },
            }),
        )));
    frame(
        &mut harness.app,
        "replay.end",
        serde_json::json!({"has_more": false}),
    );
    let projection = harness.draw_projection();
    assert!(
        projection.contains("intentional failure (ag4 fixture)"),
        "replay must keep the failure reason: {projection}"
    );
}

/// E4（返工 F1）：人工构造的无效凭据形态（Authorization: Bearer sk-…）
/// 不得原样进入可见转录——实时与回放两条入口都要遮蔽，同时保留
/// "HTTP 401"等排障信息。
#[test]
fn attached_settled_error_redacts_synthetic_credentials() {
    let mut harness = harness_native("ag4-settled-redact-live", true);
    let synthetic =
        "HTTP 401 Authorization: Bearer sk-AG4SYNTHETICONLY1234567890 rejected by upstream";
    let projection = settled(
        &mut harness,
        json!({
            "prompt_rpc_id": "ag4-rpc-redact",
            "outcome": {"type": "failed", "error": synthetic}
        }),
    );
    assert!(
        !projection.contains("sk-AG4SYNTHETICONLY1234567890")
            && !projection.contains("AG4SYNTHETICONLY"),
        "a credential-shaped value must never be visible verbatim: {projection}"
    );
    assert!(
        projection.contains("[REDACTED]"),
        "the redaction marker should be visible: {projection}"
    );
    assert!(
        projection.contains("HTTP 401"),
        "diagnostic context (status, origin) must survive redaction: {projection}"
    );
}

/// E4（返工 F1）回放入口：journal 回放的 TurnEnded::Error 同样遮蔽凭据。
#[test]
fn attached_replay_error_redacts_synthetic_credentials() {
    let mut harness = harness_native("ag4-settled-redact-replay", true);
    let epoch = harness.app.native.as_ref().expect("native state").epoch;
    let synthetic =
        "upstream 403 Authorization: Bearer sk-AG4SYNTHETICONLY1234567890 quota exhausted";
    let control = |app: &mut super::App, kind: &str| {
        app.handle_ui_event(super::UiEvent::Native(super::native::NativeEvent::Frame(
            epoch,
            crate::host::HostEvent::Control {
                kind: kind.to_owned(),
                payload: serde_json::json!({"has_more": false}),
            },
        )));
    };
    control(&mut harness.app, "replay.begin");
    harness
        .app
        .handle_ui_event(super::UiEvent::Native(super::native::NativeEvent::Frame(
            epoch,
            crate::host::HostEvent::Replay(crate::session::replay::ReplayEvent::TurnEnded {
                seq: 9,
                turn: 1,
                time_ms: 0,
                reason: crate::session::replay::ReplayTurnEnd::Error {
                    message: synthetic.into(),
                },
            }),
        )));
    control(&mut harness.app, "replay.end");
    let projection = harness.draw_projection();
    assert!(
        !projection.contains("sk-AG4SYNTHETICONLY1234567890")
            && !projection.contains("AG4SYNTHETICONLY"),
        "replay must not expose credential-shaped values either: {projection}"
    );
    assert!(projection.contains("[REDACTED]"));
    assert!(
        projection.contains("upstream 403"),
        "diagnostic context must survive redaction on the replay path: {projection}"
    );
}

/// E4（返工 F1）补充反例：U+2028/U+2029 不是 Unicode 控制字符（Cc），
/// 单行清洗必须显式折叠行/段分隔符，否则"单行通知"承诺不成立。
#[test]
fn attached_settled_error_folds_unicode_separators() {
    let mut harness = harness_native("ag4-settled-unicode-fold", true);
    let payload_text = "first\u{2028}second\u{2029}third";
    let projection = settled(
        &mut harness,
        json!({"outcome": {"type": "failed", "error": payload_text}}),
    );
    let notice_line = projection
        .lines()
        .find(|line| line.contains("error:"))
        .expect("a failure notice line");
    assert!(
        !notice_line.contains('\u{2028}') && !notice_line.contains('\u{2029}'),
        "Unicode line/paragraph separators must be folded in the one-line notice: {notice_line:?}"
    );
    assert!(notice_line.contains("first second third"));
}

/// E3（返工 F3）：同 prompt_rpc_id 的重复 settled 帧幂等——转录只能出现
/// 一张终态卡（受控协议鲁棒性；健康宿主恰一广播之外的重发防线）。
#[test]
fn duplicate_settled_frame_renders_one_terminal_card() {
    let mut harness = harness_native("ag4-settled-dup", true);
    let payload = json!({
        "prompt_rpc_id": "ag4-rpc-dup",
        "outcome": {"type": "failed", "error": "intentional failure (ag4 fixture)"}
    });
    let _ = settled(&mut harness, payload.clone());
    let projection = settled(&mut harness, payload);
    let cards = projection
        .lines()
        .filter(|line| line.contains("error:"))
        .count();
    assert_eq!(
        cards, 1,
        "a redelivered settled frame must not add a second terminal card: {projection}"
    );
    assert!(!harness.app.running);
}

/// E3（返工 F3）负控：不同 prompt_rpc_id 的两帧是两次合法结算，
/// 各自的终态卡都要保留（去重不得过度抑制）。
#[test]
fn distinct_settled_frames_render_two_cards() {
    let mut harness = harness_native("ag4-settled-distinct", true);
    let first = json!({
        "prompt_rpc_id": "ag4-rpc-a",
        "outcome": {"type": "failed", "error": "first failure"}
    });
    let second = json!({
        "prompt_rpc_id": "ag4-rpc-b",
        "outcome": {"type": "failed", "error": "second failure"}
    });
    let _ = settled(&mut harness, first);
    let projection = settled(&mut harness, second);
    let cards = projection
        .lines()
        .filter(|line| line.contains("error:"))
        .count();
    assert_eq!(
        cards, 2,
        "two distinct settlements keep two cards: {projection}"
    );
}

/// CF-R2-1（返工 r3）：换行折叠在脱敏之后——`Bearer` 与值之间以 LF 分隔
/// 时，redactor 先扫（LF 阻断）、sanitizer 后折（LF→空格），组合后凭据
/// 变为可见的 `Bearer <值>`。展示管线必须先折叠再遮蔽。live 入口。
#[test]
fn attached_settled_error_redacts_folded_bearer_live() {
    let mut harness = harness_native("ag4-r3-fold-bearer-live", true);
    let synthetic = "HTTP 401 Authorization: Bearer\nAG4SYNTHETICVALUE1234567890";
    let projection = settled(
        &mut harness,
        json!({"outcome": {"type": "failed", "error": synthetic}}),
    );
    assert!(
        !projection.contains("AG4SYNTHETICVALUE"),
        "a credential value split across lines must not survive fold+redact: {projection}"
    );
    assert!(
        projection.contains("[REDACTED]"),
        "the redaction marker must appear: {projection}"
    );
    assert!(
        projection.contains("HTTP 401"),
        "diagnostic context must survive: {projection}"
    );
}

/// CF-R2-1 回放入口同管线核对。
#[test]
fn attached_replay_error_redacts_folded_bearer() {
    let mut harness = harness_native("ag4-r3-fold-bearer-replay", true);
    let epoch = harness.app.native.as_ref().expect("native state").epoch;
    let synthetic = "upstream 403 Authorization: Bearer\r\nAG4SYNTHETICVALUE1234567890 rejected";
    let control = |app: &mut super::App, kind: &str| {
        app.handle_ui_event(super::UiEvent::Native(super::native::NativeEvent::Frame(
            epoch,
            crate::host::HostEvent::Control {
                kind: kind.to_owned(),
                payload: serde_json::json!({"has_more": false}),
            },
        )));
    };
    control(&mut harness.app, "replay.begin");
    harness
        .app
        .handle_ui_event(super::UiEvent::Native(super::native::NativeEvent::Frame(
            epoch,
            crate::host::HostEvent::Replay(crate::session::replay::ReplayEvent::TurnEnded {
                seq: 11,
                turn: 1,
                time_ms: 0,
                reason: crate::session::replay::ReplayTurnEnd::Error {
                    message: synthetic.into(),
                },
            }),
        )));
    control(&mut harness.app, "replay.end");
    let projection = harness.draw_projection();
    assert!(
        !projection.contains("AG4SYNTHETICVALUE"),
        "replay must redact folded credential values too: {projection}"
    );
    assert!(projection.contains("[REDACTED]"));
    assert!(
        projection.contains("upstream 403"),
        "diagnostic context must survive on replay: {projection}"
    );
}

/// CF-R2-2（返工 r3）：跨轮迟到终态。A/B 依次结算后，第三轮 Run 进行中
/// 重放早已处理过的 settled(A)——旧终态不得覆盖当前 run 状态、不得
/// 再推终态卡。全部经真实 RunStarted/settled 帧消费入口。
#[test]
fn late_replayed_settlement_cannot_override_active_run() {
    use crate::event::RunEvent;
    use crate::host::HostEvent;
    use crate::message::MessageContent;
    use crate::tui::native::NativeEvent;
    use crate::tui::worker::UiEvent;

    let mut harness = harness_native("ag4-r3-late-settled", true);
    let epoch = harness.app.native.as_ref().expect("native state").epoch;
    let frame = |app: &mut super::App, event: RunEvent| {
        app.handle_ui_event(UiEvent::Native(NativeEvent::Frame(
            epoch,
            HostEvent::Run(event),
        )));
    };
    let started = |round: &str| RunEvent::RunStarted {
        project: std::path::PathBuf::from("/tmp/ag4-r3"),
        message: MessageContent::text(round),
        client_message_id: None,
    };
    let settled_frame = |app: &mut super::App, rpc: &str, kind: serde_json::Value| {
        app.handle_ui_event(UiEvent::Native(NativeEvent::Frame(
            epoch,
            HostEvent::Control {
                kind: "prompt.settled".into(),
                payload: serde_json::json!({"prompt_rpc_id": rpc, "outcome": kind}),
            },
        )));
    };

    // 轮 A：真实启动 → 结算 A（failed）。
    frame(&mut harness.app, started("round A"));
    settled_frame(
        &mut harness.app,
        "ag4-rpc-a",
        serde_json::json!({"type": "failed", "error": "first failure"}),
    );
    // 轮 B：真实启动 → 结算 B（completed）。
    frame(&mut harness.app, started("round B"));
    settled_frame(
        &mut harness.app,
        "ag4-rpc-b",
        serde_json::json!({"type": "completed", "output": "ok"}),
    );
    // 轮 C：真实启动，运行中。
    frame(&mut harness.app, started("round C"));
    assert!(harness.app.running, "precondition: round C is live");

    // 迟到重放已处理过的 settled(A)：不得复位 running、不得新增终态卡。
    settled_frame(
        &mut harness.app,
        "ag4-rpc-a",
        serde_json::json!({"type": "failed", "error": "first failure"}),
    );
    assert!(
        harness.app.running,
        "a late replay of an already-settled prompt must not end the active run"
    );
    let projection = harness.draw_projection();
    let failure_cards = projection
        .lines()
        .filter(|line| line.contains("error:"))
        .count();
    assert_eq!(
        failure_cards, 1,
        "the late replay must not add a second terminal card: {projection}"
    );
    // 取消在途门同样不得被旧终态清除（E3 归属）。
    if let Some(native) = harness.app.native.as_mut() {
        native.cancel_inflight = Some(native.run_seq);
    }
    settled_frame(
        &mut harness.app,
        "ag4-rpc-a",
        serde_json::json!({"type": "failed", "error": "first failure"}),
    );
    let inflight = harness
        .app
        .native
        .as_ref()
        .and_then(|native| native.cancel_inflight);
    assert_eq!(
        inflight,
        harness.app.native.as_ref().map(|native| native.run_seq),
        "a late replay must not clear the active run's cancel gate"
    );
}

// （127.0.0.1 随机端口 + 真实握手/SSE 线格式），在线泵、传输、事件解码、
// App 管线、转录、重连全部走 attached TUI 生产路径；宿主侧行为的线形状
// 与 core serve 测试（prompt.settled 三 outcome 恰一等）同源。
// 真实宿主/持久化/冷恢复链见文件末尾的 J1-b（真实 serve 子进程）。
// ---------------------------------------------------------------------------

use crate::host::HostClient;
use std::collections::VecDeque;
use std::io::{Read as _, Write};
use std::net::TcpListener;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Mutex, mpsc};
use std::time::{Duration, Instant};

const JOINT_WAIT: Duration = Duration::from_secs(30);
const JOINT_FAILURE: &str = "intentional failure (ag4 fixture)";

/// SSE 帧队列：/api/events 连接线程持有写半；RPC 线程入队待发帧。
#[derive(Clone, Default)]
struct FrameQueue(Arc<Mutex<VecDeque<String>>>);

impl FrameQueue {
    fn push(&self, frame: String) {
        self.0.lock().expect("frame queue").push_back(frame);
    }
}

/// 当前活跃 SSE 队列槽：每个新 /api/events 连接替换槽位；重连后旧连接
/// 的转发循环自然停摆（客户端 shutdown 断流），帧只进新流。
#[derive(Clone, Default)]
struct SseSlot(Arc<Mutex<Option<FrameQueue>>>);

impl SseSlot {
    fn install(&self, queue: &FrameQueue) {
        *self.0.lock().expect("sse slot") = Some(queue.clone());
    }

    fn publish(&self, frame: String) {
        if let Some(queue) = self.0.lock().expect("sse slot").as_ref() {
            queue.push(frame);
        }
    }
}

/// CF-R3-1（返工 r4）：截断不得使可识别凭据退化为未遮蔽片段。232 字符
/// 前缀把完整可识别的 `sk-` token 推到 240 通知预算边缘——先截断的
/// 管线会把它切成低于识别阈值的 `sk-AG4SYN` 片段而泄露。阳性对照先
/// 证明该完整输入本就会被共享原语遮蔽（不需要新词表）。
#[test]
fn attached_settled_error_truncation_must_not_reveal_secret_prefix() {
    let full_token = "sk-AG4SYNTHETICONLY1234567890";
    // 阳性对照：完整原文经共享原语必然遮蔽（"原本可识别"成立）。
    let control = crate::redact_secrets(&format!("HTTP 401 {full_token}"));
    assert!(
        !control.contains(full_token) && control.contains("[REDACTED]"),
        "positive control: the shared redactor recognizes this token: {control}"
    );

    let prefix = format!("HTTP 401 {} ", "x".repeat(222));
    assert_eq!(prefix.chars().count(), 232);
    let synthetic = format!("{prefix}{full_token} and more diagnostics follow");
    let mut harness = super::snapshot_tests::harness_native_wide("ag4-r4-trunc-prefix", true, 320);
    let projection = settled(
        &mut harness,
        json!({"outcome": {"type": "failed", "error": synthetic}}),
    );
    let notice_line = projection
        .lines()
        .find(|line| line.contains("error:"))
        .expect("a failure notice line");
    assert!(
        !notice_line.contains("AG4SYNTHETICONLY") && !notice_line.contains("sk-AG4SY"),
        "a truncation boundary must not degrade a recognizable token into a visible prefix: {notice_line}"
    );
    assert!(
        notice_line.contains("[REDACTED]") || !notice_line.contains("sk-"),
        "the token must be redacted or absent: {notice_line}"
    );
    assert!(
        notice_line.contains("HTTP 401"),
        "diagnostic context must survive: {notice_line}"
    );
}

/// CF-R3-2（返工 r4）：任意历史距离的迟到终态。1+40 轮合法结算（足以
/// 淘汰任何固定窗口）后重放最早的 settled(A)，不得结束运行中的新 run、
/// 不得新增终态卡。全部经真实 RunStarted/settled 帧消费入口。
#[test]
fn late_settlement_beyond_any_window_cannot_override_active_run() {
    use crate::event::RunEvent;
    use crate::host::HostEvent;
    use crate::message::MessageContent;
    use crate::tui::native::NativeEvent;
    use crate::tui::worker::UiEvent;

    let mut harness = harness_native("ag4-r4-late-beyond-window", true);
    let epoch = harness.app.native.as_ref().expect("native state").epoch;
    let run = |round: &str| RunEvent::RunStarted {
        project: std::path::PathBuf::from("/tmp/ag4-r4"),
        message: MessageContent::text(round),
        client_message_id: None,
    };
    let settle = |app: &mut super::App, rpc: &str, kind: serde_json::Value| {
        app.handle_ui_event(UiEvent::Native(NativeEvent::Frame(
            epoch,
            HostEvent::Control {
                kind: "prompt.settled".into(),
                payload: serde_json::json!({"prompt_rpc_id": rpc, "outcome": kind}),
            },
        )));
    };
    let run_frame = |app: &mut super::App, round: &str| {
        app.handle_ui_event(UiEvent::Native(NativeEvent::Frame(
            epoch,
            HostEvent::Run(run(round)),
        )));
    };

    // 轮 0（A）：结算并记录。
    run_frame(&mut harness.app, "round A");
    settle(
        &mut harness.app,
        "ag4-rpc-a",
        serde_json::json!({"type": "failed", "error": "first failure"}),
    );
    // 40 个后续合法轮（不同 rpc_id，足以淘汰任何 32 条窗口）。
    for index in 0..40 {
        run_frame(&mut harness.app, "filler");
        settle(
            &mut harness.app,
            &format!("ag4-rpc-fill-{index}"),
            serde_json::json!({"type": "completed", "output": "ok"}),
        );
    }
    // 新轮 C 运行中，重放最早的 settled(A)。
    run_frame(&mut harness.app, "round C");
    assert!(harness.app.running, "precondition: round C is live");
    // 卡主键记忆：A 的终态卡在转录中（任意历史距离，不受窗口淘汰）。
    assert!(
        harness.app.conversation.has_settled_card("ag4-rpc-a"),
        "the transcript must still remember round A's terminal card"
    );
    settle(
        &mut harness.app,
        "ag4-rpc-a",
        serde_json::json!({"type": "failed", "error": "first failure"}),
    );
    assert!(
        harness.app.running,
        "a late settlement beyond any window must not end the active run"
    );
    // 重放不得新增终态卡：A 的卡仍只有既有一张（以主键计数不可行于
    // 视口断言——40 轮 filler 把它推出 24 行视口；以"卡存在 + running
    // 保持 + 取消门保持"三重状态断言锁定重复被拦）。
    assert!(harness.app.conversation.has_settled_card("ag4-rpc-a"));
    if let Some(native) = harness.app.native.as_mut() {
        native.cancel_inflight = Some(native.run_seq);
    }
    settle(
        &mut harness.app,
        "ag4-rpc-a",
        serde_json::json!({"type": "failed", "error": "first failure"}),
    );
    let inflight = harness
        .app
        .native
        .as_ref()
        .and_then(|native| native.cancel_inflight);
    assert_eq!(
        inflight,
        harness.app.native.as_ref().map(|native| native.run_seq),
        "the beyond-window replay must not clear the active run's cancel gate"
    );
}

// （127.0.0.1 随机端口 + 真实握手/SSE 线格式），在线泵、传输、事件解码、
// App 管线、转录、重连全部走 attached TUI 生产路径；宿主侧行为的线形状
// 与 core serve 测试（prompt.settled 三 outcome 恰一等）同源。
// 真实宿主/持久化/冷恢复链见 tests/ag4_joint_real_host.rs（J1-b）。
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// J1-a（受控协议消费层）：CF7 同款真实 HTTP fixture 宿主
// （127.0.0.1 随机端口 + 真实握手/SSE 线格式），在线泵、传输、事件解码、
// App 管线、转录、重连全部走 attached TUI 生产路径；宿主侧行为的线形状
// 与 core serve 测试（prompt.settled 三 outcome 恰一等）同源。
// 真实宿主/持久化/冷恢复链见 tests/ag4_joint_real_host.rs（J1-b）。
// ---------------------------------------------------------------------------

/// 真实 HTTP+SSE fixture 宿主（每连接一线程）：
/// - `/api/host.describe` 真实握手（协议/根身份与 CF7 fixture 同款）；
/// - `/api/events` 真实 SSE 流（`event:`/`data:` 线格式，v1 信封）；
/// - `/api/prompt.send` 捕获真实提交文本，随后在 SSE 流上按生产顺序推
///   `run_started`（回显提交文本）与 `prompt.settled`（failed+error，
///   与 serve/shapes.rs `settled_failed` 同形状）；
/// - 重连时第二条 SSE 流重放 journal 事实（user_message + turn_ended
///   error）再 `subscribed`，与真实宿主重连语义一致。
struct JointHostFixture {
    port: u16,
    root: std::path::PathBuf,
    submitted: Arc<Mutex<Vec<String>>>,
    connections: Arc<AtomicU64>,
    stop: Arc<AtomicBool>,
    slot: SseSlot,
}

impl JointHostFixture {
    fn spawn() -> Self {
        let base = std::env::temp_dir().join(format!(
            "clat-ag4-joint-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let root = base.join("home");
        std::fs::create_dir_all(&root).expect("fixture root");
        let root = root.canonicalize().expect("canonical root");
        let listener = TcpListener::bind("127.0.0.1:0").expect("fixture bind");
        let port = listener.local_addr().unwrap().port();
        let fixture = Self {
            port,
            root: root.clone(),
            submitted: Arc::new(Mutex::new(Vec::new())),
            connections: Arc::new(AtomicU64::new(0)),
            stop: Arc::new(AtomicBool::new(false)),
            slot: SseSlot::default(),
        };

        let submitted = Arc::clone(&fixture.submitted);
        let connections = Arc::clone(&fixture.connections);
        let stop = Arc::clone(&fixture.stop);
        let slot = fixture.slot.clone();
        std::thread::Builder::new()
            .name("clat-ag4-joint-accept".into())
            .spawn(move || {
                for stream in listener.incoming() {
                    let Ok(stream) = stream else { break };
                    if stop.load(Ordering::Relaxed) {
                        break;
                    }
                    let state = (
                        Arc::clone(&submitted),
                        Arc::clone(&connections),
                        Arc::clone(&stop),
                        slot.clone(),
                        root.clone(),
                    );
                    std::thread::Builder::new()
                        .name("clat-ag4-joint-conn".into())
                        .spawn(move || handle_connection(stream, state))
                        .expect("connection thread");
                }
            })
            .expect("joint fixture thread");
        fixture
    }

    fn submitted_texts(&self) -> Vec<String> {
        self.submitted.lock().expect("submitted").clone()
    }
}

impl Drop for JointHostFixture {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        let _ = std::net::TcpStream::connect(("127.0.0.1", self.port));
    }
}

type ConnState = (
    Arc<Mutex<Vec<String>>>,
    Arc<AtomicU64>,
    Arc<AtomicBool>,
    SseSlot,
    std::path::PathBuf,
);

fn handle_connection(mut stream: std::net::TcpStream, state: ConnState) {
    let (submitted, connections, stop, slot, root) = state;
    stream.set_read_timeout(Some(Duration::from_secs(30))).ok();
    let mut head = Vec::new();
    let mut byte = [0u8; 1];
    while !head.ends_with(b"\r\n\r\n") && head.len() < 64 * 1024 {
        match stream.read(&mut byte) {
            Ok(0) | Err(_) => break,
            Ok(_) => head.push(byte[0]),
        }
    }
    let head_text = String::from_utf8_lossy(&head).into_owned();
    let content_length: usize = head_text
        .lines()
        .find_map(|line| {
            let value = line.strip_prefix("Content-Length:")?;
            value.trim().parse().ok()
        })
        .unwrap_or(0);
    let mut body = vec![0u8; content_length];
    if content_length > 0 {
        let _ = stream.read_exact(&mut body);
    }
    let request_line = head_text.lines().next().unwrap_or_default().to_owned();

    if request_line.contains("/api/events") {
        // SSE 连接：按生产顺序发送订阅序列，之后转发队列帧。
        let index = connections.fetch_add(1, Ordering::SeqCst) + 1;
        let queue = FrameQueue::default();
        slot.install(&queue);
        let Ok(mut writer) = sse_handshake(&mut stream) else {
            return;
        };
        let mut send = |frame: &str| -> bool {
            writer.write_all(frame.as_bytes()).is_ok() && writer.flush().is_ok()
        };
        if index == 1 {
            send("event: replay.begin\ndata: {\"has_more\":false}\n\n");
            send("event: replay.end\ndata: {\"has_more\":false}\n\n");
            send(&control_frame(
                "subscribed",
                serde_json::json!({"selection_generation": 0}),
            ));
        } else {
            // 重连：journal 事实回放（user_message + turn_ended error），
            // 然后重新订阅。
            let text = submitted
                .lock()
                .expect("submitted")
                .last()
                .cloned()
                .unwrap_or_default();
            send("event: replay.begin\ndata: {\"has_more\":false}\n\n");
            send(&replay_frame(serde_json::json!({
                "type": "user_message", "seq": 3, "turn": 1,
                "time_ms": 0, "text": text, "content_blocks": [],
            })));
            send(&replay_frame(serde_json::json!({
                "type": "turn_ended", "seq": 7, "turn": 1,
                "time_ms": 0,
                // 线格式（codec::turn_end）：{"error": message}，
                // 与 journal 侧 kind 标签形状不同。
                "reason": {"error": JOINT_FAILURE},
            })));
            send("event: replay.end\ndata: {\"has_more\":false}\n\n");
            send(&control_frame(
                "subscribed",
                serde_json::json!({"selection_generation": 0}),
            ));
        }
        // 转发后续入队帧（prompt.send 触发的 run 事件）。
        let deadline = Instant::now() + Duration::from_secs(60);
        while Instant::now() < deadline && !stop.load(Ordering::Relaxed) {
            if let Some(frame) = queue.0.lock().expect("frame queue").pop_front() {
                if !send(&frame) {
                    break;
                }
            } else {
                std::thread::sleep(Duration::from_millis(10));
            }
        }
        return;
    }

    let (status, response_body) = if request_line.contains("/api/host.describe") {
        let value = serde_json::json!({
            "instance_id": uuid::Uuid::new_v4().to_string(),
            "storage_root": root.to_string_lossy(),
            "protocol_version": crate::host::HOST_PROTOCOL_VERSION,
            "wire_version": crate::client_ports::host::wire_version(),
            "journal_version": crate::client_ports::host::journal_format_version(),
        });
        (
            "HTTP/1.1 200 OK",
            serde_json::json!({"ok": true, "value": value}).to_string(),
        )
    } else if request_line.contains("/api/prompt.send") {
        let value: serde_json::Value =
            serde_json::from_slice(&body).unwrap_or(serde_json::Value::Null);
        let text = value["text"].as_str().unwrap_or_default().to_owned();
        submitted.lock().expect("submitted").push(text.clone());
        // 与生产同序：先入队 run 事件 + 终态结算帧，再回 RPC。
        slot.publish(run_event_frame(serde_json::json!({
            "type": "run_started",
            "project": "/tmp/ag4-joint-proj",
            "prompt": text,
        })));
        slot.publish(control_frame(
            "prompt.settled",
            serde_json::json!({
                "prompt_rpc_id": "ag4-rpc-1",
                "outcome": {"type": "failed", "error": JOINT_FAILURE},
            }),
        ));
        (
            "HTTP/1.1 200 OK",
            serde_json::json!({
                "ok": true,
                "value": {"kind": "prompt", "receipt": {"state": "committed"}},
            })
            .to_string(),
        )
    } else {
        // workbench.info / 其它 RPC：最小合法快照。
        (
            "HTTP/1.1 200 OK",
            serde_json::json!({"ok": true, "value": {}}).to_string(),
        )
    };
    let response = format!(
        "{status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{response_body}",
        response_body.len()
    );
    let _ = stream.write_all(response.as_bytes());
}

fn sse_handshake(
    stream: &mut std::net::TcpStream,
) -> std::io::Result<std::io::BufWriter<std::net::TcpStream>> {
    stream.shutdown(std::net::Shutdown::Read)?;
    let mut writer = std::io::BufWriter::new(stream.try_clone()?);
    writer.write_all(
        b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nCache-Control: no-cache\r\n\r\n",
    )?;
    writer.flush()?;
    Ok(writer)
}

/// `event:` + `data:` 帧（与 transport::HostEvents::next_frame 的解析对应）。
fn control_frame(kind: &str, payload: serde_json::Value) -> String {
    format!(
        "event: {kind}\ndata: {}\n\n",
        serde_json::json!({"v": 1, "ctl": payload})
    )
}

fn replay_frame(event: serde_json::Value) -> String {
    format!(
        "event: replay\ndata: {}\n\n",
        serde_json::json!({"v": 1, "replay": event})
    )
}

fn run_event_frame(event: serde_json::Value) -> String {
    format!(
        "event: event\ndata: {}\n\n",
        serde_json::json!({"v": 1, "event": event})
    )
}

/// 泵：把真实在线线程送回的 UiEvent 喂进 App，直到 `ready` 为真或超时；
/// 随后排空余量帧。
fn pump_until(
    app: &mut super::App,
    events: &mpsc::Receiver<super::UiEvent>,
    ready: impl Fn(&super::App) -> bool,
    stage: &str,
) {
    let deadline = Instant::now() + JOINT_WAIT;
    while !ready(app) {
        if Instant::now() > deadline {
            panic!("joint scenario timed out at stage `{stage}`");
        }
        match events.recv_timeout(Duration::from_millis(50)) {
            Ok(event) => app.handle_ui_event(event),
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                panic!("ui event channel disconnected at stage `{stage}`")
            }
        }
    }
    let drain_deadline = Instant::now() + Duration::from_millis(300);
    while Instant::now() < drain_deadline {
        match events.recv_timeout(Duration::from_millis(50)) {
            Ok(event) => app.handle_ui_event(event),
            Err(_) => break,
        }
    }
}

#[test]
fn joint_paste_submit_failure_reason_and_replay() {
    let fixture = JointHostFixture::spawn();
    let client = HostClient::connect(fixture.port, "ag4-joint-token".into(), &fixture.root)
        .expect("real host handshake");
    let mut harness = Harness {
        app: super::App::open_native(
            crate::Project::new(std::path::PathBuf::from("/tmp/ag4-joint-proj")),
            client,
        )
        .expect("attached TUI shell"),
        terminal: ratatui::Terminal::new(ratatui::backend::TestBackend::new(80, 24))
            .expect("test terminal"),
        project_root: std::path::PathBuf::from("/tmp/ag4-joint-proj"),
        storage_root: fixture.root.clone(),
    };
    // 真实在线泵：与生产 run() 同源的 ui_event_channel + start_native。
    let (sender, events) = super::ui_event_channel();
    harness.app.event_sender = Some(sender);
    harness.app.start_native();
    pump_until(
        &mut harness.app,
        &events,
        |app| app.native.as_ref().is_some_and(|native| native.online),
        "subscribe online",
    );

    // 1) 真实粘贴（终端把换行发为 CR）：草稿保留段落。
    let pasted = "AG-DOGFOOD-4 联合首段\r\n第二段：粘贴须保留段落\r\r末段：失败原因须可见";
    harness.event(super::UiEvent::Terminal(Event::Paste(pasted.into())));
    assert_eq!(
        harness.app.input.text().matches('\n').count(),
        3,
        "three paragraph breaks must survive in the draft: {:?}",
        harness.app.input.text()
    );

    // 2) 真实提交：Enter → prompt.send（fixture 捕获真实文本）→ SSE 回流
    //    run_started + settled failed+error。
    harness.key(KeyCode::Enter);
    pump_until(
        &mut harness.app,
        &events,
        |app| !app.running && !app.conversation.is_empty(),
        "run settles",
    );

    // 3) 提交面保真：宿主收到的正是归一化后的段落文本。
    let submitted = fixture.submitted_texts();
    assert_eq!(
        submitted,
        vec!["AG-DOGFOOD-4 联合首段\n第二段：粘贴须保留段落\n\n末段：失败原因须可见"],
        "the host must receive the paragraph-preserving text"
    );

    // 4) 转录：用户消息段落保留 + 真实失败原因可见（同一 Session）。
    let projection = harness.draw_projection();
    let squeeze = |line: &str| line.replace(' ', "");
    let user_row = projection
        .lines()
        .find(|line| squeeze(line).contains("联合首段"))
        .unwrap_or_else(|| panic!("submitted first paragraph missing: {projection}"));
    assert!(
        !squeeze(user_row).contains("末段：失败原因须可见"),
        "submitted paragraphs must stay on separate rows: {projection}"
    );
    assert!(
        projection
            .lines()
            .any(|line| squeeze(line).contains("末段：失败原因须可见"))
    );
    let failure_row = projection
        .lines()
        .find(|line| squeeze(line).contains("error:"))
        .unwrap_or_else(|| panic!("failure notice missing: {projection}"));
    assert!(
        squeeze(failure_row).contains("intentionalfailure"),
        "the host's real failure reason must be visible: {projection}"
    );

    // 5) 重连回放：journal 事实源重建转录，段落与失败原因仍可辨认（E2）。
    harness.app.start_native();
    pump_until(
        &mut harness.app,
        &events,
        |app| app.native.as_ref().is_some_and(|native| native.online),
        "reconnect online",
    );
    let replay_projection = harness.draw_projection();
    assert!(
        replay_projection
            .lines()
            .any(|line| squeeze(line).contains("末段：失败原因须可见")),
        "replay must keep the submitted paragraphs: {replay_projection}"
    );
    assert!(
        replay_projection.lines().any(|line| {
            squeeze(line).contains("error:") && squeeze(line).contains("intentionalfailure")
        }),
        "replay must keep the failure reason: {replay_projection}"
    );
}
