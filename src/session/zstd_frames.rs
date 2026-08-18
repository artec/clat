//! zstd frame encoding and the structural scanner (compat doc §7.1).
//! Every durable append batch is one independent frame with the content
//! checksum enabled; the first frame holds exactly one header line.

use std::io::{Read, Write};

const ZSTD_MAGIC: [u8; 4] = [0x28, 0xB5, 0x2F, 0xFD];

/// Compress one complete frame. Level 0 = libzstd default (3), matching
/// Node's `zstdCompress` which sets no level; checksum enabled.
pub(crate) fn compress_frame(plain: &[u8]) -> std::io::Result<Vec<u8>> {
    let mut encoder = zstd::stream::write::Encoder::new(Vec::new(), 0)?;
    encoder.include_checksum(true)?;
    encoder.write_all(plain)?;
    encoder.finish().map_err(std::io::Error::other)
}

/// Decompress one complete frame (verifies the checksum).
pub(crate) fn decompress_frame(frame: &[u8]) -> std::io::Result<Vec<u8>> {
    zstd::bulk::decompress(frame, expected_content_size(frame)?)
        .or_else(|_| fallback_decompress(frame))
}

fn expected_content_size(frame: &[u8]) -> std::io::Result<usize> {
    let scan = scan_frames(frame, 1)?;
    let range = scan
        .frames
        .first()
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidData, "empty frame"))?;
    let header = &frame[range.start..range.start + frame_header_size(&frame[range.start..])?];
    let descriptor = header[4];
    let flag = (descriptor >> 6) & 0x3;
    let single_segment = descriptor & 0x20 != 0;
    let size_bytes = if flag == 0 {
        usize::from(single_segment)
    } else {
        1usize << flag
    };
    if size_bytes == 0 {
        return Ok(1 << 20);
    }
    let mut size = 0usize;
    for byte in &header[5..5 + size_bytes] {
        size = (size << 8) | *byte as usize;
    }
    Ok(size.max(1 << 16))
}

fn fallback_decompress(frame: &[u8]) -> std::io::Result<Vec<u8>> {
    let mut decoder = zstd::stream::read::Decoder::new(frame)?.single_frame();
    let mut out = Vec::new();
    decoder.read_to_end(&mut out)?;
    Ok(out)
}

/// Decompress all frames of a buffer in order.
pub(crate) fn decompress_all(bytes: &[u8]) -> std::io::Result<Vec<u8>> {
    let mut decoder = zstd::stream::read::Decoder::new(bytes)?;
    let mut out = Vec::new();
    decoder.read_to_end(&mut out)?;
    Ok(out)
}

/// Salvage the decodable plaintext prefix of a structurally torn frame
/// (recovery keeps complete records inside a torn final frame).
pub(crate) fn decompress_prefix(torn_frame: &[u8]) -> Vec<u8> {
    let mut decoder = match zstd::stream::read::Decoder::new(torn_frame) {
        Ok(decoder) => decoder.single_frame(),
        Err(_) => return Vec::new(),
    };
    let mut salvaged = Vec::new();
    let mut buffer = [0u8; 64 * 1024];
    loop {
        match decoder.read(&mut buffer) {
            Ok(0) => break,
            Ok(read) => salvaged.extend_from_slice(&buffer[..read]),
            Err(_) => break,
        }
    }
    salvaged
}

#[derive(Debug, Eq, PartialEq)]
pub(crate) struct FrameScan {
    pub(crate) frames: Vec<std::ops::Range<usize>>,
    /// Byte offset where a structurally incomplete final frame starts.
    pub(crate) torn_start: Option<usize>,
}

/// Structural scan without decompression: locates frame boundaries and the
/// torn-tail offset, validating magic, reserved bits, and block structure.
pub(crate) fn scan_frames(bytes: &[u8], max_frames: usize) -> Result<FrameScan, std::io::Error> {
    let mut scan = FrameScan {
        frames: Vec::new(),
        torn_start: None,
    };
    let mut offset = 0usize;
    while offset < bytes.len() {
        if scan.frames.len() == max_frames {
            break;
        }
        match frame_end(bytes, offset) {
            Ok(Some(end)) => {
                scan.frames.push(offset..end);
                offset = end;
            }
            Ok(None) => {
                scan.torn_start = Some(offset);
                break;
            }
            Err(error) => return Err(error),
        }
    }
    Ok(scan)
}

