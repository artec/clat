//! spend implementation behind the stable model contract.

/// 花费护栏缺省硬顶（定案：10M——误报在 ~p99.9 之外，漏报封顶贵模型
/// 几十美元量级；dogfood 校准见 docs/todo/open-worklist.md B1）。
pub const RUN_TOKEN_BUDGET_DEFAULT: u64 = 10_000_000;

/// B1 花费护栏的共享账本：**唯一仪表**。recorder 在 assistant/message
/// 落账点按 INV-S6 口径（主循环 usage + 插件采样归并）记账；run.rs
/// 的每请求检查点与 50%/90% 预警都读它——预警数字与硬停终止文案
/// 同源，重采样 run 不再出现两仪表矛盾。
///
/// FP-01（2026-08-22 审计）：计量来源升级为**预留-对账**双记账（对齐
/// sampling bridge 的 W1-03 模型）——provider 自报 usage 不再是唯一
/// 计量来源。主循环计费是累计制（每轮 input≈全上下文重新计费），
/// 「每请求预留 input 估算 + output_limit、usage 到达后以实际值替换
/// 预留」与真实账单天然同构，不双算：
/// - run.rs 在每次模型请求前 [`Self::reserve`]（保守估算）；
/// - provider 回 usage → [`Self::reconcile`]（实际替换预留）；
/// - usage=None / 请求失败 / 取消 → [`Self::commit_pending`]（预留
///   兑现为已耗——上游可能已经计费，不得按 0 释放）；
/// - retry 的每次 attempt 经 [`Self::commit_retry_attempt`] 计入
///   （失败 attempt 已烧掉的 input 兑现，预留保留给同请求的下一次
///   attempt；最终成功 attempt 由 reconcile 替换）；
/// - [`Self::charge`] 保留给无预留路径（插件采样 aux 归并）。
///
/// 主循环串行（至多一条在途预留）；账本变更全部发生在 run worker
/// 线程，原子量只为跨线程读取（检查点/预警/测试）。
pub struct RunSpendLedger {
    /// FIX-1/CA-01：无符号域。曾用 i64 表达 token，`u64 as i64` 把对端
    /// 大报数变负、再被读取端裁 0 —— 反向清空已耗、护栏失效。
    used: std::sync::atomic::AtomicU64,
    pending: std::sync::atomic::AtomicU64,
    /// 护栏硬顶；None = 关闭（预警也不发）。
    pub cap: Option<u64>,
}

impl RunSpendLedger {
    pub fn new(cap: Option<u64>) -> Self {
        Self {
            used: std::sync::atomic::AtomicU64::new(0),
            pending: std::sync::atomic::AtomicU64::new(0),
            cap,
        }
    }

    /// FIX-1/CA-01：无符号饱和累加。不用 `fetch_add`——它在溢出处
    /// wrap（release 回绕成更小的已耗量）；账本只能单调不减、触顶
    /// 饱和，对端异常数值 fail-closed。
    fn saturating_add_used(&self, tokens: u64) {
        let mut current = self.used.load(std::sync::atomic::Ordering::Relaxed);
        loop {
            let next = current.saturating_add(tokens);
            match self.used.compare_exchange_weak(
                current,
                next,
                std::sync::atomic::Ordering::Relaxed,
                std::sync::atomic::Ordering::Relaxed,
            ) {
                Ok(_) => return,
                Err(observed) => current = observed,
            }
        }
    }

    /// FP-01：请求前预留（保守 = input 估算 + output_limit）。主循环
    /// 串行，覆盖式存储——被覆盖的旧值若未对账，其保守成本已由
    /// 消耗视图（`used + pending`）承担过，不丢失。
    pub fn reserve(&self, tokens: u64) {
        self.pending
            .store(tokens, std::sync::atomic::Ordering::Relaxed);
    }

    /// FP-01：usage 到达——实际值替换预留（先清后加，**不双算**）；
    /// 无在途预留时（如 recorder 直驱事件）等同 [`Self::charge`]。
    pub fn reconcile(&self, actual_tokens: u64) {
        self.pending.store(0, std::sync::atomic::Ordering::Relaxed);
        self.saturating_add_used(actual_tokens);
    }

    /// FP-01：请求完成但无 usage（或失败/取消收尾）——预留兑现为已耗
    /// （上游可能已计费），兑现后清空（该请求结束）。
    pub fn commit_pending(&self) {
        let pending = self.pending.swap(0, std::sync::atomic::Ordering::Relaxed);
        if pending > 0 {
            self.saturating_add_used(pending);
        }
    }

    /// FP-01：一次 retry attempt 失败——该 attempt 已烧掉的保守成本
    /// 兑现为已耗，**预留保留**给同请求的下一次 attempt（同一请求
    /// 会再次计费全量 input）。
    pub fn commit_retry_attempt(&self) {
        let pending = self.pending.load(std::sync::atomic::Ordering::Relaxed);
        if pending > 0 {
            self.saturating_add_used(pending);
        }
    }

    /// 落账充值（input+output 口径，aux 插件采样归并路径）。
    pub fn charge(&self, tokens: u64) {
        self.saturating_add_used(tokens);
    }

    /// 消耗视图：已耗 + 在途预留——未对账预留视同已耗（上游可能已
    /// 计费）；检查点、预警与教学文案都用它。
    pub fn used(&self) -> u64 {
        self.used
            .load(std::sync::atomic::Ordering::Relaxed)
            .saturating_add(self.pending.load(std::sync::atomic::Ordering::Relaxed))
    }

    /// 已确认落账值（不含在途预留）——诊断/测试用。
    pub fn committed(&self) -> u64 {
        self.used.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// 硬停判据：已启用且累计越过硬顶。
    pub fn exceeds_cap(&self) -> bool {
        self.cap.is_some_and(|cap| self.used() >= cap)
    }
}
