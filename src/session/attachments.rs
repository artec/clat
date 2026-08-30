//! MM-1 附件域：session-local blob store（`docs/todo/mm1-attachment-domain.md`
//! S2+S3）。承接 MM-1A 桥接期的 `attachments/<uuid>.<ext>` 平铺布局，
//! 新写入走 `attachments/blobs/<digest>`（规范化字节的内容寻址）+
//! `attachments/staging/`（in-flight）。
//!
//! ## 不变量（INV-MM1-*，测试据此推导）
//!
//! - **INV-MM1-3｜提交协议**：批次先整体预检（S1 校验 + 批次上限），
//!   逐源「读入有界 → 解码校验/规范化 → staging create-new(0600) →
//!   file fsync → 原子 rename 进 blobs/ → dir fsync」。批次中任何
//!   失败：清理本批全部 staging 路径（含写失败残件）、回收**本批
//!   rename 发布**的 blob——零可达半成品；dedup 命中的既有 blob
//!   绝不回收（可能已被历史 journal 引用）；崩溃窗口只可能留下
//!   staging 残件或不可达 blob（GC 回收）。blobs 目标已存在时按
//!   dedup 命中处理（长度先行短路 + 逐字节一致，否则视为异常冲突
//!   拒绝——不覆盖）。
//! - **INV-MM1-3C｜并发发布归属**：同一 session store 的所有 handle
//!   共享一条 admission publication lane。批次级回收期间不得让另一
//!   批次观察或认领尚可能回滚的 blob；否则成功批次可返回一个随后
//!   被失败批次删除的 descriptor。
//! - **INV-MM1-4｜orphan 回收**：`sweep_orphans` 只按引用集合 + 24h
//!   TTL 清理 staging 与 blobs；单次调用有界；从不触碰平铺 legacy
//!   文件与被引用对象。
//! - **INV-MM1-S3｜规范化**：magic 通过的输入必须**完整解码成功**
//!   （截断/损坏在此拒绝），EXIF orientation 先应用，重编码剥离全部
//!   元数据；输出确定性 PNG（超 4,000,000 bytes 转 JPEG q85，仍超则
//!   降采样阶梯至长边 ≥512 的下限）；长边 > 2048 先缩。digest 基于
//!   规范化字节（同内容恒同 id——MM-1A 的 descriptor `bytes`/
//!   `width`/`height` 自此是规范化后值，`original_*` 记源尺寸）。
//! - **INV-MM1-批次界**：单消息 ≤8 图、原始总量 ≤32 MiB、规范化
//!   总量 ≤16 MiB；MM-3 起单图源 ≤8 MiB（S3 已把规范化单图压至
//!   4,000,000 bytes，provider 预算不随源图上限放宽）。
//!
//! 解码库采购记录（S3）：`image` 0.25（default-features off；仅
//! png/jpeg——2026-08-27 负责人裁定裁剪，gif/webp 的 S1 识别保留、
//! 接纳以点名格式的可行动错误拒绝；体积实测与加回 webp 的触发
//! 条件见实施计划 S3 段）。理由：MM-I5 要求 magic+完整解码校验与
//! 确定性重编码，手写不可行（JPEG 编解码）；decoder `Limits` 提供
//! 分配前尺寸/内存上限。二进制体积差与 CVE 检索记录见
//! `docs/research/glm-5.3-flash-multimodal-external-spec.md` 附记与
//! 实施计划（S3 段）。
//!
//! 已知边界（记档，不在本切片修）：解码在接纳调用线程同步执行
//! （上限内 ≤8 图 ≤32 MiB）；并发 worker 与 UI 线程卸载归 MM-3
//! composer。规范化假定输入即 sRGB（image 不做 ICC 色彩管理）。

use crate::media::{self, ImageFamily};
use cap_primitives::fs::FollowSymlinks;
use cap_std::ambient_authority;
use cap_std::fs::{Dir, OpenOptions as CapOpenOptions};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::io::{Read as _, Write as _};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock, Weak};

/// Deterministic normalized-blob encoder identity recorded in request/header.
/// Bump whenever resize, alpha, orientation, PNG, or JPEG encoding semantics
/// change, even if the public attachment descriptor remains compatible.
pub(crate) const ATTACHMENT_ENCODER_VERSION: &str = "clat-image-normalize-v1";

/// 单消息图片张数上限（方案 MM-1 硬默认）。
pub(crate) const MAX_IMAGES_PER_MESSAGE: usize = 8;
/// 单消息原始总量上限。
pub(crate) const MAX_RAW_BATCH_BYTES: u64 = 32 * 1024 * 1024;
/// 单消息规范化总量上限。
pub(crate) const MAX_NORMALIZED_BATCH_BYTES: u64 = 16 * 1024 * 1024;
/// 单图规范化字节上限（超过则换编码/降采样）。
const MAX_NORMALIZED_SINGLE_BYTES: usize = 4_000_000;
/// 规范化长边上限。
const MAX_LONG_EDGE: u32 = 2048;
/// 降采样阶梯下限（长边）：低于此仍超界则拒绝（异常图）。
const MIN_LONG_EDGE: u32 = 512;
/// orphan/staging 回收 TTL。
const ORPHAN_TTL_SECS: u64 = 24 * 60 * 60;
/// 单次 sweep 的工作上限（不阻塞会话打开）。
const SWEEP_ENTRY_CAP: usize = 256;
const SWEEP_CURSOR_FILE: &str = ".orphan-sweep-cursor-v1";

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
enum SweepPhase {
    #[default]
    Staging,
    Blobs,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct SweepCursor {
    phase: SweepPhase,
    offset: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct SweepStats {
    pub(crate) inspected: usize,
    pub(crate) removed_staging: usize,
    pub(crate) removed_blobs: usize,
}

#[derive(Debug)]
pub(crate) enum AdmissionError {
    /// 预检失败（任何 staging 之前）：原因字符串面向用户。
    Rejected(String),
    Io(std::io::Error),
}

impl std::fmt::Display for AdmissionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AdmissionError::Rejected(reason) => write!(f, "{reason}"),
            AdmissionError::Io(error) => write!(f, "attachment storage: {error}"),
        }
    }
}

/// 接纳产物：blob id（sha256 hex，对消费者 opaque）+ 规范化元数据。
#[derive(Clone, Debug)]
pub(crate) struct StoredAttachment {
    pub id: String,
    /// blobs/<id> 的绝对路径（桥接期 journal 引用；MM-2 起请求投影
    /// 经 store 按 id 解析）。
    pub blob_path: String,
    /// 规范化输出的 MIME（PNG 或 JPEG）。
    pub media_type: &'static str,
    pub width: u64,
    pub height: u64,
    pub original_width: u64,
    pub original_height: u64,
    /// 规范化字节数。
    pub bytes: u64,
    pub display_name: Option<String>,
}

#[cfg(test)]
type BeforeSourceReadHook = Arc<dyn Fn(&Path) + Send + Sync>;

pub(crate) struct AttachmentStore {
    root: PathBuf,
    root_dir: Dir,
    blobs_dir: Dir,
    staging_dir: Dir,
    /// A batch may publish several blobs and roll all of its own publications
    /// back if a later member fails. Every handle for the same session store
    /// must therefore share one publication lane: otherwise a failing batch
    /// can mistake an identical blob concurrently published by a successful
    /// batch for its own work and delete it during rollback.
    admission_lock: Arc<Mutex<()>>,
    #[cfg(test)]
    after_publish: Option<Arc<dyn Fn(usize) + Send + Sync>>,
    #[cfg(test)]
    before_source_read: Option<BeforeSourceReadHook>,
}

fn shared_admission_lock(root: &Path) -> Arc<Mutex<()>> {
    static LOCKS: OnceLock<Mutex<HashMap<PathBuf, Weak<Mutex<()>>>>> = OnceLock::new();
    let mut locks = LOCKS
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .expect("attachment admission lock registry");
    locks.retain(|_, lock| lock.strong_count() > 0);
    if let Some(lock) = locks.get(root).and_then(Weak::upgrade) {
        return lock;
    }
    let lock = Arc::new(Mutex::new(()));
    locks.insert(root.to_path_buf(), Arc::downgrade(&lock));
    lock
}

impl AttachmentStore {
    /// 打开（或创建）会话附件域。`root` = `<session_dir>/attachments`。
    pub(crate) fn open(root: PathBuf) -> Result<Self, std::io::Error> {
        ensure_private_dir(&root, true)?;
        // Validate/create owned children one at a time. A pre-existing
        // symlink/reparse point must stop the open before any later namespace
        // entry is created through or beside it.
        ensure_private_dir(&root.join("blobs"), false)?;
        ensure_private_dir(&root.join("staging"), false)?;
        // All later namespace mutations run relative to these descriptors.
        // The display path is retained only for the bridge-era journal field;
        // replacing an ancestor after this point cannot redirect store I/O.
        let root_dir = open_dir_path_nofollow(&root)?;
        let blobs_dir =
            crate::session::root_dir::SessionRootDir::open_child(&root_dir, Path::new("blobs"))?;
        let staging_dir =
            crate::session::root_dir::SessionRootDir::open_child(&root_dir, Path::new("staging"))?;
        let admission_lock = shared_admission_lock(&root);
        Ok(Self {
            root,
            root_dir,
            blobs_dir,
            staging_dir,
            admission_lock,
            #[cfg(test)]
            after_publish: None,
            #[cfg(test)]
            before_source_read: None,
        })
    }

    /// Open/create an attachment domain relative to a capability-held session
    /// directory. Production session flows use this constructor so replacing
    /// any ambient ancestor cannot redirect directory creation or publication.
    pub(crate) fn open_in_session(
        session_dir: &Dir,
        root: PathBuf,
    ) -> Result<Self, std::io::Error> {
        let root_dir = crate::session::root_dir::SessionRootDir::open_or_create_child(
            session_dir,
            Path::new("attachments"),
        )?;
        let blobs_dir = crate::session::root_dir::SessionRootDir::open_or_create_child(
            &root_dir,
            Path::new("blobs"),
        )?;
        let staging_dir = crate::session::root_dir::SessionRootDir::open_or_create_child(
            &root_dir,
            Path::new("staging"),
        )?;
        let admission_lock = shared_admission_lock(&root);
        Ok(Self {
            root,
            root_dir,
            blobs_dir,
            staging_dir,
            admission_lock,
            #[cfg(test)]
            after_publish: None,
            #[cfg(test)]
            before_source_read: None,
        })
    }

    #[cfg(test)]
    fn with_after_publish_hook(mut self, hook: Arc<dyn Fn(usize) + Send + Sync>) -> Self {
        self.after_publish = Some(hook);
        self
    }

