//! READ-1（2026-10-15）read_file 的有界分段实现：与通用 ToolResultPruner
//! 阈值同源的分段帽、不透明字节游标、文件身份与显式失败语义。
//! 契约细节见 `ReadFileTool` 上方不变量注释与 docs/architecture.md。

use crate::CancelToken;
use crate::project::Project;
use crate::tool::{Tool, ToolDefinition, ToolEffect, ToolError};
use serde_json::{Value, json};
use std::fs;
use std::io::{BufReader, Seek};

use super::{check_cancelled, required_string_arg, string_arg, tool_io_error, usize_arg};

const DEFAULT_READ_BYTES: usize = 64 * 1024;
const MAX_READ_BYTES: usize = 1024 * 1024;

#[derive(Clone, Copy, Debug, Default)]
pub struct ReadFileTool;

/// READ-1（2026-10-15，第一用户真实症状）——有界、可续、完整、可判别
/// 的长文件阅读。四条不变量（验收 R1–R4）：
///
/// 1. **结构永不被二次裁剪**：单段结果的序列化长度恒 ≤ 通用
///    ToolResultPruner 阈值（构造预算 + 真实序列化精确校验双保险），
///    pruner 的 head/tail 替换对 read_file 永不触发——模型总能看到
///    完整的游标与状态字段。
/// 2. **可续且单调前进**：每段携带 `has_more` + `next_cursor`（字节
///    偏移 + 行号 + 行内标志），按游标续读不重叠、不遗漏、不循环，
///    直到 EOF。超长单行按字符边界切段并标记 `partial_line`；续读
///    必须用 `next_cursor`（跳行号会漏掉行内剩余内容）。
/// 3. **身份可判别**：每段携带 `file_bytes` + `file_version`（长度+
///    mtime 纳秒）；`expected_file_version` 不匹配时显式失败并指导
///    重读，拒绝把不同版本默默拼成“完整文件”。
/// 4. **有界成本**：`max_bytes` 是请求上限；分段还受与 pruner 同源的
///    上下文安全帽约束（`content_budget_chars` 回报实际生效值），
///    不放大结果体、不影响其他工具的裁剪契约。
struct ReadRequest {
    start_line: u64,
    end_line: Option<u64>,
    start_byte: Option<u64>,
    cursor: Option<ReadCursor>,
    expected_version: Option<String>,
    /// 序列化字符预算（pruner 阈值减信封预留后的内容帽）。
    serialized_budget: usize,
    /// CF3（审计返工）：`max_bytes` 恢复为 content 的 **UTF-8 字节** 上限
    /// （schema 承诺 upper bound on returned content bytes），与序列化
    /// 帽同时生效。
    byte_budget: usize,
}

/// READ-1 不透明续读游标：绝对字节偏移 + 行号 + 是否行内。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ReadCursor {
    byte: u64,
    line: u64,
    mid_line: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ReadStart {
    cursor: ReadCursor,
    /// start_line 扫描越过 EOF（显式失败语义，非陈旧游标错误）。
    beyond_eof: bool,
}

fn parse_cursor(text: &str) -> Option<ReadCursor> {
    let mut byte = None;
    let mut line = None;
    let mut mid_line = false;
    for part in text.split(';') {
        if part == "m" {
            mid_line = true;
        } else if let Some(value) = part.strip_prefix("b=") {
            byte = value.parse().ok();
        } else if let Some(value) = part.strip_prefix("l=") {
            line = value.parse().ok();
        }
    }
    Some(ReadCursor {
        byte: byte?,
        line: line?,
        mid_line,
    })
}

fn format_cursor(cursor: ReadCursor) -> String {
    if cursor.mid_line {
        format!("b={};l={};m", cursor.byte, cursor.line)
    } else {
        format!("b={};l={}", cursor.byte, cursor.line)
    }
}

/// 单字符的 JSON 转义增量（serde_json：非 ASCII 原样）。
fn escape_extra(ch: char) -> usize {
    match ch {
        '"' | '\\' | '\n' | '\r' | '\t' => 1,
        ch if ch.is_control() => 5,
        _ => 0,
    }
}

