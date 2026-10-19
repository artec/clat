/// 冷重开轮询（E2 同族，E4 于 CI 复现后升级为共享助手）：close(2)
/// 释放 flock 在高负载下有毫秒级内核可见性窗口（root_lease.rs 诊断
/// 实测 <10ms 后即可再取）——同根 close 后立即重开可能被误拒"另一
/// CLAT 进程持有"。只对这一专属错误重试，其余错误立即 panic 透传；
/// 预算耗尽也 panic——真实的他人持有不会被轮询掩盖。
pub fn reopen_after_close<T, E, F>(mut open: F) -> T
where
    F: FnMut() -> Result<T, E>,
    E: std::fmt::Display,
{
    const HELD: &str = "another CLAT process holds this storage root";
    for _ in 0..100 {
        match open() {
            Ok(value) => return value,
            Err(error) if error.to_string().contains(HELD) => {}
            Err(error) => panic!("cold reopen failed: {error}"),
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    panic!("storage-root lease stayed invisible for 1s after close");
}
