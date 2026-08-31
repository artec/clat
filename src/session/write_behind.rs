//! Write-behind batching (plan §8.4, compat doc §9.4): a fixed batch
//! window measured from the first pending event — later enqueues never
//! extend it — a write takes the whole queue as one ordered prefix, and a
//! failure re-queues the batch at the front and pauses the automatic path
//! until the next enqueue or an explicit flush.

use crate::session::event::SessionEvent;
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

pub(crate) const WRITE_BATCH_MAX_DELAY: Duration = Duration::from_millis(200);

struct State {
    pending: Vec<SessionEvent>,
    deadline: Option<Instant>,
    /// A write is in flight.
    active: bool,
    /// The automatic path paused after a failure.
    paused: bool,
    flush_requested: bool,
    closed: bool,
    last_error: Option<String>,
}

pub(crate) struct SessionWriteBehind {
    state: Arc<Mutex<State>>,
    signal: Arc<Condvar>,
    worker: Mutex<Option<std::thread::JoinHandle<()>>>,
    max_delay: Duration,
    /// 测试探针（cfg(test)）：spawn 前由父线程置位、worker 退出时清
    /// 位——每实例存活语义，不受并行套件里其它 writer 生灭影响
    /// （全局 LIVE_WRITERS 计数做"本会话有没有线程"断言会被别家
    /// 抖动顶假红/假绿，CI 实红两次的教训）。
    #[cfg(test)]
    worker_alive: Arc<std::sync::atomic::AtomicBool>,
}

impl SessionWriteBehind {
    /// `write` performs one durable batch append; it resolves only after
    /// the batch is durable (or fails, keeping the batch for retry).
    pub(crate) fn new(
        max_delay: Duration,
        write: impl Fn(&[SessionEvent]) -> Result<(), String> + Send + 'static,
    ) -> Self {
        let state = Arc::new(Mutex::new(State {
            pending: Vec::new(),
            deadline: None,
            active: false,
            paused: false,
            flush_requested: false,
            closed: false,
            last_error: None,
        }));
        let signal = Arc::new(Condvar::new());
        let worker_state = Arc::clone(&state);
        let worker_signal = Arc::clone(&signal);
        #[cfg(test)]
        let worker_alive = {
            let flag = Arc::new(std::sync::atomic::AtomicBool::new(false));
            // 父线程置位：new 返回即"worker 存在"可观察，不依赖子线程
            // 被调度的时机（worker 内置位在忙 runner 上有毫秒级延迟，
            // 全局计数断言因此假红过）。
            flag.store(true, std::sync::atomic::Ordering::SeqCst);
            flag
        };
        let spawned = {
            #[cfg(test)]
            {
                let alive_guard = Arc::clone(&worker_alive);
                std::thread::Builder::new()
                    .name("clat-session-writer".into())
                    .spawn(move || {
                        worker_loop(worker_state, worker_signal, write, max_delay, alive_guard)
                    })
            }
            #[cfg(not(test))]
            {
                std::thread::Builder::new()
                    .name("clat-session-writer".into())
                    .spawn(move || worker_loop(worker_state, worker_signal, write, max_delay))
            }
        };
        let worker = match spawned {
            Ok(worker) => worker,
            Err(error) => {
                #[cfg(test)]
                worker_alive.store(false, std::sync::atomic::Ordering::SeqCst);
                panic!("spawn session writer: {error}");
            }
        };
        Self {
            state,
            signal,
            worker: Mutex::new(Some(worker)),
            max_delay,
            #[cfg(test)]
            worker_alive,
        }
    }

    /// 测试探针：worker 存活标志的共享句柄——句柄可以在实例 drop 后
    /// 继续轮询，从而观察"drop 是否真的 join 了线程"。
    #[cfg(test)]
    pub(crate) fn worker_alive_handle_for_test(&self) -> Arc<std::sync::atomic::AtomicBool> {
        Arc::clone(&self.worker_alive)
    }

    /// Enqueue one atomic group: all events enter under one lock, so the
    /// batch deadline can never split them (A-02 fix, plan §8.5). Enqueue
    /// after `close` is an error, not a silent drop.
    pub(crate) fn enqueue(&self, events: Vec<SessionEvent>) -> Result<(), String> {
        let mut state = self.state.lock().expect("writer lock");
        if state.closed {
            return Err("session writer is closed".into());
        }
        let was_idle = state.pending.is_empty() && !state.active;
        if state.paused {
            state.paused = false;
        }
        state.pending.extend(events);
        // The deadline arms only on the idle→busy transition (or un-pause):
        // events joining an open window never extend it.
        if was_idle && state.deadline.is_none() {
            state.deadline = Some(Instant::now() + self.max_delay);
        }
        self.signal.notify_all();
        Ok(())
    }