    #[cfg(test)]
    fn with_before_source_read_hook(mut self, hook: Arc<dyn Fn(&Path) + Send + Sync>) -> Self {
        self.before_source_read = Some(hook);
        self
    }

    /// 接纳一批源图（INV-MM1-3 全序）。任何失败保证：零新 blob、
    /// 零 staging 残件、调用方可安全整体失败（journal 无痕）。
    pub(crate) fn admit(
        &self,
        sources: &[PathBuf],
    ) -> Result<Vec<StoredAttachment>, AdmissionError> {
        let _admission_guard = self.admission_lock.lock().map_err(|_| {
            AdmissionError::Io(std::io::Error::other(
                "attachment admission lock is poisoned",
            ))
        })?;
        if !directory_path_matches_handle(&self.root, &self.root_dir) {
            return Err(AdmissionError::Rejected(
                "attachment store namespace changed after it was opened".into(),
            ));
        }
        if sources.is_empty() {
            return Ok(Vec::new());
        }
        if sources.len() > MAX_IMAGES_PER_MESSAGE {
            return Err(AdmissionError::Rejected(format!(
                "a message carries at most {MAX_IMAGES_PER_MESSAGE} images (got {})",
                sources.len()
            )));
        }
        // —— 批次预检：任何 staging 之前完成（S1 校验 + 字节界）。——
        // Keep every accepted source descriptor alive through normalization.
        // Reopening by path after preflight would let a private staging name be
        // replaced between validation and commit.
        let mut prepared_sources = Vec::with_capacity(sources.len());
        let mut raw_total = 0u64;
        for source in sources {
            use std::io::{Read as _, Seek as _};

            #[cfg(test)]
            if let Some(hook) = &self.before_source_read {
                hook(source);
            }

            let (mut file, metadata) =
                open_private_regular_file_no_follow(source).map_err(|error| {
                    AdmissionError::Rejected(format!(
                        "attachment source is not a private regular no-follow file: {}: {error}",
                        source.display()
                    ))
                })?;
            if metadata.len() > media::MAX_ATTACHMENT_BYTES {
                return Err(AdmissionError::Rejected(format!(
                    "image too large ({} bytes > {}): {}",
                    metadata.len(),
                    media::MAX_ATTACHMENT_BYTES,
                    source.display()
                )));
            }
            raw_total = raw_total
                .checked_add(metadata.len())
                .ok_or_else(|| AdmissionError::Rejected("batch size overflow".into()))?;
            let mut header = Vec::new();
            (&mut file)
                .take(256 * 1024)
                .read_to_end(&mut header)
                .map_err(AdmissionError::Io)?;
            media::validate_source_header(source, &header).map_err(AdmissionError::Rejected)?;
            file.seek(std::io::SeekFrom::Start(0))
                .map_err(AdmissionError::Io)?;
            prepared_sources.push((source.clone(), file, metadata.len()));
        }
        if raw_total > MAX_RAW_BATCH_BYTES {
            return Err(AdmissionError::Rejected(format!(
                "message images total {raw_total} bytes exceed the {MAX_RAW_BATCH_BYTES}-byte batch limit"
            )));
        }
        // —— 逐源：读入 → 规范化 → staging → 发布。——
        let mut stored_all: Vec<StoredAttachment> = Vec::new();
        // 失败回收表：只记**本批 rename 发布**的 blob（M1-A）。dedup
        // 命中的既有 blob 绝不入表——它可能已被历史 journal 引用，
        // 删除即静默损坏历史消息的图（违反 INV-MM1-4 被引用不碰）。
        let mut published_blobs: Vec<String> = Vec::new();
        // 本批创建过的 staging 路径（含写失败半成品，M1-C）：批次失败
        // 全清——模块文档「清理本批 staging」由这里兜底。已 rename 走
        // 的条目再删是 ENOENT，无害。
        let mut staging_files: Vec<String> = Vec::new();
        let mut normalized_total = 0u64;
        let result = (|| -> Result<(), AdmissionError> {
            for (source, mut file, expected_len) in prepared_sources {
                let mut bytes = Vec::with_capacity(expected_len as usize);
                (&mut file)
                    .take(media::MAX_ATTACHMENT_BYTES + 1)
                    .read_to_end(&mut bytes)
                    .map_err(AdmissionError::Io)?;
                let observed_len = file.metadata().map_err(AdmissionError::Io)?.len();
                if bytes.len() as u64 != expected_len || observed_len != expected_len {
                    // The held inode changed while being read. Never normalize
                    // a partial/mixed snapshot or silently exceed batch math.
                    return Err(AdmissionError::Rejected(format!(
                        "image changed while admitting: {}",
                        source.display()
                    )));
                }
                self.publish_source_bytes(
                    &source,
                    &bytes,
                    &mut normalized_total,
                    &mut stored_all,
                    &mut published_blobs,
                    &mut staging_files,
                )?;
            }
            if !directory_path_matches_handle(&self.root, &self.root_dir) {
                return Err(AdmissionError::Rejected(
                    "attachment store namespace changed during admission".into(),
                ));
            }
            Ok(())
        })();
        match result {
            Ok(()) => Ok(stored_all),
            Err(error) => {
                // 批次失败：只回收本批 rename 发布的 blob（journal 尚未
                // 附加、无人引用——引用只来自 journal），并清空本批全部
                // staging 路径（含写失败残件）——零可达半成品。
                for name in &published_blobs {
                    let _ = self.blobs_dir.remove_file(name);
                }
                for name in &staging_files {
                    let _ = self.staging_dir.remove_file(name);
                }
                Err(error)
            }
        }
    }

    /// Normalize and publish one source inside an already-locked batch. The
    /// caller owns raw-byte admission and the final namespace identity fence;
    /// keeping publication here gives path and capability-byte inputs exactly
    /// one decoder, dedup, staging, and rollback policy.
    fn publish_source_bytes(
        &self,
        source: &Path,
        bytes: &[u8],
        normalized_total: &mut u64,
        stored_all: &mut Vec<StoredAttachment>,
        published_blobs: &mut Vec<String>,
        staging_files: &mut Vec<String>,
    ) -> Result<(), AdmissionError> {
        let (family, original_dims) =
            media::validate_source_header(source, bytes).map_err(AdmissionError::Rejected)?;
        let normalized = normalize_image(bytes, family).map_err(|reason| {
            AdmissionError::Rejected(format!("{reason}: {}", source.display()))
        })?;
        *normalized_total = normalized_total
            .checked_add(normalized.bytes.len() as u64)
            .ok_or_else(|| AdmissionError::Rejected("batch size overflow".into()))?;
        if *normalized_total > MAX_NORMALIZED_BATCH_BYTES {
            return Err(AdmissionError::Rejected(format!(
                "normalized message images exceed the {MAX_NORMALIZED_BATCH_BYTES}-byte budget"
            )));
        }
        let id = digest_hex(&normalized.bytes);
        let blob_path = self.root.join("blobs").join(&id);
        let blob_entry = self.blobs_dir.symlink_metadata(&id);
        if blob_entry.is_ok() {
            // Open the final component no-follow and compare through that
            // descriptor. Any type/link/read mismatch is an integrity error;
            // never replace an occupied content-addressed name.
            if !existing_blob_is_identical_in(&self.blobs_dir, Path::new(&id), &normalized.bytes) {
                return Err(AdmissionError::Rejected(format!(
                    "blob {id} already exists with different content"
                )));
            }
        } else if let Err(error) = blob_entry
            && error.kind() != std::io::ErrorKind::NotFound
        {
            return Err(AdmissionError::Io(error));
        } else {
            let staging_name = format!("{}.tmp", uuid::Uuid::new_v4().simple());
            staging_files.push(staging_name.clone());
            write_private_file_sync_in(
                &self.staging_dir,
                Path::new(&staging_name),
                &normalized.bytes,
            )?;
            self.staging_dir
                .rename(&staging_name, &self.blobs_dir, &id)
                .map_err(AdmissionError::Io)?;
            crate::session::root_dir::sync_dir(&self.blobs_dir).map_err(AdmissionError::Io)?;
            published_blobs.push(id.clone());
            #[cfg(test)]
            if let Some(hook) = &self.after_publish {
                hook(published_blobs.len());
            }
        }
        let (original_width, original_height) =
            original_dims.unwrap_or((normalized.width, normalized.height));
        stored_all.push(StoredAttachment {
            id,
            blob_path: blob_path.to_string_lossy().into_owned(),
            media_type: normalized.media_type,
            width: normalized.width,
            height: normalized.height,
            original_width,
            original_height,
            bytes: normalized.bytes.len() as u64,
            display_name: source
                .file_name()
                .map(|name| name.to_string_lossy().into_owned()),
        });
        Ok(())
    }

    /// Admit bytes that were already read through another capability fence
    /// (MM-2/W5 project-relative and run-scratch inputs). These bytes enter the
    /// same normalization/publication transaction directly: writing them to a
    /// private file and reopening it through the ambient display path would
    /// reintroduce an ancestor-replacement window after the original fence.
    pub(crate) fn admit_bytes(
        &self,
        bytes: &[u8],
        display_name: &str,
    ) -> Result<StoredAttachment, AdmissionError> {
        if bytes.len() as u64 > media::MAX_ATTACHMENT_BYTES {
            return Err(AdmissionError::Rejected(format!(
                "image too large ({} bytes > {})",
                bytes.len(),
                media::MAX_ATTACHMENT_BYTES
            )));
        }
        let _extension = Path::new(display_name)
            .extension()
            .and_then(|value| value.to_str())
            .filter(|value| !value.is_empty())
            .ok_or_else(|| {
                AdmissionError::Rejected(
                    "image source needs a .png, .jpg, or .jpeg extension".into(),
                )
            })?;
        let source = Path::new(display_name);
        let _admission_guard = self.admission_lock.lock().map_err(|_| {
            AdmissionError::Io(std::io::Error::other(
                "attachment admission lock is poisoned",
            ))
        })?;
        if !directory_path_matches_handle(&self.root, &self.root_dir) {
            return Err(AdmissionError::Rejected(
                "attachment store namespace changed after it was opened".into(),
            ));
        }
        #[cfg(test)]
        if let Some(hook) = &self.before_source_read {
            hook(source);
        }

        let mut stored_all = Vec::with_capacity(1);
        let mut published_blobs = Vec::new();
        let mut staging_files = Vec::new();
        let mut normalized_total = 0u64;
        let result = (|| -> Result<(), AdmissionError> {
            self.publish_source_bytes(
                source,
                bytes,
                &mut normalized_total,
                &mut stored_all,
                &mut published_blobs,
                &mut staging_files,
            )?;
            if !directory_path_matches_handle(&self.root, &self.root_dir) {
                return Err(AdmissionError::Rejected(
                    "attachment store namespace changed during admission".into(),
                ));
            }
            Ok(())
        })();
        match result {
            Ok(()) => stored_all.pop().ok_or_else(|| {
                AdmissionError::Rejected("attachment admission produced no image".into())
            }),
            Err(error) => {
                for name in &published_blobs {
                    let _ = self.blobs_dir.remove_file(name);
                }
                for name in &staging_files {
                    let _ = self.staging_dir.remove_file(name);
                }
                Err(error)
            }
        }
    }

