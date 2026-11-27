//! Deterministic selection of staged Git evidence for one model request.
use super::staged::StagedFile;
use crate::{CancelToken, model::estimate_tokens_conservative as tokens};
use std::collections::{BTreeMap, VecDeque};
use std::fmt::Write as _;

struct Input {
    remaining: usize,
    text: String,
}

impl Input {
    fn new(remaining: usize) -> Self {
        Self {
            remaining,
            text: String::new(),
        }
    }

    fn push(&mut self, text: &str) -> bool {
        let cost = tokens(text);
        if cost > self.remaining {
            return false;
        }
        self.remaining -= cost;
        self.text.push_str(text);
        true
    }
}

pub(super) fn render(
    files: &[StagedFile],
    available: usize,
    cancel: &CancelToken,
) -> Result<String, String> {
    let mut input = Input::new(available);
    let rows: Vec<_> = files.iter().map(file_row).collect();
    let mut patches: Vec<_> = files
        .iter()
        .filter_map(|file| file.patch.as_deref().map(|patch| (file, patch)))
        .collect();
    patches.sort_by_key(|(file, patch)| (!file.is_dependency_manifest(), patch.len()));
    let cover = format!(
        "\n{} staged file(s). Patch text may be partial.\n",
        files.len(),
    );
    input.push(&cover);
    let row_cost: usize = rows.iter().map(|row| tokens(row)).sum();
    let patch_cost: usize = patches.iter().map(|(_, patch)| tokens(patch)).sum();
    if row_cost + patch_cost <= input.remaining {
        for row in &rows {
            input.push(row);
        }
        for (_, patch) in patches {
            input.push(patch);
        }
        return Ok(input.text);
    }
    render_manifest(&mut input, files, &rows, !patches.is_empty());
    render_patches(&mut input, patches, cancel)?;
    Ok(input.text)
}

fn render_manifest(input: &mut Input, files: &[StagedFile], rows: &[String], has_patches: bool) {
    let room = if has_patches {
        input.remaining / 2
    } else {
        input.remaining
    };
    let all_rows: usize = rows.iter().map(|row| tokens(row)).sum();
    if all_rows <= room {
        for row in rows {
            input.push(row);
        }
        return;
    }
    let mut remaining_by_kind = BTreeMap::new();
    for file in files {
        *remaining_by_kind.entry(file.kind).or_insert(0usize) += 1;
    }
    let mut room_for_rows = room.saturating_sub(tokens(&omitted_rows(&remaining_by_kind)));
    for (file, row) in files.iter().zip(rows) {
        let cost = tokens(row);
        if cost <= room_for_rows {
            input.push(row);
            room_for_rows -= cost;
            *remaining_by_kind
                .get_mut(&file.kind)
                .expect("counted above") -= 1;
        }
    }
    input.push(&omitted_rows(&remaining_by_kind));
}

fn omitted_rows(counts: &BTreeMap<u8, usize>) -> String {
    let mut line = String::from("File list omits ");
    for (&kind, &count) in counts.iter().filter(|(_, count)| **count > 0) {
        let _ = write!(line, "{count} {}; ", kind_name(kind));
    }
    line.push_str("(omitted paths may appear in patch excerpts).\n");
    line
}

fn file_row(file: &StagedFile) -> String {
    let mut row = format!("{} {}", kind_name(file.kind), display_path(&file.paths.old));
    if let Some(new) = &file.paths.new {
        let _ = write!(row, " -> {}", display_path(new));
    }
    if let Some((add, delete)) = file.lines {
        let _ = write!(row, " +{add} -{delete}");
    } else {
        row.push_str(" binary");
    }
    if file.old_mode != file.new_mode {
        let _ = write!(row, " mode {} -> {}", file.old_mode, file.new_mode);
    }
    row.push('\n');
    row
}

fn kind_name(kind: u8) -> &'static str {
    match kind {
        b'A' => "added",
        b'C' => "copied",
        b'D' => "deleted",
        b'M' => "modified",
        b'R' => "renamed",
        b'T' => "type changed",
        b'U' => "unmerged",
        _ => "other change",
    }
}

fn display_path(path: &[u8]) -> String {
    if let Ok(path) = std::str::from_utf8(path) {
        format!("{path:?}")
    } else {
        let escaped: String = path
            .iter()
            .flat_map(|&b| std::ascii::escape_default(b))
            .map(char::from)
            .collect();
        format!("\"{escaped}\"")
    }
}