/// JSON 转义后的字符数（serde_json：非 ASCII 原样，控制字符转义为
/// `\uXXXX`）。内容字段的序列化长度 = 字符数 + 转义增量，预算按此计。
fn escape_len(text: &str) -> usize {
    text.chars().map(|ch| 1 + escape_extra(ch)).sum()
}

/// 段内一件已格式化内容：显示片 + 消耗的源字节数 + 行号 + 是否以行
/// 终止收尾（显示统一用裸 `\n`；源行终止符的 `\r` 被规范化掉）。
struct ReadPiece {
    display: String,
    bytes: u64,
    line: u64,
    ends_line: bool,
    lossy: bool,
}

struct ReadSegment {
    pieces: Vec<ReadPiece>,
    /// 段结束后游标所在的行号（下一步将读/继续读的行）。
    line: u64,
    at_line_start: bool,
    truncated: bool,
    beyond_eof: bool,
}

impl ReadSegment {
    fn content(&self) -> String {
        self.pieces
            .iter()
            .map(|piece| piece.display.as_str())
            .collect()
    }

    fn consumed_bytes(&self) -> u64 {
        self.pieces.iter().map(|piece| piece.bytes).sum()
    }

    fn last_line(&self) -> Option<u64> {
        self.pieces.last().map(|piece| piece.line)
    }

    /// 尾片是否行内截断（partial_line 标志的来源）。
    fn partial_line(&self) -> bool {
        self.pieces.last().is_some_and(|piece| !piece.ends_line)
    }
}

/// 在窗口内按字符推进，**转义与输出字节双感知**（CF1/CF3/CF6 审计
/// 返工）：(a) 遇 `\n`（或 **CRLF 作为单一行终止**，CF7——`\r` 不入
/// text、两字节一起消费，行内容不再依赖分段位置丢失 CR 归一化）且
/// 终止成本在预算内 → 行完整；(b) 序列化或输出字节预算用尽 → 行内
/// 截断；(c) 窗口耗尽。预算的**输出字节**按 `char::len_utf8()` 计
/// （U+FFFD 替换为 3 字节，CF6），源字节推进（`bytes`）按实际消费
/// 计——两者解耦，游标永远按源字节前进。窗口尾部的 `\r`（可能跨窗
/// 构成 CRLF）与截断的多字节前缀在非 EOF 时不消费、不替换（下段
/// 续读）；真正的无效序列逐字节替换（`from_utf8_lossy` 语义）并标记。
struct WindowScan {
    bytes: usize,
    text: String,
    newline: bool,
    lossy: bool,
    serialized: usize,
    out_bytes: usize,
}

/// 窗口当前位置的 `\r` 语义（CF7）：后随 `\n` → CRLF 单一终止；位于
/// 窗口尾且非 EOF → 可能跨窗构成 CRLF，留给下一段；否则孤立普通字符。
enum CrlfView {
    Terminates,
    Deferred,
    Plain,
}

fn crlf_view(rest: &[u8], at_eof: bool) -> CrlfView {
    if rest.len() >= 2 && rest[1] == b'\n' {
        CrlfView::Terminates
    } else if rest.len() == 1 && !at_eof {
        CrlfView::Deferred
    } else {
        CrlfView::Plain
    }
}

