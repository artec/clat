//! cancel implementation behind the stable model contract.
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::time::{Duration, Instant};

/// A cooperative cancellation signal shared between a client and a run.
///
/// Clones observe the same underlying flag, so a client (for example the TUI
/// on `Esc`) can set it while the run and its providers poll it. Cancellation
/// is cooperative: the run checks between turns and tool calls, and provider
/// adapters check between stream chunks. A provider that never polls the
/// token simply ignores it.
#[derive(Clone, Debug, Default)]
pub struct CancelToken {
    cancelled: Arc<AtomicBool>,
    /// 内部短任务可附带绝对 deadline。普通 Run 为 None；provider 用剩余
    /// 时间配置请求级 HTTP global/header timeout，使 deadline 覆盖 send。
    deadline: Option<Instant>,
    /// 派生 token（[`Self::child_with_deadline`]）观察完整父 token：父的
    /// 显式取消、祖先取消或更早 deadline 都等价于子取消。
    parent: Option<Arc<CancelToken>>,
}

impl CancelToken {
    pub fn new() -> Self {
        Self::default()
    }

    /// 派生一个带绝对 deadline 的子 token：父取消或 deadline 到期都
    /// 视为取消。用途（自动标题）：worker 的取消令牌本身无 deadline，
    /// 单次请求的 connect/响应头阶段不会被合作式轮询打断——派生后
    /// provider 的 `remaining()` 有值，请求级 timeout 全阶段有界。若
    /// 父/祖先已有更早 deadline，`remaining()` 继承其中最短者。
    pub(crate) fn child_with_deadline(&self, deadline: Instant) -> CancelToken {
        CancelToken {
            cancelled: Arc::new(AtomicBool::new(false)),
            deadline: Some(deadline),
            parent: Some(Arc::new(self.clone())),
        }
    }

    pub(crate) fn with_deadline(deadline: Instant) -> Self {
        Self {
            cancelled: Arc::new(AtomicBool::new(false)),
            deadline: Some(deadline),
            parent: None,
        }
    }

    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::Relaxed);
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Relaxed)
            || self
                .parent
                .as_ref()
                .is_some_and(|parent| parent.is_cancelled())
            || self
                .deadline
                .is_some_and(|deadline| Instant::now() >= deadline)
    }

    pub(crate) fn remaining(&self) -> Option<Duration> {
        let own = self
            .deadline
            .map(|deadline| deadline.saturating_duration_since(Instant::now()));
        let parent = self.parent.as_ref().and_then(|parent| parent.remaining());
        match (own, parent) {
            (Some(own), Some(parent)) => Some(own.min(parent)),
            (Some(remaining), None) | (None, Some(remaining)) => Some(remaining),
            (None, None) => None,
        }
    }
}
