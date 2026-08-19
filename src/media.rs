//! 本地图片附件的媒介工具（2026-08-19，用户附加图片进对话）。
//!
//! 三件事，全部零第三方依赖：
//! - 扩展名 → MIME（附加入口的合法性判定）；
//! - PNG/JPEG 头解析出像素尺寸（只读头部几十字节，不解码像素）；
//! - 视觉 token 估算：按 512px 网格切块（tile），每 tile 一个保守
//!   常数——图片进上下文的计量单位是视觉 token 而非 base64 字节，
//!   自动压缩的预算触发依赖这个估算（M5）。
//!
//! 估算是有意的启发式：各厂商 tile 规格不同且不公开细则，宁可高估
//! 触发压缩，不可低估撑爆窗口；后续可用 journal 里的真实
//! input_tokens 校准（同压缩预算的校准路线）。

use std::path::Path;

/// 单个附件的字节上限：入口拒绝，不进会话目录（M7 防线三）。
pub(crate) const MAX_ATTACHMENT_BYTES: u64 = 4 * 1024 * 1024;

/// tile 边长（像素）：视觉模型普遍按 ~512px 网格切块计费。
const TILE_PIXELS: u64 = 512;
/// 单 tile 保守 token 数（各厂商 280–400 不等）。
const TOKENS_PER_TILE: u64 = 350;
/// 单图固定开销。
const BASE_TOKENS: u64 = 100;
/// 单图 tile 数上限：超高清图按此封顶（而非线性膨胀）。
const MAX_TILES: u64 = 6;
/// 尺寸解析失败（未知格式/头损坏）时的保守常数。
const FALLBACK_TOKENS: u64 = 1600;

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
        // IHDR 的宽高是大端 u32，位于签名 + 长度 + 类型之后。
        let width = u32::from_be_bytes([header[16], header[17], header[18], header[19]]);
        let height = u32::from_be_bytes([header[20], header[21], header[22], header[23]]);
        return Some((width as u64, height as u64));
    }
    if header.len() >= 4 && header[0] == 0xFF && header[1] == 0xD8 {
        return jpeg_dimensions(path);
    }
    None
}

/// JPEG：扫描段标记找 SOF0/1/2（CJK 之外的常见帧头），读宽高。
fn jpeg_dimensions(path: &Path) -> Option<(u64, u64)> {
    use std::io::Read as _;
    let file = std::fs::File::open(path).ok()?;
    let mut data = Vec::new();
    file.take(256 * 1024).read_to_end(&mut data).ok()?;
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

/// 单图视觉 token 估算（M5）：尺寸 → tile 数（1..=MAX_TILES）→ token；
/// 尺寸不可得时用保守常数。宁可高估（早触发压缩）不可低估（撑爆）。
pub(crate) fn estimate_image_tokens(path: &Path) -> u64 {
    let Some((width, height)) = image_dimensions(path) else {
        return FALLBACK_TOKENS;
    };
    if width == 0 || height == 0 {
        return FALLBACK_TOKENS;
    }
    let tiles_x = width.div_ceil(TILE_PIXELS);
    let tiles_y = height.div_ceil(TILE_PIXELS);
    let tiles = (tiles_x * tiles_y).clamp(1, MAX_TILES);
    BASE_TOKENS + tiles * TOKENS_PER_TILE
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
        // 1024×768 → 2×2 tile = 4 → 100 + 4×350。
        assert_eq!(estimate_image_tokens(&path), 100 + 4 * 350);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 估算不变量：tile 封顶、退化常数、单调性。
    #[test]
    fn token_estimate_is_conservative_and_capped() {
        let dir = std::env::temp_dir().join(format!(
            "clat-media-cap-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        // 超高清（20000px）也只按 MAX_TILES 计。
        let huge = dir.join("huge.png");
        std::fs::write(&huge, png_header(20000, 20000)).unwrap();
        assert_eq!(
            estimate_image_tokens(&huge),
            BASE_TOKENS + MAX_TILES * TOKENS_PER_TILE
        );
        // 未知格式：保守常数。
        let junk = dir.join("probe.xyz");
        std::fs::write(&junk, b"not an image").unwrap();
        assert_eq!(estimate_image_tokens(&junk), FALLBACK_TOKENS);
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
}
