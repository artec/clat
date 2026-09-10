//! Per-invocation cumulative wait budget and interruptible WASI clock subscriptions.
use super::*;

/// W1-10：时钟等待的宿主侧切片上限——两次取消/预算检查点之间的最长
/// 间隔。组件的 `subscribe-duration`/`subscribe-instant` 等待以不超过
/// 此值的切片睡眠，取消令牌在片间可达（wasmtime 官方立场：epoch/fuel
/// 无法唤醒阻塞在 wasi:io/poll 睡眠里的宿主调用，见
/// wasmtime-48 `config.rs` "Interaction with blocking host calls"）。
const CLOCK_SLICE: Duration = Duration::from_millis(250);
/// W1-10：单次 invoke 的累计纯时钟等待预算（对齐 MCP `tools/call` 的
/// 120s 壁钟纪律）。超预算后的时钟订阅立即"就绪"——组件重新进入执行
/// 点，后续要么继续订阅（忙转，燃料迅速耗尽 trap）要么干活（烧燃料），
/// 所有既有防线（fuel/epoch）恢复可达。
const CALL_CLOCK_BUDGET: Duration = Duration::from_secs(120);
/// W1-10：一次 invoke 内共享的时钟等待状态（`PluginState.clock` 持有，
/// 每次 invoke 重置）。等待累计跨全部订阅求和——预算是调用级纪律，
/// 不是单次订阅纪律（恶意组件不能用"N 个小睡眠"绕过）。
#[derive(Clone, Default)]
pub(super) struct ClockShared {
    cancel: Option<CancelToken>,
    waited_ns: Arc<AtomicU64>,
    exhausted: Arc<AtomicBool>,
}

impl ClockShared {
    #[cfg(test)]
    pub(super) fn exhaust_for_test(&self) {
        self.waited_ns.store(u64::MAX, AtomicOrdering::Relaxed);
    }

    pub(super) fn begin_invoke(cancel: &CancelToken) -> Self {
        Self {
            cancel: Some(cancel.clone()),
            waited_ns: Arc::new(AtomicU64::new(0)),
            exhausted: Arc::new(AtomicBool::new(false)),
        }
    }

    fn budget_ns(&self) -> u64 {
        CALL_CLOCK_BUDGET.as_nanos() as u64
    }

    /// 本订阅是否应立即返回（视为就绪）：已取消或调用级预算耗尽。
    fn should_stop_waiting(&self) -> bool {
        if self.cancel.as_ref().is_some_and(CancelToken::is_cancelled) {
            return true;
        }
        self.waited_ns.load(AtomicOrdering::Relaxed) >= self.budget_ns()
    }
}

/// 一个有界的时钟订阅（W1-10）。与 wasmtime-wasi 默认实现的关键差异：
/// 默认对无法表示的远期时长（u64::MAX 纳秒级）落 `Deadline::Never` —
/// `pending().await` 永久阻塞宿主线程，取消/燃料/epoch 全部失效；本实现
/// 片式睡眠，片间检查取消与调用级预算，等待总时长语义不变（合法睡眠
/// 不提前就绪）。
struct ClockWait {
    remaining_ns: u64,
    shared: ClockShared,
}

#[async_trait::async_trait]
impl Pollable for ClockWait {
    async fn ready(&mut self) {
        loop {
            if self.remaining_ns == 0 || self.shared.should_stop_waiting() {
                if self.shared.waited_ns.load(AtomicOrdering::Relaxed) >= self.shared.budget_ns() {
                    self.shared.exhausted.store(true, AtomicOrdering::Release);
                }
                return;
            }
            let slice = self.remaining_ns.min(CLOCK_SLICE.as_nanos() as u64);
            std::thread::sleep(Duration::from_nanos(slice));
            self.remaining_ns -= slice;
            self.shared
                .waited_ns
                .fetch_add(slice, AtomicOrdering::Relaxed);
        }
    }
}

/// Arbitrary monotonic epoch; WASI requires elapsed nanoseconds, not wall time.
static MONOTONIC_EPOCH: std::sync::LazyLock<Instant> = std::sync::LazyLock::new(Instant::now);

impl PluginState {
    /// 建立一个有界时钟订阅（W1-10）。
    fn subscribe_clock_wait(
        &mut self,
        duration_ns: u64,
    ) -> wasmtime::Result<Resource<DynPollable>> {
        // 预算已耗尽：立即就绪（组件回到执行点，忙转由燃料收尾）。
        let wait = ClockWait {
            remaining_ns: duration_ns,
            shared: self.clock.clone(),
        };
        let resource = self.table.push(wait)?;
        wasmtime_wasi::p2::subscribe(&mut self.table, resource)
    }
}
/// W1-10：替换 wasmtime-wasi 默认 `monotonic-clock` 宿主——默认实现对
/// 无法表示的远期时长落 `Deadline::Never`（永久阻塞，见
/// wasmtime-wasi-48 `p2/host/clocks.rs`），取消/燃料/epoch 全部失效；
/// 本实现对每个订阅走 [`ClockWait`] 的片式有界等待。
impl monotonic_clock::Host for PluginState {
    fn now(&mut self) -> wasmtime::Result<monotonic_clock::Instant> {
        Ok(MONOTONIC_EPOCH.elapsed().as_nanos() as u64)
    }

    fn resolution(&mut self) -> wasmtime::Result<monotonic_clock::Instant> {
        Ok(1)
    }

    fn subscribe_instant(
        &mut self,
        when: monotonic_clock::Instant,
    ) -> wasmtime::Result<Resource<DynPollable>> {
        let remaining = when.saturating_sub(self.now()?);
        self.subscribe_clock_wait(remaining)
    }

    fn subscribe_duration(
        &mut self,
        duration: monotonic_clock::Duration,
    ) -> wasmtime::Result<Resource<DynPollable>> {
        self.subscribe_clock_wait(duration)
    }
}