fn scan_window(
    window: &[u8],
    serialized_budget: usize,
    byte_budget: usize,
    at_eof: bool,
) -> WindowScan {
    let mut scan = WindowScan {
        bytes: 0,
        text: String::new(),
        newline: false,
        lossy: false,
        serialized: 0,
        out_bytes: 0,
    };
    while scan.bytes < window.len() {
        let rest = &window[scan.bytes..];
        if rest[0] == b'\n' {
            // 纯 LF 终止：`\n` 序列化为 2、输出 1 字节；终止字节计入
            // `bytes`（与 CRLF 分支一致，piece 源字节数即 scan.bytes）。
            if scan.serialized + 2 <= serialized_budget && scan.out_bytes < byte_budget {
                scan.newline = true;
                scan.bytes += 1;
            }
            return scan;
        }
        if rest[0] == b'\r' {
            match crlf_view(rest, at_eof) {
                CrlfView::Terminates => {
                    // CRLF 单一终止：text 不含 `\r`；序列化成本 `\r\n`=4、
                    // 输出 1 字节（显示统一裸 `\n`）。预算不够则停在 `\r`
                    // 前（不消费），下一段以新预算看到完整 CRLF——归一化
                    // 不依赖分段边界（CF7）。
                    if scan.serialized + 4 <= serialized_budget && scan.out_bytes < byte_budget {
                        scan.newline = true;
                        scan.bytes += 2;
                    }
                    return scan;
                }
                CrlfView::Deferred => break,
                CrlfView::Plain => {}
            }
        }
        match next_char(rest) {
            Some((ch, len)) => {
                let out_len = ch.len_utf8();
                let serialized_cost = 1 + escape_extra(ch);
                if scan.serialized + serialized_cost > serialized_budget
                    || scan.out_bytes + out_len > byte_budget
                {
                    break;
                }
                scan.text.push(ch);
                scan.serialized += serialized_cost;
                scan.out_bytes += out_len;
                scan.bytes += len;
            }
            None if !at_eof && tail_is_incomplete_char(rest) => break,
            None => {
                // U+FFFD：序列化 6 字符、输出 3 字节、源消费 1 字节。
                if scan.serialized + 6 > serialized_budget || scan.out_bytes + 3 > byte_budget {
                    break;
                }
                scan.text.push('\u{FFFD}');
                scan.serialized += 6;
                scan.out_bytes += 3;
                scan.bytes += 1;
                scan.lossy = true;
            }
        }
    }
    scan
}

/// 解码窗口起点的一个 UTF-8 字符（≤4 字节）。
fn next_char(bytes: &[u8]) -> Option<(char, usize)> {
    for len in 1..=4 {
        if len > bytes.len() {
            break;
        }
        if let Ok(text) = std::str::from_utf8(&bytes[..len]) {
            return text.chars().next().map(|ch| (ch, len));
        }
    }
    None
}

/// 判断剩余字节是否是「被窗口截断的合法多字节字符前缀」。
fn tail_is_incomplete_char(rest: &[u8]) -> bool {
    let Some(&lead) = rest.first() else {
        return false;
    };
    let expected = match lead {
        0x00..=0x7F => 1,
        0xC0..=0xDF => 2,
        0xE0..=0xEF => 3,
        0xF0..=0xF7 => 4,
        _ => return false,
    };
    if rest.len() >= expected {
        return false;
    }
    rest[1..].iter().all(|&byte| (0x80..=0xBF).contains(&byte))
}

/// 从 reader 读取至多 `limit` 字节的窗口（有界内存）。
fn read_window(reader: &mut impl std::io::BufRead, limit: usize) -> std::io::Result<Vec<u8>> {
    let mut window = Vec::with_capacity(limit.min(64 * 1024));
    while window.len() < limit {
        let available = reader.fill_buf()?;
        if available.is_empty() {
            break;
        }
        let take = (limit - window.len()).min(available.len());
        window.extend_from_slice(&available[..take]);
        reader.consume(take);
    }
    Ok(window)
}

/// 版本身份：长度 + mtime 纳秒（“合理关联”，不持打开句柄）。
fn file_identity(metadata: &fs::Metadata) -> String {
    let mtime = metadata
        .modified()
        .ok()
        .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    format!("{}:{}", metadata.len(), mtime)
}

