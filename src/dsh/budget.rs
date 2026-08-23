//! FIX-2/CA-02（2026-08-24 审计）：DSH 载体输入预算——每种 carrier 在
//! **分配/解析前**拥有明确的单项 + 累计预算（providers/mod.rs FP-02 的
//! `take(cap + 1)` 模式照搬到 DSH 域）。网络载体超限 → 有界错误（带
//! cap 值，不带原始内容），由调用方关闭连接；文件读取到 cap+1 即止
//! → fail-soft。DSH 宿主不可当可信内存调度器：自动探测会接入任何
//! 通过弱 shape 指纹的 loopback 服务。

use std::io::Read;

/// HTTP JSON body 上限（session.list 数百会话级约百 KB 量级，8 MiB
/// 余量充足）。
pub(crate) const HTTP_BODY_CAP: usize = 8 * 1024 * 1024;
/// WS 完整 message 累计上限——与单帧上限同值（INV-F2-2：分片累计 =
/// 单帧同界）。
pub(crate) const WS_MESSAGE_CAP: usize = 16 * 1024 * 1024;
/// spawn 就绪期 stdout 总量上限。
pub(crate) const READY_TOTAL_CAP: usize = 64 * 1024;
/// 就绪期单行上限（就绪行实际 < 60 字节）。
pub(crate) const READY_LINE_CAP: usize = 4 * 1024;
/// 本地 DSH 数据文件（workspace.json / session_projcache.json）上限。
pub(crate) const STORAGE_FILE_CAP: usize = 8 * 1024 * 1024;

/// 有界字符串读取：读到 cap+1 即止；超限返回带 cap 值的错误（不含
/// 原始内容）。`what` 用于错误文案（如 "the response body"）。
pub(crate) fn read_string_capped(
    reader: impl Read,
    cap: usize,
    what: &str,
) -> Result<String, String> {
    let mut text = String::new();
    reader
        .take(cap as u64 + 1)
        .read_to_string(&mut text)
        .map_err(|error| format!("cannot read {what}: {error}"))?;
    if text.len() > cap {
        return Err(format!("{what} exceeds the {cap}-byte carrier cap"));
    }
    Ok(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// R2-1 判别腿：读取量 ≤ cap+1——载体不是内存调度器。
    struct CountingInfinite {
        read: std::rc::Rc<std::cell::Cell<usize>>,
    }
    impl Read for CountingInfinite {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            buf.fill(b'a');
            self.read.set(self.read.get() + buf.len());
            Ok(buf.len())
        }
    }

    #[test]
    fn capped_read_stops_at_cap_plus_one_bytes() {
        let counter = std::rc::Rc::new(std::cell::Cell::new(0));
        let reader = CountingInfinite {
            read: std::rc::Rc::clone(&counter),
        };
        let error = read_string_capped(reader, 1024, "the test body").expect_err("over cap");
        assert!(error.contains("exceeds"), "{error}");
        assert!(
            counter.get() <= 1024 + 1,
            "the reader must stop at cap+1, read {}",
            counter.get()
        );

        let counter = std::rc::Rc::new(std::cell::Cell::new(0));
        let reader = CountingInfinite {
            read: std::rc::Rc::clone(&counter),
        };
        // 恰在帽内：完整读取成功。
        let text = read_string_capped(reader.take(512), 1024, "the test body").expect("within cap");
        assert_eq!(text.len(), 512);
    }
}
