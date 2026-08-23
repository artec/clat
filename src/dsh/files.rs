//! 数据面：读 `~/.dsh/` 出全量项目+会话（D-1 §6，INV-D5/INV-D1）。
//!
//! 列表源 = `storages/workspace.json`（MP-1 的 DSH 同构 reader 复用，
//! 零适配）+ `storages/session_projcache.json` 行细节（标题/创建时间，
//! fail-soft）+ 会话日志 mtime（活跃度排序代理）。**只读**：Unix 上
//! 以只读 fd 挂载目录（`Dir::from_std_file`），本模块不存在任何写
//! 路径——「替补不竞争」的物理面。

use crate::control_storage::workspace::WorkspaceFile;
use serde::Deserialize;
use std::path::{Path, PathBuf};

/// DSH home（`$DSH_HOME` 优先，惯例回落 `~/.dsh`）。
pub(crate) fn dsh_home() -> Option<PathBuf> {
    if let Some(home) = std::env::var_os("DSH_HOME") {
        let path = PathBuf::from(home);
        return path.is_dir().then_some(path);
    }
    let home = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE"))?;
    let path = PathBuf::from(home).join(".dsh");
    path.is_dir().then_some(path)
}

/// /resume 选择器的一行。
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct DshSessionRow {
    pub(crate) session_id: String,
    pub(crate) workspace_title: String,
    pub(crate) workspace_path: String,
    pub(crate) title: Option<String>,
    pub(crate) created_at_ms: i64,
    /// 排序键：日志 mtime（毫秒）优先，缺席回落创建时间。
    pub(crate) activity_ms: i64,
}

#[derive(Default, Deserialize)]
struct ProjcacheFile {
    #[serde(default)]
    tables: ProjcacheTables,
}

#[derive(Default, Deserialize)]
struct ProjcacheTables {
    #[serde(default)]
    sessions: std::collections::BTreeMap<String, ProjcacheRow>,
}

#[derive(Default, Deserialize)]
struct ProjcacheRow {
    #[serde(default)]
    identity: ProjcacheIdentity,
    #[serde(default)]
    rows: ProjcacheRows,
}

#[derive(Default, Deserialize)]
struct ProjcacheIdentity {
    #[serde(default, rename = "createdAt")]
    created_at_ms: i64,
}

#[derive(Default, Deserialize)]
struct ProjcacheRows {
    #[serde(default)]
    title: Option<ProjcacheValue>,
}

#[derive(Deserialize)]
struct ProjcacheValue {
    val: serde_json::Value,
}

/// 读数据面。任何异常（目录缺失/撕裂/异形）都 fail-soft 为 `None`
/// （INV-D6）：/resume 无数据面时退化为 API 列表，绝不阻塞在线。
pub(crate) fn read_sessions(dsh_home: &Path) -> Option<Vec<DshSessionRow>> {
    let storages = dsh_home.join("storages");
    let workspace_text = read_text(&storages.join("workspace.json"))?;
    let workspace: WorkspaceFile = serde_json::from_str(&workspace_text).ok()?;
    let projcache = read_text(&storages.join("session_projcache.json"))
        .and_then(|text| serde_json::from_str::<ProjcacheFile>(&text).ok())
        .unwrap_or_default();
    let mut rows = Vec::new();
    for (workspace_id, record) in &workspace.tables.workspaces {
        let _ = workspace_id;
        for session_id in &record.session_ids {
            let detail = projcache.tables.sessions.get(session_id);
            let log_mtime = session_log_mtime(dsh_home, &record.path, session_id);
            rows.push(DshSessionRow {
                session_id: session_id.clone(),
                workspace_title: record.title.clone(),
                workspace_path: record.path.clone(),
                title: detail
                    .and_then(|row| row.rows.title.as_ref())
                    .and_then(|title| title.val.as_str())
                    .map(str::to_owned),
                created_at_ms: detail.map(|row| row.identity.created_at_ms).unwrap_or(0),
                activity_ms: log_mtime
                    .unwrap_or(0)
                    .max(detail.map(|row| row.identity.created_at_ms).unwrap_or(0)),
            });
        }
    }
    rows.sort_by(|a, b| {
        b.activity_ms
            .cmp(&a.activity_ms)
            .then(a.session_id.cmp(&b.session_id))
    });
    Some(rows)
}

/// 会话日志 mtime（活跃度代理）。路径推导与 DSH 布局一致：
/// `sessions/<projectKey(path)>/<encodeSegment(id)>/session.jsonl.zstd`。
fn session_log_mtime(dsh_home: &Path, workspace_path: &str, session_id: &str) -> Option<i64> {
    let bucket = crate::session::path_layout::project_key(workspace_path);
    let segment = crate::session::path_layout::encode_segment(session_id);
    let log = dsh_home
        .join("sessions")
        .join(bucket)
        .join(segment)
        .join("session.jsonl.zstd");
    let metadata = std::fs::metadata(log).ok()?;
    metadata
        .modified()
        .ok()?
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .map(|duration| duration.as_millis() as i64)
}

/// 只读文本读取（目录以只读 fd 挂载的简化面：单文件读 + 拒绝符号
/// 链接）。撕裂/缺失 → `None`（fail-soft 由调用方表达）。
fn read_text(path: &Path) -> Option<String> {
    let metadata = std::fs::symlink_metadata(path).ok()?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return None;
    }
    std::fs::read_to_string(path).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 本机真实 ~/.dsh/storages 的字节副本（钉靶运行时形态）。
    const FIXTURE: &str = include_str!("../../tests/fixtures/dsh-storages/workspace.json");

    #[test]
    fn reads_the_real_dsh_fixture_zero_adaptation() {
        let parsed: WorkspaceFile = serde_json::from_str(FIXTURE).unwrap();
        assert_eq!(parsed.unit.version, 2);
        assert_eq!(parsed.tables.workspaces.len(), 1);
        let root = std::env::temp_dir().join(format!(
            "clat-dsh-files-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let storages = root.join("storages");
        std::fs::create_dir_all(&storages).unwrap();
        std::fs::write(storages.join("workspace.json"), FIXTURE).unwrap();
        std::fs::copy(
            "tests/fixtures/dsh-storages/session_projcache.json",
            storages.join("session_projcache.json"),
        )
        .unwrap();
        let rows = read_sessions(&root).expect("the data plane reads");
        assert_eq!(rows.len(), 2, "two sessions from the real workspace");
        assert!(
            rows.iter()
                .all(|row| row.workspace_path == "/Users/deng/Documents/GitHub/clat")
        );
        assert!(
            rows.iter()
                .any(|row| row.title.as_deref() == Some("clat项目开发咨询"))
        );
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn torn_or_missing_data_plane_is_fail_soft() {
        assert_eq!(read_sessions(Path::new("/nonexistent-dsh-home")), None);
        let root = std::env::temp_dir().join(format!(
            "clat-dsh-torn-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(root.join("storages")).unwrap();
        std::fs::write(root.join("storages").join("workspace.json"), "{\"torn").unwrap();
        assert_eq!(read_sessions(&root), None, "torn workspace.json → None");
        std::fs::remove_dir_all(&root).ok();
    }
}