    /// Drain to quiescence: cancels the window, un-pauses, and reports a
    /// write error with the batch kept for the next retry.
    pub(crate) fn flush(&self) -> Result<(), String> {
        let mut guard = self.state.lock().expect("writer lock");
        if guard.pending.is_empty() && !guard.active && guard.last_error.is_none() {
            return Ok(());
        }
        guard.paused = false;
        guard.deadline = None;
        guard.flush_requested = true;
        // A fresh flush is a fresh attempt: clear the stale failure so the
        // worker actually retries instead of echoing it forever.
        guard.last_error = None;
        self.signal.notify_all();
        loop {
            if let Some(error) = guard.last_error.clone() {
                guard.flush_requested = false;
                return Err(error);
            }
            if guard.pending.is_empty() && !guard.active {
                guard.flush_requested = false;
                return Ok(());
            }
            let (next, _) = self
                .signal
                .wait_timeout(guard, Duration::from_secs(30))
                .expect("flush condvar");
            guard = next;
        }
    }

    /// Remove one proven-not-committed tail range after a synchronous
    /// transaction failed. The caller serializes every enqueue while this
    /// runs, and `flush` has already waited for the worker to restore the
    /// failed batch. Earlier events from other producers remain queued.
    pub(crate) fn discard_pending_tail(
        &self,
        start_seq: u64,
        end_inclusive: u64,
    ) -> Result<(), String> {
        let mut state = self.state.lock().expect("writer lock");
        if state.active {
            return Err("cannot discard a batch while a session write is active".into());
        }
        if state.pending.iter().any(|event| event.seq > end_inclusive) {
            return Err("cannot discard a non-tail session event range".into());
        }
        let expected = end_inclusive
            .checked_sub(start_seq)
            .and_then(|width| width.checked_add(1))
            .ok_or_else(|| "invalid session event range".to_owned())?
            as usize;
        let present = state
            .pending
            .iter()
            .filter(|event| (start_seq..=end_inclusive).contains(&event.seq))
            .count();
        if present != expected {
            return Err("failed session event range is not fully pending".into());
        }
        state
            .pending
            .retain(|event| event.seq < start_seq || event.seq > end_inclusive);
        if state.pending.is_empty() {
            state.deadline = None;
            state.paused = false;
            state.last_error = None;
        }
        self.signal.notify_all();
        Ok(())
    }

    /// Flush remaining work (propagating its error) and join the worker.
    /// Works through `&self`: the coordinator outlives its journal handles
    /// and must be able to retire the thread at session detach (audit
    /// P1-07 — a detached worker leaked one thread per session switch).
    ///
    /// Shutdown protocol: `closed` stops the automatic retry path. A write
    /// that fails *after* close drops its batch (the error is returned to
    /// the caller) so the worker always has a guaranteed exit — it never
    /// blocks a join on a permanently failing disk.
    pub(crate) fn close(&self) -> Result<(), String> {
        // Serialize the entire shutdown. A second close after the worker
        // was joined must return immediately; calling `flush` with no
        // worker left would otherwise wait forever on pending/error state.
        let mut worker_slot = self.worker.lock().expect("worker slot");
        if worker_slot.is_none() {
            return self
                .state
                .lock()
                .expect("writer lock")
                .last_error
                .clone()
                .map_or(Ok(()), Err);
        }
        {
            let mut state = self.state.lock().expect("writer lock");
            state.closed = true;
            state.paused = false;
            self.signal.notify_all();
        }
        let result = self.flush();
        let worker = worker_slot.take();
        if let Some(worker) = worker {
            let _ = worker.join();
        }
        result
    }
}

#[cfg(test)]
static LIVE_WRITERS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// Test-only: how many session-writer threads are currently alive.
#[cfg(test)]
pub(crate) fn live_writers_for_test() -> usize {
    LIVE_WRITERS.load(std::sync::atomic::Ordering::SeqCst)
}

