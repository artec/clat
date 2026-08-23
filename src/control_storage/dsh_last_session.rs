//! clat dsh 客户端的「最后打开会话」记忆（2026-08-24 负责人拍板 A：
//! 客户端自记——web 端 localStorage `dsh.sessions.current` 的 CLAT
//! 同款机制；宿主侧不存在「最后打开」状态：updatedAt 只认人类 prompt，
//! attach 对已是成员的会话是 no-op，事件表无「打开」类型）。存的是
//! 会话 id 引用，不是会话形状数据（模块纪律：会话事实独占 DSH 会话
//! 日志）。装饰性记忆：读写皆 fail-soft——缺席/损坏回落 None，调用
//! 方回落宿主列表头（最近被提问/创建的会话）。

use std::io::Read as _;
use std::path::{Path, PathBuf};

/// 记忆文件与会话 id 的单侧字节帽（FIX-4/CA-07）：会话 id 是单行
/// opaque 标识，4 KiB 远超任何真实形状且不擅自假定 UUID/正则形状；
/// 超帽 = 损坏/异常，读写双双 fail-soft。
const SESSION_ID_CAP: usize = 4 * 1024;

/// 记忆文件（`~/.clat/dsh-last-session`，单行会话 id；无 home → 空
/// 路径，读写为 no-op）。
pub(crate) fn last_session_path() -> PathBuf {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(|home| PathBuf::from(home).join(".clat").join("dsh-last-session"))
        .unwrap_or_default()
}

/// 读回最后打开的会话 id（缺席/空/损坏/超帽/symlink → None）。
/// FIX-4/CA-07：symlink 拒绝（不跟随）+ 有界读取（cap+1 即止）。
pub(crate) fn read_last_session_at(path: &Path) -> Option<String> {
    let name = path.file_name()?;
    let parent = path.parent()?;
    let dir = cap_std::fs::Dir::open_ambient_dir(parent, cap_std::ambient_authority()).ok()?;
    let mut options = cap_std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use cap_std::fs::OpenOptionsExt as _;
        options.custom_flags(libc::O_NOFOLLOW);
    }
    #[cfg(windows)]
    {
        use cap_std::fs::OpenOptionsExt as _;
        const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
        options.custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
    }
    let file = dir.open_with(name, &options).ok()?;
    let metadata = file.metadata().ok()?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return None;
    }
    let mut text = String::new();
    file.take(SESSION_ID_CAP as u64 + 1)
        .read_to_string(&mut text)
        .ok()?;
    if text.len() > SESSION_ID_CAP {
        return None;
    }
    let trimmed = text.trim().to_owned();
    (!trimmed.is_empty()).then_some(trimmed)
}

/// 记住最后打开的会话（fail-soft：写失败 = 下次启动回落列表头）。
/// FIX-4/CA-07：写入复用 `control_storage::json_file::write_text_atomic`
///（选型记档：不挂接 `ControlStorage`——dsh 模式不占项目 storage-root
/// lease，而该原语本就独立可用），继承 0600 temp+fsync+rename+父目录
/// fsync+前后双 symlink 拒绝的完整纪律；原子替换下并发读者只见旧
/// 完整值或新完整值。任何失败（含路径围栏）只静默丢偏好，绝不动
/// 被链接的外部目标。
pub(crate) fn remember_last_session_at(path: &Path, session: &str) {
    if path.as_os_str().is_empty() || session.len() > SESSION_ID_CAP {
        return;
    }
    let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
        return;
    };
    let Some(parent) = path.parent() else {
        return;
    };
    if std::fs::create_dir_all(parent).is_err() {
        return;
    }
    let Ok(dir) = cap_std::fs::Dir::open_ambient_dir(parent, cap_std::ambient_authority()) else {
        return;
    };
    let _ = super::json_file::write_text_atomic(&dir, parent, name, session);
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

    /// FIX-4/CA-07（2026-08-24 审计，pre-fix 红）：路径围栏——预置
    /// symlink 指向 victim 时，remember 不得跟随 symlink 截断 victim
    /// （pre-fix：`std::fs::write` 跟随 → victim 被截成会话 id → 红）；
    /// 读取同样拒绝 symlink。
    #[cfg(unix)]
    #[test]
    fn a_symlinked_memory_file_never_touches_its_victim() {
        let dir = std::env::temp_dir().join(format!(
            "clat-dsh-memo-symlink-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let victim = dir.join("victim.txt");
        std::fs::write(&victim, "PRECIOUS-BYTES").unwrap();
        let memory = dir.join("dsh-last-session");
        std::os::unix::fs::symlink(&victim, &memory).unwrap();

        remember_last_session_at(&memory, "session-evil");
        assert_eq!(
            std::fs::read_to_string(&victim).unwrap(),
            "PRECIOUS-BYTES",
            "the write must never follow the symlink to the victim"
        );
        assert_eq!(
            read_last_session_at(&memory),
            None,
            "the read must reject the symlink too"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    /// FIX-4/CA-07（pre-fix 红）：读取有界（4 KiB）——超帽记忆文件
    /// 只读 cap+1 后回落 None，不整体物化。
    #[test]
    fn an_oversized_memory_file_reads_at_most_cap_plus_one() {
        let path = temp_path("huge");
        let oversized = "s".repeat(8 * 1024);
        std::fs::write(&path, &oversized).unwrap();
        assert_eq!(
            read_last_session_at(&path),
            None,
            "an over-cap memory file is fail-soft None"
        );
        let _ = std::fs::remove_file(path);
    }

    /// FIX-4/CA-07（pre-fix 红）：写入必须是 rename 原子替换——把旧
    /// 记忆硬链接到旁路后覆写，硬链接内容必须保持**旧值**（原地写
    /// 会穿透 inode，pre-fix：硬链接内容变新值 → 红）。并发读者由此
    /// 只见旧完整值或新完整值，永不见撕裂/截断。
    #[cfg(unix)]
    #[test]
    fn writes_replace_atomically_old_bytes_stay_intact_elsewhere() {
        let dir = std::env::temp_dir().join(format!(
            "clat-dsh-memo-hardlink-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let memory = dir.join("dsh-last-session");
        remember_last_session_at(&memory, "session-old");
        let alias = dir.join("alias");
        std::fs::hard_link(&memory, &alias).unwrap();

        remember_last_session_at(&memory, "session-new");
        assert_eq!(
            std::fs::read_to_string(&alias).unwrap(),
            "session-old",
            "atomic replace swaps the directory entry; the old inode is untouched"
        );
        assert_eq!(
            read_last_session_at(&memory),
            Some("session-new".to_owned()),
            "the new value is fully visible at the memory path"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    /// FIX-4/CA-07：超长会话 id（> 4 KiB）不落盘（opaque id 有界，
    /// 不擅自假定形状）。
    #[test]
    fn an_oversized_session_id_is_rejected_fail_soft() {
        let path = temp_path("longid");
        let huge = "i".repeat(8 * 1024);
        remember_last_session_at(&path, &huge);
        assert!(!path.exists(), "an over-cap session id must not be written");
        let _ = std::fs::remove_file(path);
    }
}
