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
//!   总量 ≤16 MiB；单图源 ≤4 MiB（S3 落地前的既有界维持——见
//!   实施计划 S1 裁定）。
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
use sha2::{Digest, Sha256};
use std::collections::HashSet;
use std::io::Write as _;
use std::path::{Path, PathBuf};

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

pub(crate) struct AttachmentStore {
    root: PathBuf,
}

impl AttachmentStore {
    /// 打开（或创建）会话附件域。`root` = `<session_dir>/attachments`。
    pub(crate) fn open(root: PathBuf) -> Result<Self, std::io::Error> {
        std::fs::create_dir_all(root.join("blobs"))?;
        std::fs::create_dir_all(root.join("staging"))?;
        set_private_dir(&root)?;
        set_private_dir(&root.join("blobs"))?;
        set_private_dir(&root.join("staging"))?;
        Ok(Self { root })
    }

    /// 接纳一批源图（INV-MM1-3 全序）。任何失败保证：零新 blob、
    /// 零 staging 残件、调用方可安全整体失败（journal 无痕）。
    pub(crate) fn admit(
        &self,
        sources: &[PathBuf],
    ) -> Result<Vec<StoredAttachment>, AdmissionError> {
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
        let mut raw_total = 0u64;
        for source in sources {
            let (_family, dims) =
                media::validate_source(source).map_err(AdmissionError::Rejected)?;
            let metadata = std::fs::metadata(source).map_err(|error| {
                AdmissionError::Rejected(format!("{}: {error}", source.display()))
            })?;
            if !metadata.is_file() {
                return Err(AdmissionError::Rejected(format!(
                    "not a file: {}",
                    source.display()
                )));
            }
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
            // 头部已知的超像素在 validate_source 拒过；无头尺寸的格式
            //（部分 WebP 变体）在下面解码阶段强制。
            let _ = dims;
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
        let mut published_blobs: Vec<PathBuf> = Vec::new();
        // 本批创建过的 staging 路径（含写失败半成品，M1-C）：批次失败
        // 全清——模块文档「清理本批 staging」由这里兜底。已 rename 走
        // 的条目再删是 ENOENT，无害。
        let mut staging_files: Vec<PathBuf> = Vec::new();
        let mut normalized_total = 0u64;
        let result = (|| -> Result<(), AdmissionError> {
            for source in sources {
                let (family, original_dims) =
                    media::validate_source(source).map_err(AdmissionError::Rejected)?;
                let bytes = std::fs::read(source).map_err(|error| {
                    AdmissionError::Rejected(format!("{}: {error}", source.display()))
                })?;
                if bytes.len() as u64 > media::MAX_ATTACHMENT_BYTES {
                    // 源在预检与读入之间被替换成大文件：按读入实测拒绝。
                    return Err(AdmissionError::Rejected(format!(
                        "image grew past the byte cap while admitting: {}",
                        source.display()
                    )));
                }
                let normalized = normalize_image(&bytes, family).map_err(|reason| {
                    AdmissionError::Rejected(format!("{reason}: {}", source.display()))
                })?;
                normalized_total = normalized_total
                    .checked_add(normalized.bytes.len() as u64)
                    .ok_or_else(|| AdmissionError::Rejected("batch size overflow".into()))?;
                if normalized_total > MAX_NORMALIZED_BATCH_BYTES {
                    return Err(AdmissionError::Rejected(format!(
                        "normalized message images exceed the {MAX_NORMALIZED_BATCH_BYTES}-byte budget"
                    )));
                }
                let id = digest_hex(&normalized.bytes);
                let blob_path = self.root.join("blobs").join(&id);
                if blob_path.exists() {
                    // dedup 命中（内容寻址）。长度先行短路（读量有界），
                    // 再逐字节比对：同长不同内容同样视为异常冲突拒绝
                    // ——绝不覆盖，既有 blob 哪怕已被外部篡改也原样保留
                    // 交日志/人工处置（digest 与内容不符即完整性破产，
                    // 静默放行等于让历史消息读出被替换的字节）。
                    let length_matches = std::fs::metadata(&blob_path).is_ok_and(|existing| {
                        existing.is_file()
                            && !existing.file_type().is_symlink()
                            && existing.len() == normalized.bytes.len() as u64
                    });
                    let identical = length_matches
                        && std::fs::read(&blob_path)
                            .is_ok_and(|existing| existing == normalized.bytes);
                    if !identical {
                        return Err(AdmissionError::Rejected(format!(
                            "blob {id} already exists with different content"
                        )));
                    }
                } else {
                    // staging create-new(0600) → fsync → rename → dir fsync。
                    let staging_name = format!("{}.tmp", uuid::Uuid::new_v4().simple());
                    let staging_path = self.root.join("staging").join(&staging_name);
                    staging_files.push(staging_path.clone());
                    write_private_file_sync(&staging_path, &normalized.bytes)?;
                    if let Some(parent) = blob_path.parent() {
                        std::fs::rename(&staging_path, &blob_path).map_err(AdmissionError::Io)?;
                        sync_dir(parent).map_err(AdmissionError::Io)?;
                    }
                    published_blobs.push(blob_path.clone());
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
            }
            Ok(())
        })();
        match result {
            Ok(()) => Ok(stored_all),
            Err(error) => {
                // 批次失败：只回收本批 rename 发布的 blob（journal 尚未
                // 附加、无人引用——引用只来自 journal），并清空本批全部
                // staging 路径（含写失败残件）——零可达半成品。
                for path in &published_blobs {
                    let _ = std::fs::remove_file(path);
                }
                for path in &staging_files {
                    let _ = std::fs::remove_file(path);
                }
                Err(error)
            }
        }
    }

    /// INV-MM1-4：按引用集合 + TTL 清理。返回 (清掉的 staging 数,
    /// 清掉的 blob 数)。有界（≤ [`SWEEP_ENTRY_CAP`] 个条目/次）；
    /// 从不触碰平铺 legacy 文件（`<uuid>.<ext>`，journal 以路径引用）。
    pub(crate) fn sweep_orphans(
        &self,
        referenced: &HashSet<String>,
        now: std::time::SystemTime,
    ) -> (usize, usize) {
        let mut removed_staging = 0usize;
        let mut removed_blobs = 0usize;
        let ttl = std::time::Duration::from_secs(ORPHAN_TTL_SECS);
        let expired = |entry: &Result<std::fs::DirEntry, std::io::Error>| -> bool {
            entry
                .as_ref()
                .ok()
                .and_then(|entry| entry.metadata().ok())
                .and_then(|metadata| metadata.modified().ok())
                .is_some_and(|modified| now.duration_since(modified).unwrap_or_default() > ttl)
        };
        let mut work = SWEEP_ENTRY_CAP;
        if let Ok(entries) = std::fs::read_dir(self.root.join("staging")) {
            for entry in entries {
                if work == 0 {
                    break;
                }
                if expired(&entry)
                    && std::fs::remove_file(
                        entry.as_ref().ok().map(|e| e.path()).unwrap_or_default(),
                    )
                    .is_ok()
                {
                    removed_staging += 1;
                    work -= 1;
                }
            }
        }
        if let Ok(entries) = std::fs::read_dir(self.root.join("blobs")) {
            for entry in entries {
                if work == 0 {
                    break;
                }
                let Ok(entry) = entry else { continue };
                let unreferenced = entry
                    .file_name()
                    .to_str()
                    .is_none_or(|name| !referenced.contains(name));
                let path = entry.path();
                let expired = std::fs::metadata(&path)
                    .ok()
                    .and_then(|metadata| metadata.modified().ok())
                    .is_some_and(|modified| now.duration_since(modified).unwrap_or_default() > ttl);
                if unreferenced && expired && std::fs::remove_file(entry.path()).is_ok() {
                    removed_blobs += 1;
                    work -= 1;
                }
            }
        }
        (removed_staging, removed_blobs)
    }
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
    let format = match family {
        ImageFamily::Png => image::ImageFormat::Png,
        ImageFamily::Jpeg => image::ImageFormat::Jpeg,
        // 负责人裁定（2026-08-27 审计 M1-F）：解码器只采购 png+jpeg。
        // S1 的四族 magic 识别保留（格式无关）；gif/webp 在此以点名
        // 格式的可行动错误干净拒绝——不 panic、不静默。加回 webp 的
        // 触发条件见实施计划 S3 采购记录（dogfood 粘贴被拒 ≥2 次）。
        // 负责人裁定（2026-08-27 审计 M1-F）：解码器只采购 png+jpeg。
        // S1 的四族 magic 识别保留（格式无关）；gif/webp 在此以点名
        // 格式的可行动错误干净拒绝——不 panic、不静默。加回 webp 的
        // 触发条件见实施计划 S3 采购记录（dogfood 粘贴被拒 ≥2 次）。
        ImageFamily::Gif => {
            return Err(
                "GIF images are not supported yet; please re-save the image as PNG or JPEG".into(),
            );
        }
        ImageFamily::Webp => {
            return Err(
                "WebP images are not supported yet; please re-save the image as PNG or JPEG".into(),
            );
        }
    };
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

/// create-new(0600) + 写满 + fsync。存在即失败（uuid 命名，冲突即异常）。
fn write_private_file_sync(path: &Path, bytes: &[u8]) -> Result<(), AdmissionError> {
    use std::os::unix::fs::OpenOptionsExt as _;
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        options.mode(0o600);
    }
    let mut file = options.open(path).map_err(AdmissionError::Io)?;
    file.write_all(bytes).map_err(AdmissionError::Io)?;
    file.sync_all().map_err(AdmissionError::Io)?;
    Ok(())
}

fn set_private_dir(path: &Path) -> Result<(), std::io::Error> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let metadata = std::fs::metadata(path)?;
        let mut permissions = metadata.permissions();
        permissions.set_mode(0o700);
        std::fs::set_permissions(path, permissions)?;
    }
    #[cfg(not(unix))]
    {
        // Windows 无 POSIX 位；目录已由 create_dir_all 建在用户 profile
        // 下的 ~/.clat 内（ACL 继承），不额外处理（记档）。
        let _ = path;
    }
    Ok(())
}

