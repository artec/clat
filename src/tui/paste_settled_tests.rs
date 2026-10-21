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
///
/// fixture 行为开关（AG-DOGFOOD-8）：`snapshot_recent_inputs=false` 模拟
/// 不携带 session.recent_inputs 的旧宿主快照，用于单独判别“回执入史”
///（remember）与“快照重建”（replace_history）两条腿。
#[derive(Clone)]
struct FixtureOptions {
    snapshot_recent_inputs: bool,
}

struct JointHostFixture {
    port: u16,
    root: std::path::PathBuf,
    submitted: Arc<Mutex<Vec<String>>>,
    /// 当前会话 id（B8-R2-1 同类判别：测试可切到 B 会话，生产中由其它客
    // 户端切换会话后随快照到达）。
    session: Arc<Mutex<String>>,
    connections: Arc<AtomicU64>,
    stop: Arc<AtomicBool>,
    options: Arc<FixtureOptions>,
    slot: SseSlot,
}

impl JointHostFixture {
    fn spawn() -> Self {
        Self::spawn_with(true, Vec::new())
    }

    /// `snapshot_recent_inputs=false`：workbench.info 不携带
    /// session.recent_inputs（旧宿主形状）；`seed`：预置“其它客户端”
    /// 已提交输入（仅快照可达，不经本 TUI 提交路径）。
    fn spawn_with(snapshot_recent_inputs: bool, seed: Vec<String>) -> Self {
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
            submitted: Arc::new(Mutex::new(seed)),
            session: Arc::new(Mutex::new("ag8-hist-session".to_owned())),
            connections: Arc::new(AtomicU64::new(0)),
            stop: Arc::new(AtomicBool::new(false)),
            options: Arc::new(FixtureOptions {
                snapshot_recent_inputs,
            }),
            slot: SseSlot::default(),
        };

        let submitted = Arc::clone(&fixture.submitted);
        let session = Arc::clone(&fixture.session);
        let connections = Arc::clone(&fixture.connections);
        let stop = Arc::clone(&fixture.stop);
        let slot = fixture.slot.clone();
        let options = Arc::clone(&fixture.options);
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
                        Arc::clone(&session),
                        Arc::clone(&connections),
                        Arc::clone(&stop),
                        slot.clone(),
                        root.clone(),
                        options.clone(),
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
    Arc<Mutex<String>>,
    Arc<AtomicU64>,
    Arc<AtomicBool>,
    SseSlot,
    std::path::PathBuf,
    Arc<FixtureOptions>,
);

