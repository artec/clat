//! clat dsh 客户端的「最后打开会话」记忆（2026-08-24 负责人拍板 A：
//! 客户端自记——web 端 localStorage `dsh.sessions.current` 的 CLAT
//! 同款机制；宿主侧不存在「最后打开」状态：updatedAt 只认人类 prompt，
//! attach 对已是成员的会话是 no-op，事件表无「打开」类型）。存的是
//! 会话 id 引用，不是会话形状数据（模块纪律：会话事实独占 DSH 会话
//! 日志）。装饰性记忆：读写皆 fail-soft——缺席/损坏回落 None，调用
//! 方回落宿主列表头（最近被提问/创建的会话）。

use std::path::{Path, PathBuf};

/// 记忆文件（`~/.clat/dsh-last-session`，单行会话 id；无 home → 空
/// 路径，读写为 no-op）。
pub(crate) fn last_session_path() -> PathBuf {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(|home| PathBuf::from(home).join(".clat").join("dsh-last-session"))
        .unwrap_or_default()
}

/// 读回最后打开的会话 id（缺席/空/损坏 → None）。
pub(crate) fn read_last_session_at(path: &Path) -> Option<String> {
    std::fs::read_to_string(path)
        .ok()
        .map(|content| content.trim().to_owned())
        .filter(|id| !id.is_empty())
}

/// 记住最后打开的会话（fail-soft：写失败 = 下次启动回落列表头）。
pub(crate) fn remember_last_session_at(path: &Path, session: &str) {
    if path.as_os_str().is_empty() {
        return;
    }
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::write(path, session);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_path(tag: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "clat-dsh-memo-{tag}-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ))
    }

    /// 拍板 A 判别：写读往返；缺席/空白/空路径的 fail-soft 腿。
    #[test]
    fn last_session_round_trips_and_fails_soft() {
        let path = temp_path("rt");
        assert_eq!(read_last_session_at(&path), None, "absent → None");
        remember_last_session_at(&path, "session-aite");
        assert_eq!(read_last_session_at(&path), Some("session-aite".to_owned()));
        // 覆写（最后一次切换获胜）。
        remember_last_session_at(&path, "session-clat");
        assert_eq!(read_last_session_at(&path), Some("session-clat".to_owned()));
        // 空白文件 → None（损坏记忆不复活）。
        std::fs::write(&path, "   \n").unwrap();
        assert_eq!(read_last_session_at(&path), None);
        // 空路径 no-op（无 home 的环境）。
        remember_last_session_at(Path::new(""), "x");
        assert_eq!(read_last_session_at(Path::new("")), None);
        let _ = std::fs::remove_file(path);
    }
}