    /// INV-MM1-4：按引用集合 + TTL 清理。返回检查/清掉的条目数。
    /// 每次最多**检查** [`SWEEP_ENTRY_CAP`] 项；保留项同样消耗工作量，
    /// 避免大量 fresh/referenced 文件让冷打开退化成无界全目录扫描。
    /// 从不触碰平铺 legacy 文件（`<uuid>.<ext>`，journal 以路径引用）。
    pub(crate) fn sweep_orphans(
        &self,
        referenced: &HashSet<String>,
        now: std::time::SystemTime,
    ) -> SweepStats {
        // Admission and sweep share one per-store lane. Besides serializing
        // cursor publication, this keeps a deliberately future-dated test or
        // a wildly wrong wall clock from unlinking a just-published blob before
        // the admitting batch returns it to the journal layer.
        let Ok(_guard) = self.admission_lock.lock() else {
            return SweepStats {
                inspected: 0,
                removed_staging: 0,
                removed_blobs: 0,
            };
        };
        let mut inspected = 0usize;
        let mut removed_staging = 0usize;
        let mut removed_blobs = 0usize;
        let ttl = std::time::Duration::from_secs(ORPHAN_TTL_SECS);
        let expired = |entry: &cap_std::fs::DirEntry| -> bool {
            entry
                .metadata()
                .ok()
                .and_then(|metadata| metadata.modified().ok())
                .is_some_and(|modified| {
                    now.duration_since(modified.into_std()).unwrap_or_default() > ttl
                })
        };
        let mut cursor = self.load_sweep_cursor();
        let mut work = SWEEP_ENTRY_CAP;
        let mut completed_cycle = false;
        while work > 0 && !completed_cycle {
            let directory = match cursor.phase {
                SweepPhase::Staging => &self.staging_dir,
                SweepPhase::Blobs => &self.blobs_dir,
            };
            let Ok(mut entries) = directory.entries() else {
                match cursor.phase {
                    SweepPhase::Staging => {
                        cursor = SweepCursor {
                            phase: SweepPhase::Blobs,
                            offset: 0,
                        };
                    }
                    SweepPhase::Blobs => {
                        cursor = SweepCursor::default();
                        completed_cycle = true;
                    }
                }
                continue;
            };

            // Directory iteration has no portable seek cookie. Reopening a
            // directory therefore walks past the persisted logical offset,
            // but only entries after it consume the inspection/stat/removal
            // budget. Deletions can shift an entry behind the offset; the
            // cursor's next full cycle deliberately revisits from zero.
            let mut skipped = 0usize;
            while skipped < cursor.offset {
                if entries.next().is_none() {
                    break;
                }
                skipped += 1;
            }
            if skipped < cursor.offset {
                cursor.offset = 0;
                match cursor.phase {
                    SweepPhase::Staging => cursor.phase = SweepPhase::Blobs,
                    SweepPhase::Blobs => {
                        cursor.phase = SweepPhase::Staging;
                        completed_cycle = true;
                    }
                }
                continue;
            }

            let mut exhausted = false;
            while work > 0 {
                let Some(entry) = entries.next() else {
                    exhausted = true;
                    break;
                };
                cursor.offset = cursor.offset.saturating_add(1);
                work -= 1;
                inspected += 1;
                let Ok(entry) = entry else { continue };
                match cursor.phase {
                    SweepPhase::Staging => {
                        if expired(&entry) && entry.remove_file().is_ok() {
                            removed_staging += 1;
                        }
                    }
                    SweepPhase::Blobs => {
                        let unreferenced = entry
                            .file_name()
                            .to_str()
                            .is_none_or(|name| !referenced.contains(name));
                        if unreferenced && expired(&entry) && entry.remove_file().is_ok() {
                            removed_blobs += 1;
                        }
                    }
                }
            }
            if exhausted {
                cursor.offset = 0;
                match cursor.phase {
                    SweepPhase::Staging => cursor.phase = SweepPhase::Blobs,
                    SweepPhase::Blobs => {
                        cursor.phase = SweepPhase::Staging;
                        completed_cycle = true;
                    }
                }
            }
        }
        // Cursor durability is an availability optimization, not a condition
        // for session correctness. A failed save safely causes repeat work on
        // the next arm; removal decisions above remain valid on their own.
        let _ = self.save_sweep_cursor(cursor);
        SweepStats {
            inspected,
            removed_staging,
            removed_blobs,
        }
    }

    pub(crate) fn open_blob_verified(
        &self,
        attachment_id: &str,
    ) -> Result<(Vec<u8>, u64), std::io::Error> {
        if expected_content_address(Path::new(attachment_id)).is_none() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "attachment id is not a content address",
            ));
        }
        let (mut file, metadata) =
            open_private_regular_file_in(&self.blobs_dir, Path::new(attachment_id))?;
        let bytes = metadata.len();
        if bytes > media::MAX_ATTACHMENT_BYTES {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "attachment exceeds the image byte limit",
            ));
        }
        let snapshot =
            read_open_file_verified_snapshot(&mut file, Path::new(attachment_id), bytes)?;
        Ok((snapshot, bytes))
    }

    fn load_sweep_cursor(&self) -> SweepCursor {
        use std::io::Read as _;

        let Ok((file, metadata)) =
            open_private_regular_file_in(&self.root_dir, Path::new(SWEEP_CURSOR_FILE))
        else {
            return SweepCursor::default();
        };
        if metadata.len() > 128 {
            return SweepCursor::default();
        }
        let mut value = String::new();
        if file.take(129).read_to_string(&mut value).is_err()
            || value.len() as u64 != metadata.len()
        {
            return SweepCursor::default();
        }
        let mut fields = value.trim().split(':');
        let phase = match fields.next() {
            Some("staging") => SweepPhase::Staging,
            Some("blobs") => SweepPhase::Blobs,
            _ => return SweepCursor::default(),
        };
        let Some(offset) = fields.next().and_then(|field| field.parse::<usize>().ok()) else {
            return SweepCursor::default();
        };
        if fields.next().is_some() {
            return SweepCursor::default();
        }
        SweepCursor { phase, offset }
    }

    fn save_sweep_cursor(&self, cursor: SweepCursor) -> Result<(), std::io::Error> {
        let phase = match cursor.phase {
            SweepPhase::Staging => "staging",
            SweepPhase::Blobs => "blobs",
        };
        let temp_name = format!(".orphan-sweep-cursor-{}.tmp", uuid::Uuid::new_v4().simple());
        if let Err(error) = write_private_file_sync_in(
            &self.root_dir,
            Path::new(&temp_name),
            format!("{phase}:{}\n", cursor.offset).as_bytes(),
        )
        .map_err(|error| match error {
            AdmissionError::Io(error) => error,
            AdmissionError::Rejected(reason) => std::io::Error::other(reason),
        }) {
            let _ = self.root_dir.remove_file(&temp_name);
            return Err(error);
        }
        if let Err(error) = self
            .root_dir
            .rename(&temp_name, &self.root_dir, SWEEP_CURSOR_FILE)
        {
            let _ = self.root_dir.remove_file(&temp_name);
            return Err(error);
        }
        crate::session::root_dir::sync_dir(&self.root_dir)
    }
}

/// Full decoder validation for an uploaded draft before it becomes addressable
/// by an opaque upload id. Normalization/publication still belongs to the
/// target session's `AttachmentStore::admit` transaction at prompt commit, so
/// this early rejection does not allocate its encoded output. It shares the
/// decoder-format and resource policy with the eventual transaction.
pub(crate) fn validate_draft_source(path: &Path) -> Result<(), String> {
    use std::io::{Read as _, Seek as _};

    let (mut file, metadata) = open_private_regular_file_no_follow(path).map_err(|error| {
        format!("uploaded image staging target is not a private regular no-follow file: {error}")
    })?;
    if metadata.len() == 0 || metadata.len() > media::MAX_ATTACHMENT_BYTES {
        return Err(format!(
            "uploaded image must be 1..={} bytes (got {})",
            media::MAX_ATTACHMENT_BYTES,
            metadata.len()
        ));
    }
    let mut header = Vec::new();
    (&mut file)
        .take(256 * 1024)
        .read_to_end(&mut header)
        .map_err(|error| format!("read uploaded image header: {error}"))?;
    file.seek(std::io::SeekFrom::Start(0))
        .map_err(|error| format!("rewind uploaded image: {error}"))?;
    let (family, _) = media::validate_source_header(path, &header)?;
    // Raw uploads have already been written to our private staging file. Do
    // not make a second whole-file `Vec<u8>` merely to reject a corrupt image
    // before the eventual admission transaction: an 8MiB browser upload
    // would otherwise carry that transient raw copy in addition to decoder
    // allocations. The same decoder limits as normalization remain the
    // authority, while `AttachmentStore::admit` later performs the only
    // byte-owning normalize/publish pass.
    let mut reader =
        image::ImageReader::with_format(std::io::BufReader::new(file), decoder_format(family)?);
    reader.limits(decoder_limits());
    reader
        .decode()
        .map(|_| ())
        .map_err(|error| format!("image decode failed: {error}"))
}

/// 规范化产物（编码后字节 + 元数据）。
#[derive(Debug)]
struct Normalized {
    bytes: Vec<u8>,
    media_type: &'static str,
    width: u64,
    height: u64,
}

