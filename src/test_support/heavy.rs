//! 大户测试的全局并发闸门与统一挂死边界。
//!
//! libtest 只限制测试线程数，管不到测试内部起的子进程和编译器：
//! 一个在测试里穷举组合、连开多个 host 或反复起进程的"大户"能吃满
//! 整台 runner，把并行邻居的等待预算挤过线（2026-11 C 族六案，
//! 根因见登记簿 G4）。大户测试在开头拿 `heavy()` 守卫，全局同时
//! 至多一个大户在跑；轻量测试不受影响，照常并行。
//!
//! 规则（docs/testing.md 同步）：隔离耗时超过 ~8s 或要起 rustc/
//! wasm 编译、连开多个 host 的测试，必须拿闸门。

use std::sync::{Mutex, MutexGuard};
use std::time::Duration;

static HEAVY: Mutex<()> = Mutex::new(());

/// 统一的测试等待挂死边界（recv/subprocess 预算一律用它，不再散落魔数）。
/// 它只兜底"永远不来"，性能回归靠度量抓，不靠超时。
pub const HANG: Duration = Duration::from_secs(60);

/// RAII 守卫：持有期间独占全局大户槽，drop 释放。
pub struct HeavyGuard {
    _slot: MutexGuard<'static, ()>,
}

/// 进入大户临界区。panic 后按锁中毒恢复（into_inner），不卡死后续测试。
pub fn heavy() -> HeavyGuard {
    HeavyGuard {
        _slot: HEAVY
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()),
    }
}
