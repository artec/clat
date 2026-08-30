//! 本地图片附件的媒介工具（2026-08-19，用户附加图片进对话；
//! MM-1 S1 拆出**校验**职责，见 `docs/todo/mm1-attachment-domain.md`）。
//!
//! 职责（逐步拆分中，全部零第三方依赖）：
//! - **校验**（S1）：magic 字节族嗅探、扩展名 ↔ 字节族一致性、
//!   读头阶段的像素/字节资源上限（INV-MM1-1/2）；
//! - 扩展名 → MIME（附加入口的合法性判定）；
//! - PNG/JPEG/GIF/WebP 头解析出像素尺寸（只读头部，不解码像素）；
//! - 视觉 token 估算：按 512px 网格切块（tile），每 tile 一个保守
//!   常数——图片进上下文的计量单位是视觉 token 而非 base64 字节，
//!   自动压缩的预算触发依赖这个估算（M5）。
//!
//! 规范化（decode/EXIF/重编码）是 S3 的职责，依赖解码库决策（采购
//! 记录见实施计划）。估算是有意的启发式：宁可高估触发压缩，不可
//! 低估撑爆窗口；后续可用 journal 里的真实 input_tokens 校准。

use std::path::Path;

/// 单个附件的字节上限：入口拒绝，不进会话目录（M7 防线三）。
/// MM-3 入口启用后的源图上限。S3 已保证完整解码、长边 2048 resize 与
/// 单图规范化 ≤4,000,000 bytes，因此源可放宽到冻结方案的 8 MiB；
/// provider 请求仍只看到规范化后的更窄预算。
pub(crate) const MAX_ATTACHMENT_BYTES: u64 = 8 * 1024 * 1024;

/// INV-MM1-2：单图解码像素上限（16M px）。读头阶段先判（方案 MM-1
/// 硬默认）——这是权威闸。S3 解码器的 Limits 是更宽的内存兜底而非
/// 同值强制（差异记档见 attachments.rs 的 M1-D 注：能到达解码的
/// png/jpeg 头尺寸即解码尺寸，已在此拦下）。
pub(crate) const MAX_DECODED_PIXELS: u64 = 16 * 1024 * 1024;

/// tile 边长（像素）：视觉模型普遍按 ~512px 网格切块计费。
const TILE_PIXELS: u64 = 512;
/// 单 tile 保守 token 数（各厂商 280–400 不等）。
const TOKENS_PER_TILE: u64 = 350;
/// 单图固定开销。
const BASE_TOKENS: u64 = 100;
/// 尺寸解析失败（未知格式/头损坏）时的保守常数。
const FALLBACK_TOKENS: u64 = 1600;
/// INV-MM2-4 保护系数：基线公式之上 ×2.0（首版保守口径，MM-0 实测
/// token 随像素扩张方向一致但样本不足以去系数）。下调只允许依据
/// 留档 campaign 的 observed upper error + 20% margin。
pub(crate) const IMAGE_TOKEN_SAFETY_FACTOR: u64 = 2;
/// Durable request/header identity for the visual token formula. Changing the
/// formula or its safety factor requires a new value so replay diagnostics do
/// not compare unlike estimates.
pub(crate) const IMAGE_TOKEN_ESTIMATOR_VERSION: &str = "tile-512-v1-sf2";
/// MM-0 live calibration set used to justify the current conservative factor.
pub(crate) const IMAGE_TOKEN_CALIBRATION_VERSION: &str = "glm-mm0-2026-08-27-v1";

/// 扩展名 → MIME；不认识的扩展名返回 None（附加入口拒绝）。
pub(crate) fn media_type_for_path(path: &Path) -> Option<&'static str> {
    match path.extension()?.to_str()?.to_ascii_lowercase().as_str() {
        "png" => Some("image/png"),
        "jpg" | "jpeg" => Some("image/jpeg"),
        "webp" => Some("image/webp"),
        "gif" => Some("image/gif"),
        _ => None,
    }
}

/// magic 嗅探出的图片字节族（INV-MM1-1：字节为准，声明不可信——
/// MM-0 probe 实证服务端同样按字节嗅探）。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ImageFamily {
    Png,
    Jpeg,
    Gif,
    Webp,
}

impl ImageFamily {
    fn from_extension(path: &Path) -> Option<Self> {
        match path.extension()?.to_str()?.to_ascii_lowercase().as_str() {
            "png" => Some(ImageFamily::Png),
            "jpg" | "jpeg" => Some(ImageFamily::Jpeg),
            "webp" => Some(ImageFamily::Webp),
            "gif" => Some(ImageFamily::Gif),
            _ => None,
        }
    }
}