/// INV-MM1-S3：完整解码（分配前 Limits）→ orientation → 缩放 →
/// 确定性编码阶梯。解码失败（截断/损坏/超限）在接纳阶段拒绝——
/// 这是 S1 magic 检查之后的第二道、也是最终的内容闸。
fn normalize_image(bytes: &[u8], family: ImageFamily) -> Result<Normalized, String> {
    use image::ImageDecoder as _;
    let format = decoder_format(family)?;
    let limits = decoder_limits();
    let reader = image::ImageReader::new(std::io::Cursor::new(bytes))
        .with_guessed_format()
        .map_err(|error| format!("image decode failed: {error}"))?;
    let mut reader = reader;
    reader.limits(limits.clone());
    if reader.format() != Some(format) {
        return Err("image content does not match its detected family".into());
    }
    let orientation = reader
        .into_decoder()
        .and_then(|mut decoder| decoder.orientation())
        .unwrap_or(image::metadata::Orientation::NoTransforms);
    reader = image::ImageReader::new(std::io::Cursor::new(bytes))
        .with_guessed_format()
        .map_err(|error| format!("image decode failed: {error}"))?;
    reader.limits(limits);
    let mut decoded = reader
        .decode()
        .map_err(|error| format!("image decode failed: {error}"))?;
    decoded.apply_orientation(orientation);
    let (mut width, mut height) = (decoded.width(), decoded.height());
    if width == 0 || height == 0 {
        return Err("image decodes to an empty canvas".into());
    }
    if decoded.color().has_alpha() {
        // 确定性输出走不透明通道：alpha 合成到白底（sRGB 假定记档）。
        let rgba = decoded.to_rgba8();
        let mut rgb = image::RgbImage::new(rgba.width(), rgba.height());
        for (x, y, pixel) in rgba.enumerate_pixels() {
            let alpha = pixel[3] as u32;
            let blend = |channel: u8| -> u8 {
                ((channel as u32 * alpha + 255 * (255 - alpha)) / 255) as u8
            };
            rgb.put_pixel(
                x,
                y,
                image::Rgb([blend(pixel[0]), blend(pixel[1]), blend(pixel[2])]),
            );
        }
        decoded = image::DynamicImage::ImageRgb8(rgb);
    }
    loop {
        if width > MAX_LONG_EDGE || height > MAX_LONG_EDGE {
            decoded = decoded.thumbnail(MAX_LONG_EDGE, MAX_LONG_EDGE);
            width = decoded.width();
            height = decoded.height();
        }
        // 编码阶梯：PNG →（超界）JPEG q85 →（仍超）降采样 ×0.8。
        let png = encode_png(&decoded)?;
        if png.len() <= MAX_NORMALIZED_SINGLE_BYTES {
            return Ok(Normalized {
                bytes: png,
                media_type: "image/png",
                width: width as u64,
                height: height as u64,
            });
        }
        let jpeg = encode_jpeg(&decoded, 85)?;
        if jpeg.len() <= MAX_NORMALIZED_SINGLE_BYTES {
            return Ok(Normalized {
                bytes: jpeg,
                media_type: "image/jpeg",
                width: width as u64,
                height: height as u64,
            });
        }
        if width.max(height) <= MIN_LONG_EDGE {
            return Err(format!(
                "normalized image stays above {} bytes at the minimum scale",
                MAX_NORMALIZED_SINGLE_BYTES
            ));
        }
        let next_width = ((width as f64 * 0.8) as u32).max(MIN_LONG_EDGE);
        let next_height = ((height as f64 * 0.8) as u32).max(MIN_LONG_EDGE);
        decoded = decoded.thumbnail(next_width, next_height);
        width = decoded.width();
        height = decoded.height();
    }
}

fn decoder_format(family: ImageFamily) -> Result<image::ImageFormat, String> {
    match family {
        ImageFamily::Png => Ok(image::ImageFormat::Png),
        ImageFamily::Jpeg => Ok(image::ImageFormat::Jpeg),
        // 负责人裁定（2026-08-27 审计 M1-F）：解码器只采购 png+jpeg。
        // S1 的四族 magic 识别保留（格式无关）；gif/webp 在此以点名
        // 格式的可行动错误干净拒绝——不 panic、不静默。加回 webp 的
        // 触发条件见实施计划 S3 采购记录（dogfood 粘贴被拒 ≥2 次）。
        ImageFamily::Gif => {
            Err("GIF images are not supported yet; please re-save the image as PNG or JPEG".into())
        }
        ImageFamily::Webp => {
            Err("WebP images are not supported yet; please re-save the image as PNG or JPEG".into())
        }
    }
}

fn decoder_limits() -> image::Limits {
    let mut limits = image::Limits::default();
    // 防御性后备闸（M1-D 记档）：能到达解码的只有 png/jpeg，两格式的
    // 头尺寸即解码尺寸，S1 读头已在 16M px 上拦截（权威闸）；原本可
    // 无头放行到这里的 WebP 变体已被上面的整族拒绝。因此 Limits 的
    // 单边 16384 + 96MiB 分配闸**宽于** 16M px（灰区 ~24M px），仅作
    // S1 头闸失效（对抗构造/头损坏）时的内存兜底，不自称同口径。
    limits.max_image_width = Some(16_384);
    limits.max_image_height = Some(16_384);
    // 内存闸：16M px × 4B = 64MiB 解码缓冲 + 编码工作区余量。
    limits.max_alloc = Some(96 * 1024 * 1024);
    limits
}

fn encode_png(image: &image::DynamicImage) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    image
        .write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png)
        .map_err(|error| format!("png encode failed: {error}"))?;
    Ok(out)
}

fn encode_jpeg(image: &image::DynamicImage, quality: u8) -> Result<Vec<u8>, String> {
    let rgb = image.to_rgb8();
    let mut out = Vec::new();
    let encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, quality);
    rgb.write_with_encoder(encoder)
        .map_err(|error| format!("jpeg encode failed: {error}"))?;
    Ok(out)
}

fn digest_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    let digest = hasher.finalize();
    let mut hex = String::with_capacity(digest.len() * 2);
    for byte in digest {
        hex.push_str(&format!("{byte:02x}"));
    }
    hex
}

fn expected_content_address(path: &Path) -> Option<&str> {
    let name = path.file_name()?.to_str()?;
    (name.len() == 64
        && name
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)))
    .then_some(name)
}

/// Flat legacy attachment names are not content addresses. New blob names are
/// lowercase SHA-256 and must authenticate the bytes before either provider or
/// frontend exposure.
pub(crate) fn content_address_matches(path: &Path, bytes: &[u8]) -> bool {
    expected_content_address(path).is_none_or(|expected| digest_hex(bytes) == expected)
}

/// Read a bounded attachment through its already-open descriptor and verify
/// any content-address claim against the resulting snapshot. Returning the
/// snapshot closes the verify-then-stream race: a later mutation of the store
/// inode cannot change bytes already authorized for frontend exposure.
pub(crate) fn read_open_file_verified_snapshot(
    file: &mut std::fs::File,
    path: &Path,
    expected_len: u64,
) -> Result<Vec<u8>, std::io::Error> {
    use std::io::{Read as _, Seek as _};

    file.seek(std::io::SeekFrom::Start(0))?;
    let capacity = usize::try_from(expected_len).map_err(|_| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "attachment length does not fit in memory",
        )
    })?;
    let mut snapshot = Vec::with_capacity(capacity);
    file.take(expected_len.saturating_add(1))
        .read_to_end(&mut snapshot)?;
    if snapshot.len() as u64 != expected_len {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "attachment changed during content-address verification",
        ));
    }
    let Some(expected) = expected_content_address(path) else {
        return Ok(snapshot);
    };
    let actual = digest_hex(&snapshot);
    if actual != expected {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "attachment content address does not match its bytes",
        ));
    }
    Ok(snapshot)
}

/// Compare an occupied content-addressed blob name without ever following
/// its final component. Any open/type/length/read drift is an integrity
/// mismatch, not a reason to replace the existing entry.
fn existing_blob_is_identical_in(dir: &Dir, name: &Path, expected: &[u8]) -> bool {
    let Ok((mut file, metadata)) = open_private_regular_file_in(dir, name) else {
        return false;
    };
    if metadata.len() != expected.len() as u64 {
        return false;
    }
    let mut existing = Vec::with_capacity(expected.len());
    file.read_to_end(&mut existing).is_ok() && existing == expected
}

fn open_private_regular_file_in(
    dir: &Dir,
    name: &Path,
) -> Result<(std::fs::File, std::fs::Metadata), std::io::Error> {
    let mut options = CapOpenOptions::new();
    options.read(true)._cap_fs_ext_follow(FollowSymlinks::No);
    let file = dir.open_with(name, &options)?.into_std();
    let metadata = file.metadata()?;
    if !metadata.is_file()
        || metadata.file_type().is_symlink()
        || !has_single_hard_link(&file, &metadata)
    {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "attachment is not a private regular no-follow file",
        ));
    }
    Ok((file, metadata))
}

/// Open one core-owned attachment file without following its final component,
/// then prove type and unique-link ownership from that same descriptor. Every
/// caller performs its length/decode/read checks through the returned handle;
/// no path-based metadata/open split may reintroduce a replacement window.
pub(crate) fn open_private_regular_file_no_follow(
    path: &Path,
) -> Result<(std::fs::File, std::fs::Metadata), std::io::Error> {
    if is_session_content_address_path(path) {
        return open_private_regular_file_all_components_nofollow(path);
    }
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.custom_flags(libc::O_NOFOLLOW);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt as _;
        const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
        options.custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
    }
    let file = options.open(path)?;
    let metadata = file.metadata()?;
    if !metadata.is_file()
        || metadata.file_type().is_symlink()
        || !has_single_hard_link(&file, &metadata)
    {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "attachment is not a private regular no-follow file",
        ));
    }
    Ok((file, metadata))
}

fn is_session_content_address_path(path: &Path) -> bool {
    expected_content_address(path).is_some()
        && path.parent().and_then(Path::file_name) == Some(std::ffi::OsStr::new("blobs"))
        && path
            .parent()
            .and_then(Path::parent)
            .and_then(Path::file_name)
            == Some(std::ffi::OsStr::new("attachments"))
        && path
            .components()
            .any(|component| component.as_os_str() == std::ffi::OsStr::new("sessions"))
}

fn open_private_regular_file_all_components_nofollow(
    path: &Path,
) -> Result<(std::fs::File, std::fs::Metadata), std::io::Error> {
    use std::path::Component;

    if !path.is_absolute() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "strict attachment path is not absolute",
        ));
    }
    let mut root = PathBuf::new();
    let mut names = Vec::new();
    for component in path.components() {
        match component {
            Component::Prefix(prefix) => root.push(prefix.as_os_str()),
            Component::RootDir => root.push(std::path::MAIN_SEPARATOR_STR),
            Component::Normal(name) => names.push(name.to_os_string()),
            Component::CurDir => {}
            Component::ParentDir => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "strict attachment path contains parent traversal",
                ));
            }
        }
    }
    let file_name = names.pop().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "strict attachment path has no file name",
        )
    })?;
    let session_boundary = names
        .iter()
        .position(|name| name == std::ffi::OsStr::new("sessions"))
        .ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "strict attachment path has no sessions boundary",
            )
        })?;
    // The storage-root capability owns everything from `sessions` down. The
    // prefix may legitimately contain a platform alias (`/var` on macOS), so
    // open that prefix once, then reject every reparse/symlink component in
    // the session-owned subtree while retaining each directory handle.
    for name in &names[..session_boundary] {
        root.push(name);
    }
    let mut directory = Dir::open_ambient_dir(&root, ambient_authority())?;
    for name in &names[session_boundary..] {
        directory =
            crate::session::root_dir::SessionRootDir::open_child(&directory, Path::new(name))?;
    }
    open_private_regular_file_in(&directory, Path::new(&file_name))
}

