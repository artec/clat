//! WASM 组件的文件系统写授予记录（B5 / C1 定案，2026-08-22）。
//!
//! 病历 W1-15：FullAccess 档下全局安装的组件静默获得项目根与
//! extra_dirs 写权——对 agent 的信任被批发给第三方供应链代码。
//! C1 拍板：写授予审批化 + hash 绑定；本模块是记录的持久化与匹配。
//!
//! # 不变量（先于代码立档）
//!
//! - **INV-W1（三要素绑定）**：授权记录 = (插件名, 组件 sha256, 目录
//!   并集)。静默授予仅当「本次请求的 RW 目录集 ⊆ 记录目录并集」且
//!   插件名与 sha256 匹配——绝不静默超出已批准面；扩面（新目录、
//!   组件 hash 变更、插件更名）必重问。缩面不重问：授予少于批准面
//!   仍在批准范围之内，重问是纯噪音（对「任一失配即失效」的窄化
//!   解释，偏差记于 worklist 实施补记）。
//! - **INV-W4（fail-closed 存储）**：记录文件缺失/损坏/版本不识 →
//!   视为空集（下次重问，不阻塞启动）+ stderr 警告；写入原子
//!   （tmp + rename），失败不致命（下次再问）。多进程并发写
//!   last-writer-wins——丢失记录的后果只是重问，方向安全。
//!
//! INV-W2/W3/W5/W6（授予时机、per-run 缓存、审批通道、桥外无审批面）
//! 由 `wasm.rs` 的 `WasmInstance::resolve_write_grant` 承载，见彼处。

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

const GRANTS_VERSION: u16 = 1;

/// 记录文件名（storage_root 下、plugins.json 同级——安装清单侧）。
pub(crate) const GRANTS_FILE_NAME: &str = "plugin-grants.json";

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) struct WriteGrantRecord {
    pub(crate) plugin: String,
    /// 组件 sha256（小写 hex；匹配大小写不敏感以容忍手写记录）。
    pub(crate) sha256: String,
    /// 已批准的宿主目录并集（绝对路径字符串，排序展示）。
    pub(crate) dirs: Vec<String>,
}

#[derive(Serialize, Deserialize)]
struct WriteGrantFile {
    version: u16,
    grants: Vec<WriteGrantRecord>,
}

pub(crate) fn grants_path(storage_root: &Path) -> PathBuf {
    storage_root.join(GRANTS_FILE_NAME)
}

/// 记录文件里目录的规范字符串形态（与请求侧 `write_dirs` 同一渲染，
/// 字符串精确比对——同目录异拼写（符号链接等）只会落向重问，安全）。
fn dir_string(path: &Path) -> String {
    path.display().to_string()
}

/// INV-W4：缺失 → 空；读失败/解析失败/版本不识 → 空 + stderr 警告。
pub(crate) fn load_grants(path: &Path) -> Vec<WriteGrantRecord> {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Vec::new(),
        Err(error) => {
            eprintln!("clat: warning: cannot read {}: {error}", path.display());
            return Vec::new();
        }
    };
    match serde_json::from_slice::<WriteGrantFile>(&bytes) {
        Ok(file) if file.version == GRANTS_VERSION => file.grants,
        Ok(file) => {
            eprintln!(
                "clat: warning: {} has unsupported version {}; treating as no grants",
                path.display(),
                file.version
            );
            Vec::new()
        }
        Err(error) => {
            eprintln!(
                "clat: warning: {} is not a valid grants file ({error}); treating as no grants",
                path.display()
            );
            Vec::new()
        }
    }
}

/// INV-W1：请求目录集 ⊆ 记录并集（且插件名 + sha256 匹配）才静默授予。
pub(crate) fn covers(
    records: &[WriteGrantRecord],
    plugin: &str,
    sha256: &str,
    requested: &[PathBuf],
) -> bool {
    records.iter().any(|record| {
        record.plugin == plugin
            && record.sha256.eq_ignore_ascii_case(sha256)
            && requested.iter().all(|dir| {
                record
                    .dirs
                    .iter()
                    .any(|granted| granted == &dir_string(dir))
            })
    })
}