fn sync_dir(path: &Path) -> Result<(), std::io::Error> {
    #[cfg(unix)]
    {
        let file = std::fs::File::open(path)?;
        file.sync_all()
    }
    #[cfg(not(unix))]
    {
        // Windows 的目录元数据随文件写入落盘；无 fsync(3) 对应物。
        let _ = path;
        Ok(())
    }
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
        let (s, b) = store.sweep_orphans(&HashSet::new(), now);
        assert_eq!((s, b), (0, 0), "fresh unreferenced blobs stay within TTL");
        let (s, b) = store.sweep_orphans(&HashSet::from([first.id.clone()]), now);
        assert_eq!((s, b), (0, 0));
        // 时钟推进 TTL 以上 = 现有 blob 全部"过期"。
        let future = now + std::time::Duration::from_secs(ORPHAN_TTL_SECS + 60);
        let (s, b) = store.sweep_orphans(&HashSet::new(), future);
        assert_eq!((s, b), (0, 1), "expired unreferenced blob reclaimed");
        assert!(!Path::new(&first.blob_path).exists());
        // 引用集合保护的过期 blob 不被回收。
        let source2 = source_file("blue", "png", &valid_png(8, 8, [0, 0, 255]));
        let kept = store.admit(&[source2]).expect("admit");
        let (s, b) = store.sweep_orphans(&HashSet::from([kept[0].id.clone()]), future);
        assert_eq!((s, b), (0, 0), "referenced blobs survive the sweep");
        assert!(Path::new(&kept[0].blob_path).exists());

        std::fs::remove_dir_all(&root).ok();
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