fn worker_loop(
    state: Arc<Mutex<State>>,
    signal: Arc<Condvar>,
    write: impl Fn(&[SessionEvent]) -> Result<(), String>,
    max_delay: Duration,
    #[cfg(test)] alive: Arc<std::sync::atomic::AtomicBool>,
) {
    #[cfg(test)]
    LIVE_WRITERS.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    // guard 在任何退出路径（含 panic）清掉每实例存活标志——drop 后
    // 轮询该标志即"线程真的退出了"的可靠证据。
    #[cfg(test)]
    let _alive_guard = AliveGuard(alive);
    loop {
        // Decide under a fresh lock; drop it while waiting or writing.
        let batch = loop {
            let mut guard = state.lock().expect("writer lock");
            let now = Instant::now();
            let due = guard.deadline.is_some_and(|deadline| deadline <= now);
            let wants = (guard.flush_requested || due)
                && !guard.pending.is_empty()
                && !guard.active
                && (guard.flush_requested || !guard.paused);
            if wants {
                guard.active = true;
                guard.deadline = None;
                guard.flush_requested = false;
                break std::mem::take(&mut guard.pending);
            }
            if guard.closed && guard.pending.is_empty() && !guard.active {
                return;
            }
            let wait = match guard.deadline {
                Some(deadline) if !guard.paused => deadline.saturating_duration_since(now),
                _ => Duration::from_secs(3600),
            };
            let (next, _) = signal.wait_timeout(guard, wait).expect("writer condvar");
            drop(next);
        };

        let result = write(&batch);

        let mut guard = state.lock().expect("writer lock");
        guard.active = false;
        match result {
            Ok(()) => {
                guard.last_error = None;
                guard.paused = false;
                // Re-arm the window for events that arrived while the
                // previous batch was writing (fourth-pass F-B): the
                // deadline only arms on the idle→busy transition, so
                // without this a streaming run's chunks sat uncommitted
                // until the next explicit flush — a crash-loss window of a
                // whole turn instead of one batch delay.
                if !guard.pending.is_empty()
                    && guard.deadline.is_none()
                    && !guard.closed
                    && !guard.paused
                {
                    guard.deadline = Some(Instant::now() + max_delay);
                }
            }
            Err(error) => {
                if guard.closed {
                    // Closed and failing: stop retrying, drop the batch,
                    // report the error, and exit — the join in `close` must
                    // always make progress (audit P1-07).
                    guard.pending.clear();
                    guard.last_error = Some(error);
                    signal.notify_all();
                    return;
                }
                let mut restored = batch;
                restored.append(&mut guard.pending);
                guard.pending = restored;
                guard.paused = !guard.flush_requested;
                guard.last_error = Some(error);
            }
        }
        signal.notify_all();
    }
}

/// 测试专用退出守卫：worker 无论从哪条路径离开（正常收尾、close 后
/// 放弃批次、panic）都递减全局计数并清掉每实例存活标志——散落各
/// return 点的手工清理曾覆盖不全且易双重递减。
#[cfg(test)]
struct AliveGuard(Arc<std::sync::atomic::AtomicBool>);