pub(crate) fn has_single_hard_link(file: &std::fs::File, metadata: &std::fs::Metadata) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        let _ = file;
        metadata.nlink() == 1
    }
    #[cfg(windows)]
    {
        use std::os::windows::io::AsRawHandle as _;
        use windows_sys::Win32::Storage::FileSystem::{
            BY_HANDLE_FILE_INFORMATION, GetFileInformationByHandle,
        };

        let _ = metadata;
        let mut information = unsafe { std::mem::zeroed::<BY_HANDLE_FILE_INFORMATION>() };
        // SAFETY: `file` remains alive for the call and `information` points to
        // writable storage of the exact structure required by Win32.
        (unsafe { GetFileInformationByHandle(file.as_raw_handle(), &mut information) }) != 0
            && information.nNumberOfLinks == 1
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = (file, metadata);
        false
    }
}

/// create-new(0600) + 写满 + fsync。存在即失败（uuid 命名，冲突即异常）。
fn write_private_file_sync_in(dir: &Dir, name: &Path, bytes: &[u8]) -> Result<(), AdmissionError> {
    let mut options = CapOpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use cap_std::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    options._cap_fs_ext_follow(FollowSymlinks::No);
    let mut file = dir.open_with(name, &options).map_err(AdmissionError::Io)?;
    file.write_all(bytes).map_err(AdmissionError::Io)?;
    file.sync_all().map_err(AdmissionError::Io)?;
    Ok(())
}

fn open_dir_path_nofollow(path: &Path) -> Result<Dir, std::io::Error> {
    let parent = path
        .parent()
        .ok_or_else(|| std::io::Error::other("attachment root has no parent"))?;
    let name = path
        .file_name()
        .ok_or_else(|| std::io::Error::other("attachment root has no file name"))?;
    let parent = Dir::open_ambient_dir(parent, ambient_authority())?;
    crate::session::root_dir::SessionRootDir::open_child(&parent, Path::new(name))
}

fn directory_path_matches_handle(path: &Path, held: &Dir) -> bool {
    let Ok(current) = open_dir_path_nofollow(path) else {
        return false;
    };
    let Ok(held_file) = held.try_clone().map(Dir::into_std_file) else {
        return false;
    };
    let current_file = current.into_std_file();
    same_file_identity(&held_file, &current_file)
}

#[cfg(unix)]
fn same_file_identity(left: &std::fs::File, right: &std::fs::File) -> bool {
    use std::os::unix::fs::MetadataExt as _;
    let (Ok(left), Ok(right)) = (left.metadata(), right.metadata()) else {
        return false;
    };
    left.dev() == right.dev() && left.ino() == right.ino()
}

#[cfg(windows)]
fn same_file_identity(left: &std::fs::File, right: &std::fs::File) -> bool {
    use std::os::windows::io::AsRawHandle as _;
    use windows_sys::Win32::Storage::FileSystem::{
        BY_HANDLE_FILE_INFORMATION, GetFileInformationByHandle,
    };

    let information = |file: &std::fs::File| {
        let mut value = unsafe { std::mem::zeroed::<BY_HANDLE_FILE_INFORMATION>() };
        // SAFETY: the handle stays live for the call and `value` is writable.
        ((unsafe { GetFileInformationByHandle(file.as_raw_handle(), &mut value) }) != 0)
            .then_some(value)
    };
    let (Some(left), Some(right)) = (information(left), information(right)) else {
        return false;
    };
    left.dwVolumeSerialNumber == right.dwVolumeSerialNumber
        && left.nFileIndexHigh == right.nFileIndexHigh
        && left.nFileIndexLow == right.nFileIndexLow
}

#[cfg(not(any(unix, windows)))]
fn same_file_identity(_left: &std::fs::File, _right: &std::fs::File) -> bool {
    false
}

fn ensure_private_dir(path: &Path, create_parents: bool) -> Result<(), std::io::Error> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) => validate_owned_dir(path, &metadata)?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let created = if create_parents {
                std::fs::create_dir_all(path)
            } else {
                std::fs::create_dir(path)
            };
            if let Err(error) = created
                && error.kind() != std::io::ErrorKind::AlreadyExists
            {
                return Err(error);
            }
            let metadata = std::fs::symlink_metadata(path)?;
            validate_owned_dir(path, &metadata)?;
        }
        Err(error) => return Err(error),
    }
    set_private_dir(path)
}

fn validate_owned_dir(path: &Path, metadata: &std::fs::Metadata) -> Result<(), std::io::Error> {
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!(
                "attachment store path is not an owned regular directory: {}",
                path.display()
            ),
        ));
    }
    Ok(())
}