/// Read only the magic prefix needed to classify a path. Production
/// attachment admission uses `validate_source_header` on a held descriptor;
/// this path helper remains for diagnostics/tests that do not mint authority.
#[cfg(all(test, unix))]
pub(crate) fn sniff_image_family(path: &Path) -> Option<ImageFamily> {
    use std::io::Read as _;
    let mut file = std::fs::File::open(path).ok()?;
    let mut header = [0u8; 16];
    let read = file.read(&mut header).ok()?;
    sniff_image_family_bytes(&header[..read])
}

fn sniff_image_family_bytes(header: &[u8]) -> Option<ImageFamily> {
    if header.starts_with(&[0x89, b'P', b'N', b'G', b'\r', b'\n', 0x1a, b'\n']) {
        return Some(ImageFamily::Png);
    }
    if header.len() >= 2 && header[0] == 0xFF && header[1] == 0xD8 {
        return Some(ImageFamily::Jpeg);
    }
    if header.len() >= 6 && (&header[0..3] == b"GIF") {
        let version = &header[3..6];
        if version == b"87a" || version == b"89a" {
            return Some(ImageFamily::Gif);
        }
        return None;
    }
    if header.len() >= 12 && &header[0..4] == b"RIFF" && &header[8..12] == b"WEBP" {
        return Some(ImageFamily::Webp);
    }
    None
}

/// Verify a durable MIME claim against image magic already read through the
/// caller's authority. Content-address verification proves byte identity but
/// does not bind descriptor metadata to those bytes; provider/PWA exposure
/// must therefore perform this independent check.
pub(crate) fn media_type_matches_bytes(media_type: &str, bytes: &[u8]) -> bool {
    matches!(
        (media_type, sniff_image_family_bytes(bytes)),
        ("image/png", Some(ImageFamily::Png))
            | ("image/jpeg", Some(ImageFamily::Jpeg))
            | ("image/gif", Some(ImageFamily::Gif))
            | ("image/webp", Some(ImageFamily::Webp))
    )
}

/// INV-MM1-1/2 的接纳校验（S1 生产路径）：扩展名声明的字节族必须与
/// magic 嗅探一致（伪扩展/polyglot 拒），头部可得的像素尺寸不得超过
/// [`MAX_DECODED_PIXELS`]（解码炸弹在读头阶段先挡）。成功返回实测
/// 字节族与可得尺寸（None = 头部无尺寸信息，S3 全解码后该档关闭）。
pub(crate) fn validate_source(path: &Path) -> Result<(ImageFamily, Option<(u64, u64)>), String> {
    use std::io::Read as _;
    let file = std::fs::File::open(path)
        .map_err(|error| format!("cannot open image {}: {error}", path.display()))?;
    let mut header = Vec::new();
    file.take(256 * 1024)
        .read_to_end(&mut header)
        .map_err(|error| format!("cannot read image {}: {error}", path.display()))?;
    validate_source_header(path, &header)
}

/// Validate an image from bytes read through a caller-owned descriptor. This
/// is the TOCTOU-safe entrypoint for private staging and attachment admission:
/// extension policy still comes from the display path, while magic and
/// dimensions come only from the already-open file.
pub(crate) fn validate_source_header(
    path: &Path,
    header: &[u8],
) -> Result<(ImageFamily, Option<(u64, u64)>), String> {
    let declared = ImageFamily::from_extension(path)
        .ok_or_else(|| format!("unsupported image type: {}", path.display()))?;
    let sniffed = sniff_image_family_bytes(header).ok_or_else(|| {
        format!(
            "not a recognizable PNG/JPEG/GIF/WebP image: {}",
            path.display()
        )
    })?;
    if declared != sniffed {
        return Err(format!(
            "image extension .{} does not match its content ({:?}): {}",
            path.extension()
                .and_then(|e| e.to_str())
                .unwrap_or_default(),
            sniffed,
            path.display()
        ));
    }
    let dimensions = match sniffed {
        ImageFamily::Png | ImageFamily::Jpeg => image_dimensions_bytes(header),
        ImageFamily::Gif => gif_dimensions_bytes(header),
        ImageFamily::Webp => webp_dimensions_bytes(header),
    };
    if let Some((width, height)) = dimensions
        && width
            .checked_mul(height)
            .is_none_or(|pixels| pixels > MAX_DECODED_PIXELS)
    {
        return Err(format!(
            "image exceeds the {MAX_DECODED_PIXELS}-pixel limit ({width}x{height}): {}",
            path.display()
        ));
    }
    Ok((sniffed, dimensions))
}