/// 审批通过后并入记录：同 (插件, sha256) 合并为目录并集——并集里的
/// 每个目录都来自一次显式批准。返回更新后的记录集（含既有无关项）。
pub(crate) fn upsert(
    records: &mut Vec<WriteGrantRecord>,
    plugin: &str,
    sha256: &str,
    approved: &[PathBuf],
) {
    let record = records
        .iter_mut()
        .find(|record| record.plugin == plugin && record.sha256.eq_ignore_ascii_case(sha256));
    if let Some(record) = record {
        for dir in approved {
            let dir = dir_string(dir);
            if !record.dirs.contains(&dir) {
                record.dirs.push(dir);
            }
        }
        record.dirs.sort();
    } else {
        let mut dirs: Vec<String> = approved.iter().map(|dir| dir_string(dir)).collect();
        dirs.sort();
        dirs.dedup();
        records.push(WriteGrantRecord {
            plugin: plugin.to_owned(),
            sha256: sha256.to_owned(),
            dirs,
        });
    }
}

/// INV-W4：原子写（同目录 tmp + rename；tmp 名带 pid 防多进程互踩）。
pub(crate) fn save_grants(path: &Path, records: &[WriteGrantRecord]) -> std::io::Result<()> {
    let file = WriteGrantFile {
        version: GRANTS_VERSION,
        grants: records.to_vec(),
    };
    let bytes = serde_json::to_vec_pretty(&file)
        .map_err(|error| std::io::Error::other(format!("serialize grants: {error}")))?;
    let tmp = path.with_file_name(format!(
        "{}.{}.tmp",
        path.file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default(),
        std::process::id()
    ));
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(&tmp, path)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn path(dir: &str) -> PathBuf {
        PathBuf::from(dir)
    }

    #[test]
    fn grants_cover_only_within_the_approved_dir_union() {
        let records = vec![WriteGrantRecord {
            plugin: "read".into(),
            sha256: "abc123".into(),
            dirs: vec!["/a".into(), "/b".into()],
        }];
        // 三要素全匹配 + 请求 ⊆ 并集。
        assert!(covers(&records, "read", "ABC123", &[path("/a")]));
        assert!(covers(
            &records,
            "read",
            "abc123",
            &[path("/a"), path("/b")]
        ));
        // 扩面（未批准目录）：不覆盖 → 重问。
        assert!(!covers(
            &records,
            "read",
            "abc123",
            &[path("/a"), path("/c")]
        ));
        // sha 失配：不覆盖。
        assert!(!covers(&records, "read", "def456", &[path("/a")]));
        // 插件名失配：不覆盖。
        assert!(!covers(&records, "other", "abc123", &[path("/a")]));
    }

    #[test]
    fn upsert_merges_into_a_dir_union_per_plugin_and_component() {
        let mut records = Vec::new();
        upsert(&mut records, "read", "abc", &[path("/a")]);
        upsert(&mut records, "read", "abc", &[path("/b"), path("/a")]);
        // 同 (plugin, sha) 合并并集，不新增记录。
        assert_eq!(
            records,
            vec![WriteGrantRecord {
                plugin: "read".into(),
                sha256: "abc".into(),
                dirs: vec!["/a".into(), "/b".into()],
            }]
        );
        // 组件 hash 变更 → 新记录（旧记录保留：同插件可能多版本并存）。
        upsert(&mut records, "read", "xyz", &[path("/a")]);
        assert_eq!(records.len(), 2);
        assert!(covers(&records, "read", "xyz", &[path("/a")]));
    }

    #[test]
    fn grants_store_round_trip_and_fail_closed_loads() {
        let root = std::env::temp_dir().join(format!(
            "clat-grants-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).expect("root");
        let file = grants_path(&root);

        // 缺失 → 空集。
        assert!(load_grants(&file).is_empty());

        // round-trip。
        let mut records = Vec::new();
        upsert(&mut records, "read", "abc", &[path("/a")]);
        save_grants(&file, &records).expect("save");
        assert_eq!(load_grants(&file), records);
        assert!(
            !file
                .with_file_name(format!("{}.{}.tmp", GRANTS_FILE_NAME, std::process::id()))
                .exists()
        );

        // 损坏 → 空集（fail-closed，load 不 panic）。
        std::fs::write(&file, b"{ not json").expect("corrupt");
        assert!(load_grants(&file).is_empty());

        // 未知版本 → 空集。
        std::fs::write(
            &file,
            br#"{"version": 99, "grants": [{"plugin":"p","sha256":"s","dirs":["/a"]}]}"#,
        )
        .expect("future version");
        assert!(load_grants(&file).is_empty());

        let _ = std::fs::remove_dir_all(root);
    }
}
