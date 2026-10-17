//! Read hints only: no snippets, fuzzy mutation, extra reads or scope expansion.
use super::*;

pub(super) fn mismatch(content: &str, old: &str, index: usize) -> ToolError {
    let count = offsets(content, old).count();
    let (code, message) = if count == 0 {
        (
            "not_found",
            "edit_file: `old_str` not found — read the file and copy the exact text".into(),
        )
    } else {
        (
            "ambiguous",
            format!(
                "edit_file: `old_str` matches {count} times — include surrounding lines to make it unique"
            ),
        )
    };
    let anchor: String = old
        .lines()
        .find(|line| !line.trim().is_empty())
        .unwrap_or("")
        .trim_start()
        .chars()
        .take(16)
        .collect();
    let locator = if count > 0 { old } else { &anchor };
    let mut candidates = Vec::new();
    let mut omitted = false;
    let total = content.lines().count().max(1);
    if count > 0 || anchor.chars().count() >= 8 {
        for (seen, offset) in offsets(content, locator).enumerate() {
            if seen == 32 {
                omitted = true;
                break;
            }
            let line = content[..offset].bytes().filter(|b| *b == b'\n').count() + 1;
            let start = line.saturating_sub(2).max(1);
            let hint = json!({"start_line":start,"end_line":(line + 2).min(total)});
            if candidates.contains(&hint) {
                continue;
            }
            if candidates.len() == 3 {
                omitted = true;
                break;
            }
            candidates.push(hint);
        }
    }
    let line_endings = if content.contains("\r\n") {
        "contains_crlf"
    } else {
        "lf_or_single_line"
    };
    ToolError::new(message).with_recovery(json!({
        "code":code,"edit_index":index,"applied":false,"candidates":candidates,
        "candidates_omitted":omitted,"line_endings":line_endings,
        "next_action":"Read the candidate windows with read_file (same path, start_line/end_line); hints are not exact matches. If empty, search this file for surrounding context. Copy byte-exact old_str and include unique context; CRLF bytes must be preserved. Retry only after inspecting the file."
    }))
}

// Advance by one Unicode scalar, not pattern length: overlapping occurrences
// are ambiguous too ("aa" appears twice in "aaa").
pub(super) fn offsets<'a>(content: &'a str, needle: &'a str) -> impl Iterator<Item = usize> + 'a {
    let mut cursor = 0;
    std::iter::from_fn(move || {
        let start = cursor + content.get(cursor..)?.find(needle)?;
        cursor = start + content[start..].chars().next()?.len_utf8();
        Some(start)
    })
}