fn parse_read_request(arguments: &Value, requested: &str) -> Result<ReadRequest, ToolError> {
    let start_line =
        u64::try_from(usize_arg(arguments, "start_line").unwrap_or(1).max(1)).unwrap_or(u64::MAX);
    let end_line =
        usize_arg(arguments, "end_line").map(|end| u64::try_from(end).unwrap_or(u64::MAX));
    if end_line.is_some_and(|end| end < start_line) {
        return Err(ToolError::new(
            "read_file: `end_line` must be greater than or equal to `start_line`",
        ));
    }
    let start_byte =
        usize_arg(arguments, "start_byte").map(|byte| u64::try_from(byte).unwrap_or(0));
    let cursor = match string_arg(arguments, "cursor") {
        Some(text) => Some(parse_cursor(text).ok_or_else(|| {
            ToolError::new("read_file: `cursor` must be a value returned as `next_cursor`")
        })?),
        None => None,
    };
    let max_bytes = usize_arg(arguments, "max_bytes")
        .unwrap_or(DEFAULT_READ_BYTES)
        .clamp(1, MAX_READ_BYTES);
    // 分段帽：pruner 阈值减信封预留（路径回显的**转义后**长度 + 键/
    // 数字 + 数值波动），与最终真实序列化校验（见 invoke）双保险。
    // CF5（审计返工）：路径里的引号/反斜杠会成倍膨胀序列化信封，
    // 预留必须按 escape_len 计，否则构造预算虚高、首片被收缩弹空。
    let envelope_reserve = 512 + escape_len(requested);
    let segment_cap = crate::plugins::THRESHOLD_CHARS
        .saturating_sub(envelope_reserve)
        .max(64);
    Ok(ReadRequest {
        start_line,
        end_line,
        start_byte,
        cursor,
        expected_version: string_arg(arguments, "expected_file_version").map(str::to_owned),
        serialized_budget: max_bytes.min(segment_cap),
        byte_budget: max_bytes,
    })
}

/// 解析段起点：cursor > start_byte > start_line（行号顺序扫描）。
fn resolve_read_start(
    request: &ReadRequest,
    file: &mut fs::File,
    file_bytes: u64,
) -> Result<ReadStart, ToolError> {
    let start = if let Some(cursor) = request.cursor {
        ReadStart {
            cursor,
            beyond_eof: false,
        }
    } else if let Some(byte) = request.start_byte {
        ReadStart {
            cursor: ReadCursor {
                byte,
                line: request.start_line,
                mid_line: byte != 0,
            },
            beyond_eof: false,
        }
    } else {
        seek_read_line_start(request.start_line, file, file_bytes)?
    };
    if start.cursor.byte > file_bytes {
        return Err(ToolError::new(format!(
            "read_file: cursor byte {} is past the end of the file ({} bytes); \
             the file may have changed — restart the read from the start",
            start.cursor.byte, file_bytes
        )));
    }
    Ok(start)
}

/// 逐窗口数换行定位 1 基行号行首（与旧行为同阶：顺序扫描）。
fn seek_read_line_start(
    target_line: u64,
    file: &mut fs::File,
    file_bytes: u64,
) -> Result<ReadStart, ToolError> {
    if target_line <= 1 {
        return Ok(ReadStart {
            cursor: ReadCursor {
                byte: 0,
                line: 1,
                mid_line: false,
            },
            beyond_eof: false,
        });
    }
    let seek_error =
        |error: std::io::Error| ToolError::new(format!("read_file: seek failed: {error}"));
    file.seek(std::io::SeekFrom::Start(0)).map_err(seek_error)?;
    let mut reader = BufReader::new(&mut *file);
    let mut remaining = target_line - 1;
    let mut offset: u64 = 0;
    while remaining > 0 {
        let window = read_window(&mut reader, 64 * 1024).map_err(seek_error)?;
        if window.is_empty() {
            return Ok(ReadStart {
                cursor: ReadCursor {
                    byte: file_bytes,
                    line: target_line,
                    mid_line: false,
                },
                beyond_eof: true,
            });
        }
        offset += u64::try_from(window.len()).unwrap_or(u64::MAX);
        let mut seen = 0u64;
        let mut line_start = None;
        for (index, &byte) in window.iter().enumerate() {
            if byte == b'\n' {
                seen += 1;
                if seen == remaining {
                    line_start =
                        Some(offset - u64::try_from(window.len() - index - 1).unwrap_or(u64::MAX));
                    break;
                }
            }
        }
        if let Some(start) = line_start {
            return Ok(ReadStart {
                cursor: ReadCursor {
                    byte: start,
                    line: target_line,
                    mid_line: false,
                },
                beyond_eof: false,
            });
        }
        remaining -= seen;
    }
    Ok(ReadStart {
        cursor: ReadCursor {
            byte: offset,
            line: target_line,
            mid_line: false,
        },
        beyond_eof: false,
    })
}

