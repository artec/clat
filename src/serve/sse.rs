//! SSE 连接生命周期（§7.2 六步）：鉴权在连接层完成（serve.rs 三闸），
//! 这里从「注册订阅」开始：
//!
//! ```text
//! 1.（三闸，serve.rs）
//! 2. 注册活流订阅并取得此刻 run 缓冲的独立快照
//! 3. replay.begin → session.history 尾页 → replay.end（journal 域）
//! 4. subscribed { last_seq }（committed_seq 水位，竞态自检用）
//! 5. 直写注册时的 run 前缀快照（事件域）
//! 6. 泵活流队列 → 实时（recv_timeout 15s → 心跳 comment）
//! ```
//!
//! 步 2 与步 5/6 的衔接靠独立快照：重发段与队列在同一锁内取值，
//! 无重叠无丢失（INV-S4 判别锚）。重连 = 重建：断开后客户端重新
//! GET /api/events，服务端重走全流程——无续传游标（§7.2）。

use super::protocol;
use super::state::ServeShared;
use super::state::SseFrame;
use std::io::Write;
use std::net::TcpStream;
use std::sync::Arc;
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::time::{Duration, Instant};

/// 重放 + 缓冲前缀阶段的总预算（PWA2-01：订阅生命周期必须
/// 「达成 SUBSCRIBED 或显式失败」，不允许无限停留在重放段）。
const REPLAY_PHASE_BUDGET: Duration = Duration::from_secs(30);

pub(crate) fn handle(stream: &mut TcpStream, shared: &Arc<ServeShared>) {
    let _ = stream.set_write_timeout(Some(Duration::from_secs(10)));
    let _ = stream.set_read_timeout(Some(Duration::from_secs(1)));
    if super::http::write_sse_head(stream).is_err() {
        return;
    }
    let deadline = Instant::now() + REPLAY_PHASE_BUDGET;
    // Admission/selection cannot cross registration and history. Registration
    // releases inner before any Application method is called.
    let app = shared.app.lock().expect("application lock");
    let (subscriber_id, queue, prefix) = shared.register_replay_subscriber();
    let before_seq = prefix.as_ref().and_then(|prefix| prefix.before_seq);
    let snapshot = replay_snapshot(&app, before_seq, shared.selection_generation());
    drop(app);
    let mut connection = SseConnection {
        stream,
        seq: 0,
        shared,
        subscriber_id,
    };
    match snapshot {
        Ok(snapshot) => {
            let frames = prefix.map(|prefix| prefix.frames).unwrap_or_default();
            if write_reconstruction(&mut connection, snapshot, frames, deadline).is_ok() {
                pump(&mut connection, &queue);
            }
        }
        Err(error) => {
            let ctl = serde_json::json!({
                "kind": "internal", "payload": {"error": error.to_string()},
            });
            let _ = connection.write_frame("notice", &super::shapes::ctl_data(&ctl));
        }
    }
    connection.cleanup();
}

struct ReplaySnapshot {
    events: Vec<crate::session::replay::ReplayEvent>,
    has_more: bool,
    subscribed: serde_json::Value,
}

fn replay_snapshot(
    app: &crate::TrustedProjectApplication,
    before_seq: Option<u64>,
    selection_generation: u64,
) -> Result<ReplaySnapshot, crate::ApplicationError> {
    let page = app.session_history(before_seq, 50)?;
    let outline = app.session_message_outline()?;
    Ok(ReplaySnapshot {
        events: page.events,
        has_more: page.has_more,
        subscribed: serde_json::json!({
            "last_seq": app.committed_seq(),
            "session_id": app.current_session_id().map(|id| id.as_str().to_owned()),
            "message_outline": outline.into_iter().map(|item| serde_json::json!({
                "seq": item.seq, "turn": item.turn, "role": item.role, "preview": item.preview,
            })).collect::<Vec<_>>(),
            "replaying": false,
            "selection_generation": selection_generation,
        }),
    })
}

