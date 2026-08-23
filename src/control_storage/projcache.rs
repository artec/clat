//! `storages/session_projcache.json`：会话列表投影缓存（MP-1 §4.1，
//! 文件名与 DSH 实机一致——研究档 §5；设计文档的连字符为笔误，已在
//! 实现修正记录中记档）。
//!
//! **纯缓存**：撕裂/缺失/版本错位一律静默重建（区别于事实类文件的
//! 抢救路径与版本门——缓存没有用户数据），事实源永远是会话日志。

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::Path;

use super::json_file::{self, Loaded, UnitTag};
use crate::session::use_cases::SessionSummary;

pub(crate) const PROJCACHE_FILE_NAME: &str = "session_projcache.json";
pub(crate) const PROJCACHE_UNIT: (&str, u64) = ("session_projcache", 1);

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct ProjcacheRow {
    #[serde(rename = "workspaceId")]
    pub workspace_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(rename = "createdAtMs")]
    pub created_at_ms: i64,
    #[serde(rename = "lastActivityMs")]
    pub last_activity_ms: i64,
    #[serde(rename = "messageCount")]
    pub message_count: u64,
    pub turns: u64,
}

impl ProjcacheRow {
    pub(crate) fn from_summary(workspace_id: &str, summary: &SessionSummary) -> Self {
        Self {
            workspace_id: Some(workspace_id.to_owned()),
            title: summary.title.clone(),
            created_at_ms: summary.created_at_ms,
            last_activity_ms: summary.last_activity_ms,
            message_count: summary.message_count,
            turns: summary.turns,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct ProjcacheFile {
    pub unit: UnitTag,
    #[serde(default)]
    pub sessions: BTreeMap<String, ProjcacheRow>,
}

/// 加载：任何异常形态都等价于「无缓存」（重建路径的同义词）。
pub(crate) fn load(storages_dir: &cap_std::fs::Dir, storages_root: &Path) -> ProjcacheFile {
    match json_file::load::<ProjcacheFile>(
        storages_dir,
        storages_root,
        PROJCACHE_FILE_NAME,
        PROJCACHE_UNIT,
    ) {
        Ok(Loaded::Intact(file)) => file,
        _ => ProjcacheFile {
            unit: UnitTag::new(PROJCACHE_UNIT.0, PROJCACHE_UNIT.1),
            sessions: BTreeMap::new(),
        },
    }
}

/// 全量替换某工作区的行（其它工作区的行保留；本工作区不再存在的
/// 会话行随之消失）。
pub(crate) fn replace_workspace_rows(
    file: &mut ProjcacheFile,
    workspace_id: &str,
    rows: Vec<(String, ProjcacheRow)>,
) {
    let workspace_id = workspace_id.to_owned();
    file.sessions
        .retain(|_, row| row.workspace_id.as_deref() != Some(workspace_id.as_str()));
    for (session_id, row) in rows {
        file.sessions.insert(session_id, row);
    }
}

pub(crate) fn save(
    storages_dir: &cap_std::fs::Dir,
    storages_root: &Path,
    file: &ProjcacheFile,
) -> Result<(), String> {
    json_file::write(storages_dir, storages_root, PROJCACHE_FILE_NAME, file)
}