/// 有界分段主循环：从起点按预算/行界/EOF 组装内容片。
fn read_segment(
    file: &mut fs::File,
    request: &ReadRequest,
    start: ReadStart,
    file_bytes: u64,
    cancel: &CancelToken,
) -> Result<ReadSegment, ToolError> {
    let mut segment = ReadSegment {
        pieces: Vec::new(),
        line: start.cursor.line,
        at_line_start: !start.cursor.mid_line,
        truncated: false,
        beyond_eof: start.beyond_eof,
    };
    if start.beyond_eof || start.cursor.byte >= file_bytes {
        return Ok(segment);
    }
    let seek_error =
        |error: std::io::Error| ToolError::new(format!("read_file: seek failed: {error}"));
    file.seek(std::io::SeekFrom::Start(start.cursor.byte))
        .map_err(seek_error)?;
    let mut reader = BufReader::new(&mut *file);
    let mut budget = PieceBudget {
        serialized: request.serialized_budget,
        bytes: request.byte_budget,
    };
    let mut consumed = start.cursor.byte;
    let mut at_line_start = !start.cursor.mid_line;
    loop {
        check_cancelled(cancel)?;
        if at_line_start
            && let Some(end) = request.end_line
            && segment.line > end
        {
            break;
        }
        match take_piece(
            &mut reader,
            &segment,
            at_line_start,
            budget,
            consumed,
            file_bytes,
            cancel,
        )? {
            PieceStep::EndOfFile => break,
            PieceStep::BudgetExhausted => {
                // CF2：首片就放不下任何可见字符 → 显式失败并给出可
                // 执行指导；已有内容则按预算截断收尾（truncated）。
                if segment.pieces.is_empty() {
                    return Err(ToolError::new(format!(
                        "read_file: max_bytes={} is too small to fit a visible \
                         numbered-line prefix plus one character; raise max_bytes",
                        request.byte_budget
                    )));
                }
                segment.truncated = true;
                break;
            }
            PieceStep::Piece(piece) => {
                absorb_piece(
                    &mut segment,
                    &mut budget,
                    &mut consumed,
                    &mut at_line_start,
                    piece,
                );
            }
        }
    }
    segment.at_line_start = at_line_start;
    Ok(segment)
}

/// 吸收一片：扣减双预算、推进消耗与行状态、入列。
fn absorb_piece(
    segment: &mut ReadSegment,
    budget: &mut PieceBudget,
    consumed: &mut u64,
    at_line_start: &mut bool,
    piece: ReadPiece,
) {
    budget.serialized = budget.serialized.saturating_sub(escape_len(&piece.display));
    budget.bytes = budget.bytes.saturating_sub(piece.display.len());
    *consumed += piece.bytes;
    *at_line_start = piece.ends_line;
    if piece.ends_line {
        segment.line += 1;
    }
    segment.pieces.push(piece);
}

/// 一次取片的三态结果。
enum PieceStep {
    Piece(ReadPiece),
    EndOfFile,
    /// 本段预算已不足以放入任何可见字符（含前缀开销）。
    BudgetExhausted,
}

/// 双口径预算（序列化字符 + UTF-8 字节），取片与段循环同源。
#[derive(Clone, Copy, Debug)]
struct PieceBudget {
    serialized: usize,
    bytes: usize,
}