/// GIF logical screen descriptor：头 6 字节版本 + LE u16 宽高。
fn gif_dimensions_bytes(header: &[u8]) -> Option<(u64, u64)> {
    if header.len() < 10 {
        return None;
    }
    let width = u16::from_le_bytes([header[6], header[7]]) as u64;
    let height = u16::from_le_bytes([header[8], header[9]]) as u64;
    (width > 0 && height > 0).then_some((width, height))
}

/// WebP 帧尺寸：VP8L（无损，14bit 打包）/ VP8X（扩展，24bit canvas）
/// / VP8（有损 keyframe）。读头 64 字节内判别；不认识的 chunk 布局
/// 返回 None（unknown-dims 档，S3 关闭）。
fn webp_dimensions_bytes(header: &[u8]) -> Option<(u64, u64)> {
    if header.len() < 12 || &header[0..4] != b"RIFF" || &header[8..12] != b"WEBP" {
        return None;
    }
    let chunk = &header[12..];
    if chunk.len() >= 5 && &chunk[0..4] == b"VP8L" && chunk[8] == 0x2F {
        // chunk 头(8) + 1 字节签名后是 LE u32：低 14 位 w-1、次 14 位 h-1。
        let packed = u32::from_le_bytes([chunk[9], chunk[10], chunk[11], chunk[12]]);
        let width = (packed & 0x3FFF) as u64 + 1;
        let height = ((packed >> 14) & 0x3FFF) as u64 + 1;
        return Some((width, height));
    }
    if chunk.len() >= 10 && &chunk[0..4] == b"VP8X" {
        // chunk 头(8) + flags(1) + 24bit canvas：3 字节 LE w-1、3 字节 h-1。
        let width = (chunk[9] as u64 | (chunk[10] as u64) << 8 | (chunk[11] as u64) << 16) + 1;
        let height = (chunk[12] as u64 | (chunk[13] as u64) << 8 | (chunk[14] as u64) << 16) + 1;
        return Some((width, height));
    }
    if chunk.len() >= 18 && &chunk[0..4] == b"VP8 " {
        // chunk 头(8) + frame tag(3) + sync code(3, 9D 01 2A) 之后是
        // 两个 14-bit LE 尺寸（各占 u16，高 2 位保留）。
        if chunk[11..14] != [0x9D, 0x01, 0x2A] {
            return None;
        }
        let width = u16::from_le_bytes([chunk[14], chunk[15]]) as u64 & 0x3FFF;
        let height = u16::from_le_bytes([chunk[16], chunk[17]]) as u64 & 0x3FFF;
        return (width > 0 && height > 0).then_some((width, height));
    }
    None
}

/// PNG（IHDR）与 JPEG（SOF0/1/2 帧头）的像素尺寸。只读文件头，不
/// 解码；其他格式/损坏头返回 None（调用方走保守 token 常数）。
pub(crate) fn image_dimensions(path: &Path) -> Option<(u64, u64)> {
    use std::io::Read as _;
    let mut file = std::fs::File::open(path).ok()?;
    let mut header = [0u8; 64];
    let read = file.read(&mut header).ok()?;
    let header = &header[..read];
    if header.len() >= 24
        && header.starts_with(&[0x89, b'P', b'N', b'G', b'\r', b'\n', 0x1a, b'\n'])
    {
        return image_dimensions_bytes(header);
    }
    if header.len() >= 4 && header[0] == 0xFF && header[1] == 0xD8 {
        return jpeg_dimensions(path);
    }
    None
}

pub(crate) fn image_dimensions_bytes(header: &[u8]) -> Option<(u64, u64)> {
    if header.len() >= 24
        && header.starts_with(&[0x89, b'P', b'N', b'G', b'\r', b'\n', 0x1a, b'\n'])
    {
        let width = u32::from_be_bytes([header[16], header[17], header[18], header[19]]);
        let height = u32::from_be_bytes([header[20], header[21], header[22], header[23]]);
        return Some((width as u64, height as u64));
    }
    if header.len() >= 4 && header[0] == 0xFF && header[1] == 0xD8 {
        return jpeg_dimensions_bytes(header);
    }
    None
}

