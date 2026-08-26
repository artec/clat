//! serve 共享状态：订阅者注册表、run 缓冲、pending 审批表、
//! fanout sink、settler 与 ApplicationEvent 转发线程
//!（docs/todo/serve-rpc.md §7/§8.2）。
//!
//! 锁纪律（死锁防线）：持 `inner` 锁期间绝不调用门面方法——fanout
//! 的 `try_send` 非阻塞；`prompt.send` 先占 run 槽（`try_claim_run`）
//! 再锁应用，失败回滚，busy 判定与缓冲累积之间没有窗口。
//! run worker 上的 `EventSink::emit` 经 `fanout_run_event` 快进快出，
//! 永不阻塞（INV-S7）。

use crate::application::join_with_grace;
use crate::event::RunEvent;
use crate::permission::PermissionDecision;
use crate::{
    ApplicationEvent, ApplicationRunDone, ApplicationRunFailure, TrustedProjectApplication,
};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, SyncSender, channel, sync_channel};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

/// 每 SSE 连接的有界帧队列（INV-S7：满即断连，慢消费者走「重连=重建」）。
pub(crate) const SUBSCRIBER_QUEUE_FRAMES: usize = 1024;

/// SSE 空闲心跳间隔（§5.1）。
pub(crate) const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(15);

/// 一条下行帧：`event` 是 SSE frame-type，`data` 是单行 JSON 载荷。
pub(crate) struct SseFrame {
    pub event: &'static str,
    pub data: String,
}

struct Subscriber {
    id: u64,
    tx: SyncSender<SseFrame>,
}

/// active run 的服务侧账目：受理 id、起点与**全量 RunEvent 帧缓冲**
///（§7.2-5 订阅重发源；run 终态即随 active_run 清空释放，内存由
/// B1 花费护栏间接封顶——§7.3 的显式权衡）。
pub(crate) struct ActiveRun {
    pub rpc_id: String,
    pub started_ms: i64,
    buffer: Vec<String>,
}

struct ServeInner {
    subscribers: Vec<Subscriber>,
    active_run: Option<ActiveRun>,
}

pub(crate) struct PendingApproval {
    pub decision_tx: std::sync::mpsc::Sender<PermissionDecision>,
}

pub(crate) struct ServeShared {
    pub app: Arc<Mutex<TrustedProjectApplication>>,
    pub token: String,
    pub port: u16,
    /// 每订阅者队列容量（生产 = [`SUBSCRIBER_QUEUE_FRAMES`]；测试可
    /// 调小以确定性触发 INV-S7 的溢出摘除——内核发送缓冲会吸收可观
    /// 流量，容量 1024 在网络层难以确定性打满）。
    pub(crate) queue_frames: usize,
    inner: Mutex<ServeInner>,
    pub pending: Mutex<HashMap<String, PendingApproval>>,
    shutting_down: AtomicBool,
    /// settler / notice 转发（关停时统一有界 join）。
    workers: Mutex<Vec<JoinHandle<()>>>,
    connections: Mutex<Vec<JoinHandle<()>>>,
    next_subscriber_id: AtomicU64,
}

pub(crate) fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or(0)
}

impl ServeShared {
    pub(crate) fn new(
        app: Arc<Mutex<TrustedProjectApplication>>,
        token: String,
        port: u16,
    ) -> Self {
        Self {
            app,
            token,
            port,
            queue_frames: SUBSCRIBER_QUEUE_FRAMES,
            inner: Mutex::new(ServeInner {
                subscribers: Vec::new(),
                active_run: None,
            }),
            pending: Mutex::new(HashMap::new()),
            shutting_down: AtomicBool::new(false),
            workers: Mutex::new(Vec::new()),
            connections: Mutex::new(Vec::new()),
            next_subscriber_id: AtomicU64::new(1),
        }
    }

    pub(crate) fn is_shutting_down(&self) -> bool {
        self.shutting_down.load(Ordering::SeqCst)
    }

    // —— 订阅与 fanout ——————————————————————————————————————————————

    /// 注册订阅。返回 `(id, 接收端, buffered_at)`——`buffered_at` 是注册
    /// 时刻 run 缓冲的长度：SSE 连接先直写 `buffer[0..buffered_at]`，
    /// 队列天然从 `buffered_at` 接续——两段在同一锁内取值，注册窗口的
    /// 帧「既在重发段又在队列」的重叠在结构上不可能（§7.2 六步的
    /// 缝隙消解，INV-S4 判别锚）。
    pub(crate) fn register_subscriber(&self) -> (u64, Receiver<SseFrame>, Option<usize>) {
        let (tx, rx) = sync_channel(self.queue_frames);
        let mut inner = self.inner.lock().expect("serve inner lock");
        let id = self.next_subscriber_id.fetch_add(1, Ordering::Relaxed);
        let buffered_at = inner.active_run.as_ref().map(|run| run.buffer.len());
        inner.subscribers.push(Subscriber { id, tx });
        (id, rx, buffered_at)
    }

