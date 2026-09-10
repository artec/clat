use crate::RunHandle;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};

/// 中断路由器：进程边界（main.rs 的信号处理器）与 exec 内部共享。
/// 信号安装归属进程边界；本类型只做状态与取消，可克隆、可重复使用
/// （HL-05：库内同进程多次 headless run 不互相破坏）。
#[derive(Clone, Default)]
pub struct ExecCancel {
    inner: Arc<ExecCancelInner>,
}

#[derive(Default)]
struct ExecCancelInner {
    slot: Mutex<Option<RunHandle>>,
    handle_ready: Condvar,
    interrupted: AtomicBool,
}

/// `ExecCancel::on_interrupt` 的决策结果；进程语义（如硬退出）归调用方。
pub enum InterruptOutcome {
    /// 已请求活动 run 优雅取消。
    RunCancelled,
    /// run 句柄尚未就位：已记录中断，句柄就位后立即取消（HL-03）。
    PendingRunStart,
    /// 第二次（及以后）中断：调用方应立即以 130 退出。
    MustExit,
}

impl ExecCancel {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn interrupted(&self) -> bool {
        self.inner.interrupted.load(Ordering::SeqCst)
    }

    /// 进程边界收到中断信号时调用。第一次：取消活动 run，或（句柄
    /// 未就位时）记录 pending；第二次起返回 `MustExit`。本方法绝不
    /// 吞掉中断（修复前的缺陷：无句柄的首次信号 cancel 一个 None 后
    /// 被静默忽略，run 照常完成并以 0 退出）。
    pub fn on_interrupt(&self) -> InterruptOutcome {
        let already = self.inner.interrupted.swap(true, Ordering::SeqCst);
        let guard = self.inner.slot.lock();
        match interrupt_action(already, guard.as_ref().is_ok_and(|g| g.is_some())) {
            InterruptAction::CancelRun => {
                if let Ok(guard) = guard
                    && let Some(handle) = guard.as_ref()
                {
                    handle.cancel();
                }
                InterruptOutcome::RunCancelled
            }
            InterruptAction::PendingRunStart => InterruptOutcome::PendingRunStart,
            InterruptAction::ExitHard => InterruptOutcome::MustExit,
        }
    }

    /// 发布 run handle，并兑现句柄就位前已经到达的 Ctrl-C。
    pub(super) fn attach(&self, handle: RunHandle) {
        if let Ok(mut guard) = self.inner.slot.lock() {
            *guard = Some(handle.clone());
            self.inner.handle_ready.notify_all();
        }
        if self.interrupted() {
            handle.cancel();
        }
    }

    pub(super) fn detach(&self) {
        if let Ok(mut guard) = self.inner.slot.lock() {
            *guard = None;
        }
    }

    /// stdout 写失败发生在 run worker 内。`start_run` 可能尚未把 handle
    /// 返回给调用线程；此时 sink 必须等待 handle 发布并先 cancel，再
    /// 允许 worker 继续到工具执行阶段（HL-04）。
    pub(super) fn cancel_active_run_waiting_for_handle(&self) {
        let Ok(mut guard) = self.inner.slot.lock() else {
            return;
        };
        while guard.is_none() {
            let Ok(next) = self.inner.handle_ready.wait(guard) else {
                return;
            };
            guard = next;
        }
        if let Some(handle) = guard.as_ref() {
            handle.cancel();
        }
    }
}

/// Ctrl-C 决策（纯函数，供单测穷举）。
pub(super) enum InterruptAction {
    CancelRun,
    PendingRunStart,
    ExitHard,
}

pub(super) fn interrupt_action(already_interrupted: bool, has_handle: bool) -> InterruptAction {
    match (already_interrupted, has_handle) {
        (false, true) => InterruptAction::CancelRun,
        // 尚未起 run：记录 pending，绝不升级成硬退（HL-03：文档承诺
        // 第一次优雅取消；硬退只属于第二次信号）。
        (false, false) => InterruptAction::PendingRunStart,
        (true, _) => InterruptAction::ExitHard,
    }
}
