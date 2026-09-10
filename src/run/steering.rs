use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

/// In-run steering queue (DSH `steer()` semantics): messages submitted by
/// the frontend while a run is active. The run claims them at the next
/// model-request boundary — never interrupting the in-flight request — and
/// pending steering extends a run that would otherwise complete, because
/// the model still owes the user a response. Messages that were never
/// claimed (cancel, race at the end) leave no durable trace.
///
/// MM-1A：队列元素是 typed [`PendingMessage`]（客户端幂等键随消息
/// travel，claim 后随 `SteeringApplied` 进 journal）。图片 steering 的
/// admission 属 MM-1/MM-2——`Application::steer` 在此之前只放行纯文本
/// 消息（fail-closed），类型已就位、语义不越前开放。
///
/// 终态封口（W1-04，2026-08-22）：`sealed` 与 deque 同锁。"队列是否仍
/// 接受新消息"与"终态空队列判定"必须在同一临界区内完成——否则
/// check-then-act 窗口里，worker 已决定结束而 `busy` 尚未落 false，前端
/// 还能入队一条永远不会被 claim 的消息。封口后 `try_push` 返回
/// [`PushOutcome::Sealed`]（调用方回退普通提交）；未 claim 的消息仍可
/// `recall_last` 退还编辑框。
#[derive(Clone, Default)]
pub(crate) struct SteeringQueue {
    pending: Arc<Mutex<SteeringState>>,
}

#[derive(Default)]
struct SteeringState {
    queue: VecDeque<crate::message::PendingMessage>,
    sealed: bool,
}

/// `try_push` 的结果：入队成功，或队列已封口（run 已判定终态）。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PushOutcome {
    Accepted,
    Sealed,
}

/// Run 执行期封口兜底：正常返回、错误返回和 panic unwind 都会 Drop。
/// terminal 分支仍应在发终态事件前主动 seal；本 guard 专门覆盖任何
/// 意外 unwind/未来新增 early-exit，保证队列生命周期不会漏出口。
pub(super) struct SteeringSealGuard(pub(super) SteeringQueue);

impl Drop for SteeringSealGuard {
    fn drop(&mut self) {
        self.0.seal();
    }
}

impl SteeringQueue {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// 前端入队：open 时入队并返回 `Accepted`；sealed 时绝不接受。
    pub(crate) fn try_push(&self, message: crate::message::PendingMessage) -> PushOutcome {
        match self.pending.lock() {
            Ok(mut state) if !state.sealed => {
                state.queue.push_back(message);
                PushOutcome::Accepted
            }
            _ => PushOutcome::Sealed,
        }
    }

    pub(crate) fn pop(&self) -> Option<crate::message::PendingMessage> {
        self.pending
            .lock()
            .ok()
            .and_then(|mut state| state.queue.pop_front())
    }

    /// 召回最后一条未 claim 的消息（LIFO；投递是 FIFO `pop`）。与
    /// worker 在模型请求边界的 drain 在同一把锁上竞争：claim 先到则
    /// 该消息已生效、不可召回（返回更晚的或 None）——召回永远不可能
    /// 撤回已被 claim 的消息（docs/todo/steering-visibility-recall.md
    /// INV-SV3）。封口不影响召回：终态后未 claim 的消息正应退还前端。
    /// MM-1A：返回完整 typed 消息（内容 + 客户端幂等键），前端可原样
    /// 重发（MM-I11 recall 语义）。
    pub(crate) fn recall_last(&self) -> Option<crate::message::PendingMessage> {
        self.pending
            .lock()
            .ok()
            .and_then(|mut state| state.queue.pop_back())
    }

    /// 终态判定（原子，W1-04）：已封口，或"队列空 → 当场封口"都放行
    /// 终态；仅"open 且非空"返回 false（还有消息待 claim，run 继续）。
    pub(crate) fn seal_if_empty(&self) -> bool {
        let Ok(mut state) = self.pending.lock() else {
            return true;
        };
        if state.sealed || state.queue.is_empty() {
            state.sealed = true;
            return true;
        }
        false
    }

    /// 无条件封口（fail/cancelled 等一切退出路径的兜底）：此后
    /// `try_push` 一律 `Sealed`。
    pub(crate) fn seal(&self) {
        if let Ok(mut state) = self.pending.lock() {
            state.sealed = true;
        }
    }

    pub(crate) fn is_sealed(&self) -> bool {
        self.pending.lock().is_ok_and(|state| state.sealed)
    }
}
