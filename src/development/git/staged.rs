//! Git metadata and textual evidence from the same frozen index and base tree.
use super::index::StagedDiff;
use crate::CancelToken;
use std::collections::BTreeMap;
use std::ffi::OsString;

const CAP: usize = 16 * 1024 * 1024;

#[derive(Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(super) struct Paths {
    pub(super) old: Vec<u8>,
    pub(super) new: Option<Vec<u8>>,
}

impl Paths {
    pub(super) fn current(&self) -> &[u8] {
        self.new.as_deref().unwrap_or(&self.old)
    }
}

#[derive(Debug)]
pub(super) struct StagedFile {
    pub(super) paths: Paths,
    pub(super) kind: u8,
    pub(super) old_mode: String,
    pub(super) new_mode: String,
    pub(super) lines: Option<(usize, usize)>,
    pub(super) patch: Option<String>,
}

impl StagedFile {
    fn changed_lines(&self) -> usize {
        self.lines
            .map_or(0, |(add, delete)| add.saturating_add(delete))
    }

    fn wants_patch(&self) -> bool {
        self.changed_lines() != 0
            && self.kind != b'T'
            && self.old_mode != "160000"
            && self.new_mode != "160000"
            && !stats_only(&self.paths.old)
            && self
                .paths
                .new
                .as_deref()
                .is_none_or(|path| !stats_only(path))
    }

    pub(super) fn is_dependency_manifest(&self) -> bool {
        matches!(
            basename(self.paths.current()),
            b"Cargo.toml"
                | b"package.json"
                | b"pyproject.toml"
                | b"requirements.txt"
                | b"Pipfile"
                | b"go.mod"
                | b"Gemfile"
                | b"composer.json"
                | b"pubspec.yaml"
                | b"Package.swift"
                | b"Podfile"
                | b"build.gradle"
                | b"build.gradle.kts"
                | b"pom.xml"
        )
    }
}

fn basename(path: &[u8]) -> &[u8] {
    path.rsplit(|&byte| byte == b'/').next().unwrap_or(path)
}

fn stats_only(path: &[u8]) -> bool {
    let name = basename(path);
    matches!(
        name,
        b"Cargo.lock"
            | b"package-lock.json"
            | b"npm-shrinkwrap.json"
            | b"pnpm-lock.yaml"
            | b"yarn.lock"
            | b"bun.lock"
            | b"bun.lockb"
            | b"uv.lock"
            | b"poetry.lock"
            | b"Pipfile.lock"
            | b"Gemfile.lock"
            | b"composer.lock"
            | b"go.sum"
            | b"pubspec.lock"
            | b"Package.resolved"
            | b"Podfile.lock"
            | b"gradle.lockfile"
    ) || name.ends_with(b".min.js")
        || name.ends_with(b".min.css")
}

pub(super) fn collect(
    diff: &StagedDiff<'_>,
    cancel: &CancelToken,
) -> Result<Vec<StagedFile>, String> {
    let output = diff.capture(
        &["--raw", "--numstat", "-z", "--find-renames"],
        &[],
        CAP,
        cancel,
    )?;
    if !output.status.success() {
        return Err("git_failed".into());
    }
    let mut files = manifest(&output.stdout)?;
    let mut candidates: Vec<_> = files
        .iter()
        .enumerate()
        .filter(|(_, file)| file.wants_patch())
        .map(|(i, file)| (i, !file.is_dependency_manifest(), file.changed_lines()))
        .collect();
    // Keep early capture failures and large changes from crowding out small source files.
    candidates.sort_by_key(|&(i, ordinary, lines)| (ordinary, lines, i));
    let mut batches = vec![Vec::new()];
    let mut bytes = 0;
    for (index, _, _) in candidates {
        let cost = path_cost(&files[index].paths);
        // Bound OS command-line size independently of the model window.
        if bytes + cost > 8 * 1024 && bytes > 0 {
            batches.push(Vec::new());
            bytes = 0;
        }
        batches.last_mut().expect("initial batch").push(index);
        bytes += cost;
    }
    let mut remaining = CAP;
    for batch in batches.iter().filter(|batch| !batch.is_empty()) {
        if cancel.is_cancelled() {
            return Err("cancelled".into());
        }
        collect_patches(diff, &mut files, batch, &mut remaining, cancel)?;
    }
    Ok(files)
}

fn path_cost(paths: &Paths) -> usize {
    paths.old.len() + paths.new.as_ref().map_or(0, |path| path.len() + 4) + 4
}

fn path_arg(path: &[u8]) -> Result<OsString, String> {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStringExt;
        Ok(OsString::from_vec(path.to_vec()))
    }
    #[cfg(not(unix))]
    {
        Ok(OsString::from(
            std::str::from_utf8(path).map_err(|_| "git_encoding")?,
        ))
    }
}

