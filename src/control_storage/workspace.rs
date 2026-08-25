//! `storages/workspace.json`：工作区注册表（MP-1 §4.2）——DSH 同构
//! schema（钉靶 `b150a551b8`，unit{workspace,2}/global/tables），读真实
//! DSH 文件零适配（serde 容忍未知字段——上游演进面）。
//!
//! 两处记档的 CLAT 扩展（设计文档「实现修正记录」）：
//! - 记录内 `activeSessionId`：每工作区当前会话指针（负责人拍板
//!   2026-08-23——多项目交替使用时 TUI 重开各自恢复）；
//! - `global.activeWorkspaceId`/`global.activeSessionId`：恢复现场。
//!
//! `sessionIds` 是**投影**：目录（事实源）永远赢，修复全程保序
//! （INV-MP5——数组顺序是显示顺序的事实，修复不得重排）。`tables`
//! 的 path/title/时间戳无处派生，是**事实**：文件撕裂走抢救路径
//! （json_file 纪律），绝不静默重建。

use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::path::Path;

use super::json_file::{self, Loaded, UnitTag};
use super::timestamp;
use crate::session::path_layout;

pub(crate) const WORKSPACE_FILE_NAME: &str = "workspace.json";
pub(crate) const WORKSPACE_UNIT: (&str, u64) = ("workspace", 2);

/// DSH 五字段 + CLAT 扩展 `activeSessionId`（驼峰对齐 DSH）。
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub(crate) struct WorkspaceRecord {
    /// 创建时盖章的 realpath 规范形（DSH canon 语义）。
    pub path: String,
    pub title: String,
    /// 有序所有权账：数组顺序即显示顺序（投影，可对账修复）。
    #[serde(default, rename = "sessionIds")]
    pub session_ids: Vec<String>,
    #[serde(rename = "createdAt")]
    pub created_at: String,
    #[serde(rename = "updatedAt")]
    pub updated_at: String,
    /// CLAT 扩展：本工作区的当前会话（null = Fresh）。DSH zod 读取端
    /// 会剥除未知字段，互不影响（我们也从不写 DSH 的文件）。
    #[serde(
        default,
        rename = "activeSessionId",
        skip_serializing_if = "Option::is_none"
    )]
    pub active_session_id: Option<String>,
}