/// JPEG：扫描段标记找 SOF0/1/2（CJK 之外的常见帧头），读宽高。
fn jpeg_dimensions(path: &Path) -> Option<(u64, u64)> {
    use std::io::Read as _;
    let file = std::fs::File::open(path).ok()?;
    let mut data = Vec::new();
    file.take(256 * 1024).read_to_end(&mut data).ok()?;
    jpeg_dimensions_bytes(&data)
}

fn jpeg_dimensions_bytes(data: &[u8]) -> Option<(u64, u64)> {
    let mut offset = 2usize; // 跳过 SOI
    while offset + 9 < data.len() {
        if data[offset] != 0xFF {
            offset += 1;
            continue;
        }
        let marker = data[offset + 1];
        // 段长不含标记本身（高位在前）。
        let length = u16::from_be_bytes([data[offset + 2], data[offset + 3]]) as usize;
        let is_sof = matches!(
            marker,
            0xC0 | 0xC1
                | 0xC2
                | 0xC3
                | 0xC5
                | 0xC6
                | 0xC7
                | 0xC9
                | 0xCA
                | 0xCB
                | 0xCD
                | 0xCE
                | 0xCF
        );
        if is_sof {
            let height = u16::from_be_bytes([data[offset + 5], data[offset + 6]]) as u64;
            let width = u16::from_be_bytes([data[offset + 7], data[offset + 8]]) as u64;
            return Some((width, height));
        }
        // SOS 之后是熵编码数据，不再有可靠的段结构。
        if marker == 0xDA {
            return None;
        }
        offset += 2 + length;
    }
    None
}

