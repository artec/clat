//! SSE 连接生命周期（§7.2 六步）：鉴权在连接层完成（serve.rs 三闸），
//! 这里从「注册订阅」开始：
//!
//! ```text
//! 1.（三闸，serve.rs）
//! 2. 注册活流订阅（buffered_at = 此刻 run 缓冲长度）
//! 3. replay.begin → snapshot().replay 全量 → replay.end（journal 域）
//! 4. subscribed { last_seq }（committed_seq 水位，竞态自检用）
//! 5. active run 存在 → 直写 run_buffer[0..buffered_at]（事件域）
//! 6. 泵活流队列 → 实时（recv_timeout 15s → 心跳 comment）
//! ```
//!
//! 步 2 与步 5/6 的衔接靠 buffered_at：重发段与队列在同一锁内取值，
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
    // 写超时：慢消费者的 OS 缓冲填满后，泵线程的写不至于永久阻塞
    //（INV-S7 的服务端半边——超时即断连清理）。读超时只服务于
    // peek 探测（泵不消费客户端字节，保持只写姿态）。
    let _ = stream.set_write_timeout(Some(Duration::from_secs(10)));
    let _ = stream.set_read_timeout(Some(Duration::from_secs(1)));
    if super::http::write_sse_head(stream).is_err() {
        return;
    }
    let (subscriber_id, queue, buffered_at) = shared.register_subscriber();
    let mut connection = SseConnection {
        stream,
        seq: 0,
        shared,
        subscriber_id,
    };

    // 步 3：journal 域重放（一次锁内取 snapshot + 水位，避免两读间
    // 漂移——last_seq 至少覆盖重放尾）。
    //
    // PWA2-01：重放+缓冲前缀阶段有**总时长预算**（每帧写另有 10s
    // 超时）。预算耗尽 → 显式断连——订阅生命周期状态机保证
    // CONNECT →（REPLAYING）→ SUBSCRIBED 或 FAILED/CLOSED，任何连接
    // 不得无限停留在「已重放未确认」之间（慢客户端 × 巨 journal
    // 的组合由预算封顶）。
    let phase_deadline = Instant::now() + REPLAY_PHASE_BUDGET;
    let replay_result = {
        let mut app = shared.app.lock().expect("application lock");
        app.snapshot()
            .map(|snapshot| (snapshot.replay, app.committed_seq(), snapshot.session_id))
    };
    let (replay, last_seq, session_id) = match replay_result {
        Ok(value) => value,
        Err(error) => {
            // 重放源失败：fail-closed——以 notice 形态告知后断流，绝不
            // 静默给出半截历史。
            let ctl = serde_json::json!({
                "kind": "internal",
                "payload": {"error": error.to_string()},
            });
            let _ = connection.write_frame("notice", &super::shapes::ctl_data(&ctl));
            connection.cleanup();
            return;
        }
    };
    if connection.write_frame("replay.begin", "{}").is_err() {
        connection.cleanup();
        return;
    }
    for event in &replay {
        if Instant::now() >= phase_deadline {
            connection.fail_phase_budget();
            return;
        }
        let data = super::shapes::replay_data(event);
        if connection.write_frame("replay", &data).is_err() {
            connection.cleanup();
            return;
        }
    }
    if connection.write_frame("replay.end", "{}").is_err() {
        connection.cleanup();
        return;
    }

    // 步 4：订阅确认（last_seq 是诊断辅助，不是正确性依赖——§7.2）。
    let ctl = serde_json::json!({
        "last_seq": last_seq,
        "session_id": session_id.map(|id| id.as_str().to_owned()),
        "replaying": false,
    });
    if connection
        .write_frame("subscribed", &super::shapes::ctl_data(&ctl))
        .is_err()
    {
        connection.cleanup();
        return;
    }

    // 步 5：active run 的缓冲前缀直写（事件域从 run 头开始，无中段
    // 截断——INV-S4 判别锚）。同受阶段预算约束（PWA2-01）。
    if let Some(buffered_at) = buffered_at {
        for data in shared.run_buffer_prefix(buffered_at) {
            if Instant::now() >= phase_deadline {
                connection.fail_phase_budget();
                return;
            }
            if connection.write_frame("event", &data).is_err() {
                connection.cleanup();
                return;
            }
        }
    }

    // 步 6：活流泵。
    pump(&mut connection, &queue);
    connection.cleanup();
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