fn set_private_dir(path: &Path) -> Result<(), std::io::Error> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::{OpenOptionsExt as _, PermissionsExt as _};
        let mut options = std::fs::OpenOptions::new();
        options
            .read(true)
            .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW);
        let directory = options.open(path)?;
        let metadata = directory.metadata()?;
        if !metadata.is_dir() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!(
                    "attachment store path is not a directory: {}",
                    path.display()
                ),
            ));
        }
        directory.set_permissions(std::fs::Permissions::from_mode(0o700))?;
    }
    #[cfg(not(unix))]
    {
        // Windows 无 POSIX 位；目录已由 create_dir_all 建在用户 profile
        // 下的 ~/.clat 内（ACL 继承），不额外处理（记档）。
        let metadata = std::fs::symlink_metadata(path)?;
        validate_owned_dir(path, &metadata)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_store(tag: &str) -> (AttachmentStore, PathBuf) {
        let root = std::env::temp_dir().join(format!(
            "clat-attstore-{tag}-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let store = AttachmentStore::open(root.clone()).expect("open store");
        (store, root)
    }

    /// 经 image 编码器生成合法 PNG（测试的可靠图源）。
    fn valid_png(width: u32, height: u32, color: [u8; 3]) -> Vec<u8> {
        let mut canvas = image::GrayImage::new(width, height);
        let _ = &mut canvas;
        let rgb = image::RgbImage::from_pixel(width, height, image::Rgb(color));
        let mut out = Vec::new();
        image::DynamicImage::ImageRgb8(rgb)
            .write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png)
            .expect("encode png");
        out
    }

    fn source_file(tag: &str, extension: &str, bytes: &[u8]) -> PathBuf {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!("clat-attsrc-{tag}-{unique}.{extension}"));
        std::fs::write(&path, bytes).expect("write source");
        path
    }

    /// INV-MM1-3: the attachment namespace itself is not ambient path
    /// authority. A pre-existing root or owned child symlink must be rejected
    /// before `open` creates directories or changes permissions through it.
    #[cfg(unix)]
    #[test]
    fn store_open_rejects_root_and_owned_child_symlinks() {
        use std::os::unix::fs::symlink;

        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let base = std::env::temp_dir().join(format!("clat-attstore-link-{unique}"));
        let root_target = base.join("root-target");
        let root_link = base.join("root-link");
        std::fs::create_dir_all(&root_target).expect("root target");
        symlink(&root_target, &root_link).expect("root symlink");
        assert!(
            AttachmentStore::open(root_link).is_err(),
            "opening a symlinked attachment root must fail closed"
        );
        assert!(
            !root_target.join("blobs").exists() && !root_target.join("staging").exists(),
            "rejection happens before creating owned children through the link"
        );

        let child_root = base.join("child-root");
        let child_target = base.join("child-target");
        std::fs::create_dir_all(&child_root).expect("child root");
        std::fs::create_dir_all(&child_target).expect("child target");
        symlink(&child_target, child_root.join("blobs")).expect("blobs symlink");
        assert!(
            AttachmentStore::open(child_root.clone()).is_err(),
            "opening a store with a symlinked blobs directory must fail closed"
        );
        assert!(
            !child_root.join("staging").exists(),
            "no later owned directory is created through a rejected child"
        );
        std::fs::remove_dir_all(base).ok();
    }

    /// INV-MM1-3 namespace capability leg: validating the attachment root at
    /// `open` time is not enough. If the session directory is renamed and the
    /// old spelling is replaced with a symlink, later admission must continue
    /// through the already-open owned directory handles, never the attacker
    /// controlled replacement path.
    #[cfg(unix)]
    #[test]
    fn store_operations_do_not_follow_a_replaced_root_ancestor() {
        use std::os::unix::fs::symlink;

        let (store, root) = temp_store("root-replacement");
        let parked = root.with_file_name(format!(
            "{}-parked",
            root.file_name().expect("root name").to_string_lossy()
        ));
        let outside = root.with_file_name(format!(
            "{}-outside",
            root.file_name().expect("root name").to_string_lossy()
        ));
        std::fs::rename(&root, &parked).expect("park opened store");
        std::fs::create_dir_all(outside.join("blobs")).expect("outside blobs");
        std::fs::create_dir_all(outside.join("staging")).expect("outside staging");
        symlink(&outside, &root).expect("replace display spelling");

        let error = store
            .admit_bytes(&valid_png(8, 8, [4, 5, 6]), "root-race.png")
            .expect_err("bridge-era display path replacement must fail closed");
        assert!(
            error.to_string().contains("namespace changed"),
            "the failure identifies the stale namespace: {error}"
        );
        assert_eq!(
            std::fs::read_dir(parked.join("blobs"))
                .expect("read held blobs")
                .count(),
            0,
            "the unpublished private staging source is cleaned by capability"
        );
        assert_eq!(
            std::fs::read_dir(outside.join("blobs"))
                .expect("read outside blobs")
                .count(),
            0,
            "the replacement path receives no blob"
        );
        assert_eq!(
            std::fs::read_dir(outside.join("staging"))
                .expect("read outside staging")
                .count(),
            0,
            "the replacement path receives no staging source"
        );

        std::fs::remove_file(root).ok();
        std::fs::remove_dir_all(parked).ok();
        std::fs::remove_dir_all(outside).ok();
    }

    /// Bytes already obtained through a capability fence must not be written
    /// to staging and then reopened through the ambient display path. If the
    /// path is replaced after the store identity check, admission must keep
    /// consuming the caller-owned bytes and fail only at the final namespace
    /// identity fence; it must never decode a replacement-tree file.
    #[cfg(unix)]
    #[test]
    fn byte_admission_does_not_reopen_source_through_replaced_namespace() {
        use std::os::unix::fs::symlink;

        let (store, root) = temp_store("byte-source-replacement");
        let parked = root.with_file_name(format!(
            "{}-parked",
            root.file_name().expect("root name").to_string_lossy()
        ));
        let outside = root.with_file_name(format!(
            "{}-outside",
            root.file_name().expect("root name").to_string_lossy()
        ));
        let replaced = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let hook_root = root.clone();
        let hook_parked = parked.clone();
        let hook_outside = outside.clone();
        let hook_replaced = Arc::clone(&replaced);
        let store = store.with_before_source_read_hook(Arc::new(move |source| {
            use std::sync::atomic::Ordering;

            if hook_replaced.swap(true, Ordering::SeqCst) {
                return;
            }
            std::fs::rename(&hook_root, &hook_parked).expect("park opened store");
            std::fs::create_dir_all(hook_outside.join("blobs")).expect("outside blobs");
            std::fs::create_dir_all(hook_outside.join("staging")).expect("outside staging");
            std::fs::write(
                hook_outside
                    .join("staging")
                    .join(source.file_name().expect("staged source name")),
                b"attacker-controlled non-image bytes",
            )
            .expect("replacement source");
            symlink(&hook_outside, &hook_root).expect("replace display spelling");
        }));

        let error = store
            .admit_bytes(&valid_png(8, 8, [4, 5, 6]), "capability.png")
            .expect_err("namespace replacement must fail closed");
        assert!(
            error.to_string().contains("namespace changed"),
            "admission must not decode the replacement-tree file: {error}"
        );
        assert!(replaced.load(std::sync::atomic::Ordering::SeqCst));
        assert_eq!(
            std::fs::read_dir(parked.join("blobs"))
                .expect("read held blobs")
                .count(),
            0,
            "the held namespace is rolled back before returning"
        );

        std::fs::remove_file(root).ok();
        std::fs::remove_dir_all(parked).ok();
        std::fs::remove_dir_all(outside).ok();
    }

    /// INV-MM1-3: browser uploads are decoded from core-owned private
    /// staging. Validation must not follow a replaced final component or
    /// split its type check from the decoder open; otherwise a symlink can
    /// make an external image addressable as an opaque upload id.
    #[cfg(unix)]
    #[test]
    fn draft_validation_rejects_a_symlinked_staging_file() {
        use std::os::unix::fs::symlink;

        let target = source_file("draft-symlink-target", "png", &valid_png(8, 8, [9, 8, 7]));
        let link = target.with_file_name(format!(
            "clat-attsrc-draft-symlink-link-{}.png",
            uuid::Uuid::new_v4().simple()
        ));
        symlink(&target, &link).expect("create staging replacement symlink");

        let error = validate_draft_source(&link)
            .expect_err("draft validation must not follow a staging symlink");
        assert!(
            error.contains("private regular no-follow file"),
            "the rejection identifies the private-file boundary: {error}"
        );
        assert!(target.is_file(), "validation never touches the target");

        std::fs::remove_file(link).ok();
        std::fs::remove_file(target).ok();
    }

    /// The commit-time admission pass must preserve the same no-follow source
    /// identity as draft validation. A staging name can be replaced after the
    /// early decoder check, so reopening it through ordinary path APIs would
    /// reintroduce the exact authority escalation the early gate rejected.
    #[cfg(unix)]
    #[test]
    fn admission_rejects_a_symlinked_source_file() {
        use std::os::unix::fs::symlink;

        let (store, root) = temp_store("source-symlink");
        let target = source_file("source-symlink-target", "png", &valid_png(8, 8, [6, 7, 8]));
        let link = target.with_file_name(format!(
            "clat-attsrc-source-symlink-link-{}.png",
            uuid::Uuid::new_v4().simple()
        ));
        symlink(&target, &link).expect("create source replacement symlink");

        let error = store
            .admit(std::slice::from_ref(&link))
            .expect_err("admission must not follow a source symlink");
        assert!(
            error.to_string().contains("private regular no-follow file"),
            "the rejection identifies the source capability boundary: {error}"
        );
        assert_eq!(
            std::fs::read_dir(root.join("blobs"))
                .expect("read blob directory")
                .count(),
            0,
            "no external target is published"
        );

        std::fs::remove_file(link).ok();
        std::fs::remove_file(target).ok();
        std::fs::remove_dir_all(root).ok();
    }

    /// INV-MM1-3 + S3：合法源接纳为规范化 blob；同内容重传 dedup 命中
    /// 同一 id（内容寻址）；journal 尚未引用的新 blob 可被 sweep 回收。
    #[test]
    fn admit_normalizes_dedups_and_sweep_reclaims() {
        let (store, root) = temp_store("basic");
        let source = source_file("red", "png", &valid_png(64, 48, [255, 0, 0]));

        let stored = store.admit(std::slice::from_ref(&source)).expect("admit");
        assert_eq!(stored.len(), 1);
        let first = &stored[0];
        assert_eq!(first.media_type, "image/png");
        assert_eq!(first.width, 64);
        assert_eq!(first.height, 48);
        assert_eq!(first.original_width, 64);
        assert!(first.id.len() == 64, "sha256 hex id");
        assert!(Path::new(&first.blob_path).is_file(), "blob published");
        assert!(
            first.blob_path.contains("blobs"),
            "new writes go to the content-addressed blobs dir: {}",
            first.blob_path
        );
        let normalized_len = std::fs::metadata(&first.blob_path).unwrap().len();
        assert_eq!(normalized_len, first.bytes);

        // dedup：同内容再接纳 → 同 id，无第二个 blob。
        let again = store
            .admit(std::slice::from_ref(&source))
            .expect("admit again");
        assert_eq!(again[0].id, first.id, "content addressing dedups");
        let blob_count = std::fs::read_dir(root.join("blobs")).unwrap().count();
        assert_eq!(blob_count, 1);

        // sweep：未引用 + 新鲜 → 不回收；引用集合内 → 也不回收；
        // 伪造过期 mtime → 回收（有界、且不碰 staging 之外的域）。
        let now = std::time::SystemTime::now();
        let stats = store.sweep_orphans(&HashSet::new(), now);
        assert_eq!(
            (stats.removed_staging, stats.removed_blobs),
            (0, 0),
            "fresh unreferenced blobs stay within TTL"
        );
        let stats = store.sweep_orphans(&HashSet::from([first.id.clone()]), now);
        assert_eq!((stats.removed_staging, stats.removed_blobs), (0, 0));
        // 时钟推进 TTL 以上 = 现有 blob 全部"过期"。
        let future = now + std::time::Duration::from_secs(ORPHAN_TTL_SECS + 60);
        let stats = store.sweep_orphans(&HashSet::new(), future);
        assert_eq!(
            (stats.removed_staging, stats.removed_blobs),
            (0, 1),
            "expired unreferenced blob reclaimed"
        );
        assert!(!Path::new(&first.blob_path).exists());
        // 引用集合保护的过期 blob 不被回收。
        let source2 = source_file("blue", "png", &valid_png(8, 8, [0, 0, 255]));
        let kept = store.admit(&[source2]).expect("admit");
        let stats = store.sweep_orphans(&HashSet::from([kept[0].id.clone()]), future);
        assert_eq!(
            (stats.removed_staging, stats.removed_blobs),
            (0, 0),
            "referenced blobs survive the sweep"
        );
        assert!(Path::new(&kept[0].blob_path).exists());

        std::fs::remove_dir_all(&root).ok();
    }

    /// INV-MM1-3 store integrity: a blob-name symlink is not a dedup hit,
    /// even when its target happens to contain byte-identical normalized
    /// content. Following it would publish a descriptor that the provider's
    /// final no-follow reader later rejects, and a replacement race could
    /// redirect the comparison outside the attachment store.
    #[cfg(unix)]
    #[test]
    fn dedup_never_follows_a_blob_symlink() {
        use std::os::unix::fs::symlink;

        let (store, root) = temp_store("dedup-symlink");
        let source = source_file("dedup-symlink", "png", &valid_png(8, 8, [1, 2, 3]));
        let first = store
            .admit(std::slice::from_ref(&source))
            .expect("initial admission")
            .remove(0);
        let normalized = std::fs::read(&first.blob_path).expect("read normalized blob");
        let outside = root.join("outside-store-object");
        std::fs::write(&outside, &normalized).expect("write byte-identical target");
        std::fs::remove_file(&first.blob_path).expect("remove real blob");
        symlink(&outside, &first.blob_path).expect("replace blob with symlink");

        let error = store
            .admit(std::slice::from_ref(&source))
            .expect_err("a blob symlink must never be accepted as dedup authority");
        assert!(
            error.to_string().contains("different content"),
            "the occupied unsafe blob name is an integrity conflict: {error}"
        );
        assert_eq!(
            std::fs::read(&outside).expect("target remains intact"),
            normalized
        );
        assert!(
            std::fs::symlink_metadata(&first.blob_path)
                .expect("symlink remains for diagnosis")
                .file_type()
                .is_symlink()
        );
        std::fs::remove_dir_all(root).ok();
    }

    /// INV-MM1-3 also rejects hardlink replacement. A regular-file/type
    /// check is insufficient because an occupied digest name can be linked
    /// to a second namespace entry and still contain byte-identical data.
    /// Accepting it would let later mutation through either name silently
    /// corrupt a durable attachment.
    #[cfg(unix)]
    #[test]
    fn dedup_rejects_a_multiply_linked_blob() {
        let (store, root) = temp_store("dedup-hardlink");
        let source = source_file("dedup-hardlink", "png", &valid_png(8, 8, [4, 5, 6]));
        let first = store
            .admit(std::slice::from_ref(&source))
            .expect("initial admission")
            .remove(0);
        let normalized = std::fs::read(&first.blob_path).expect("read normalized blob");
        let alias = root.join("outside-store-hardlink");
        std::fs::hard_link(&first.blob_path, &alias).expect("create second hardlink name");

        let error = store
            .admit(std::slice::from_ref(&source))
            .expect_err("a multiply-linked blob must not be accepted as store authority");
        assert!(
            error.to_string().contains("different content"),
            "the occupied unsafe blob name is an integrity conflict: {error}"
        );
        assert_eq!(
            std::fs::read(&alias).expect("alias remains intact"),
            normalized
        );
        assert_eq!(
            std::fs::read(&first.blob_path).expect("blob remains intact"),
            normalized
        );
        std::fs::remove_dir_all(root).ok();
    }

    /// INV-MM1-4 bounded-work leg: preserved entries still cost scan work.
    /// A directory full of fresh staging files must stop after the cap rather
    /// than walking the whole directory just because nothing was deleted.
    /// Moving the budget decrement back into the removal branch makes the
    /// inspected count jump above the cap and turns this test red.
    #[test]
    fn orphan_sweep_caps_entries_inspected_not_only_entries_removed() {
        let (store, root) = temp_store("bounded-sweep");
        for index in 0..(SWEEP_ENTRY_CAP + 44) {
            std::fs::write(
                root.join("staging").join(format!("fresh-{index:04}.tmp")),
                b"x",
            )
            .expect("write fresh staging entry");
        }
        let stats = store.sweep_orphans(&HashSet::new(), std::time::SystemTime::now());
        assert_eq!(stats.inspected, SWEEP_ENTRY_CAP);
        assert_eq!(stats.removed_staging, 0);
        assert_eq!(stats.removed_blobs, 0);
        assert_eq!(
            std::fs::read_dir(root.join("staging"))
                .expect("read staging")
                .count(),
            SWEEP_ENTRY_CAP + 44,
            "fresh entries are preserved while the scan remains bounded"
        );
        std::fs::remove_dir_all(root).ok();
    }

    /// INV-MM1-4 cursor leg: a full cap of retained staging entries must not
    /// permanently starve the blob phase. The cursor also has to survive a
    /// fresh `AttachmentStore` handle because production constructs one at
    /// each session arm. Before the fix every invocation restarted at the
    /// first staging entry, so the expired orphan below was never inspected.
    #[test]
    fn orphan_sweep_cursor_survives_reopen_and_reaches_later_phases() {
        let (store, root) = temp_store("sweep-cursor");
        for index in 0..SWEEP_ENTRY_CAP {
            std::fs::write(
                root.join("staging").join(format!("fresh-{index:04}.tmp")),
                b"x",
            )
            .expect("write retained staging entry");
        }
        let orphan = root.join("blobs").join("expired-orphan");
        std::fs::write(&orphan, b"orphan").expect("write orphan");
        let old = std::time::UNIX_EPOCH + std::time::Duration::from_secs(1);
        std::fs::File::options()
            .write(true)
            .open(&orphan)
            .expect("open orphan")
            .set_times(std::fs::FileTimes::new().set_modified(old))
            .expect("age orphan");

        let first = store.sweep_orphans(&HashSet::new(), std::time::SystemTime::now());
        assert_eq!(first.inspected, SWEEP_ENTRY_CAP);
        assert!(orphan.exists(), "first bounded slice only covers staging");
        drop(store);

        let reopened = AttachmentStore::open(root.clone()).expect("reopen store");
        let second = reopened.sweep_orphans(&HashSet::new(), std::time::SystemTime::now());
        assert_eq!(
            second.removed_blobs, 1,
            "the persisted cursor reaches blobs"
        );
        assert!(!orphan.exists(), "the later expired orphan is reclaimed");
        std::fs::remove_dir_all(root).ok();
    }

    /// INV-MM1-3 concurrency leg: a failing batch publishes image X, pauses,
    /// then fails on its second image. A concurrent successful batch importing
    /// the same X must wait: otherwise it can return a descriptor while the
    /// first batch still considers X its own publication and deletes the blob
    /// during rollback. Deleting the guard in `admit` makes the successful
    /// batch finish during the pause and turns the assertion red.
    #[test]
    fn same_store_handles_serialize_admission_batches() {
        let (_, root) = temp_store("concurrent-admission");
        let published = Arc::new(std::sync::Barrier::new(2));
        let release_failure = Arc::new(std::sync::Barrier::new(2));
        let failing = AttachmentStore::open(root.clone())
            .expect("open failing store handle")
            .with_after_publish_hook(Arc::new({
                let published = Arc::clone(&published);
                let release_failure = Arc::clone(&release_failure);
                move |published_count| {
                    if published_count == 1 {
                        published.wait();
                        release_failure.wait();
                    }
                }
            }));
        let successful = AttachmentStore::open(root.clone()).expect("open successful handle");
        let shared = source_file("concurrent", "png", &valid_png(8, 8, [4, 5, 6]));
        let truncated = {
            let valid = valid_png(8, 8, [7, 8, 9]);
            source_file("concurrent-truncated", "png", &valid[..valid.len() / 2])
        };

        let failing_shared = shared.clone();
        let failing_worker =
            std::thread::spawn(move || failing.admit(&[failing_shared, truncated]));
        published.wait();

        let (finished_sender, finished_receiver) = std::sync::mpsc::sync_channel(0);
        let successful_shared = shared.clone();
        let successful_worker = std::thread::spawn(move || {
            finished_sender
                .send(successful.admit(&[successful_shared]))
                .expect("return successful admission result");
        });
        let completed_early =
            match finished_receiver.recv_timeout(std::time::Duration::from_millis(250)) {
                Ok(result) => Some(result),
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => None,
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                    panic!("successful admission worker disconnected")
                }
            };
        release_failure.wait();
        assert!(
            failing_worker
                .join()
                .expect("failing admission worker joins")
                .is_err(),
            "the deliberately truncated second image fails the first batch"
        );
        assert!(
            completed_early.is_none(),
            "a successful batch must not observe a blob still owned by a rollback-capable batch"
        );
        let stored = completed_early
            .unwrap_or_else(|| {
                finished_receiver
                    .recv_timeout(std::time::Duration::from_secs(5))
                    .expect("successful batch resumes after rollback")
            })
            .expect("successful batch succeeds");
        assert_eq!(stored.len(), 1);
        assert!(
            Path::new(&stored[0].blob_path).is_file(),
            "the returned descriptor retains a published blob after the other batch rolls back"
        );
        successful_worker
            .join()
            .expect("successful admission worker joins");
        std::fs::remove_file(shared).ok();
        std::fs::remove_dir_all(root).ok();
    }

    /// INV-MM1-3 批次失败语义（两形态）：
    /// 1. 第二源坏（截断图）→ 整批拒绝、零新 blob、零 staging 残件
    ///    ——半成品绝不留存；
    /// 2. **M1-A 红测**：dedup 命中的既有 blob（历史 journal 已引用）
    ///    在批次失败回收中绝不被删——回收表只记本批 rename 发布的
    ///    blob。删 M1-A 修复（把 dedup 命中也推进回收表）即红。
    #[test]
    fn batch_failure_leaves_no_reachable_artifacts() {
        let (store, root) = temp_store("batch-fail");
        let good = source_file("good", "png", &valid_png(16, 16, [0, 255, 0]));
        let truncated = {
            let full = valid_png(16, 16, [255, 0, 0]);
            source_file("trunc", "png", &full[..full.len() / 2])
        };
        let result = store.admit(&[good, truncated.clone()]);
        assert!(result.is_err(), "the truncated image fails the batch");
        assert_eq!(
            std::fs::read_dir(root.join("blobs")).unwrap().count(),
            0,
            "zero published blobs after a failed batch"
        );
        assert_eq!(
            std::fs::read_dir(root.join("staging")).unwrap().count(),
            0,
            "zero staging leftovers after a failed batch"
        );

        // —— M1-A 形态：先接纳图 X（journal 引用 blob X），再提交
        // [同内容图（dedup 命中）, 坏图] → 整批失败 → blob X 原样
        // 存活且内容完好。——
        let keep = source_file("keep", "png", &valid_png(24, 24, [9, 9, 9]));
        let first = store.admit(std::slice::from_ref(&keep)).expect("admit X");
        let blob_x = PathBuf::from(&first[0].blob_path);
        let blob_x_bytes = std::fs::read(&blob_x).unwrap();
        let same_pixels = source_file("keep-copy", "png", &valid_png(24, 24, [9, 9, 9]));
        let result = store.admit(&[same_pixels, truncated]);
        assert!(result.is_err(), "the truncated image still fails the batch");
        assert!(
            blob_x.is_file(),
            "a dedup-hit blob referenced by earlier history survives the failed batch"
        );
        assert_eq!(
            std::fs::read(&blob_x).unwrap(),
            blob_x_bytes,
            "the surviving blob keeps its exact content"
        );
        // 存活的 blob 内容仍有效：同内容再接纳 dedup 命中同一 id。
        let again = store.admit(&[keep]).expect("re-admit after failed batch");
        assert_eq!(again[0].id, first[0].id, "the surviving blob still dedups");
        assert_eq!(std::fs::read_dir(root.join("blobs")).unwrap().count(), 1);
        std::fs::remove_dir_all(&root).ok();
    }

    /// INV-MM1-批次界：超过 8 张直接拒绝（任何 I/O 之前）。
    #[test]
    fn batch_count_bound_rejects_before_any_io() {
        let (store, root) = temp_store("count");
        let sources: Vec<PathBuf> = (0..9)
            .map(|index| source_file(&format!("c{index}"), "png", &valid_png(4, 4, [1, 2, 3])))
            .collect();
        let error = store.admit(&sources).unwrap_err();
        assert!(error.to_string().contains("at most 8 images"));
        assert_eq!(std::fs::read_dir(root.join("blobs")).unwrap().count(), 0);
        std::fs::remove_dir_all(&root).ok();
    }

    /// MM-3 raises the per-source ceiling to 8 MiB, making the independent
    /// 32 MiB raw-batch fence reachable. Five individually valid 7 MiB PNG
    /// sources must fail before decode/staging.
    #[test]
    fn raw_batch_byte_bound_is_independent_from_the_per_image_bound() {
        let (store, root) = temp_store("raw-batch");
        let mut sources = Vec::new();
        for index in 0..5 {
            let path = source_file(
                &format!("raw{index}"),
                "png",
                &valid_png(1, 1, [index, 2, 3]),
            );
            std::fs::OpenOptions::new()
                .write(true)
                .open(&path)
                .unwrap()
                .set_len(7 * 1024 * 1024)
                .unwrap();
            sources.push(path);
        }
        let error = store.admit(&sources).unwrap_err();
        assert!(
            error.to_string().contains("raw-batch")
                || error.to_string().contains("33554432-byte batch limit"),
            "{error}"
        );
        assert_eq!(std::fs::read_dir(root.join("blobs")).unwrap().count(), 0);
        assert_eq!(std::fs::read_dir(root.join("staging")).unwrap().count(), 0);
        for source in sources {
            let _ = std::fs::remove_file(source);
        }
        std::fs::remove_dir_all(&root).ok();
    }

    /// Browser raw staging is validated before registration, but that early
    /// check is file-backed: it must retain the same complete-decode and
    /// format-family rejection policy as the byte-owning admission pass.
    #[test]
    fn draft_file_validation_accepts_complete_png_and_rejects_truncation() {
        let valid = valid_png(32, 24, [5, 7, 11]);
        let path = source_file("draft-file-validation", "png", &valid);
        validate_draft_source(&path).expect("complete staged png is valid");

        std::fs::write(&path, &valid[..valid.len() - 10]).expect("truncate staged png");
        let error = validate_draft_source(&path).expect_err("truncated staged png rejects");
        assert!(error.contains("decode failed"), "{error}");
        std::fs::remove_file(path).ok();
    }

    /// INV-MM1-S3：S1 magic 通过但内容损坏（截断）在解码阶段拒绝；
    /// 超长边输入被缩到 ≤2048；transparent alpha 图输出不透明 PNG。
    #[test]
    fn normalize_validates_decodes_and_scales() {
        // 截断 PNG：magic 完好，解码必败。
        let full = valid_png(32, 32, [9, 9, 9]);
        let error = normalize_image(&full[..full.len() - 10], ImageFamily::Png).unwrap_err();
        assert!(error.contains("decode failed"), "{error}");

        // 3000×2000 → 长边 2048（等比）。
        let big = valid_png(3000, 2000, [5, 5, 5]);
        let normalized = normalize_image(&big, ImageFamily::Png).expect("normalize");
        assert_eq!(normalized.width.max(normalized.height), 2048);

        // 带真实尺寸的端到端：admit 的 original_* 记源尺寸。
        let (store, root) = temp_store("scale");
        let source = source_file("big", "png", &big);
        let stored = store.admit(&[source]).expect("admit");
        assert_eq!(stored[0].width, 2048);
        assert_eq!(stored[0].height, 1365); // 2000 × 2048/3000
        assert_eq!(stored[0].original_width, 3000);
        assert_eq!(stored[0].original_height, 2000);
        std::fs::remove_dir_all(&root).ok();
    }

    /// INV-MM1-S3 alpha 合成（M1-B 补断言）：透明像素落到白底
    /// （sRGB 假定记档），输出为不透明图。RGBA(255,0,0,128) →
    /// (255,127,127)；RGBA(0,0,0,0) → (255,255,255)。
    #[test]
    fn normalize_blends_alpha_onto_white() {
        use image::GenericImageView as _;
        let mut rgba = image::RgbaImage::from_pixel(8, 8, image::Rgba([255, 0, 0, 128]));
        rgba.put_pixel(0, 0, image::Rgba([0, 0, 0, 0]));
        let mut encoded = Vec::new();
        image::DynamicImage::ImageRgba8(rgba)
            .write_to(
                &mut std::io::Cursor::new(&mut encoded),
                image::ImageFormat::Png,
            )
            .expect("encode rgba png");
        let normalized = normalize_image(&encoded, ImageFamily::Png).expect("normalize");
        assert_eq!(normalized.media_type, "image/png");
        let decoded = image::load_from_memory(&normalized.bytes).expect("re-decode");
        assert_eq!(decoded.get_pixel(0, 0), image::Rgba([255, 255, 255, 255]));
        assert_eq!(decoded.get_pixel(4, 4), image::Rgba([255, 127, 127, 255]));
    }

    /// INV-MM1-S3 EXIF orientation（M1-B 补断言）：orientation=6
    ///（rotate 90°CW）在规范化前应用——40×20 的源输出 20×40；无
    /// EXIF 的同图保持原尺寸（对照）。删 `apply_orientation` 即红。
    #[test]
    fn normalize_applies_exif_orientation() {
        let plain = valid_jpeg(40, 20, [80, 80, 80]);
        let oriented = with_exif_orientation(&plain, 6);
        let normalized = normalize_image(&oriented, ImageFamily::Jpeg).expect("normalize");
        assert_eq!((normalized.width, normalized.height), (20, 40));
        let control = normalize_image(&plain, ImageFamily::Jpeg).expect("control");
        assert_eq!((control.width, control.height), (40, 20));
    }

    /// INV-MM1-3 dedup 冲突（M1-B 红测）：blobs/<digest> 被外部替换
    /// 成不同内容 → 逐字节比对后拒绝，既有文件（哪怕已被篡改）原样
    /// 保留——绝不覆盖。同长不同内容与异长两形态；把比对修复退化成
    /// 仅长度短路（同长放行）即红。
    #[test]
    fn dedup_conflict_rejects_without_overwriting() {
        let (store, root) = temp_store("dedup-conflict");
        let source = source_file("conf", "png", &valid_png(32, 32, [6, 6, 6]));
        let first = store.admit(std::slice::from_ref(&source)).expect("admit");
        let blob = PathBuf::from(&first[0].blob_path);
        let published = std::fs::read(&blob).unwrap();

        // 同长不同内容：翻转末字节（IEND CRC 位）。
        let mut same_len = published.clone();
        let last = same_len.len() - 1;
        same_len[last] ^= 0xFF;
        std::fs::write(&blob, &same_len).unwrap();
        let error = store.admit(std::slice::from_ref(&source)).unwrap_err();
        assert!(error.to_string().contains("different content"), "{}", error);
        assert_eq!(
            std::fs::read(&blob).unwrap(),
            same_len,
            "the tampered blob is never overwritten"
        );

        // 异长：追加一字节。
        let mut different_len = published;
        different_len.push(0);
        std::fs::write(&blob, &different_len).unwrap();
        let error = store.admit(&[source]).unwrap_err();
        assert!(error.to_string().contains("different content"), "{}", error);
        assert_eq!(
            std::fs::read(&blob).unwrap().len(),
            different_len.len(),
            "still never overwritten"
        );

        // 冲突拒绝不留任何新产物。
        assert_eq!(std::fs::read_dir(root.join("blobs")).unwrap().count(), 1);
        assert_eq!(std::fs::read_dir(root.join("staging")).unwrap().count(), 0);
        std::fs::remove_dir_all(&root).ok();
    }

    /// M1-F（2026-08-27 负责人裁定：解码器裁剪为 png+jpeg）红测：
    /// gif/webp 的 S1 magic 识别保留（格式无关），接纳在 S3 以点名
    /// 格式的可行动错误干净拒绝——不 panic、不静默、零产物。删命名
    /// 分支（落回解码器泛型 unsupported 错误、丢失可行动文案）即红。
    #[test]
    fn unsupported_families_are_recognized_then_rejected_with_actionable_errors() {
        let (store, root) = temp_store("unsupported");

        // WebP：VP8L 头（S1 可识别、尺寸可得）。
        let webp = source_file("pic", "webp", &webp_vp8l_bytes(64, 48));
        let (family, dims) = media::validate_source(&webp).expect("S1 still recognizes WebP");
        assert_eq!(family, ImageFamily::Webp);
        assert_eq!(dims, Some((64, 48)));
        let error = store.admit(&[webp]).unwrap_err();
        let message = error.to_string();
        assert!(message.contains("WebP"), "{message}");
        assert!(
            message.contains("re-save the image as PNG or JPEG"),
            "the rejection names the format and the remedy: {message}"
        );

        // GIF：logical screen 头同理。
        let gif = source_file("anim", "gif", &gif_bytes(320, 240));
        let (family, _) = media::validate_source(&gif).expect("S1 still recognizes GIF");
        assert_eq!(family, ImageFamily::Gif);
        let error = store.admit(&[gif]).unwrap_err();
        let message = error.to_string();
        assert!(message.contains("GIF"), "{message}");
        assert!(
            message.contains("re-save the image as PNG or JPEG"),
            "{message}"
        );

        // 干净拒绝：零 blob、零 staging 残件。
        assert_eq!(std::fs::read_dir(root.join("blobs")).unwrap().count(), 0);
        assert_eq!(std::fs::read_dir(root.join("staging")).unwrap().count(), 0);
        std::fs::remove_dir_all(&root).ok();
    }

    /// 经 image 编码器生成合法 JPEG（EXIF 测试的图源）。
    fn valid_jpeg(width: u32, height: u32, color: [u8; 3]) -> Vec<u8> {
        let rgb = image::RgbImage::from_pixel(width, height, image::Rgb(color));
        let mut out = Vec::new();
        let encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, 85);
        image::DynamicImage::ImageRgb8(rgb)
            .write_with_encoder(encoder)
            .expect("encode jpeg");
        out
    }

    /// 在 SOI 后插入携带 Orientation 的 EXIF APP1 段（little-endian
    /// TIFF IFD0，单条目）。仅为测试构造——生产代码不产 EXIF。
    fn with_exif_orientation(jpeg: &[u8], orientation: u16) -> Vec<u8> {
        let mut app1 = Vec::new();
        app1.extend_from_slice(b"Exif\0\0");
        app1.extend_from_slice(b"II");
        app1.extend_from_slice(&0x2A_u16.to_le_bytes());
        app1.extend_from_slice(&8_u32.to_le_bytes()); // IFD0 at offset 8
        app1.extend_from_slice(&1_u16.to_le_bytes()); // one entry
        app1.extend_from_slice(&0x0112_u16.to_le_bytes()); // Orientation
        app1.extend_from_slice(&3_u16.to_le_bytes()); // SHORT
        app1.extend_from_slice(&1_u32.to_le_bytes()); // count
        app1.extend_from_slice(&orientation.to_le_bytes());
        app1.extend_from_slice(&0_u16.to_le_bytes()); // value padding
        app1.extend_from_slice(&0_u32.to_le_bytes()); // no next IFD
        let mut out = Vec::with_capacity(jpeg.len() + app1.len() + 4);
        out.extend_from_slice(&jpeg[..2]); // SOI
        out.extend_from_slice(&[0xFF, 0xE1]);
        out.extend_from_slice(&((app1.len() + 2) as u16).to_be_bytes());
        out.extend_from_slice(&app1);
        out.extend_from_slice(&jpeg[2..]);
        out
    }

    /// GIF89a 头 + logical screen descriptor（与 media.rs 测试同构：
    /// 仅头部，供 S1 识别；M1-F 后接纳必然在 S3 拒绝）。
    fn gif_bytes(width: u16, height: u16) -> Vec<u8> {
        let mut bytes = b"GIF89a".to_vec();
        bytes.extend_from_slice(&width.to_le_bytes());
        bytes.extend_from_slice(&height.to_le_bytes());
        bytes.extend_from_slice(&[0x77, 0x00]); // GCT flag + background
        bytes
    }

    /// 最小 VP8L WebP 头（与 media.rs 测试同构）。
    fn webp_vp8l_bytes(width: u32, height: u32) -> Vec<u8> {
        let packed = (width - 1) | ((height - 1) << 14);
        let mut payload = vec![0x2F_u8];
        payload.extend_from_slice(&packed.to_le_bytes());
        payload.extend_from_slice(&[0xFF, 0xFF, 0xFF, 0xFF]); // ARGB filler
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"RIFF");
        bytes.extend_from_slice(&((payload.len() + 4) as u32).to_le_bytes());
        bytes.extend_from_slice(b"WEBP");
        bytes.extend_from_slice(b"VP8L");
        bytes.extend_from_slice(&(payload.len() as u32).to_le_bytes());
        bytes.extend_from_slice(&payload);
        bytes
    }
}