/// DSH global 状态 + CLAT 恢复现场扩展。
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub(crate) struct GlobalState {
    #[serde(default)]
    pub initialized: bool,
    #[serde(default, rename = "workspaceIds")]
    pub workspace_ids: Vec<String>,
    #[serde(default, rename = "archivedSessionIds")]
    pub archived_session_ids: Vec<String>,
    #[serde(
        default,
        rename = "activeWorkspaceId",
        skip_serializing_if = "Option::is_none"
    )]
    pub active_workspace_id: Option<String>,
    #[serde(
        default,
        rename = "activeSessionId",
        skip_serializing_if = "Option::is_none"
    )]
    pub active_session_id: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub(crate) struct Tables {
    #[serde(default)]
    pub(crate) workspaces: BTreeMap<String, WorkspaceRecord>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct WorkspaceFile {
    #[serde(default = "default_unit")]
    pub unit: UnitTag,
    #[serde(default)]
    pub global: GlobalState,
    #[serde(default)]
    pub tables: Tables,
}

fn default_unit() -> UnitTag {
    UnitTag::new(WORKSPACE_UNIT.0, WORKSPACE_UNIT.1)
}

/// 内存注册表 + 全部域操作。写入由 `ControlStorage` 负责（同一把互斥
/// 下读-改-写 + 落盘——§4.6 的单写者纪律）。
#[derive(Clone, Debug)]
pub(crate) struct WorkspaceRegistry {
    file: WorkspaceFile,
}

/// `enter` 的结果：命中与否 + `global.active*` 是否因此变动（未变则
/// 调用方不落盘）。
pub(crate) struct EnterResult {
    pub(crate) workspace: Option<(String, WorkspaceRecord)>,
    pub(crate) changed: bool,
}

/// 一次对账的结果（诊断用）。
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct ReconcileReport {
    pub pruned: Vec<(String, String)>,
    pub adopted: Vec<(String, String)>,
    pub changed: bool,
}

impl WorkspaceRegistry {
    pub(crate) fn empty() -> Self {
        Self {
            file: WorkspaceFile {
                unit: default_unit(),
                global: GlobalState {
                    initialized: true,
                    ..GlobalState::default()
                },
                tables: Tables::default(),
            },
        }
    }

    pub(crate) fn global(&self) -> &GlobalState {
        &self.file.global
    }

    /// 显示序视图（排序权威在 `global.workspaceIds`；游离键防御性追加
    /// 到尾——对账会同步修齐）。
    pub(crate) fn ordered(&self) -> Vec<(String, WorkspaceRecord)> {
        let mut seen = HashSet::new();
        let mut out = Vec::new();
        for id in &self.file.global.workspace_ids {
            if let Some(record) = self.file.tables.workspaces.get(id) {
                out.push((id.clone(), record.clone()));
                seen.insert(id.clone());
            }
        }
        for (id, record) in &self.file.tables.workspaces {
            if !seen.contains(id) {
                out.push((id.clone(), record.clone()));
            }
        }
        out
    }

    pub(crate) fn find_by_path(&self, path: &str) -> Option<(String, WorkspaceRecord)> {
        self.file
            .tables
            .workspaces
            .iter()
            .find(|(_, record)| record.path == path)
            .map(|(id, record)| (id.clone(), record.clone()))
    }

    /// 注册工作区（幂等：path 已存在则返回既有 id 并并入 `session_ids`）。
    pub(crate) fn register(&mut self, path: &str, title: &str, session_ids: Vec<String>) -> String {
        if let Some((id, record)) = self.find_by_path(path) {
            let mut merged = record.session_ids.clone();
            for id in session_ids {
                if !merged.contains(&id) {
                    merged.push(id);
                }
            }
            if merged != record.session_ids {
                self.touch(id.clone(), |record| record.session_ids = merged);
            }
            return id;
        }
        let id = uuid::Uuid::new_v4().to_string();
        let now = timestamp::now_iso8601();
        self.file.tables.workspaces.insert(
            id.clone(),
            WorkspaceRecord {
                path: path.to_owned(),
                title: title.to_owned(),
                session_ids,
                created_at: now.clone(),
                updated_at: now,
                active_session_id: None,
            },
        );
        self.file.global.initialized = true;
        self.file.global.workspace_ids.push(id.clone());
        self.file.global.active_workspace_id = Some(id.clone());
        id
    }

    /// 追加一个会话到指定工作区（缺席才加；保持既有顺序不动）。
    pub(crate) fn append_session(&mut self, workspace_id: &str, session_id: &str) {
        self.touch(workspace_id.to_owned(), |record| {
            if !record.session_ids.iter().any(|id| id == session_id) {
                record.session_ids.push(session_id.to_owned());
            }
        });
    }

    /// 设置工作区当前会话指针 + 同步 `global.active*`（恢复现场）。
    pub(crate) fn set_selection(&mut self, workspace_id: &str, session: Option<&str>) -> bool {
        let mut changed = false;
        if let Some(record) = self.file.tables.workspaces.get_mut(workspace_id) {
            let next = session.map(str::to_owned);
            if record.active_session_id != next {
                record.active_session_id = next;
                record.updated_at = timestamp::now_iso8601();
                changed = true;
            }
        }
        let global_session = session.map(str::to_owned);
        if self.file.global.active_workspace_id.as_deref() != Some(workspace_id) {
            self.file.global.active_workspace_id = Some(workspace_id.to_owned());
            changed = true;
        }
        if self.file.global.active_session_id != global_session {
            self.file.global.active_session_id = global_session;
            changed = true;
        }
        changed
    }

    /// 进入工作区（§4.4：命中即进入并置 `global.active*`——activeSession
    /// 以记录内指针为准同步）。未命中返回 `None` 且 `changed == false`
    /// （惰性：调用方据此跳过落盘）。
    pub(crate) fn enter(&mut self, path: &str) -> EnterResult {
        let Some(found) = self.find_by_path(path) else {
            return EnterResult {
                workspace: None,
                changed: false,
            };
        };
        let id = found.0.clone();
        let record_session = found.1.active_session_id.clone();
        let changed = self.set_selection(&id, record_session.as_deref());
        EnterResult {
            workspace: Some(found),
            changed,
        }
    }

    /// 漂移对账（INV-MP5）：**会话日志目录永远赢**，修复全程保序。
    ///
    /// - 剔除：`sessionIds` 中目录里已无日志的 id（保序留下其余）；
    /// - 收编：bucket 中未入账的会话（header cwd 与工作区 path 一致才
    ///   认领——损失性碰撞的表亲会话不抢），append 到尾，新入账者按
    ///   （创建时间, id）确定性排序；
    /// - `global.workspaceIds` 与表键同步（列表引用不存在的记录 → 剔；
    ///   表键游离于列表 → append 尾）。
    pub(crate) fn reconcile(&mut self, sessions_root: &Path) -> Result<ReconcileReport, String> {
        let mut report = ReconcileReport::default();
        let buckets: BTreeSet<String> = self
            .file
            .tables
            .workspaces
            .values()
            .map(|record| path_layout::project_key(&record.path))
            .collect();
        let mut scans: BTreeMap<String, Vec<ScannedSession>> = BTreeMap::new();
        for bucket in buckets {
            scans.insert(bucket.clone(), scan_bucket(sessions_root, &bucket)?);
        }
        let workspace_ids: Vec<String> = self.file.tables.workspaces.keys().cloned().collect();
        for workspace_id in workspace_ids {
            let path = self
                .file
                .tables
                .workspaces
                .get(&workspace_id)
                .map(|record| record.path.clone())
                .expect("keys collected under the same borrow");
            let bucket = path_layout::project_key(&path);
            let Some(scan) = scans.get(&bucket) else {
                continue;
            };
            let present: HashSet<&str> = scan.iter().map(|s| s.id.as_str()).collect();
            let old = self
                .file
                .tables
                .workspaces
                .get(&workspace_id)
                .map(|record| record.session_ids.clone())
                .expect("keys collected under the same borrow");
            let mut next: Vec<String> = Vec::with_capacity(old.len());
            for id in &old {
                if present.contains(id.as_str()) {
                    next.push(id.clone());
                } else {
                    report.pruned.push((workspace_id.clone(), id.clone()));
                }
            }
            let known: HashSet<&str> = next.iter().map(String::as_str).collect();
            let mut newcomers: Vec<&ScannedSession> = scan
                .iter()
                .filter(|s| {
                    !known.contains(s.id.as_str()) && s.header_cwd.as_deref() == Some(path.as_str())
                })
                .collect();
            newcomers.sort_by(|a, b| {
                (a.created_at_ms, a.id.as_str()).cmp(&(b.created_at_ms, b.id.as_str()))
            });
            for newcomer in newcomers {
                report
                    .adopted
                    .push((workspace_id.clone(), newcomer.id.clone()));
                next.push(newcomer.id.clone());
            }
            if next != old {
                self.touch(workspace_id, |record| record.session_ids = next);
                report.changed = true;
            }
        }
        // workspaceIds ↔ 表键同步（防御性漂移：剔除引用幽灵的槽位、
        // 收编游离键到尾）。
        let keys: BTreeSet<String> = self.file.tables.workspaces.keys().cloned().collect();
        let list = &mut self.file.global.workspace_ids;
        let before = list.clone();
        list.retain(|id| keys.contains(id));
        for key in &keys {
            if !list.contains(key) {
                list.push(key.clone());
            }
        }
        if *list != before {
            report.changed = true;
        }
        Ok(report)
    }

    fn touch(&mut self, workspace_id: String, mutate: impl FnOnce(&mut WorkspaceRecord)) {
        if let Some(record) = self.file.tables.workspaces.get_mut(&workspace_id) {
            mutate(record);
            record.updated_at = timestamp::now_iso8601();
        }
    }
}

/// 扫描一个 bucket 里的物化会话（有日志文件的目录才算——header-only
/// 残骸与 DSH 附件目录不可见）。header 读取失败不致命：该目录仍算
/// 「存在」（不剔除已入账者），但不可收编（无法验明归属）。
struct ScannedSession {
    id: String,
    created_at_ms: i64,
    header_cwd: Option<String>,
}

fn scan_bucket(sessions_root: &Path, bucket: &str) -> Result<Vec<ScannedSession>, String> {
    let bucket_dir = sessions_root.join(bucket);
    let entries = match std::fs::read_dir(&bucket_dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => {
            return Err(format!(
                "cannot scan session bucket {}: {error}",
                bucket_dir.display()
            ));
        }
    };
    let mut scanned = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|error| error.to_string())?;
        let name = entry.file_name().to_string_lossy().into_owned();
        let file_type = entry.file_type().map_err(|error| error.to_string())?;
        if file_type.is_symlink() {
            return Err(format!(
                "session entry must not be a symbolic link: {}",
                entry.path().display()
            ));
        }
        if !file_type.is_dir() || name == ".DS_Store" {
            continue;
        }
        let Some(id) = path_layout::decode_segment(&name) else {
            continue;
        };
        let log = entry.path().join("session.jsonl.zstd");
        if !log.is_file() {
            continue;
        }
        let (header_cwd, created_at_ms) = match std::fs::File::open(&log) {
            Ok(mut file) => {
                match crate::session::persistence::read_header_from_reader(
                    &mut file,
                    crate::session::persistence::JsonlCompression::Zstd,
                ) {
                    Ok(Some(header)) => (header.cwd, header.created_at),
                    _ => (None, 0),
                }
            }
            Err(_) => (None, 0),
        };
        scanned.push(ScannedSession {
            id,
            created_at_ms,
            header_cwd,
        });
    }
    Ok(scanned)
}

