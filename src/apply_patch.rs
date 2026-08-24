//! Pure parser and in-memory applier for CLAT's single-file patch language.
//!
//! Filesystem authority and atomic commit live in `plugins/apply_patch.rs`;
//! this module deliberately cannot write. Every hunk is applied to an owned
//! working copy, so the caller can validate the complete patch before one
//! capability-bound compare-and-swap commit.

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ParsedPatch {
    pub path: String,
    hunks: Vec<PatchHunk>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct PatchHunk {
    label: Option<String>,
    lines: Vec<PatchLine>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum PatchLine {
    Context(String),
    Delete(String),
    Add(String),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum LineEnding {
    None,
    Lf,
    CrLf,
}

impl LineEnding {
    fn text(self) -> &'static str {
        match self {
            Self::None => "",
            Self::Lf => "\n",
            Self::CrLf => "\r\n",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct FileLine {
    text: String,
    ending: LineEnding,
}

pub(crate) fn parse(input: &str) -> Result<ParsedPatch, String> {
    let lines = input
        .split('\n')
        .map(|line| line.strip_suffix('\r').unwrap_or(line))
        .collect::<Vec<_>>();
    let mut index = 0usize;
    expect_line(&lines, &mut index, "*** Begin Patch")?;
    let header = lines
        .get(index)
        .ok_or_else(|| "apply_patch: missing `*** Update File:` header".to_owned())?;
    let Some(path) = header.strip_prefix("*** Update File: ") else {
        return Err(unsupported_header(header));
    };
    let path = path.trim();
    if path.is_empty() {
        return Err("apply_patch: update path must not be empty".into());
    }
    index += 1;

    let mut hunks = Vec::new();
    let mut current: Option<PatchHunk> = None;
    let mut saw_end = false;
    while let Some(line) = lines.get(index).copied() {
        index += 1;
        if line == "*** End Patch" {
            if let Some(hunk) = current.take() {
                validate_hunk(&hunk, hunks.len() + 1)?;
                hunks.push(hunk);
            }
            saw_end = true;
            break;
        }
        if line.starts_with("*** ") {
            return Err(unsupported_header(line));
        }
        if line == "@@" || line.starts_with("@@ ") {
            if let Some(hunk) = current.take() {
                validate_hunk(&hunk, hunks.len() + 1)?;
                hunks.push(hunk);
            }
            let label = line.strip_prefix("@@ ").unwrap_or_default().trim();
            current = Some(PatchHunk {
                label: (!label.is_empty()).then(|| label.to_owned()),
                lines: Vec::new(),
            });
            continue;
        }
        if line.starts_with("@@") {
            return Err(format!(
                "apply_patch: hunk header must be `@@` or `@@ <label>`, got `{line}`"
            ));
        }
        let Some(hunk) = current.as_mut() else {
            return Err(format!(
                "apply_patch: expected `@@` before patch body, got `{line}`"
            ));
        };
        let (prefix, body) = line.split_at_checked(1).ok_or_else(|| {
            "apply_patch: empty hunk line must carry a context/add/delete prefix".to_owned()
        })?;
        let body = body.to_owned();
        hunk.lines.push(match prefix {
            " " => PatchLine::Context(body),
            "-" => PatchLine::Delete(body),
            "+" => PatchLine::Add(body),
            _ => {
                return Err(format!(
                    "apply_patch: hunk lines must start with space, `-`, or `+`; got `{line}`"
                ));
            }
        });
    }

    if !saw_end {
        return Err("apply_patch: missing `*** End Patch`".into());
    }
    let trailing = &lines[index..];
    if trailing.len() > 1 || trailing.first().is_some_and(|line| !line.is_empty()) {
        return Err("apply_patch: unexpected content after `*** End Patch`".into());
    }
    if hunks.is_empty() {
        return Err("apply_patch: at least one hunk is required".into());
    }
    Ok(ParsedPatch {
        path: path.to_owned(),
        hunks,
    })
}

pub(crate) fn apply(original: &str, patch: &ParsedPatch) -> Result<String, String> {
    let (bom, content) = original
        .strip_prefix('\u{feff}')
        .map_or((false, original), |content| (true, content));
    let had_final_newline = content.ends_with('\n');
    let mut lines = split_file_lines(content);
    let default_ending = dominant_ending(&lines);

    for (index, hunk) in patch.hunks.iter().enumerate() {
        apply_hunk(&mut lines, hunk, index + 1, default_ending)?;
    }
    normalize_line_endings(&mut lines, default_ending, had_final_newline);

    let mut output = String::with_capacity(original.len());
    if bom {
        output.push('\u{feff}');
    }
    for line in lines {
        output.push_str(&line.text);
        output.push_str(line.ending.text());
    }
    if output == original {
        return Err("apply_patch: patch would not change the file".into());
    }
    Ok(output)
}

fn expect_line(lines: &[&str], index: &mut usize, expected: &str) -> Result<(), String> {
    let actual = lines.get(*index).copied().unwrap_or_default();
    if actual != expected {
        return Err(format!(
            "apply_patch: expected `{expected}` as line {}, got `{actual}`",
            *index + 1
        ));
    }
    *index += 1;
    Ok(())
}

fn unsupported_header(header: &str) -> String {
    if header.starts_with("*** Add File:")
        || header.starts_with("*** Delete File:")
        || header.starts_with("*** Move to:")
        || header.starts_with("*** Update File:")
    {
        format!(
            "apply_patch: v1 supports exactly one existing `*** Update File:` target; unsupported `{header}`"
        )
    } else {
        format!("apply_patch: invalid patch header `{header}`")
    }
}

fn validate_hunk(hunk: &PatchHunk, number: usize) -> Result<(), String> {
    if hunk.lines.is_empty() {
        return Err(format!("apply_patch: hunk {number} has no body"));
    }
    let old_count = hunk
        .lines
        .iter()
        .filter(|line| !matches!(line, PatchLine::Add(_)))
        .count();
    let changed = hunk
        .lines
        .iter()
        .any(|line| !matches!(line, PatchLine::Context(_)));
    if old_count == 0 {
        return Err(format!(
            "apply_patch: hunk {number} has no context/deletion anchor"
        ));
    }
    if !changed {
        return Err(format!("apply_patch: hunk {number} contains no change"));
    }
    Ok(())
}

fn split_file_lines(content: &str) -> Vec<FileLine> {
    let mut lines = Vec::new();
    let mut start = 0usize;
    for (index, byte) in content.bytes().enumerate() {
        if byte != b'\n' {
            continue;
        }
        let raw = &content[start..index];
        let (text, ending) = raw
            .strip_suffix('\r')
            .map_or((raw, LineEnding::Lf), |text| (text, LineEnding::CrLf));
        lines.push(FileLine {
            text: text.to_owned(),
            ending,
        });
        start = index + 1;
    }
    if start < content.len() {
        lines.push(FileLine {
            text: content[start..].to_owned(),
            ending: LineEnding::None,
        });
    }
    lines
}

fn dominant_ending(lines: &[FileLine]) -> LineEnding {
    let mut lf = 0usize;
    let mut crlf = 0usize;
    for line in lines {
        match line.ending {
            LineEnding::Lf => lf += 1,
            LineEnding::CrLf => crlf += 1,
            LineEnding::None => {}
        }
    }
    if crlf > lf {
        LineEnding::CrLf
    } else {
        LineEnding::Lf
    }
}

fn apply_hunk(
    lines: &mut Vec<FileLine>,
    hunk: &PatchHunk,
    number: usize,
    default_ending: LineEnding,
) -> Result<(), String> {
    let old = hunk
        .lines
        .iter()
        .filter_map(|line| match line {
            PatchLine::Context(text) | PatchLine::Delete(text) => Some(text.as_str()),
            PatchLine::Add(_) => None,
        })
        .collect::<Vec<_>>();
    let matches = sequence_matches(lines, &old);
    let at = match matches.as_slice() {
        [] => {
            return Err(format!(
                "apply_patch: hunk {number}{} did not match; read the file and retry with exact context",
                hunk_label(hunk)
            ));
        }
        [at] => *at,
        positions => {
            return Err(format!(
                "apply_patch: hunk {number}{} is ambiguous ({} matches at lines {}); add surrounding context",
                hunk_label(hunk),
                positions.len(),
                positions
                    .iter()
                    .map(|position| (position + 1).to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
    };
    let local_ending = lines[at..at + old.len()]
        .iter()
        .find_map(|line| (line.ending != LineEnding::None).then_some(line.ending))
        .unwrap_or(default_ending);
    let followed_by_existing = at + old.len() < lines.len();
    let mut old_cursor = at;
    let mut replacement = Vec::new();
    for line in &hunk.lines {
        match line {
            PatchLine::Context(_) => {
                replacement.push(lines[old_cursor].clone());
                old_cursor += 1;
            }
            PatchLine::Delete(_) => old_cursor += 1,
            PatchLine::Add(text) => replacement.push(FileLine {
                text: text.clone(),
                ending: local_ending,
            }),
        }
    }
    for line in replacement.iter_mut().rev().skip(1) {
        if line.ending == LineEnding::None {
            line.ending = local_ending;
        }
    }
    if followed_by_existing
        && let Some(last) = replacement.last_mut()
        && last.ending == LineEnding::None
    {
        last.ending = local_ending;
    }
    lines.splice(at..at + old.len(), replacement);
    Ok(())
}

fn sequence_matches(lines: &[FileLine], old: &[&str]) -> Vec<usize> {
    if old.is_empty() || old.len() > lines.len() {
        return Vec::new();
    }
    lines
        .windows(old.len())
        .enumerate()
        .filter_map(|(index, window)| {
            window
                .iter()
                .map(|line| line.text.as_str())
                .eq(old.iter().copied())
                .then_some(index)
        })
        .collect()
}

fn hunk_label(hunk: &PatchHunk) -> String {
    hunk.label
        .as_deref()
        .map_or_else(String::new, |label| format!(" (`{label}`)"))
}

fn normalize_line_endings(lines: &mut [FileLine], default: LineEnding, had_final_newline: bool) {
    let line_count = lines.len();
    for (index, line) in lines.iter_mut().enumerate() {
        let last = index + 1 == line_count;
        if !last && line.ending == LineEnding::None {
            line.ending = default;
        }
        if last {
            line.ending = if had_final_newline {
                if line.ending == LineEnding::None {
                    default
                } else {
                    line.ending
                }
            } else {
                LineEnding::None
            };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn update(path: &str, body: &str) -> ParsedPatch {
        parse(&format!(
            "*** Begin Patch\n*** Update File: {path}\n{body}\n*** End Patch"
        ))
        .expect("parse")
    }

    #[test]
    fn applies_multiple_hunks_only_after_exact_unique_matches() {
        let patch = update(
            "demo.txt",
            "@@ alpha\n fn alpha() {\n-    1\n+    10\n }\n@@ omega\n fn omega() {\n-    2\n+    20\n }",
        );
        let original = "fn alpha() {\n    1\n}\n\nfn omega() {\n    2\n}\n";
        let updated = apply(original, &patch).expect("apply");
        assert_eq!(
            updated,
            "fn alpha() {\n    10\n}\n\nfn omega() {\n    20\n}\n"
        );

        let conflict = update(
            "demo.txt",
            "@@ first\n-    1\n+    10\n@@ stale second\n-    99\n+    20",
        );
        assert!(apply(original, &conflict).unwrap_err().contains("hunk 2"));
    }

    #[test]
    fn preserves_bom_mixed_line_endings_and_final_newline_state() {
        let patch = update("mixed.txt", "@@\n beta\n+inserted\n gamma");
        let original = "\u{feff}alpha\r\nbeta\ngamma";
        assert_eq!(
            apply(original, &patch).expect("apply"),
            "\u{feff}alpha\r\nbeta\ninserted\ngamma"
        );

        let crlf = update("crlf.txt", "@@\n-old\n+new");
        assert_eq!(apply("\u{feff}old\r\n", &crlf).unwrap(), "\u{feff}new\r\n");
    }

    #[test]
    fn rejects_ambiguous_or_unsupported_patches() {
        let patch = update("x.txt", "@@\n-same\n+new");
        assert!(
            apply("same\nsame\n", &patch)
                .unwrap_err()
                .contains("ambiguous")
        );
        assert!(
            parse("*** Begin Patch\n*** Add File: x\n+x\n*** End Patch")
                .unwrap_err()
                .contains("supports exactly one")
        );
        assert!(
            parse("*** Begin Patch\n*** Update File: x\n@@\n-a\n+b\n*** Update File: y\n@@\n-c\n+d\n*** End Patch")
                .unwrap_err()
                .contains("supports exactly one")
        );
        assert!(
            parse("*** Begin Patch\n*** Update File: x\n@@bad\n-a\n+b\n*** End Patch")
                .unwrap_err()
                .contains("hunk header")
        );
    }
}