#[cfg(test)]
impl Drop for AliveGuard {
    fn drop(&mut self) {
        LIVE_WRITERS.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
        self.0.store(false, std::sync::atomic::Ordering::SeqCst);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::event::{SessionEvent, payloads};
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn event(seq: u64) -> SessionEvent {
        SessionEvent::new(
            "assistant/chunk",
            seq,
            1000 + seq as i64,
            payloads::assistant_chunk(1, 0, serde_json::json!({ "i": seq })),
        )
    }

    #[test]
    fn window_defers_writes_and_flush_drains_to_quiescence() {
        let writes = Arc::new(AtomicUsize::new(0));
        let seen = Arc::new(Mutex::new(Vec::<Vec<SessionEvent>>::new()));
        let behind = {
            let writes = Arc::clone(&writes);
            let seen = Arc::clone(&seen);
            SessionWriteBehind::new(Duration::from_millis(10_000), move |batch| {
                writes.fetch_add(1, Ordering::SeqCst);
                seen.lock().expect("seen").push(batch.to_vec());
                Ok(())
            })
        };
        behind.enqueue(vec![event(0), event(1)]).expect("enqueue");
        behind.enqueue(vec![event(2)]).expect("enqueue");
        std::thread::sleep(Duration::from_millis(50));
        assert_eq!(
            writes.load(Ordering::SeqCst),
            0,
            "nothing writes before the deadline"
        );
        behind.flush().expect("flush");
        assert_eq!(
            writes.load(Ordering::SeqCst),
            1,
            "one batch takes the whole queue"
        );
        let batches = seen.lock().expect("seen");
        assert_eq!(batches[0].len(), 3, "all three events in one ordered batch");
        assert_eq!(batches[0][2].seq, 2);
        drop(batches);
        behind.close().expect("close");
    }

    #[test]
    fn write_failure_keeps_batch_and_flush_surfaces_the_error_then_retries() {
        let attempts = Arc::new(AtomicUsize::new(0));
        let behind = {
            let attempts = Arc::clone(&attempts);
            SessionWriteBehind::new(Duration::from_millis(10_000), move |_| {
                if attempts.fetch_add(1, Ordering::SeqCst) == 0 {
                    Err("disk full".into())
                } else {
                    Ok(())
                }
            })
        };
        behind.enqueue(vec![event(0), event(1)]).expect("enqueue");
        assert_eq!(behind.flush().unwrap_err(), "disk full");
        // Batch retained: an explicit flush retries it and succeeds.
        behind.flush().expect("retry succeeds");
        assert_eq!(attempts.load(Ordering::SeqCst), 2);
        behind.close().expect("close");
    }

    #[test]
    fn failed_transaction_can_discard_only_its_tail_and_keep_earlier_events() {
        let attempts = Arc::new(AtomicUsize::new(0));
        let seen = Arc::new(Mutex::new(Vec::<Vec<u64>>::new()));
        let behind = {
            let attempts = Arc::clone(&attempts);
            let seen = Arc::clone(&seen);
            SessionWriteBehind::new(Duration::from_millis(10_000), move |batch| {
                seen.lock()
                    .expect("seen")
                    .push(batch.iter().map(|event| event.seq).collect());
                if attempts.fetch_add(1, Ordering::SeqCst) == 0 {
                    Err("disk full".into())
                } else {
                    Ok(())
                }
            })
        };
        behind.enqueue(vec![event(0), event(1)]).expect("prefix");
        behind.enqueue(vec![event(2)]).expect("transaction tail");
        assert_eq!(behind.flush().unwrap_err(), "disk full");
        behind
            .discard_pending_tail(2, 2)
            .expect("discard only failed transaction tail");
        behind.flush().expect("commit retained prefix");
        assert_eq!(*seen.lock().expect("seen"), vec![vec![0, 1, 2], vec![0, 1]]);
        behind.close().expect("close");
    }

    /// 并行测试会同时持有各自的 writer：全局计数的"回到基线"轮询只
    /// 做兜底；持有实例的测试一律用每实例存活探针（close 同步 join，
    /// 返回即已退出，零抖动面）。
    #[test]
    fn close_with_a_permanently_failing_write_returns_the_error_and_exits() {
        let behind =
            SessionWriteBehind::new(Duration::from_millis(10), |_| Err("disk gone".into()));
        let alive = behind.worker_alive_handle_for_test();
        behind.enqueue(vec![event(0)]).expect("enqueue");
        // 修复前：closed 后写失败会把 batch 原样放回并停在 1 小时等待，
        // close 内的 join 永久阻塞（审计 P1-07 的挂死反例）。
        let error = behind.close().expect_err("close surfaces the failure");
        assert_eq!(error, "disk gone");
        assert!(
            !alive.load(Ordering::SeqCst),
            "close joins the worker even when the write keeps failing"
        );
    }

    /// 第四轮复审 F-B：写入进行期间到达的事件必须获得新的窗口，而不是
    /// 等到显式 flush——否则流式 run 的 chunk 崩溃丢失窗口从一批延迟
    /// 退化为一整个 turn。
    #[test]
    fn events_arriving_during_a_write_get_their_own_window() {
        let writes = Arc::new(AtomicUsize::new(0));
        let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
        let release = Arc::new(std::sync::Mutex::new(Some(release_rx)));
        let behind = {
            let writes = Arc::clone(&writes);
            let release = Arc::clone(&release);
            SessionWriteBehind::new(Duration::from_millis(40), move |batch| {
                writes.fetch_add(1, Ordering::SeqCst);
                if batch.first().is_some_and(|event| event.seq == 0) {
                    // Simulate a slow disk under a stream: block the first
                    // write until the test drops the sender.
                    if let Some(receiver) = release.lock().unwrap().take() {
                        let _ = receiver.recv();
                    }
                }
                Ok(())
            })
        };
        behind.enqueue(vec![event(0)]).expect("enqueue 0");
        // Wait until the first write is in flight (and blocked).
        while writes.load(Ordering::SeqCst) == 0 {
            std::thread::sleep(Duration::from_millis(2));
        }
        // The tail joins the queue while batch [0] is still writing.
        behind
            .enqueue(vec![event(1), event(2)])
            .expect("enqueue tail");
        drop(release_tx);
        // Without the re-arm, the tail never commits until an explicit
        // flush; with it, a fresh 40ms window fires after the first write.
        for _ in 0..250 {
            if writes.load(Ordering::SeqCst) >= 2 {
                break;
            }
            std::thread::sleep(Duration::from_millis(4));
        }
        assert_eq!(
            writes.load(Ordering::SeqCst),
            2,
            "the tail batch commits within its own window, no explicit flush"
        );
        behind.close().expect("close");
    }

    #[test]
    fn idle_close_exits_the_worker() {
        let behind = SessionWriteBehind::new(Duration::from_millis(10), |_| Ok(()));
        let alive = behind.worker_alive_handle_for_test();
        behind.close().expect("close");
        assert!(
            !alive.load(Ordering::SeqCst),
            "an idle worker exits on close"
        );
    }

    #[test]
    fn close_is_idempotent_after_the_worker_is_joined() {
        let behind = SessionWriteBehind::new(Duration::from_secs(10), |_| Ok(()));
        behind.close().expect("first close");
        behind
            .close()
            .expect("second close returns without waiting");
    }
}