/// 受纪律加载（撕裂抢救 / 版本门 / 缺失空表）。
pub(crate) fn load(
    storages_dir: &cap_std::fs::Dir,
    storages_root: &Path,
) -> Result<(WorkspaceRegistry, Vec<String>), super::ControlError> {
    match json_file::load::<WorkspaceFile>(
        storages_dir,
        storages_root,
        WORKSPACE_FILE_NAME,
        WORKSPACE_UNIT,
    ) {
        Ok(Loaded::Missing) => Ok((WorkspaceRegistry::empty(), Vec::new())),
        Ok(Loaded::Intact(file)) => Ok((WorkspaceRegistry { file }, Vec::new())),
        Ok(Loaded::Salvaged { remnant }) => {
            let diagnostic = format!(
                "{WORKSPACE_FILE_NAME} was torn (crash artifact); the remnant is preserved \
                 as {remnant} and a fresh empty registry was started — sessions survive in \
                 their logs and will be re-adopted as each project is opened"
            );
            Ok((WorkspaceRegistry::empty(), vec![diagnostic]))
        }
        Err(error) => Err(super::control_error(error.message())),
    }
}

pub(crate) fn save(
    storages_dir: &cap_std::fs::Dir,
    storages_root: &Path,
    registry: &WorkspaceRegistry,
) -> Result<(), super::ControlError> {
    json_file::write(
        storages_dir,
        storages_root,
        WORKSPACE_FILE_NAME,
        &registry.file,
    )
    .map_err(|error| super::control_error(format!("cannot save {WORKSPACE_FILE_NAME}: {error}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_the_real_dsh_shape_zero_adaptation() {
        // 本机 ~/.dsh/storages/workspace.json 的字节副本（钉靶
        // b150a551b8 运行时形态）。未知字段容忍 + 驼峰对齐 = 零适配。
        let fixture = include_str!("../../tests/fixtures/dsh-workspace.json");
        let file: WorkspaceFile = serde_json::from_str(fixture).expect("parses the DSH file");
        assert_eq!(file.unit.name, "workspace");
        assert_eq!(file.unit.version, 2);
        assert!(file.global.initialized);
        assert_eq!(file.global.workspace_ids.len(), 1);
        assert_eq!(file.global.archived_session_ids, Vec::<String>::new());
        let (id, record) = file
            .tables
            .workspaces
            .iter()
            .find(|(_, record)| record.title == "clat")
            .expect("the clat workspace");
        assert_eq!(record.path, "/Users/deng/Documents/GitHub/clat");
        assert_eq!(record.session_ids.len(), 2);
        assert!(
            record
                .session_ids
                .iter()
                .all(|sid| sid.starts_with("session-"))
        );
        assert_eq!(record.active_session_id, None);
        assert_eq!(file.global.active_workspace_id.as_deref(), None);
        // 序列化往返不丢字段（扩展字段缺省时省略——对 DSH 形状零污染）。
        let rewritten = serde_json::to_string(&file).unwrap();
        let reparsed: WorkspaceFile = serde_json::from_str(&rewritten).unwrap();
        assert_eq!(reparsed.tables.workspaces.get(id).unwrap(), record);
    }

    #[test]
    fn pinned_workspace_oracle_fields_round_trip_without_adaptation() {
        let oracle: serde_json::Value = serde_json::from_str(include_str!(
            "../../tests/fixtures/dsh-oracle/workspace-model.json"
        ))
        .expect("workspace oracle");
        let sections = &oracle["sections"];
        assert_eq!(
            sections["domain"],
            serde_json::json!({"name": "workspace", "version": 2})
        );
        let mut record: WorkspaceRecord =
            serde_json::from_value(sections["record"].clone()).expect("DSH record shape");
        assert_eq!(record.path, "/oracle/project");
        assert_eq!(record.session_ids.len(), 1);
        assert_eq!(record.active_session_id, None);
        record.active_session_id = Some("clat-local-pointer".into());
        let encoded = serde_json::to_value(&record).expect("serialize CLAT extension");
        assert_eq!(encoded["path"], sections["record"]["path"]);
        assert_eq!(encoded["sessionIds"], sections["record"]["sessionIds"]);

        let state: GlobalState =
            serde_json::from_value(sections["stateDefaulting"].clone()).expect("DSH global shape");
        assert!(state.initialized);
        assert_eq!(state.workspace_ids, vec!["workspace-1"]);
        assert!(state.archived_session_ids.is_empty());
        assert_eq!(state.active_workspace_id, None);
    }

    #[test]
    fn register_enter_and_selection_round_trip() {
        let mut registry = WorkspaceRegistry::empty();
        let id = registry.register("/proj/a", "a", vec!["session-1".into()]);
        assert_eq!(registry.ordered().len(), 1);
        assert_eq!(
            registry.global().active_workspace_id.as_deref(),
            Some(id.as_str())
        );

        let entered = registry.enter("/proj/a");
        assert!(entered.workspace.is_some());
        assert_eq!(
            entered.workspace.expect("hit").1.session_ids,
            vec!["session-1".to_owned()]
        );
        let missed = registry.enter("/proj/missing");
        assert!(
            missed.workspace.is_none() && !missed.changed,
            "惰性：未命中零变动"
        );

        registry.set_selection(&id, Some("session-1"));
        assert_eq!(
            registry
                .find_by_path("/proj/a")
                .unwrap()
                .1
                .active_session_id,
            Some("session-1".into())
        );
        assert_eq!(
            registry.global().active_session_id.as_deref(),
            Some("session-1")
        );
        registry.set_selection(&id, None);
        assert_eq!(
            registry
                .find_by_path("/proj/a")
                .unwrap()
                .1
                .active_session_id,
            None
        );
    }

    #[test]
    fn reconcile_prunes_preserving_order_and_adopts_at_tail() {
        let sessions = std::env::temp_dir().join(format!(
            "clat-ws-reconcile-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let bucket = path_layout::project_key("/proj/a");
        let bucket_dir = sessions.join(&bucket);
        // old-gone 没有目录（物理消失）；old-kept 在账上；new-orphan
        // 在目录里但未入账（崩溃窗口等价物）。
        for (id, when) in [("old-kept", 1), ("new-orphan", 3)] {
            let dir = bucket_dir.join(path_layout::encode_segment(&format!("session-{id}")));
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(
                dir.join("session.jsonl.zstd"),
                log_with_header(id, "/proj/a", when),
            )
            .unwrap();
        }
        // 孤儿目录：cwd 指向别处（损失性碰撞的表亲）——不收编。
        let cousin = bucket_dir.join(path_layout::encode_segment("session-cousin"));
        std::fs::create_dir_all(&cousin).unwrap();
        std::fs::write(
            cousin.join("session.jsonl.zstd"),
            log_with_header("cousin", "/somewhere/else", 9),
        )
        .unwrap();

        let mut registry = WorkspaceRegistry::empty();
        registry.register(
            "/proj/a",
            "a",
            vec!["session-old-gone".into(), "session-old-kept".into()],
        );
        let report = registry.reconcile(&sessions).expect("reconcile");
        assert!(report.changed);
        assert_eq!(
            report.pruned,
            vec![(
                registry.find_by_path("/proj/a").unwrap().0,
                "session-old-gone".to_owned()
            )]
        );
        let record = registry.find_by_path("/proj/a").unwrap().1;
        assert_eq!(
            record.session_ids,
            vec![
                "session-old-kept".to_owned(),
                "session-new-orphan".to_owned()
            ],
            "剔除保序、收编 append 尾（INV-MP5）"
        );
        // 表亲会话未入账、未被剔除（它本就不在账上）。
        assert!(!record.session_ids.iter().any(|id| id == "session-cousin"));
        std::fs::remove_dir_all(&sessions).ok();
    }

    /// 一个仅含 header 帧的会话日志（真实 SessionHeader 序列化 + 单个
    /// 独立 zstd 帧，与 DSH 契约同形）。
    fn log_with_header(id: &str, cwd: &str, created_at_ms: i64) -> Vec<u8> {
        use std::io::Write as _;
        let header = crate::session::header::SessionHeader::new(
            crate::session::id::SessionId::new(format!("session-{id}")),
            Some(cwd.to_owned()),
            created_at_ms,
        );
        let mut line = header.to_line();
        line.push('\n');
        let mut buffer = Vec::new();
        let mut encoder = zstd::stream::Encoder::new(&mut buffer, 3).unwrap();
        encoder.write_all(line.as_bytes()).unwrap();
        encoder.finish().unwrap();
        buffer
    }
}