/// 读一窗并构造下一片（行完整 / 行内截断 / EOF 收尾三态）。
fn take_piece(
    reader: &mut BufReader<&mut fs::File>,
    segment: &ReadSegment,
    at_line_start: bool,
    budget: PieceBudget,
    consumed: u64,
    file_bytes: u64,
    cancel: &CancelToken,
) -> Result<PieceStep, ToolError> {
    // 预算覆盖完整显示行（`N | ` 前缀 + 文本 + 行终止）：序列化口径
    // 前缀各字符 1、`\n` 计 2；字节口径前缀字节数 + 1。
    let (overhead_serialized, overhead_bytes) = if at_line_start {
        let prefix = format!("{} | ", segment.line);
        (prefix.chars().count() + 2, prefix.len() + 1)
    } else {
        (2, 1)
    };
    let scan_serialized = budget.serialized.saturating_sub(overhead_serialized);
    let scan_bytes = budget.bytes.saturating_sub(overhead_bytes);
    // CF2（审计返工）：预算放不下任何可见字符时如实上报，绝不产出
    // 成功的零进度段。
    if scan_serialized == 0 || scan_bytes == 0 {
        return Ok(PieceStep::BudgetExhausted);
    }
    let limit = scan_serialized
        .saturating_mul(6)
        .saturating_add(scan_bytes)
        .saturating_add(16);
    let window = read_window(reader, limit)
        .map_err(|error| ToolError::new(format!("read_file: read failed: {error}")))?;
    if window.is_empty() {
        return Ok(PieceStep::EndOfFile);
    }
    check_cancelled(cancel)?;
    let at_eof = consumed.saturating_add(window.len() as u64) >= file_bytes;
    let scan = scan_window(&window, scan_serialized, scan_bytes, at_eof);
    // 关键进度保证（CF1/CF2 返工的构造级落地）：不换行且未消费任何
    // 源字节的扫描（如下一个多字节字符放不进剩余字节预算）绝不能
    // 产出 bytes=0 的“前缀片”——那会让 consumed 原地踏步、单次调用
    // 内死循环。按预算不足上报：本段截断收尾，下一段以全新预算从同
    // 一字节继续，必然前进。
    if !scan.newline && scan.bytes == 0 {
        return Ok(PieceStep::BudgetExhausted);
    }
    let piece = piece_from_scan(scan, at_line_start, segment.line, at_eof, window.len());
    // 关键：窗口是一次性预读的；piece 只消费了它的前缀。把 reader
    // 回退到 piece 末尾，否则下一段会跳过未消费字节（乱序 + 跨字符
    // 边界 + 游标错位——READ-1 测试族正是钉住这一回归）。
    let rewind = window.len() as i64 - piece.bytes as i64;
    if rewind > 0
        && reader
            .seek_relative(-rewind)
            .map_err(|error| ToolError::new(format!("read_file: seek failed: {error}")))
            .is_err()
    {
        return Err(ToolError::new("read_file: seek failed while rewinding"));
    }
    Ok(PieceStep::Piece(piece))
}

/// 由扫描结果构造内容片（行完整 / 行内截断 / EOF 收尾三态）。
fn piece_from_scan(
    scan: WindowScan,
    at_line_start: bool,
    line: u64,
    at_eof: bool,
    window_len: usize,
) -> ReadPiece {
    if scan.newline {
        return complete_line_piece(
            scan.text,
            at_line_start,
            line,
            scan.bytes as u64,
            scan.lossy,
        );
    }
    if at_eof && scan.bytes >= window_len {
        // EOF 收尾的末行（无尾换行）：按行完整交付，显示仍补 `\n`。
        return complete_line_piece(
            scan.text,
            at_line_start,
            line,
            scan.bytes as u64,
            scan.lossy,
        );
    }
    // 预算用尽的行内截断（超长单行 / 预算边界）：partial 段。
    let display = if at_line_start {
        format!("{line} | {}", scan.text)
    } else {
        scan.text
    };
    ReadPiece {
        display,
        bytes: scan.bytes as u64,
        line,
        ends_line: false,
        lossy: scan.lossy,
    }
}