fn handle_connection(mut stream: std::net::TcpStream, state: ConnState) {
    let (submitted, session, connections, stop, slot, root, options) = state;
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
        if text.starts_with("AG8-FORCE-FAIL") {
            // AG-DOGFOOD-8 / T2：未入账的失败提交（宿主拒绝，无 committed
            // 回执）——不入 submitted（近似 journal），供历史不污染负控。
            (
                "HTTP/1.1 200 OK",
                serde_json::json!({
                    "ok": false,
                    "error": {"code": "internal", "message": "forced failure"}
                })
                .to_string(),
            )
        } else {
            let prompt_rpc_id = {
                let mut submitted = submitted.lock().expect("submitted");
                submitted.push(text.clone());
                format!("ag4-rpc-{}", submitted.len())
            };
            // 与生产同序：先入队 run 事件 + 终态结算帧，再回 RPC。
            slot.publish(run_event_frame(serde_json::json!({
                "type": "run_started",
                "project": "/tmp/ag4-joint-proj",
                "prompt": text,
            })));
            slot.publish(control_frame(
                "prompt.settled",
                serde_json::json!({
                    "prompt_rpc_id": prompt_rpc_id,
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
        }
    } else if request_line.contains("/api/workbench.info") {
        // AG-DOGFOOD-8 / HIST-TUI-1：镜像真实 serve 契约——session.recent_inputs
        // 是当前 Session 已入账 user 文本（fixture 以捕获的 prompt.send 提交
        // + 预置种子近似 journal 投影；slash 命令与 steering 不进此列表）。
        // `snapshot_recent_inputs=false` 模拟旧宿主（无此字段）。
        let recent = submitted.lock().expect("submitted").clone();
        let session_id = session.lock().expect("session").clone();
        let session = if options.snapshot_recent_inputs {
            serde_json::json!({"id": session_id, "recent_inputs": recent})
        } else {
            serde_json::json!({"id": session_id})
        };
        (
            "HTTP/1.1 200 OK",
            serde_json::json!({"ok": true, "value": {"session": session}}).to_string(),
        )
    } else {
        // 其它 RPC：最小合法快照。
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

// ---------------------------------------------------------------------------
// AG-DOGFOOD-8 / HIST-TUI-1：默认 Host 模式输入历史召回（T1/T2/T3）。
//
// 权威边界：会话 journal（fixture 以捕获的 prompt.send 提交近似）是已提交
// 用户输入的唯一事实源；成功接纳的非 slash 消息进入历史，失败/未入账提交
// 与 steering 不污染；快照 session.recent_inputs 重建浏览列表；↑↓ 有序浏
// 览且越过最新项准确还原未发送草稿（UTF-8/CJK/换行）。
// ---------------------------------------------------------------------------

/// 提交回执（HTTP reply）在 fixture 里晚于 SSE 帧——泵到 run 落定后再固定
/// 吸收一段时间，确保 Submitted 回执与随后的 workbench.info 快照都被处理。
fn drain_for(app: &mut super::App, events: &mpsc::Receiver<super::UiEvent>, millis: u64) {
    let deadline = Instant::now() + Duration::from_millis(millis);
    while Instant::now() < deadline {
        if let Ok(event) = events.recv_timeout(Duration::from_millis(20)) {
            app.handle_ui_event(event);
        }
    }
}

fn ag8_attached_harness(tag: &str) -> (Harness, JointHostFixture, mpsc::Receiver<super::UiEvent>) {
    ag8_attach_fixture(tag, JointHostFixture::spawn())
}

fn ag8_attach_fixture(
    tag: &str,
    fixture: JointHostFixture,
) -> (Harness, JointHostFixture, mpsc::Receiver<super::UiEvent>) {
    let client = HostClient::connect(fixture.port, "ag8-hist-token".into(), &fixture.root)
        .expect("real host handshake");
    let mut harness = Harness {
        app: super::App::open_native(
            crate::Project::new(std::path::PathBuf::from(format!("/tmp/{tag}-proj"))),
            client,
        )
        .expect("attached TUI shell"),
        terminal: ratatui::Terminal::new(ratatui::backend::TestBackend::new(80, 24))
            .expect("test terminal"),
        project_root: std::path::PathBuf::from(format!("/tmp/{tag}-proj")),
        storage_root: fixture.root.clone(),
    };
    let (sender, events) = super::ui_event_channel();
    harness.app.event_sender = Some(sender);
    harness.app.start_native();
    pump_until(
        &mut harness.app,
        &events,
        |app| app.native.as_ref().is_some_and(|native| native.online),
        "ag8 subscribe online",
    );
    // 首个快照（含/不含 recent_inputs）落地后再交给测试体。
    drain_for(&mut harness.app, &events, 200);
    (harness, fixture, events)
}

/// T1：真实提交路径（Enter → prompt.send → 回执）后，↑/↓ 有序召回，越过
/// 最新项 ↓ 精确还原未发送草稿（含 CJK 与换行）。
#[test]
fn ag8_native_submit_history_recall_roundtrip() {
    let (mut harness, fixture, events) = ag8_attached_harness("ag8-hist-recall");

    harness.app.input.insert_str("alpha committed line");
    harness.key(KeyCode::Enter);
    pump_until(
        &mut harness.app,
        &events,
        |app| !app.running && !app.conversation.is_empty(),
        "ag8 first run settles",
    );
    drain_for(&mut harness.app, &events, 400);
    assert_eq!(
        fixture.submitted_texts(),
        vec!["alpha committed line".to_owned()]
    );
    assert!(
        !harness.app.input.history_is_empty(),
        "an accepted host submission must become recallable input history"
    );

    harness.app.input.insert_str("beta second line");
    harness.key(KeyCode::Enter);
    pump_until(
        &mut harness.app,
        &events,
        |app| !app.running && fixture.submitted_texts().len() == 2,
        "ag8 second run settles",
    );
    drain_for(&mut harness.app, &events, 400);
    assert!(harness.app.conversation.has_settled_card("ag4-rpc-1"));
    assert!(
        harness.app.conversation.has_settled_card("ag4-rpc-2"),
        "distinct accepted prompts must keep distinct settlement identities"
    );

    // ↑→最新、再↑→更旧、↓→回新、越过最新↓→原草稿（此处为空）。
    harness.key(KeyCode::Up);
    assert_eq!(harness.app.input.text(), "beta second line");
    harness.key(KeyCode::Up);
    assert_eq!(harness.app.input.text(), "alpha committed line");
    harness.key(KeyCode::Down);
    assert_eq!(harness.app.input.text(), "beta second line");
    harness.key(KeyCode::Down);
    assert_eq!(
        harness.app.input.text(),
        "",
        "past-newest Down restores the empty draft"
    );

    // 有内容的未发送草稿（CJK + 换行）在浏览后必须逐字还原。
    let draft = "未发送草稿\n第二行：浏览后必须还原";
    harness.app.input.insert_str(draft);
    harness.key(KeyCode::Up);
    assert_eq!(harness.app.input.text(), "beta second line");
    harness.key(KeyCode::Down);
    assert_eq!(
        harness.app.input.text(),
        draft,
        "the untouched draft must survive browsing"
    );
    drop(harness);
}

/// T3：快照（session.recent_inputs）从权威投影重建输入历史——含其它客户
/// 端的提交（种子），重连后与本地提交按序召回。
#[test]
fn ag8_native_snapshot_restores_input_history_after_reconnect() {
    // 种子 = “其它客户端”已提交输入：只能经快照进入本 TUI（判别快照腿）。
    let (mut harness, _fixture, events) = ag8_attach_fixture(
        "ag8-hist-restore",
        JointHostFixture::spawn_with(true, vec!["seeded by another client".into()]),
    );
    assert!(
        !harness.app.input.history_is_empty(),
        "the attach snapshot must restore another client's committed inputs"
    );
    harness.key(KeyCode::Up);
    assert_eq!(harness.app.input.text(), "seeded by another client");
    harness.app.input.clear();

    harness.app.input.insert_str("restored by snapshot");
    harness.key(KeyCode::Enter);
    pump_until(
        &mut harness.app,
        &events,
        |app| !app.running && !app.conversation.is_empty(),
        "ag8 restore run settles",
    );
    drain_for(&mut harness.app, &events, 400);

    // 重连（生产 /reconnect 路径）：replay + workbench.info 重建转录与历史。
    harness.app.start_native();
    pump_until(
        &mut harness.app,
        &events,
        |app| app.native.as_ref().is_some_and(|native| native.online),
        "ag8 reconnect online",
    );
    drain_for(&mut harness.app, &events, 300);
    harness.app.input.clear();
    harness.key(KeyCode::Up);
    assert_eq!(harness.app.input.text(), "restored by snapshot");
    harness.key(KeyCode::Up);
    assert_eq!(harness.app.input.text(), "seeded by another client");
    drop(harness);
}

/// T2/A1 判别腿：旧宿主快照（无 recent_inputs 字段）下，回执入史是唯一
/// 来源——接纳后 ↑ 必须立即可用，不等第二次快照。
#[test]
fn ag8_native_remember_covers_legacy_snapshot_gap() {
    let (mut harness, fixture, events) = ag8_attach_fixture(
        "ag8-hist-legacy",
        JointHostFixture::spawn_with(false, Vec::new()),
    );
    assert!(harness.app.input.history_is_empty());

    harness.app.input.insert_str("legacy gap probe");
    harness.key(KeyCode::Enter);
    pump_until(
        &mut harness.app,
        &events,
        |app| !app.running && !app.conversation.is_empty(),
        "ag8 legacy run settles",
    );
    drain_for(&mut harness.app, &events, 400);
    assert_eq!(
        fixture.submitted_texts(),
        vec!["legacy gap probe".to_owned()]
    );
    assert!(
        !harness.app.input.history_is_empty(),
        "the admission receipt alone must make the input recallable"
    );
    harness.key(KeyCode::Up);
    assert_eq!(harness.app.input.text(), "legacy gap probe");
    drop(harness);
}

/// T2：失败提交（未入账）不入历史、草稿保留；slash 命令与 running 态
/// steering 不进入已提交提示词历史。
#[test]
fn ag8_native_submission_history_guards() {
    let (mut harness, fixture, events) = ag8_attached_harness("ag8-hist-guards");

    // 未入账的失败提交：草稿保留，历史不污染。
    harness.app.input.insert_str("AG8-FORCE-FAIL rejected text");
    harness.key(KeyCode::Enter);
    pump_until(
        &mut harness.app,
        &events,
        |app| !app.input.text().is_empty(),
        "ag8 failed receipt restores the draft",
    );
    drain_for(&mut harness.app, &events, 300);
    assert!(
        harness
            .app
            .input
            .text()
            .contains("AG8-FORCE-FAIL rejected text"),
        "the failed submission draft must be retained: {:?}",
        harness.app.input.text()
    );
    assert!(
        harness.app.input.history_is_empty(),
        "a rejected submission must never enter input history"
    );
    assert!(fixture.submitted_texts().is_empty());
    harness.app.input.clear();

    // slash 命令（command.run）不属于已提交提示词历史。
    harness.app.input.insert_str("/help");
    harness.key(KeyCode::Enter);
    drain_for(&mut harness.app, &events, 400);
    assert!(
        harness.app.input.history_is_empty(),
        "slash commands must not become prompt history"
    );

    // running 态提交走 steer.send：接纳 ≠ user journal 事件，不入历史。
    harness.app.running = true;
    harness.app.input.insert_str("queued steering probe");
    harness.key(KeyCode::Enter);
    drain_for(&mut harness.app, &events, 400);
    assert!(
        harness.app.input.history_is_empty(),
        "steering must join history only via the durable claim, not the queue receipt"
    );
    drop(harness);
}

/// B8-R2（重复快照）：同 Session 的**完全相同**快照不得打断浏览——游标
/// 与原未发送草稿（CJK/换行）保持可用，生产入口 refresh_native → HTTP
/// fixture → Snapshot → native_snapshot 全程真实。浏览到更旧项后再接收
/// 相同快照，↓ 必须仍按序走回更新项（判别值锚迁移，而非游标复位）。
#[test]
fn ag8_native_duplicate_snapshot_preserves_browsing_draft() {
    let (mut harness, _fixture, events) = ag8_attach_fixture(
        "ag8-hist-dup-snapshot",
        JointHostFixture::spawn_with(
            true,
            vec!["committed alpha".into(), "committed beta".into()],
        ),
    );
    let draft = "未发送草稿\n第二行：重复快照后必须还原";
    harness.app.input.insert_str(draft);
    harness.key(KeyCode::Up);
    assert_eq!(harness.app.input.text(), "committed beta");
    harness.key(KeyCode::Up);
    assert_eq!(harness.app.input.text(), "committed alpha");

    // 生产快照路径回流（fixture 返回完全相同的列表）。
    harness.app.refresh_native();
    drain_for(&mut harness.app, &events, 300);

    // 游标未失效：仍显示同一召回项，↓ 按序回到更新项而非直归草稿。
    assert_eq!(harness.app.input.text(), "committed alpha");
    harness.key(KeyCode::Down);
    assert_eq!(
        harness.app.input.text(),
        "committed beta",
        "an identical snapshot must keep the browsing cursor in place"
    );
    // 越过最新项 ↓ 仍精确还原原草稿。
    harness.key(KeyCode::Down);
    assert_eq!(
        harness.app.input.text(),
        draft,
        "an identical snapshot must not void the browsing draft"
    );
    // 浏览继续可用：↑ 回到最新项。
    harness.key(KeyCode::Up);
    assert_eq!(harness.app.input.text(), "committed beta");
    drop(harness);
}

/// B8-R2（真变化迁移）：浏览中列表追加（其它客户端提交）时，游标按当前
/// 召回值锚点迁移，新条目可经 ↓ 到达，越过最新项仍还原原草稿。
#[test]
fn ag8_native_history_growth_migrates_browse_and_keeps_draft() {
    let (mut harness, fixture, events) = ag8_attach_fixture(
        "ag8-hist-growth",
        JointHostFixture::spawn_with(true, vec!["committed alpha".into()]),
    );
    let draft = "原草稿：追加后仍可还原";
    harness.app.input.insert_str(draft);
    harness.key(KeyCode::Up);
    assert_eq!(harness.app.input.text(), "committed alpha");

    // 其它客户端追加一条已提交输入（fixture 的 journal 近似），随后生产
    // 快照路径回流新列表。
    fixture
        .submitted
        .lock()
        .expect("submitted")
        .push("committed by another client".into());
    harness.app.refresh_native();
    drain_for(&mut harness.app, &events, 300);

    // 锚点迁移：仍显示同一召回项，不跳跃、不重复。
    assert_eq!(
        harness.app.input.text(),
        "committed alpha",
        "the browsing cursor must migrate by value anchor"
    );
    harness.key(KeyCode::Down);
    assert_eq!(harness.app.input.text(), "committed by another client");
    harness.key(KeyCode::Down);
    assert_eq!(
        harness.app.input.text(),
        draft,
        "the pre-browse draft must stay recoverable after list growth"
    );
    drop(harness);
}

/// B8-R2（窗口滑动）：浏览中的锚点滑出有界窗口（压缩/滑窗）时游标复
/// 位，但**待还原原草稿仍可经 ↓ 取回**——不能因列表变化丢失用户未发送
/// 的输入。
#[test]
fn ag8_native_window_slide_keeps_draft_recoverable() {
    let (mut harness, fixture, events) = ag8_attach_fixture(
        "ag8-hist-window-slide",
        JointHostFixture::spawn_with(true, vec!["committed alpha".into()]),
    );
    let draft = "原草稿：锚点滑出窗口后仍可还原";
    harness.app.input.insert_str(draft);
    harness.key(KeyCode::Up);
    assert_eq!(harness.app.input.text(), "committed alpha");

    // 权威列表滑窗：锚点条目离开窗口，仅剩新条目。
    *fixture.submitted.lock().expect("submitted") = vec!["newer window entry".into()];
    harness.app.refresh_native();
    drain_for(&mut harness.app, &events, 300);

    // 游标复位后 ↓ 仍能精确取回原草稿（暂存未被丢弃）。
    harness.key(KeyCode::Down);
    assert_eq!(
        harness.app.input.text(),
        draft,
        "a slid anchor must not destroy the recoverable draft"
    );
    drop(harness);
}

/// B8-R2-3（消费者级）：浏览中权威窗口变**空**（有界窗口滑空/压缩）时，
// 真实 ↓ 按键（keys.rs 路由）必须仍能取回待还原草稿——空历史不得把 ↓
// 改道成会话滚动。
#[test]
fn b8_r2_empty_window_restores_pending_draft_via_real_down() {
    let (mut harness, fixture, events) = ag8_attach_fixture(
        "ag8-hist-empty-window",
        JointHostFixture::spawn_with(true, vec!["committed anchor".into()]),
    );
    let draft = "原草稿：空窗口仍应可还原";
    harness.app.input.insert_str(draft);
    harness.key(KeyCode::Up);
    assert_eq!(harness.app.input.text(), "committed anchor");

    // 权威窗口变空（合法空窗口，非伪造宿主状态），经生产快照入口回流。
    fixture.submitted.lock().expect("submitted").clear();
    harness.app.refresh_native();
    drain_for(&mut harness.app, &events, 300);
    assert!(
        harness.app.input.history_is_empty(),
        "the consumer must have received the empty window"
    );

    // 真实 ↓ 路由：取回草稿而非滚动。
    harness.key(KeyCode::Down);
    assert_eq!(
        harness.app.input.text(),
        draft,
        "a real Down key must restore the pending draft even with an empty window"
    );
    drop(harness);
}

/// B8-R2-3（分支护栏）：无历史且无待恢复暂存时，↓ 保持既有语义（不产生
/// 幻影还原，输入保持为空）。
#[test]
fn ag8_native_down_without_history_or_stash_keeps_scroll_semantics() {
    let (mut harness, _fixture, events) = ag8_attach_fixture(
        "ag8-hist-no-stash",
        JointHostFixture::spawn_with(true, Vec::new()),
    );
    drain_for(&mut harness.app, &events, 200);
    assert!(harness.app.input.history_is_empty());
    assert!(!harness.app.input.has_pending_history_draft());
    harness.app.input.insert_str("");
    harness.key(KeyCode::Down);
    assert_eq!(
        harness.app.input.text(),
        "",
        "Down with no history and no stash must never materialize a draft"
    );
    drop(harness);
}

/// B8-R2-1 同类（TUI 腿）：其它客户端切到新会话后，旧会话的浏览游标与
/// 待还原暂存随快照整体退役——新会话 ↓ 不得恢复 A 的未发送草稿，↑ 只
/// 能召回新会话自己的提交。
#[test]
fn ag8_native_session_switch_retires_old_session_browse() {
    let (mut harness, fixture, events) = ag8_attach_fixture(
        "ag8-hist-session-retire",
        JointHostFixture::spawn_with(true, vec!["committed anchor".into()]),
    );
    let draft_a = "A 未发送草稿：不得进入 B 会话";
    harness.app.input.insert_str(draft_a);
    harness.key(KeyCode::Up);
    assert_eq!(harness.app.input.text(), "committed anchor");

    // 其它客户端切换会话（新会话已有自己的提交），随后生产快照回流。
    *fixture.session.lock().expect("session") = "ag8-hist-session-b".to_owned();
    *fixture.submitted.lock().expect("submitted") = vec!["committed in B".to_owned()];
    harness.app.refresh_native();
    drain_for(&mut harness.app, &events, 300);

    // B 会话内：↓ 不得恢复 A 的待还原草稿（暂存已退役，正文保持切换前
    // 的召回项，而不是变成 A 的草稿）。
    harness.key(KeyCode::Down);
    assert_eq!(
        harness.app.input.text(),
        "committed anchor",
        "the old session's pending draft must not be restorable in B"
    );
    harness.app.input.clear();
    // ↑ 只召回 B 的提交，越过最新项还原空。
    harness.key(KeyCode::Up);
    assert_eq!(harness.app.input.text(), "committed in B");
    harness.key(KeyCode::Up);
    assert_eq!(
        harness.app.input.text(),
        "committed in B",
        "no older cross-session entry may appear"
    );
    harness.key(KeyCode::Down);
    assert_eq!(harness.app.input.text(), "");
    drop(harness);
}

// ---------------------------------------------------------------------------
// BELL-HOST-1（AG-DOGFOOD-10，2026-10-19）：attached 腿 run 终态提醒铃。
//
// 缺陷（用户实战 + 静态核准）：standalone（run_events.rs `finish_run`）
// 与 dsh（dsh_events.rs `turn/end`）的 run 结束都会响铃，attached 腿的
// 宿主结算 `prompt.settled` 分支不调 `notify`——后台 Host 上 Run 结束
// 后，失焦终端的用户永远等不到提醒（本地与 DSH 均有通知，唯独缺这腿）。
//
// 判别面约定（与 CF1 返工同款）：全程真实事件驱动（在线泵同一入口
// `NativeEvent::Frame`，含 RunStarted），不手写被测标志；铃声出口用
// 受控无声 sink——`BellMode::Command` 写 marker 文件，与生产
// Sound/Command 完全同一 `ring_bell` 调用链，测试不实际发声、不依赖
// 系统音量。
// ---------------------------------------------------------------------------

use crate::event::RunEvent;
use crate::host::HostEvent;
use crate::message::MessageContent;
use crate::tui::native::NativeEvent;
use crate::tui::worker::UiEvent;

/// 受控无声铃探针：命令只向 marker 文件追加一次 `bell` 标记。
struct BellProbe {
    marker: std::path::PathBuf,
}

impl BellProbe {
    fn new(tag: &str) -> Self {
        let marker = std::env::temp_dir().join(format!(
            "clat-bellhost1-{tag}-{}-{}.marker",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = std::fs::remove_file(&marker);
        Self { marker }
    }

    /// 挂到被测 App：与生产 `CLAT_BELL_COMMAND` 完全同一条
    /// `notify → should_ring → ring_bell` 链，只是命令换成写 marker。
    fn arm(&self, harness: &mut Harness) {
        harness.app.bell = super::BellMode::Command(bell_probe_command(&self.marker));
    }

    fn rings(&self) -> usize {
        std::fs::read_to_string(&self.marker)
            .map(|text| text.matches("bell").count())
            .unwrap_or(0)
    }

    /// 响铃是 detached spawn：有界等待 marker 达到期望次数。
    fn wait_for_rings(&self, expected: usize) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            let rings = self.rings();
            assert!(
                rings <= expected,
                "the bell rang {rings} times, at most {expected} allowed"
            );
            if rings == expected {
                return;
            }
            if std::time::Instant::now() > deadline {
                panic!(
                    "expected {expected} bell ring(s), saw {rings} within 5s (marker: {})",
                    self.marker.display()
                );
            }
            std::thread::sleep(std::time::Duration::from_millis(25));
        }
    }

    /// 负向：有界静默窗口后仍无 marker（detached spawn 已有足够时隙）。
    fn assert_silent(&self) {
        std::thread::sleep(std::time::Duration::from_millis(400));
        assert_eq!(
            self.rings(),
            0,
            "the bell must stay silent (marker: {})",
            self.marker.display()
        );
    }
}

/// 真实 SSE 事件帧（与在线泵线程同一入口）驱动 run 事件。
fn bell_run_frame(harness: &mut Harness, event: RunEvent) {
    let epoch = harness.app.native.as_ref().expect("native state").epoch;
    harness
        .app
        .handle_ui_event(UiEvent::Native(NativeEvent::Frame(
            epoch,
            HostEvent::Run(event),
        )));
}

fn bell_run_started(round: &str) -> RunEvent {
    RunEvent::RunStarted {
        project: std::path::PathBuf::from("/tmp/probe"),
        message: MessageContent::text(round),
        client_message_id: None,
    }
}

/// 主链：attached run completed 结算必须响一声（pre-fix 红：该分支
/// 无 notify，AFK 用户等不到）。RunStarted 先于 settled 到达，
/// running 归属成立。
#[test]
fn bell_host1_attached_run_completed_rings_once() {
    let mut harness = harness_native("bellhost1-completed", true);
    let probe = BellProbe::new("completed");
    probe.arm(&mut harness);
    bell_run_frame(&mut harness, bell_run_started("bell round one"));
    assert!(harness.app.running, "RunStarted drives running=true");
    settled(
        &mut harness,
        json!({"prompt_rpc_id": "rpc-bell-1", "outcome": {"type": "completed", "turns": 1}}),
    );
    probe.wait_for_rings(1);
    assert!(!harness.app.running, "prompt.settled ends the run");
}

/// 失败同样要响（AFK 下更需要知道），且与 standalone 失败语义同源。
#[test]
fn bell_host1_attached_run_failed_rings() {
    let mut harness = harness_native("bellhost1-failed", true);
    let probe = BellProbe::new("failed");
    probe.arm(&mut harness);
    bell_run_frame(&mut harness, bell_run_started("bell round fail"));
    settled(
        &mut harness,
        json!({"prompt_rpc_id": "rpc-bell-2", "outcome": {"type": "failed", "error": "model 500"}}),
    );
    probe.wait_for_rings(1);
}

/// 主动取消不响：与 standalone（cancelled 不 notify）同语义。
#[test]
fn bell_host1_attached_run_cancelled_stays_silent() {
    let mut harness = harness_native("bellhost1-cancelled", true);
    let probe = BellProbe::new("cancelled");
    probe.arm(&mut harness);
    bell_run_frame(&mut harness, bell_run_started("bell round cancel"));
    settled(
        &mut harness,
        json!({"prompt_rpc_id": "rpc-bell-3", "outcome": {"type": "cancelled", "turns": 1}}),
    );
    probe.assert_silent();
}

/// 同 RPC 二次 settled（迟到重放/重复帧）只响一次：F3 去重门在铃之前。
#[test]
fn bell_host1_duplicate_settled_frame_does_not_ring_twice() {
    let mut harness = harness_native("bellhost1-duplicate", true);
    let probe = BellProbe::new("duplicate");
    probe.arm(&mut harness);
    bell_run_frame(&mut harness, bell_run_started("bell round dup"));
    let payload = json!({"prompt_rpc_id": "rpc-bell-4", "outcome": {"type": "completed"}});
    settled(&mut harness, payload.clone());
    probe.wait_for_rings(1);
    settled(&mut harness, payload);
    std::thread::sleep(std::time::Duration::from_millis(400));
    assert_eq!(
        probe.rings(),
        1,
        "a duplicate settled frame must not ring again"
    );
}

/// 焦点三态（B-1）在本腿同判：FocusGained（屏前看得见）静音；
/// FocusLost 后恢复响。判别：删掉 focused 记录或 should_ring 门即红。
#[test]
fn bell_host1_focus_tri_state_gates_the_settled_ring() {
    let mut harness = harness_native("bellhost1-focus", true);
    let probe = BellProbe::new("focus");
    probe.arm(&mut harness);
    harness.event(super::UiEvent::Terminal(Event::FocusGained));
    assert_eq!(harness.app.focused, Some(true));
    bell_run_frame(&mut harness, bell_run_started("focused round"));
    settled(
        &mut harness,
        json!({"prompt_rpc_id": "rpc-bell-5", "outcome": {"type": "completed"}}),
    );
    probe.assert_silent();

    harness.event(super::UiEvent::Terminal(Event::FocusLost));
    assert_eq!(harness.app.focused, Some(false));
    bell_run_frame(&mut harness, bell_run_started("unfocused round"));
    settled(
        &mut harness,
        json!({"prompt_rpc_id": "rpc-bell-6", "outcome": {"type": "completed"}}),
    );
    probe.wait_for_rings(1);
}

/// CLAT_NO_BELL 语义不退化：Off 模式下结算静音。B-R1 F1（返工）：负控
/// 必须观察**被禁止的出口**——同一 marker 命令作为 CLAT_BELL_COMMAND
/// 同时在场，经与真实环境同一条 `bell_mode_from_env` 裁决链断言
/// NO_BELL 优先级把它压成 Off 且 marker 不响。单点退化（优先级翻转
/// 让 Command 赢）本测试立刻红：mode 断言与 marker 双保险。
#[test]
fn bell_host1_no_bell_mode_suppresses_the_armed_command_outlet() {
    let mut harness = harness_native("bellhost1-nobell", true);
    let probe = BellProbe::new("nobell-armed");
    let mode = super::bell_mode_from_env(
        Some("1".to_owned()),
        Some(format!("printf bell >> {}", probe.marker.display())),
    );
    assert_eq!(
        mode,
        super::BellMode::Off,
        "CLAT_NO_BELL must win over a configured CLAT_BELL_COMMAND"
    );
    harness.app.bell = mode;
    bell_run_frame(&mut harness, bell_run_started("silent round"));
    settled(
        &mut harness,
        json!({"prompt_rpc_id": "rpc-bell-7", "outcome": {"type": "completed"}}),
    );
    probe.assert_silent();
}

/// 归属边界：本 TUI 未观察到 running 的结算（切会话 replay.end 后的
/// 旧会话迟到结算 / 未跟跑的孤立 settled 帧）不得乱响。
#[test]
fn bell_host1_settlement_without_an_observed_run_stays_silent() {
    let mut harness = harness_native("bellhost1-unattributed", true);
    let probe = BellProbe::new("unattributed");
    probe.arm(&mut harness);
    assert!(!harness.app.running, "no run was observed by this client");
    settled(
        &mut harness,
        json!({"prompt_rpc_id": "rpc-bell-8", "outcome": {"type": "completed"}}),
    );
    probe.assert_silent();
}

/// Host 审批提醒（既有路径）独立正常发出且不被本修复抑制：
/// approval.requested 一声，随后同一 run 窗口 completed 结算再一声，
/// 总共恰两声——不多不少。
#[test]
fn bell_host1_approval_ring_survives_and_settled_adds_exactly_one_more() {
    let mut harness = harness_native("bellhost1-approval", true);
    let probe = BellProbe::new("approval");
    probe.arm(&mut harness);
    // native_approval 的后台应答线程需要真实事件回线（与 CF7 同款注入）。
    let (sender, _events) = super::ui_event_channel();
    harness.app.event_sender = Some(sender);
    let epoch = harness.app.native.as_ref().expect("native state").epoch;
    harness
        .app
        .handle_ui_event(UiEvent::Native(NativeEvent::Frame(
            epoch,
            HostEvent::Control {
                kind: "approval.requested".into(),
                payload: json!({
                    "rpc_id": "appr-bell-1",
                    "request": {
                        "tool": "write_file",
                        "reason": "bell probe",
                        "call_id": "call-bell-1",
                        "effect": "write",
                        "arguments": {}
                    }
                }),
            },
        )));
    probe.wait_for_rings(1);
    // 真实 run 随后完成：settled 只再补一声（不重复审批铃、不吞结算铃）。
    bell_run_frame(&mut harness, bell_run_started("after approval"));
    settled(
        &mut harness,
        json!({"prompt_rpc_id": "rpc-bell-9", "outcome": {"type": "completed"}}),
    );
    probe.wait_for_rings(2);
    // 关闭权限框（拒绝），让后台应答线程收尾。
    harness.key(KeyCode::Esc);
    drop(harness);
}

fn bell_probe_command(marker: &std::path::Path) -> String {
    // Git sh accepts C:/ paths; quoting preserves spaces and shell metacharacters.
    let path = marker.to_string_lossy();
    #[cfg(windows)]
    let path = path.replace('\\', "/");
    format!("printf bell >> '{}'", path.replace('\'', "'\"'\"'"))
}

#[test]
fn bell_host1_probe_preserves_shell_sensitive_marker_path() {
    let mut probe = BellProbe::new("path-quoting");
    let directory = probe
        .marker
        .with_extension("space ' dollar $ backtick ` directory");
    std::fs::create_dir_all(&directory).unwrap();
    probe.marker = directory.join("bell ' $ ` marker");
    super::bell::ring_bell(&super::BellMode::Command(bell_probe_command(&probe.marker)));
    probe.wait_for_rings(1);
    std::fs::remove_dir_all(directory).unwrap();
}