fn collect_patches(
    diff: &StagedDiff<'_>,
    files: &mut [StagedFile],
    batch: &[usize],
    remaining: &mut usize,
    cancel: &CancelToken,
) -> Result<(), String> {
    if *remaining == 0 {
        return Ok(());
    }
    let mut args = Vec::new();
    for &index in batch {
        let paths = &files[index].paths;
        args.push(path_arg(&paths.old)?);
        if let Some(new) = &paths.new {
            args.push(path_arg(new)?);
        }
    }
    let options = [
        "--raw",
        "-z",
        "--patch",
        "--find-renames",
        "--diff-filter=ACDMR",
        "--src-prefix=a/",
        "--dst-prefix=b/",
        "--output-indicator-new=+",
        "--output-indicator-old=-",
        "--output-indicator-context= ",
    ];
    let capture = diff.capture(&options, &args, *remaining, cancel);
    let output = match capture {
        Ok(output) if output.status.success() => output.stdout,
        Ok(_) => return Err("git_failed".into()),
        Err(error) if error == "process output exceeded byte limit" => {
            if batch.len() > 1 {
                let (left, right) = batch.split_at(batch.len() / 2);
                collect_patches(diff, files, left, remaining, cancel)?;
                collect_patches(diff, files, right, remaining, cancel)?;
            }
            return Ok(());
        }
        Err(error) => return Err(error),
    };
    let patches = patch_sections(&output)?;
    for &index in batch {
        let file = &mut files[index];
        if let Some(body) = patches.get(&file.paths) {
            let text = String::from_utf8_lossy(body);
            if text.len() <= *remaining {
                *remaining -= text.len();
                file.patch = Some(text.into_owned());
            }
        }
    }
    Ok(())
}

fn manifest(bytes: &[u8]) -> Result<Vec<StagedFile>, String> {
    let mut fields = bytes.split(|&byte| byte == 0).peekable();
    let mut files = raw_files(&mut fields)?;
    for file in &mut files {
        let field = fields.next().ok_or("git_encoding")?;
        let mut columns = field.splitn(3, |&byte| byte == b'\t');
        let added = columns.next().ok_or("git_encoding")?;
        let removed = columns.next().ok_or("git_encoding")?;
        let name = columns.next().ok_or("git_encoding")?;
        let correct_path = if let Some(new) = &file.paths.new {
            name.is_empty() && fields.next() == Some(&file.paths.old) && fields.next() == Some(new)
        } else {
            name == file.paths.old
        };
        if !correct_path {
            return Err("git_encoding".into());
        }
        file.lines = if added == b"-" && removed == b"-" {
            None
        } else {
            let number = |bytes: &[u8]| {
                String::from_utf8_lossy(bytes)
                    .parse::<usize>()
                    .map_err(|_| "git_encoding")
            };
            Some((number(added)?, number(removed)?))
        };
    }
    if fields.any(|field| !field.is_empty()) {
        return Err("git_encoding".into());
    }
    Ok(files)
}

fn raw_files<'a>(
    fields: &mut std::iter::Peekable<impl Iterator<Item = &'a [u8]>>,
) -> Result<Vec<StagedFile>, String> {
    let mut files = Vec::new();
    while fields.peek().is_some_and(|field| field.starts_with(b":")) {
        let header = std::str::from_utf8(fields.next().ok_or("git_encoding")?)
            .map_err(|_| "git_encoding")?;
        let columns: Vec<_> = header[1..].split_whitespace().collect();
        let [old_mode, new_mode, _, _, status] = columns.as_slice() else {
            return Err("git_encoding".into());
        };
        let kind = *status.as_bytes().first().ok_or("git_encoding")?;
        let old = fields
            .next()
            .filter(|path| !path.is_empty())
            .ok_or("git_encoding")?
            .to_vec();
        let new = if matches!(kind, b'R' | b'C') {
            Some(
                fields
                    .next()
                    .filter(|path| !path.is_empty())
                    .ok_or("git_encoding")?
                    .to_vec(),
            )
        } else {
            None
        };
        files.push(StagedFile {
            paths: Paths { old, new },
            kind,
            old_mode: (*old_mode).into(),
            new_mode: (*new_mode).into(),
            lines: None,
            patch: None,
        });
    }
    Ok(files)
}

fn patch_sections(bytes: &[u8]) -> Result<BTreeMap<Paths, &[u8]>, String> {
    if bytes.is_empty() {
        return Ok(BTreeMap::new());
    }
    let boundary = bytes
        .windows(2)
        .position(|pair| pair == b"\0\0")
        .ok_or("git_encoding")?;
    let files = raw_files(&mut bytes[..boundary].split(|&byte| byte == 0).peekable())?;
    let body = &bytes[boundary + 2..];
    let mut offsets = vec![0];
    offsets.extend(
        body.windows(12)
            .enumerate()
            .filter_map(|(i, part)| (part == b"\ndiff --git ").then_some(i + 1)),
    );
    offsets.push(body.len());
    if !body.starts_with(b"diff --git ") || offsets.len() != files.len() + 1 {
        return Err("git_encoding".into());
    }
    Ok(files
        .into_iter()
        .zip(offsets.windows(2))
        .map(|(file, span)| (file.paths, &body[span[0]..span[1]]))
        .collect())
}