/// 构造一个以行终止收尾的内容片（规范化尾部 `\r`，显示补 `\n`）。
/// 构造一个以行终止收尾的内容片。CR 归一化职责**全部**在扫描层
/// （`scan_window` 的 `crlf_view`）：CRLF 终止的 `\r` 从不入 text；
/// 正文中的孤立 `\r`（行终止前或 EOF 收尾）是普通字符，必须原样
/// 保留——组装层不得再剥（CF8：这里曾对 text 尾部 CR 无差别 pop，
/// 导致 `a\r\r\nb\r` 随分段预算丢失正文 CR）。
fn complete_line_piece(
    text: String,
    at_line_start: bool,
    line: u64,
    bytes: u64,
    lossy: bool,
) -> ReadPiece {
    let display = if at_line_start {
        format!("{line} | {text}\n")
    } else {
        format!("{text}\n")
    };
    ReadPiece {
        display,
        bytes,
        line,
        ends_line: true,
        lossy,
    }
}

/// 组装模型可见输出（游标与状态字段全部同源于此）。
fn build_read_output(
    segment: &ReadSegment,
    requested: &str,
    start: ReadStart,
    file_bytes: u64,
    file_version: &str,
    request: &ReadRequest,
) -> Value {
    let content = segment.content();
    let offset = start.cursor.byte.saturating_add(segment.consumed_bytes());
    let has_more = offset < file_bytes && !segment.beyond_eof;
    let next = has_more.then_some(ReadCursor {
        byte: offset,
        line: segment.line,
        mid_line: !segment.at_line_start,
    });
    let end_line = segment
        .last_line()
        .unwrap_or(start.cursor.line.saturating_sub(1));
    let mut output = json!({
        "path": requested,
        "start_line": start.cursor.line,
        "end_line": end_line,
        "content": content,
        "truncated": segment.truncated,
        "has_more": has_more,
        "content_budget_chars": request.serialized_budget,
        "file_bytes": file_bytes,
        "file_version": file_version,
    });
    if let Some(cursor) = next {
        output["next_cursor"] = json!(format_cursor(cursor));
        if cursor.mid_line {
            output["partial_line"] = json!(true);
        } else {
            output["next_start_line"] = json!(cursor.line);
        }
    } else if segment.partial_line() {
        output["partial_line"] = json!(true);
    }
    if segment.beyond_eof {
        output["beyond_eof"] = json!(true);
    }
    if segment.pieces.iter().any(|piece| piece.lossy) {
        output["invalid_utf8"] = json!(true);
    }
    output
}

/// R1 兜底：以真实序列化长度校验；超出则从尾部整片收缩。取片已
/// 转义/字节双感知，正常路径构造即命中；一旦收缩会让段变空（只剩
/// 一片仍超）就显式失败——绝不返回成功零进度段（CF5：弹空后的空
/// 信封低于阈值时循环会以成功空段退出，必须在此拦截，不能依赖
/// while 条件自然退出）。
fn shrink_to_pruner_threshold(
    segment: &mut ReadSegment,
    requested: &str,
    start: ReadStart,
    file_bytes: u64,
    file_version: &str,
    request: &ReadRequest,
) -> Result<Value, ToolError> {
    let mut output =
        build_read_output(segment, requested, start, file_bytes, file_version, request);
    while exceeds_pruner_threshold(&output) {
        if segment.pieces.len() <= 1 {
            return Err(ToolError::new(format!(
                "read_file: the serialized segment for `{requested}` cannot fit \
                 the context budget (path or envelope too large); reduce the \
                 path length or report this input"
            )));
        }
        segment.pieces.pop();
        segment.truncated = true;
        segment.at_line_start = segment.pieces.last().is_some_and(|piece| piece.ends_line);
        segment.line = segment
            .pieces
            .last()
            .map(|piece| {
                if piece.ends_line {
                    piece.line + 1
                } else {
                    piece.line
                }
            })
            .unwrap_or(start.cursor.line);
        output = build_read_output(segment, requested, start, file_bytes, file_version, request);
    }
    Ok(output)
}