    pub(crate) fn remove_subscriber(&self, id: u64) {
        self.inner
            .lock()
            .expect("serve inner lock")
            .subscribers
            .retain(|subscriber| subscriber.id != id);
    }

    /// 关停时清空订阅者（发送端落空 → 各 SSE 泵的 recv 变 Disconnected，
    /// 连接线程随即收尾）。
    pub(crate) fn clear_subscribers(&self) {
        self.inner
            .lock()
            .expect("serve inner lock")
            .subscribers
            .clear();
    }

    pub(crate) fn subscriber_count(&self) -> usize {
        self.inner
            .lock()
            .expect("serve inner lock")
            .subscribers
            .len()
    }

    /// run 缓冲的 `[0..buffered_at]` 前缀（订阅重发段；只读不改）。
    pub(crate) fn run_buffer_prefix(&self, buffered_at: usize) -> Vec<String> {
        self.inner
            .lock()
            .expect("serve inner lock")
            .active_run
            .as_ref()
            .map(|run| run.buffer[..buffered_at.min(run.buffer.len())].to_vec())
            .unwrap_or_default()
    }

    /// 控制帧广播（approval.requested / prompt.settled / notice / …）：
    /// 不进 run 缓冲——控制面是活流，不落 durable（§6.1）。
    pub(crate) fn broadcast(&self, frame: SseFrame) {
        let mut inner = self.inner.lock().expect("serve inner lock");
        Self::deliver(&mut inner, frame);
    }

    /// 实时族（INV-S2 零转译）：`envelope_line` 去掉行尾换行的原文，
    /// 同一锁内追加 run 缓冲 + 广播。
    pub(crate) fn fanout_run_event(&self, event: &RunEvent) {
        let data = super::shapes::realtime_data(event);
        let mut inner = self.inner.lock().expect("serve inner lock");
        if let Some(run) = inner.active_run.as_mut() {
            run.buffer.push(data.clone());
        }
        Self::deliver(
            &mut inner,
            SseFrame {
                event: "event",
                data,
            },
        );
    }

    /// 有界投递：**非阻塞** `try_send`——满/断即摘除该订阅者（INV-S7
    /// 慢消费者政策）。注意不能用 `send`：那是阻塞语义，队列满时
    /// run worker 会持 `inner` 锁卡死，整个 fanout 被最慢消费者拖住。
    fn deliver(inner: &mut ServeInner, frame: SseFrame) {
        inner.subscribers.retain(|subscriber| {
            subscriber
                .tx
                .try_send(SseFrame {
                    event: frame.event,
                    data: frame.data.clone(),
                })
                .is_ok()
        });
    }

    // —— run 账目 ————————————————————————————————————————————————————

    /// `prompt.send` 受理前占位（busy 判定 + 缓冲归属）。已占用 → false。
    pub(crate) fn try_claim_run(&self, rpc_id: &str, started_ms: i64) -> bool {
        let mut inner = self.inner.lock().expect("serve inner lock");
        if inner.active_run.is_some() {
            return false;
        }
        inner.active_run = Some(ActiveRun {
            rpc_id: rpc_id.to_owned(),
            started_ms,
            buffer: Vec::new(),
        });
        true
    }

    /// `start_run` 失败时回滚占位。
    pub(crate) fn release_run_claim(&self) {
        self.inner.lock().expect("serve inner lock").active_run = None;
    }

    /// `session.info` 的 active_run 字段。
    pub(crate) fn active_run_info(&self) -> Value {
        let inner = self.inner.lock().expect("serve inner lock");
        match &inner.active_run {
            Some(run) => json!({
                "prompt_rpc_id": run.rpc_id,
                "started": run.started_ms,
            }),
            None => Value::Null,
        }
    }

    /// settler 收尾：同一锁内广播 settled 帧（恰一）并释放 run 账目
    ///（INV-S6）。broadcast 之后再清——晚到的订阅者看到 journal 重放
    /// 而非半截 run，不会拿到 settled 却没有前文。
    fn finish_run(&self, settled_data: String) {
        let mut inner = self.inner.lock().expect("serve inner lock");
        Self::deliver(
            &mut inner,
            SseFrame {
                event: "prompt.settled",
                data: settled_data,
            },
        );
        inner.active_run = None;
    }

    // —— 后台线程 ————————————————————————————————————————————————————