/// `Ok(Some(end))` for a structurally complete frame, `Ok(None)` when the
/// remainder is too short anywhere (torn tail).
fn frame_end(bytes: &[u8], start: usize) -> Result<Option<usize>, std::io::Error> {
    let corrupt = |message: String| std::io::Error::new(std::io::ErrorKind::InvalidData, message);
    if bytes.len() - start < 4 {
        return Ok(None);
    }
    if bytes[start..start + 4] != ZSTD_MAGIC {
        return Err(corrupt(format!("invalid frame magic at byte {start}")));
    }
    let descriptor = bytes[start + 4];
    if descriptor & 0x18 != 0 {
        return Err(corrupt("reserved frame-header bit".into()));
    }
    let content_size_flag = (descriptor >> 6) & 0x3;
    let single_segment = descriptor & 0x20 != 0;
    let checksum = descriptor & 0x04 != 0;
    let dict_id_code = descriptor & 0x3;
    let content_size_bytes = if content_size_flag == 0 {
        usize::from(single_segment)
    } else {
        1usize << content_size_flag
    };
    let dict_id_bytes = match dict_id_code {
        0 => 0,
        1 => 1,
        2 => 2,
        3 => 4,
        _ => unreachable!(),
    };
    // A non-single-segment frame carries one window-descriptor byte after
    // the frame header (zstd format §3.1.1.1.2) — easy to miss, fatal to
    // boundary math.
    let window_descriptor = usize::from(!single_segment);
    let mut offset = start + 5 + content_size_bytes + dict_id_bytes + window_descriptor;
    loop {
        if bytes.len() - offset < 3 {
            return Ok(None);
        }
        let header = u32::from_le_bytes([bytes[offset], bytes[offset + 1], bytes[offset + 2], 0]);
        let last_block = header & 1 != 0;
        let block_type = (header >> 1) & 0x3;
        let block_size = (header >> 3) as usize;
        if block_type == 3 {
            return Err(corrupt("reserved block type".into()));
        }
        offset += 3;
        let payload = if block_type == 1 { 1 } else { block_size };
        if bytes.len() - offset < payload {
            return Ok(None);
        }
        offset += payload;
        if last_block {
            break;
        }
    }
    if checksum && bytes.len() - offset < 4 {
        return Ok(None);
    }
    if checksum {
        offset += 4;
    }
    Ok(Some(offset))
}

fn frame_header_size(frame: &[u8]) -> Result<usize, std::io::Error> {
    let descriptor = frame[4];
    let content_size_flag = (descriptor >> 6) & 0x3;
    let single_segment = descriptor & 0x20 != 0;
    let dict_id_bytes = match descriptor & 0x3 {
        0 => 0,
        1 => 1,
        2 => 2,
        _ => 4,
    };
    let content_size_bytes = if content_size_flag == 0 {
        usize::from(single_segment)
    } else {
        1usize << content_size_flag
    };
    Ok(5 + content_size_bytes + dict_id_bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_round_trip_and_scan_finds_exact_boundaries() {
        let first = compress_frame(b"line-one\n").expect("compress");
        let second = compress_frame(b"line-two\nline-three\n").expect("compress");
        let mut file = first.clone();
        file.extend_from_slice(&second);

        let scan = scan_frames(&file, usize::MAX).expect("scan");
        assert_eq!(scan.torn_start, None);
        assert_eq!(scan.frames.len(), 2);
        assert_eq!(scan.frames[0], 0..first.len());
        assert_eq!(scan.frames[1].start, first.len());
        assert_eq!(scan.frames[1].end, file.len());

        assert_eq!(decompress_frame(&first).expect("decompress"), b"line-one\n");
        assert_eq!(
            decompress_all(&file).expect("decompress all"),
            b"line-one\nline-two\nline-three\n"
        );
    }

    #[test]
    fn max_frames_stops_early_for_first_frame_reads() {
        let mut file = compress_frame(b"h\n").expect("compress");
        file.extend_from_slice(&compress_frame(b"body").expect("compress"));
        let scan = scan_frames(&file, 1).expect("scan");
        assert_eq!(scan.frames.len(), 1);
        assert_eq!(scan.torn_start, None);
    }

    #[test]
    fn torn_tail_is_located_not_errors() {
        let complete = compress_frame(b"abc").expect("compress");
        let mut file = complete.clone();
        file.extend_from_slice(&compress_frame(b"0123456789").expect("compress")[..7]);
        let scan = scan_frames(&file, usize::MAX).expect("scan");
        assert_eq!(scan.frames.len(), 1);
        assert_eq!(scan.torn_start, Some(complete.len()));
    }

    #[test]
    fn bad_magic_is_corruption_not_tear() {
        let mut file = compress_frame(b"abc").expect("compress");
        file[0] ^= 0xFF;
        assert!(scan_frames(&file, usize::MAX).is_err());
    }

    #[test]
    fn checksum_enabled_frames_reject_tampered_content() {
        let frame = compress_frame(b"payload").expect("compress");
        let mut tampered = frame.clone();
        let scan = scan_frames(&tampered, 1).expect("scan");
        let content = scan.frames[0].clone();
        // Flip a byte in the middle of the compressed payload.
        tampered[(content.start + content.end) / 2] ^= 0x01;
        assert!(decompress_frame(&tampered).is_err());
    }

    #[test]
    fn prefix_salvage_returns_decodable_prefix_of_torn_frame() {
        let plain: Vec<u8> = (0..200_000u32).map(|i| (i % 251) as u8).collect();
        let frame = compress_frame(&plain).expect("compress");
        // Cut mid-frame: salvage should return a prefix of the plaintext.
        let torn = &frame[..frame.len() * 2 / 3];
        let salvaged = decompress_prefix(torn);
        assert!(salvaged.len() <= plain.len());
        assert_eq!(&salvaged[..], &plain[..salvaged.len()]);
    }
}