/// 真实序列化长度是否超过通用 pruner 阈值（R1 兜底校验）。
fn exceeds_pruner_threshold(output: &Value) -> bool {
    serde_json::to_string(output)
        .map(|text| text.chars().count() > crate::plugins::THRESHOLD_CHARS)
        .unwrap_or(false)
}

impl Tool for ReadFileTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "read_file".into(),
            description: "Read UTF-8 text from a file with line numbers. Paths are project-relative; absolute paths (anywhere on disk) are also accepted — reads are never confined to the project root. Returns one bounded segment per call with `has_more` and `next_cursor`: pass `next_cursor` back for the next contiguous segment until `has_more` is false. `max_bytes` is an upper bound; `content_budget_chars` reports the applied context-safe cap. Lines longer than a segment are split at character boundaries and marked `partial_line` — continue with `next_cursor`. Pass `expected_file_version` from the previous segment to fail explicitly when the file changed.".into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Project-relative or absolute file path"
                    },
                    "start_line": {
                        "type": "integer",
                        "minimum": 1,
                        "description": "First line to return. Defaults to 1. With `cursor`/`start_byte` it is only the line-number hint"
                    },
                    "end_line": {
                        "type": "integer",
                        "minimum": 1,
                        "description": "Last line to return, inclusive"
                    },
                    "start_byte": {
                        "type": "integer",
                        "minimum": 0,
                        "description": "Zero-based absolute UTF-8 byte offset to start from (use offsets returned by previous segments)"
                    },
                    "max_bytes": {
                        "type": "integer",
                        "minimum": 1,
                        "maximum": MAX_READ_BYTES,
                        "description": "Upper bound on returned content in UTF-8 bytes, including line-number prefixes. Defaults to 65536; a context-safe serialized-segment cap also applies (`content_budget_chars`)"
                    },
                    "cursor": {
                        "type": "string",
                        "description": "Opaque continuation cursor from a previous segment's `next_cursor`; returns the contiguous next segment"
                    },
                    "expected_file_version": {
                        "type": "string",
                        "description": "Value of `file_version` from the previous segment; the call fails with a change notice when the file changed"
                    }
                },
                "required": ["path"],
                "additionalProperties": false
            }),
            effect: ToolEffect::Read,
            strict: false,
        }
    }

    fn invoke(
        &self,
        arguments: &Value,
        project: &Project,
        cancel: &CancelToken,
    ) -> Result<Value, ToolError> {
        let requested = required_string_arg(arguments, "path", "read_file")?;
        let request = parse_read_request(arguments, requested)?;
        let path = project
            .resolve_existing(requested)
            .map_err(|error| tool_io_error("read_file", requested, error))?;

        if !path.is_file() {
            return Err(ToolError::new(format!(
                "read_file: `{requested}` is not a file"
            )));
        }

        let metadata =
            fs::metadata(&path).map_err(|error| tool_io_error("read_file", requested, error))?;
        let file_bytes = metadata.len();
        let file_version = file_identity(&metadata);
        if let Some(expected) = &request.expected_version
            && expected != &file_version
        {
            return Err(ToolError::new(format!(
                "read_file: `{requested}` changed since the last read (expected version \
                 {expected}, now {file_version}); the previous cursor may point into \
                 different content — restart the read from the start"
            )));
        }
        let mut file =
            fs::File::open(&path).map_err(|error| tool_io_error("read_file", requested, error))?;
        let start = resolve_read_start(&request, &mut file, file_bytes)?;
        let mut segment = read_segment(&mut file, &request, start, file_bytes, cancel)?;
        shrink_to_pruner_threshold(
            &mut segment,
            requested,
            start,
            file_bytes,
            &file_version,
            &request,
        )
    }
}