    fn register_worker(&self, handle: JoinHandle<()>) {
        self.workers
            .lock()
            .expect("serve workers lock")
            .push(handle);
    }

    pub(crate) fn register_connection(&self, handle: JoinHandle<()>) {
        self.connections
            .lock()
            .expect("serve connections lock")
            .retain(|handle| !handle.is_finished());
        self.connections
            .lock()
            .expect("serve connections lock")
            .push(handle);
    }

    /// ApplicationEvent → notice 帧转发（§5.3）。订阅 sender 由
    /// application.close 释放，通道断开即线程退出。
    pub(crate) fn spawn_notice_forwarder(self: &Arc<Self>) {
        let (tx, rx) = channel::<ApplicationEvent>();
        {
            let app = self.app.lock().expect("application lock");
            app.subscribe(tx);
        }
        let shared = Arc::clone(self);
        let handle = std::thread::Builder::new()
            .name("clat-serve-notice".into())
            .spawn(move || {
                loop {
                    // 双退出条件：通道断开（应用 close 释放订阅 sender）
                    // 或关停旗。后者是硬保证——monitor 等后台线程若在
                    // close 宽限后被放弃，其 sender 克隆仍存活，仅靠通道
                    // 断开判断会永久挂起。
                    match rx.recv_timeout(Duration::from_millis(200)) {
                        Ok(event) => {
                            let ctl = super::shapes::notice_ctl(&event);
                            shared.broadcast(SseFrame {
                                event: "notice",
                                data: super::shapes::ctl_data(&ctl),
                            });
                        }
                        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                            if shared.is_shutting_down() {
                                return;
                            }
                        }
                        Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => return,
                    }
                }
            })
            .expect("spawn notice forwarder");
        self.register_worker(handle);
    }

    /// settler（§5.5）：completion 通道到达即广播恰一 `prompt.settled`
    ///（core 保证该时点在持久化与 run-scope 清理之后），随后 join
    /// run worker 观察崩溃。
    pub(crate) fn spawn_settler(
        self: &Arc<Self>,
        rpc_id: String,
        completion: Receiver<Result<ApplicationRunDone, ApplicationRunFailure>>,
        handle: crate::RunHandle,
    ) {
        let shared = Arc::clone(self);
        let worker = std::thread::Builder::new()
            .name("clat-serve-settle".into())
            .spawn(move || {
                // M-03（审查 2026-08-27）：settled 帧携带 committed 回执
                //（完成/取消/失败三态同源——MM-I11：跨过 commit point 的
                // 任何终态都证明消息已耐久；无客户端键的 run 不加字段）。
                let (receipt, settled) = match completion.recv() {
                    Ok(Ok(done)) if done.cancelled => (
                        done.receipt.clone(),
                        super::shapes::settled_cancelled(done.turns, &done.usage),
                    ),
                    Ok(Ok(done)) => (
                        done.receipt.clone(),
                        super::shapes::settled_completed(&done.output, done.turns, &done.usage),
                    ),
                    Ok(Err(failure)) => (
                        failure.receipt.clone(),
                        super::shapes::settled_failed(&failure.error),
                    ),
                    Err(_) => (
                        None,
                        super::shapes::settled_failed("run worker exited without a result"),
                    ),
                };
                let settled = super::shapes::with_prompt_rpc_id(settled, &rpc_id);
                let settled = super::shapes::with_admission_receipt(settled, receipt.as_deref());
                shared.finish_run(super::shapes::ctl_data(&settled));
                let _ = handle.join();
            })
            .expect("spawn settler");
        self.register_worker(worker);
    }

    // —— 关停 ————————————————————————————————————————————————————————

    pub(crate) fn mark_shutting_down(&self) {
        self.shutting_down.store(true, Ordering::SeqCst);
    }

    pub(crate) fn cancel_active_run(&self) {
        let app = self.app.lock().expect("application lock");
        app.cancel_active_run();
    }

    /// 有界 join 全部连接线程（复用 core 的退出宽限纪律）。
    pub(crate) fn drain_connections(&self) {
        let mut handles = self
            .connections
            .lock()
            .expect("serve connections lock")
            .drain(..)
            .collect::<Vec<_>>();
        // stable sort 无意义，逐个有界等待即可。
        for handle in handles.drain(..) {
            let _ = join_with_grace(handle, Duration::from_secs(2), "serve connection");
        }
    }

    pub(crate) fn drain_workers(&self) {
        let mut handles = self
            .workers
            .lock()
            .expect("serve workers lock")
            .drain(..)
            .collect::<Vec<_>>();
        for handle in handles.drain(..) {
            let _ = join_with_grace(handle, Duration::from_secs(5), "serve background worker");
        }
    }
}