/// 单图视觉 token 估算（INV-MM2-4，MM-2 W4 起 route-aware）：
/// `(100 + ceil(w/512) × ceil(h/512) × 350) × 2.0`，**无 tile cap**
/// ——按实际（规范化 blob = 最终发送的）尺寸线性计，超高清不封顶；
/// 尺寸不可得时保守常数 ×2.0。宁可高估（早触发压缩）不可低估
///（撑爆窗口）。压缩/context 计量/steering 共用本函数（统一
/// image walker 的当前形态）。
pub(crate) fn estimate_image_tokens(path: &Path) -> u64 {
    let Some((width, height)) = image_dimensions(path) else {
        return FALLBACK_TOKENS * IMAGE_TOKEN_SAFETY_FACTOR;
    };
    if width == 0 || height == 0 {
        return FALLBACK_TOKENS * IMAGE_TOKEN_SAFETY_FACTOR;
    }
    let tiles_x = width.div_ceil(TILE_PIXELS);
    let tiles_y = height.div_ceil(TILE_PIXELS);
    (BASE_TOKENS + tiles_x * tiles_y * TOKENS_PER_TILE) * IMAGE_TOKEN_SAFETY_FACTOR
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 最小合法 PNG 头：签名 + IHDR chunk（1×1 像素）。
    fn png_header(width: u32, height: u32) -> Vec<u8> {
        let mut bytes = vec![
            0x89, b'P', b'N', b'G', b'\r', b'\n', 0x1a, b'\n', 0, 0, 0, 13, b'I', b'H', b'D', b'R',
        ];
        bytes.extend_from_slice(&width.to_be_bytes());
        bytes.extend_from_slice(&height.to_be_bytes());
        bytes.extend_from_slice(&[8, 6, 0, 0, 0]); // bit depth, color type, …
        bytes
    }

    #[test]
    fn dimensions_come_from_png_headers_without_decoding() {
        let dir = std::env::temp_dir().join(format!(
            "clat-media-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("probe.png");
        std::fs::write(&path, png_header(1024, 768)).unwrap();
        assert_eq!(image_dimensions(&path), Some((1024, 768)));
        // 1024×768 → 2×2 tile = 4 → (100 + 4×350) × 2.0（MM-2 口径）。
        assert_eq!(estimate_image_tokens(&path), (100 + 4 * 350) * 2);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// INV-MM2-4（MM-2 W4 红测，先红后绿）：route-aware 口径——
    /// (100 + ceil(w/512)×ceil(h/512)×350) × 2.0，**无 tile cap**、
    ///随像素单调。pre-fix（6-tile 封顶）本测试红。
    #[test]
    fn image_token_estimate_is_uncapped_monotonic_and_doubled() {
        let dir = std::env::temp_dir().join(format!(
            "clat-media-mm2-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        // 20000×20000 → 40×40 = 1600 tile（远超旧 6-tile 封顶）。
        let huge = dir.join("huge.png");
        std::fs::write(&huge, png_header(20000, 20000)).unwrap();
        assert_eq!(
            estimate_image_tokens(&huge),
            (100 + 40 * 40 * 350) * 2,
            "no tile cap: full 1600 tiles counted"
        );
        // 单 tile 内的小图 = (100+350)×2。
        let small = dir.join("small.png");
        std::fs::write(&small, png_header(500, 500)).unwrap();
        assert_eq!(estimate_image_tokens(&small), (100 + 350) * 2);
        // 跨 tile 边界单调：513×513 → 2×2 tile。
        let edge = dir.join("edge.png");
        std::fs::write(&edge, png_header(513, 513)).unwrap();
        assert_eq!(estimate_image_tokens(&edge), (100 + 4 * 350) * 2);
        assert!(estimate_image_tokens(&edge) > estimate_image_tokens(&small));
        // 尺寸不可得：保守常数同样携带 ×2.0。
        let junk = dir.join("probe.xyz");
        std::fs::write(&junk, b"not an image").unwrap();
        assert_eq!(estimate_image_tokens(&junk), 1600 * 2);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn media_types_come_from_extensions() {
        assert_eq!(
            media_type_for_path(Path::new("/tmp/a.PNG")),
            Some("image/png")
        );
        assert_eq!(
            media_type_for_path(Path::new("/tmp/a.jpeg")),
            Some("image/jpeg")
        );
        assert_eq!(
            media_type_for_path(Path::new("/tmp/a.webp")),
            Some("image/webp")
        );
        assert_eq!(media_type_for_path(Path::new("/tmp/a.mp4")), None);
        assert_eq!(media_type_for_path(Path::new("/tmp/noext")), None);
    }

    // —— MM-1 S1（INV-MM1-1/2）：校验职责的判别测试 ————————————

    /// `tag` 形如 "ok-png.png"——扩展名参与判定（validate_source 读
    /// 它），唯一性数字插在主干与扩展名之间。
    fn temp_file(tag: &str, bytes: &[u8]) -> std::path::PathBuf {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let file_name = match tag.split_once('.') {
            Some((stem, extension)) => format!("clat-media-s1-{stem}-{unique}.{extension}"),
            None => format!("clat-media-s1-{tag}-{unique}"),
        };
        let path = std::env::temp_dir().join(file_name);
        std::fs::write(&path, bytes).unwrap();
        path
    }

    fn gif_bytes(width: u16, height: u16) -> Vec<u8> {
        let mut bytes = b"GIF89a".to_vec();
        bytes.extend_from_slice(&width.to_le_bytes());
        bytes.extend_from_slice(&height.to_le_bytes());
        bytes.extend_from_slice(&[0x77, 0x00]); // GCT flag + background
        bytes
    }

    fn webp_vp8l_bytes(width: u32, height: u32) -> Vec<u8> {
        let packed = (width - 1) | ((height - 1) << 14);
        let mut payload = vec![0x2Fu8];
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

    /// INV-MM1-1：magic 先于扩展名——伪扩展（JPEG 字节挂 .png）、
    /// 截断 PNG、无 magic 的杂字节全部拒绝；pre-fix（只查扩展名）
    /// 前两者通过，本测试红。
    #[test]
    fn validate_source_rejects_forged_extensions_and_truncation() {
        let dir = |tag: &str, bytes: &[u8]| temp_file(tag, bytes);

        // 伪扩展：JPEG SOI 挂 .png。
        let forged = dir("forged.png", &[0xFF, 0xD8, 0xFF, 0xE0, 0, 0x10, b'J', b'F']);
        let err = validate_source(&forged).unwrap_err();
        assert!(err.contains("does not match its content"), "{err}");
        let _ = std::fs::remove_file(&forged);

        // 截断 PNG：只有签名前 4 字节。
        let truncated = dir("trunc.png", &png_header(10, 10)[..4]);
        assert!(validate_source(&truncated).is_err());
        let _ = std::fs::remove_file(&truncated);

        // 纯文本挂 .png。
        let junk = dir("junk.png", b"definitely not an image");
        assert!(validate_source(&junk).is_err());
        let _ = std::fs::remove_file(&junk);

        // 空文件。
        let empty = dir("empty.png", b"");
        assert!(validate_source(&empty).is_err());
        let _ = std::fs::remove_file(&empty);
    }

    /// INV-MM1-2：像素上限在读头阶段先判——IHDR 声明 5000×5000
    ///（25M px > 16M）即拒，无需解码；GIF logical screen 同律。
    /// pre-fix（无像素检查）通过，本测试红。
    #[test]
    fn validate_source_rejects_super_pixel_headers() {
        let huge_png = temp_file("huge-png.png", &png_header(5000, 5000));
        let err = validate_source(&huge_png).unwrap_err();
        assert!(err.contains("pixel limit"), "{err}");
        let _ = std::fs::remove_file(&huge_png);

        let huge_gif = temp_file("huge-gif.gif", &gif_bytes(5000, 5000));
        assert!(validate_source(&huge_gif).is_err());
        let _ = std::fs::remove_file(&huge_gif);

        // VP8L 打包尺寸：9999×2 → 19998 px，合法。
        let ok_webp = temp_file("ok-webp.webp", &webp_vp8l_bytes(9999, 2));
        let (family, dims) = validate_source(&ok_webp).expect("vp8l admits");
        assert_eq!(family, ImageFamily::Webp);
        assert_eq!(dims, Some((9999, 2)));
        let _ = std::fs::remove_file(&ok_webp);
        // VP8L 打包超限：8192×4096 = 33M px。
        let huge_webp = temp_file("huge-webp.webp", &webp_vp8l_bytes(8192, 4096));
        assert!(validate_source(&huge_webp).is_err());
        let _ = std::fs::remove_file(&huge_webp);
    }

    /// 合法四族照常通过（family 与实测尺寸正确）。
    #[test]
    fn validate_source_admits_all_four_families() {
        let png = temp_file("ok-png.png", &png_header(64, 48));
        assert_eq!(
            validate_source(&png).expect("png"),
            (ImageFamily::Png, Some((64, 48)))
        );
        let _ = std::fs::remove_file(&png);

        // 最小 JPEG：SOI + APP0 + SOF0（高度/宽度在 SOF 内，jpeg_dimensions 可读）。
        let mut jpeg = vec![
            0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x10, b'J', b'F', b'I', b'F', 0,
        ];
        jpeg.extend_from_slice(&[0x01, 0x01, 0x00, 0x00, 0x01, 0x00, 0x01, 0x00, 0x00]);
        // 直接拼一个 SOF0 段：marker + length + precision + h + w。
        jpeg.extend_from_slice(&[0xFF, 0xC0, 0x00, 0x11, 0x08, 0x00, 30, 0x00, 40]);
        jpeg.extend_from_slice(&[0x01, 0x00, 0x22, 0x00, 0x02, 0x11, 0x01, 0x03, 0x11, 0x01]);
        let jpeg_path = temp_file("ok-jpeg.jpg", &jpeg);
        assert_eq!(
            validate_source(&jpeg_path).expect("jpeg"),
            (ImageFamily::Jpeg, Some((40, 30)))
        );
        let _ = std::fs::remove_file(&jpeg_path);

        let gif = temp_file("ok-gif.gif", &gif_bytes(320, 240));
        assert_eq!(
            validate_source(&gif).expect("gif"),
            (ImageFamily::Gif, Some((320, 240)))
        );
        let _ = std::fs::remove_file(&gif);

        let webp = temp_file("ok-webp2.webp", &webp_vp8l_bytes(64, 64));
        assert_eq!(
            validate_source(&webp).expect("webp"),
            (ImageFamily::Webp, Some((64, 64)))
        );
        let _ = std::fs::remove_file(&webp);
    }

    /// magic 嗅探本身：mislabeled MIME 的字节族判定（服务端按字节
    /// 嗅探——MM-0 probe 实证；我们同样以 sniff 结果为准）。
    #[test]
    fn sniff_takes_bytes_over_declarations() {
        assert_eq!(
            sniff_image_family_bytes(&[0xFF, 0xD8, 0xFF]),
            Some(ImageFamily::Jpeg)
        );
        assert_eq!(
            sniff_image_family_bytes(b"GIF87a\x10\x00\x10\x00"),
            Some(ImageFamily::Gif)
        );
        assert_eq!(sniff_image_family_bytes(b"GIF88a"), None);
        let mut riff = b"RIFF\x00\x00\x00\x00WEBP".to_vec();
        riff.extend_from_slice(b"VP8L");
        assert_eq!(sniff_image_family_bytes(&riff), Some(ImageFamily::Webp));
        assert_eq!(sniff_image_family_bytes(b"RIFF\x00\x00\x00\x00WAVE"), None);
        assert_eq!(sniff_image_family_bytes(b""), None);
    }
}
