use std::io::Write as _;
use std::sync::mpsc;
use std::time::Duration;

/// 进程退出时后台线程的有界等待上限。
pub(crate) const EXIT_JOIN_GRACE: Duration = Duration::from_secs(2);

/// `start_run` 对 MCP 后台启动的有界等待上限（INV-M3 的例外通道）：
/// 覆盖 npx 冷启动 + 握手（10s 超时）的现实组合；超时后以现状冻结，
/// 迟到的 server 由状态面板报告、下一次 run 可见。
pub(super) const MCP_STARTUP_RUN_WAIT: Duration = Duration::from_secs(20);

/// 取消后至多等待 `grace`；超时则放弃该线程（stderr 留一条记录），
/// 返回 Ok——放弃是退出路径的正当结果，不是失败。快速退出的线程
/// 正常 join，panic 映射为错误字符串（保持旧 shutdown 的语义）。
pub(crate) fn join_with_grace(
    handle: std::thread::JoinHandle<()>,
    grace: Duration,
    who: &str,
) -> Result<(), String> {
    let (done, signal) = mpsc::channel::<Result<(), String>>();
    let watcher = std::thread::Builder::new()
        .name("clat-exit-join".into())
        .spawn(move || {
            let outcome = handle.join().map_err(|_| "worker panicked".to_owned());
            let _ = done.send(outcome);
        })
        .map_err(|error| format!("spawn exit-join watcher: {error}"))?;
    match signal.recv_timeout(grace) {
        Ok(outcome) => {
            // 快路径：watcher send 后立刻结束，join 它只是回收。
            let _ = watcher.join();
            outcome
        }
        Err(mpsc::RecvTimeoutError::Timeout) => {
            // 放弃：watcher 与目标线程一起留给进程退出回收。
            let _ = writeln!(
                std::io::stderr(),
                "clat: {who} still busy at exit; abandoning after {grace:?}"
            );
            Ok(())
        }
        // watcher 在 send 前消失才会走到这里（防御性）：视为已完成。
        Err(mpsc::RecvTimeoutError::Disconnected) => Ok(()),
    }
}