fn write_reconstruction(
    connection: &mut SseConnection<'_>,
    snapshot: ReplaySnapshot,
    prefix: Vec<String>,
    deadline: Instant,
) -> std::io::Result<()> {
    write_before_deadline(
        connection,
        "replay.begin",
        &serde_json::json!({"has_more": snapshot.has_more}).to_string(),
        deadline,
    )?;
    for event in snapshot.events {
        write_before_deadline(
            connection,
            "replay",
            &super::shapes::replay_data(&event),
            deadline,
        )?;
    }
    write_before_deadline(connection, "replay.end", "{}", deadline)?;
    write_before_deadline(
        connection,
        "subscribed",
        &super::shapes::ctl_data(&snapshot.subscribed),
        deadline,
    )?;
    for data in prefix {
        write_before_deadline(connection, "event", &data, deadline)?;
    }
    Ok(())
}

fn write_before_deadline(
    connection: &mut SseConnection<'_>,
    event: &str,
    data: &str,
    deadline: Instant,
) -> std::io::Result<()> {
    if Instant::now() >= deadline {
        connection.fail_phase_budget();
        return Err(std::io::Error::new(
            std::io::ErrorKind::TimedOut,
            "replay phase budget",
        ));
    }
    connection.write_frame(event, data)
}
fn pump(connection: &mut SseConnection<'_>, queue: &Receiver<SseFrame>) {
    loop {
        if connection.shared.is_shutting_down() {
            return;
        }
        match queue.recv_timeout(super::state::HEARTBEAT_INTERVAL) {
            Ok(frame) => {
                if connection.write_frame(frame.event, &frame.data).is_err() {
                    return;
                }
            }
            Err(RecvTimeoutError::Timeout) => {
                // 空闲断线检测（§6.1c 的前提）：peek 不消费——对端已
                // 关闭返回 Ok(0)，此刻摘除订阅者，审批的「订阅全断 →
                // Deny」才能在无帧可写的静默期生效。
                let mut probe = [0u8; 1];
                match connection.stream.peek(&mut probe) {
                    Ok(0) => return,
                    Ok(_) => {}
                    Err(ref error)
                        if error.kind() == std::io::ErrorKind::WouldBlock
                            || error.kind() == std::io::ErrorKind::TimedOut => {}
                    Err(_) => return,
                }
                if connection
                    .write_raw(super::protocol::encode_heartbeat().as_bytes())
                    .is_err()
                {
                    return;
                }
            }
            Err(RecvTimeoutError::Disconnected) => return,
        }
    }
}

struct SseConnection<'a> {
    stream: &'a mut TcpStream,
    seq: u64,
    shared: &'a Arc<ServeShared>,
    subscriber_id: u64,
}

impl<'a> SseConnection<'a> {
    fn write_raw(&mut self, bytes: &[u8]) -> std::io::Result<()> {
        self.stream.write_all(bytes)?;
        self.stream.flush()
    }

    fn write_frame(&mut self, event: &str, data: &str) -> std::io::Result<()> {
        self.seq += 1;
        let frame = protocol::encode_sse_frame(event, self.seq, data);
        self.write_raw(frame.as_bytes())
    }

    /// 阶段预算耗尽的显式失败：一条诊断 notice 后断连（不静默、
    /// 不悬挂——客户端据 notice 与连接关闭走重连=重建）。
    fn fail_phase_budget(&mut self) {
        let ctl = serde_json::json!({
            "kind": "internal",
            "payload": {"error": "replay phase budget exceeded; reconnect to retry"},
        });
        let _ = self.write_frame("notice", &super::shapes::ctl_data(&ctl));
        self.cleanup();
    }

    fn cleanup(&mut self) {
        self.shared.remove_subscriber(self.subscriber_id);
        // 主动半关：让对端 pump 的 recv 侧立刻看到结束（对慢消费者
        // 断连同样适用——INV-S7 的服务端半边）。
        let _ = self.stream.shutdown(std::net::Shutdown::Write);
    }
}