fn render_patches(
    input: &mut Input,
    patches: Vec<(&StagedFile, &str)>,
    cancel: &CancelToken,
) -> Result<(), String> {
    let share = input.remaining / patches.len().max(1);
    let mut pending = VecDeque::new();
    for (file, patch) in patches {
        if cancel.is_cancelled() {
            return Err("cancelled".into());
        }
        let allowed = if file.is_dependency_manifest() {
            share.max(input.remaining / 2)
        } else {
            share
        };
        if tokens(patch) <= allowed && input.push(patch) {
            continue;
        }
        let path = display_path(file.paths.current());
        let hunks = hunks(patch);
        if !hunks.is_empty() {
            pending.push_back((path, VecDeque::from(hunks)));
        }
    }
    while let Some((path, mut hunks)) = pending.pop_front() {
        if cancel.is_cancelled() {
            return Err("cancelled".into());
        }
        if input.remaining <= tokens("Patch excerpt") {
            break;
        }
        let file_share = input.remaining / (pending.len() + 1);
        // Budget room for later regions of the same large file as well.
        let useful_fragment = tokens(&path) + 64;
        let share = (file_share / hunks.len()).max(useful_fragment.min(file_share));
        let hunk = hunks.pop_front().expect("queue holds nonempty hunk lists");
        let evidence = compact_hunk(&path, hunk, share);
        if !evidence.is_empty() {
            input.push(&evidence);
        }
        if !hunks.is_empty() {
            pending.push_back((path, hunks));
        }
    }
    Ok(())
}

fn hunks(patch: &str) -> Vec<&str> {
    let mut starts: Vec<_> = patch
        .match_indices("\n@@ ")
        .map(|(offset, _)| offset + 1)
        .collect();
    starts.push(patch.len());
    starts
        .windows(2)
        .map(|span| &patch[span[0]..span[1]])
        .collect()
}

fn compact_hunk(path: &str, hunk: &str, budget: usize) -> String {
    let mut lines = hunk.lines();
    let header = short_line(lines.next().unwrap_or_default());
    let heading = format!("\nPatch excerpt for {path}\n{header}\n");
    let mut counts = BTreeMap::new();
    let mut changed = Vec::new();
    for line in lines.filter(|line| line.starts_with(['+', '-'])) {
        let count = counts.entry(line).or_insert(0usize);
        if *count == 0 {
            changed.push(line);
        }
        *count += 1;
    }
    // When even compacted evidence is too big, retain unique changes before repetitions.
    let mut order: Vec<_> = (0..changed.len()).collect();
    order.sort_by_key(|&i| counts[changed[i]]);
    let mut selected = Vec::new();
    let marker = "[additional changed lines omitted]\n";
    let mut remaining = budget.saturating_sub(tokens(&heading) + tokens(marker));
    for index in order {
        let line = changed[index];
        let count = counts[line];
        let mut text = short_line(line);
        if count > 1 {
            let _ = write!(text, " [occurs {count} times in this hunk]");
        }
        text.push('\n');
        let cost = tokens(&text);
        if cost <= remaining {
            remaining -= cost;
            selected.push((index, text));
        }
    }
    if selected.is_empty() {
        return String::new();
    }
    selected.sort_by_key(|(index, _)| *index);
    let incomplete = selected.len() != changed.len();
    let mut text = heading;
    for (_, line) in selected {
        text.push_str(&line);
    }
    if incomplete {
        text.push_str(marker);
    }
    text
}

fn short_line(line: &str) -> String {
    if line.len() <= 512 {
        return line.to_owned();
    }
    let first = &line[..line.floor_char_boundary(256)];
    let last = &line[line.ceil_char_boundary(line.len() - 128)..];
    format!("{first} [… {} bytes total …] {last}", line.len())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::development::git::staged::Paths;

    #[test]
    fn development_git_evidence_reserves_space_for_later_hunks() {
        let additions: String = (0..800)
            .map(|line| format!("+first region has a distinct line {line}\n"))
            .collect();
        let patch = format!(
            "diff --git a/src.rs b/src.rs\n@@ -1 +1,800 @@\n{additions}\n@@ -900 +1700 @@\n+LATE_FIX\n"
        );
        let files = [StagedFile {
            paths: Paths {
                old: b"src.rs".to_vec(),
                new: None,
            },
            kind: b'M',
            old_mode: "100644".into(),
            new_mode: "100644".into(),
            lines: Some((801, 0)),
            patch: Some(patch),
        }];
        let evidence = render(&files, 256, &CancelToken::new()).unwrap();
        assert!(tokens(&evidence) <= 256);
        assert!(evidence.contains("+first region"));
        assert!(evidence.contains("+LATE_FIX"));
    }

    #[test]
    fn development_git_evidence_reports_inventory_omissions_by_kind() {
        let files: Vec<_> = (0..500)
            .map(|i| StagedFile {
                paths: Paths {
                    old: format!("unusually-long-directory-name-{i}/package-lock.json")
                        .into_bytes(),
                    new: None,
                },
                kind: if i == 499 { b'D' } else { b'A' },
                old_mode: "100644".into(),
                new_mode: "100644".into(),
                lines: Some((10, 4)),
                patch: None,
            })
            .collect();
        let evidence = render(&files, 256, &CancelToken::new()).unwrap();
        assert!(tokens(&evidence) <= 256);
        let listed = evidence
            .lines()
            .filter(|line| line.starts_with("added "))
            .count();
        assert!(listed > 0 && listed < 499);
        assert!(evidence.contains(&format!("{} added", 499 - listed)));
        assert!(evidence.contains("1 deleted"));
        assert!(evidence.contains("500 staged file(s)"));
    }
}
