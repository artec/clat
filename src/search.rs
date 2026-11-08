//! Bounded, Git-aware search engine for `builtin.search`.

use crate::{CancelToken, Project};
use globset::{GlobBuilder, GlobSet, GlobSetBuilder};
use ignore::WalkBuilder;
use regex::{Regex, RegexBuilder};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

pub(crate) const DEFAULT_RESULTS: usize = 50;
pub(crate) const MAX_RESULTS_PER_PAGE: usize = 500;
const MAX_QUERY_BYTES: usize = 16 * 1024;
const MAX_PATTERNS: usize = 32;
const MAX_PATTERN_BYTES: usize = 1024;
const MAX_FILES: usize = 20_000;
const MAX_FILE_BYTES: usize = 1024 * 1024;
const MAX_TOTAL_BYTES: usize = 64 * 1024 * 1024;
const MAX_TOTAL_MATCHES: usize = 10_000;
const MAX_MATCH_LINE_BYTES: usize = 1024;
const MAX_WALK_DEPTH: usize = 64;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SearchMode {
    Literal,
    Regex,
}

#[derive(Clone, Debug)]
struct SearchRequest {
    /// 内容检索词；`None` = glob 列文件模式（include_globs 必填）。
    query: Option<String>,
    requested_path: String,
    mode: SearchMode,
    case_sensitive: bool,
    include_globs: Vec<String>,
    exclude_globs: Vec<String>,
    extensions: BTreeSet<String>,
    include_hidden: bool,
    respect_gitignore: bool,
    max_results: usize,
    cursor: Option<SearchCursor>,
}

#[derive(Clone, Debug)]
struct SearchCursor {
    query_digest: String,
    snapshot_digest: String,
    offset: usize,
}

#[derive(Clone, Debug)]
struct Candidate {
    path: PathBuf,
    match_path: String,
    display_path: String,
}

#[derive(Clone, Debug)]
struct FoundMatch {
    path: String,
    line: usize,
    text: String,
    text_truncated: bool,
}

pub(crate) fn execute(
    arguments: &Value,
    project: &Project,
    cancel: &CancelToken,
) -> Result<Value, String> {
    let request = SearchRequest::parse(arguments)?;
    check_cancelled(cancel)?;
    reject_explicit_symlink(project, &request.requested_path)?;
    let root = project
        .resolve_existing(&request.requested_path)
        .map_err(|error| format!("search: `{}`: {error}", request.requested_path))?;
    let project_root = project
        .resolve_existing(".")
        .map_err(|error| format!("search: cannot resolve project root: {error}"))?;
    let include = build_globset(&request.include_globs, "include_globs")?;
    let exclude = build_globset(&request.exclude_globs, "exclude_globs")?;
    if request.query.is_none() {
        return execute_glob(
            &request,
            &root,
            &project_root,
            include.as_ref(),
            exclude.as_ref(),
            cancel,
        );
    }
    let matcher = build_matcher(&request, request.query.as_deref().unwrap_or_default())?;
    let query_digest = request.query_digest(&root);
    if let Some(cursor) = &request.cursor
        && cursor.query_digest != query_digest
    {
        return Err("search: cursor does not belong to this query/configuration".into());
    }

    let candidates = collect_candidates(
        &root,
        &project_root,
        &request,
        include.as_ref(),
        exclude.as_ref(),
        cancel,
    )?;
    let mut snapshot = Sha256::new();
    let mut matches = Vec::new();
    let mut files_searched = 0usize;
    let mut files_skipped = 0usize;
    let mut bytes_searched = 0usize;
    let mut any_line_truncated = false;

    for candidate in candidates {
        check_cancelled(cancel)?;
        let Some(bytes) = read_candidate(&candidate, project, &project_root)? else {
            files_skipped += 1;
            continue;
        };
        snapshot.update(candidate.match_path.as_bytes());
        snapshot.update([0]);
        snapshot.update((bytes.len() as u64).to_le_bytes());
        snapshot.update(Sha256::digest(&bytes));
        bytes_searched = bytes_searched.saturating_add(bytes.len());
        if bytes_searched > MAX_TOTAL_BYTES {
            return Err(format!(
                "search: scanned bytes exceed {MAX_TOTAL_BYTES}; narrow path/globs/extensions"
            ));
        }
        if bytes.len() > MAX_FILE_BYTES || bytes.contains(&0) {
            files_skipped += 1;
            continue;
        }
        let Ok(text) = std::str::from_utf8(&bytes) else {
            files_skipped += 1;
            continue;
        };
        files_searched += 1;
        for (line_index, line) in text.lines().enumerate() {
            check_cancelled(cancel)?;
            if !matcher.is_match(line) {
                continue;
            }
            if matches.len() >= MAX_TOTAL_MATCHES {
                return Err(format!(
                    "search: matches exceed {MAX_TOTAL_MATCHES}; narrow query/path/globs"
                ));
            }
            let (text, text_truncated) = truncate_utf8(line, MAX_MATCH_LINE_BYTES);
            any_line_truncated |= text_truncated;
            matches.push(FoundMatch {
                path: candidate.display_path.clone(),
                line: line_index + 1,
                text,
                text_truncated,
            });
        }
    }

    let snapshot_digest = format!("{:x}", snapshot.finalize());
    let offset = request.cursor.as_ref().map_or(0, |cursor| cursor.offset);
    if let Some(cursor) = &request.cursor {
        if cursor.snapshot_digest != snapshot_digest {
            return Err("search: cursor invalidated because the searchable files changed".into());
        }
        if offset > matches.len() {
            return Err("search: cursor offset is beyond the current result set".into());
        }
    }
    let end = offset
        .saturating_add(request.max_results)
        .min(matches.len());
    let page = &matches[offset..end];
    let next_cursor = (end < matches.len()).then(|| {
        SearchCursor {
            query_digest: query_digest.clone(),
            snapshot_digest: snapshot_digest.clone(),
            offset: end,
        }
        .encode()
    });
    let rows = page
        .iter()
        .map(|found| {
            json!({
                "path": found.path,
                "line": found.line,
                "text": found.text,
                "text_truncated": found.text_truncated
            })
        })
        .collect::<Vec<_>>();

    Ok(json!({
        "query": request.query,
        "path": request.requested_path,
        "mode": request.mode.as_str(),
        "matches": rows,
        "total_matches": matches.len(),
        "files_searched": files_searched,
        "files_skipped": files_skipped,
        "bytes_searched": bytes_searched,
        "next_cursor": next_cursor,
        "truncated": next_cursor.is_some() || any_line_truncated
    }))
}

#[derive(Clone, Debug)]
struct FoundFile {
    path: String,
    modified_secs: Option<u64>,
}

/// glob 列文件模式（query 省略、include_globs 必填）：不读内容，只按
/// 模式匹配相对路径；按修改时间倒序（Claude Code Glob 同款，并列按
/// 路径稳定排序）；游标快照 = 路径+mtime 摘要——文件一动游标即失效，
/// 与内容模式同等纪律。
fn execute_glob(
    request: &SearchRequest,
    root: &Path,
    project_root: &Path,
    include: Option<&GlobSet>,
    exclude: Option<&GlobSet>,
    cancel: &CancelToken,
) -> Result<Value, String> {
    let candidates = collect_candidates(root, project_root, request, include, exclude, cancel)?;
    let mut files = Vec::new();
    for candidate in candidates {
        check_cancelled(cancel)?;
        let Ok(metadata) = std::fs::symlink_metadata(&candidate.path) else {
            continue;
        };
        if !metadata.file_type().is_file() {
            continue;
        }
        let modified_secs = metadata
            .modified()
            .ok()
            .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|duration| duration.as_secs());
        files.push(FoundFile {
            path: candidate.display_path,
            modified_secs,
        });
    }
    files.sort_by(|left, right| {
        right
            .modified_secs
            .cmp(&left.modified_secs)
            .then_with(|| left.path.cmp(&right.path))
    });

    let mut snapshot = Sha256::new();
    for file in &files {
        snapshot.update(file.path.as_bytes());
        snapshot.update([0]);
        snapshot.update(file.modified_secs.unwrap_or(0).to_le_bytes());
    }
    let snapshot_digest = format!("{:x}", snapshot.finalize());
    let query_digest = request.query_digest(root);
    let offset = request.cursor.as_ref().map_or(0, |cursor| cursor.offset);
    if let Some(cursor) = &request.cursor {
        if cursor.snapshot_digest != snapshot_digest {
            return Err("search: cursor invalidated because the matched files changed".into());
        }
        if offset > files.len() {
            return Err("search: cursor offset is beyond the current result set".into());
        }
    }
    let end = offset.saturating_add(request.max_results).min(files.len());
    let page = &files[offset..end];
    let next_cursor = (end < files.len()).then(|| {
        SearchCursor {
            query_digest: query_digest.clone(),
            snapshot_digest: snapshot_digest.clone(),
            offset: end,
        }
        .encode()
    });
    let rows = page
        .iter()
        .map(|file| {
            json!({
                "path": file.path,
                "modified_at": file.modified_secs,
            })
        })
        .collect::<Vec<_>>();
    Ok(json!({
        "query": Value::Null,
        "path": request.requested_path,
        "mode": "glob",
        "matches": rows,
        "total_matches": files.len(),
        "files_searched": files.len(),
        "files_skipped": 0,
        "bytes_searched": 0,
        "next_cursor": next_cursor,
        "truncated": next_cursor.is_some()
    }))
}

fn reject_explicit_symlink(project: &Project, requested: &str) -> Result<(), String> {
    let requested = Path::new(requested);
    let display = if requested.is_absolute() {
        requested.to_path_buf()
    } else {
        project.root().join(requested)
    };
    match std::fs::symlink_metadata(&display) {
        Ok(metadata) if metadata.file_type().is_symlink() => Err(format!(
            "search: explicit search root must not be a symlink: {}",
            display.display()
        )),
        Ok(_) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(format!(
            "search: could not inspect explicit root `{}`: {error}",
            display.display()
        )),
    }
}

impl SearchMode {
    fn as_str(self) -> &'static str {
        match self {
            Self::Literal => "literal",
            Self::Regex => "regex",
        }
    }
}

impl SearchRequest {
    fn parse(arguments: &Value) -> Result<Self, String> {
        let object = arguments
            .as_object()
            .ok_or_else(|| "search: arguments must be an object".to_owned())?;
        const ALLOWED: [&str; 11] = [
            "query",
            "path",
            "mode",
            "case_sensitive",
            "include_globs",
            "exclude_globs",
            "extensions",
            "include_hidden",
            "respect_gitignore",
            "max_results",
            "cursor",
        ];
        if let Some(unknown) = object.keys().find(|key| !ALLOWED.contains(&key.as_str())) {
            return Err(format!("search: unknown argument `{unknown}`"));
        }
        let query = match string_optional(arguments, "query")? {
            Some("") => {
                return Err("search: `query` cannot be empty".into());
            }
            Some(query) if query.len() > MAX_QUERY_BYTES => {
                return Err(format!("search: query exceeds {MAX_QUERY_BYTES} bytes"));
            }
            other => other,
        };
        let include_globs = string_array(arguments, "include_globs")?;
        if query.is_none() && include_globs.is_empty() {
            return Err(
                "search: `query` is required unless `include_globs` lists patterns (glob mode)"
                    .into(),
            );
        }
        let mode = match string_optional(arguments, "mode")?.unwrap_or("literal") {
            "literal" => SearchMode::Literal,
            "regex" => SearchMode::Regex,
            other => return Err(format!("search: unknown mode `{other}`")),
        };
        let max_results = arguments
            .get("max_results")
            .map_or(Ok(DEFAULT_RESULTS), |value| {
                value
                    .as_u64()
                    .and_then(|value| usize::try_from(value).ok())
                    .filter(|value| (1..=MAX_RESULTS_PER_PAGE).contains(value))
                    .ok_or_else(|| {
                        format!("search: `max_results` must be 1..={MAX_RESULTS_PER_PAGE}")
                    })
            })?;
        let exclude_globs = string_array(arguments, "exclude_globs")?;
        let extensions = string_array(arguments, "extensions")?
            .into_iter()
            .map(|extension| normalize_extension(&extension))
            .collect::<Result<BTreeSet<_>, _>>()?;
        let cursor = string_optional(arguments, "cursor")?
            .map(SearchCursor::parse)
            .transpose()?;
        Ok(Self {
            query: query.map(str::to_owned),
            requested_path: string_optional(arguments, "path")?
                .unwrap_or(".")
                .to_owned(),
            mode,
            case_sensitive: bool_optional(arguments, "case_sensitive")?.unwrap_or(false),
            include_globs,
            exclude_globs,
            extensions,
            include_hidden: bool_optional(arguments, "include_hidden")?.unwrap_or(false),
            respect_gitignore: bool_optional(arguments, "respect_gitignore")?.unwrap_or(true),
            max_results,
            cursor,
        })
    }

    fn query_digest(&self, root: &Path) -> String {
        let value = json!({
            "v": 1,
            "root": root.to_string_lossy(),
            "query": self.query,
            "mode": self.mode.as_str(),
            "case_sensitive": self.case_sensitive,
            "include_globs": self.include_globs,
            "exclude_globs": self.exclude_globs,
            "extensions": self.extensions,
            "include_hidden": self.include_hidden,
            "respect_gitignore": self.respect_gitignore
        });
        format!(
            "{:x}",
            Sha256::digest(serde_json::to_vec(&value).expect("search query JSON"))
        )
    }
}

impl SearchCursor {
    fn parse(raw: &str) -> Result<Self, String> {
        if raw.len() > 256 {
            return Err("search: cursor is too long".into());
        }
        let mut parts = raw.split('.');
        let version = parts.next();
        let query_digest = parts.next();
        let snapshot_digest = parts.next();
        let offset = parts.next();
        if version != Some("v1") || parts.next().is_some() {
            return Err("search: malformed cursor".into());
        }
        let (Some(query_digest), Some(snapshot_digest), Some(offset)) =
            (query_digest, snapshot_digest, offset)
        else {
            return Err("search: malformed cursor".into());
        };
        if !is_digest(query_digest) || !is_digest(snapshot_digest) {
            return Err("search: malformed cursor digest".into());
        }
        let offset = offset
            .parse::<usize>()
            .map_err(|_| "search: malformed cursor offset".to_owned())?;
        Ok(Self {
            query_digest: query_digest.into(),
            snapshot_digest: snapshot_digest.into(),
            offset,
        })
    }

    fn encode(&self) -> String {
        format!(
            "v1.{}.{}.{}",
            self.query_digest, self.snapshot_digest, self.offset
        )
    }
}

fn collect_candidates(
    root: &Path,
    project_root: &Path,
    request: &SearchRequest,
    include: Option<&GlobSet>,
    exclude: Option<&GlobSet>,
    cancel: &CancelToken,
) -> Result<Vec<Candidate>, String> {
    let mut paths = Vec::new();
    if root.is_file() {
        paths.push(root.to_path_buf());
    } else if root.is_dir() {
        let mut builder = WalkBuilder::new(root);
        builder
            .hidden(!request.include_hidden)
            .ignore(request.respect_gitignore)
            .git_ignore(request.respect_gitignore)
            .git_exclude(request.respect_gitignore)
            .parents(request.respect_gitignore)
            .require_git(false)
            .follow_links(false)
            .max_depth(Some(MAX_WALK_DEPTH));
        let walker = builder
            .filter_entry(|entry| {
                entry.depth() == 0
                    || !entry.file_type().is_some_and(|kind| kind.is_dir())
                    || !is_hard_ignored_dir(&entry.file_name().to_string_lossy())
            })
            .build();
        for entry in walker {
            check_cancelled(cancel)?;
            let entry = entry.map_err(|error| format!("search: walk failed: {error}"))?;
            if !entry
                .file_type()
                .is_some_and(|file_type| file_type.is_file())
            {
                continue;
            }
            if paths.len() >= MAX_FILES {
                return Err(format!(
                    "search: candidate files exceed {MAX_FILES}; narrow path/globs/extensions"
                ));
            }
            paths.push(entry.into_path());
        }
    } else {
        return Err(format!(
            "search: `{}` is not a file or directory",
            root.display()
        ));
    }

    let search_base = if root.is_dir() {
        root
    } else {
        root.parent().unwrap_or(root)
    };
    let mut candidates = Vec::new();
    for path in paths {
        let match_path = path
            .strip_prefix(search_base)
            .unwrap_or(&path)
            .to_string_lossy()
            .replace('\\', "/");
        if include.is_some_and(|set| !set.is_match(&match_path))
            || exclude.is_some_and(|set| set.is_match(&match_path))
            || !extension_matches(&path, &request.extensions)
        {
            continue;
        }
        let display_path = path
            .strip_prefix(project_root)
            .map(|relative| relative.to_string_lossy().replace('\\', "/"))
            .unwrap_or_else(|_| path.to_string_lossy().replace('\\', "/"));
        candidates.push(Candidate {
            path,
            match_path,
            display_path,
        });
    }
    candidates.sort_by(|left, right| left.match_path.cmp(&right.match_path));
    Ok(candidates)
}

fn read_candidate(
    candidate: &Candidate,
    project: &Project,
    project_root: &Path,
) -> Result<Option<Vec<u8>>, String> {
    let before = std::fs::symlink_metadata(&candidate.path)
        .map_err(|error| format!("search: stat `{}` failed: {error}", candidate.display_path))?;
    if before.file_type().is_symlink() || !before.is_file() {
        return Ok(None);
    }
    let bytes = match candidate.path.strip_prefix(project_root) {
        Ok(relative) => project
            .read_file_limited(relative, MAX_FILE_BYTES + 1)
            .map_err(|error| format!("search: read `{}` failed: {error}", candidate.display_path))?
            .ok_or_else(|| format!("search: `{}` disappeared", candidate.display_path))?,
        Err(_) => read_absolute_no_follow(&candidate.path).map_err(|error| {
            format!("search: read `{}` failed: {error}", candidate.display_path)
        })?,
    };
    let after = std::fs::symlink_metadata(&candidate.path).map_err(|error| {
        format!(
            "search: restat `{}` failed: {error}",
            candidate.display_path
        )
    })?;
    if metadata_stamp(&before) != metadata_stamp(&after) {
        return Err(format!(
            "search: `{}` changed during scan; retry",
            candidate.display_path
        ));
    }
    Ok(Some(bytes))
}

fn read_absolute_no_follow(path: &Path) -> std::io::Result<Vec<u8>> {
    let file_name = path.file_name().ok_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::InvalidInput, "path has no file name")
    })?;
    let parent = path.parent().unwrap_or_else(|| Path::new("/"));
    let parent = parent.canonicalize()?;
    let dir = cap_std::fs::Dir::open_ambient_dir(parent, cap_std::ambient_authority())?;
    let mut options = cap_std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use cap_std::fs::OpenOptionsExt as _;
        options.custom_flags(libc::O_NOFOLLOW);
    }
    #[cfg(windows)]
    {
        use cap_std::fs::OpenOptionsExt as _;
        options.custom_flags(0x0020_0000);
    }
    let file = dir.open_with(file_name, &options)?;
    let metadata = file.metadata()?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "search target must be a regular non-symlink file",
        ));
    }
    let mut bytes = Vec::new();
    file.take((MAX_FILE_BYTES + 1) as u64)
        .read_to_end(&mut bytes)?;
    Ok(bytes)
}

fn metadata_stamp(metadata: &std::fs::Metadata) -> (u64, u128) {
    let modified = metadata
        .modified()
        .ok()
        .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
        .map_or(0, |duration| duration.as_nanos());
    (metadata.len(), modified)
}

fn build_matcher(request: &SearchRequest, query: &str) -> Result<Regex, String> {
    let pattern = match request.mode {
        SearchMode::Literal => regex::escape(query),
        SearchMode::Regex => query.to_owned(),
    };
    RegexBuilder::new(&pattern)
        .case_insensitive(!request.case_sensitive)
        .size_limit(1024 * 1024)
        .dfa_size_limit(4 * 1024 * 1024)
        .build()
        .map_err(|error| format!("search: invalid {} pattern: {error}", request.mode.as_str()))
}

fn build_globset(patterns: &[String], field: &str) -> Result<Option<GlobSet>, String> {
    if patterns.is_empty() {
        return Ok(None);
    }
    let mut builder = GlobSetBuilder::new();
    for pattern in patterns {
        let glob = GlobBuilder::new(pattern)
            .literal_separator(true)
            .backslash_escape(true)
            .build()
            .map_err(|error| format!("search: invalid {field} pattern `{pattern}`: {error}"))?;
        builder.add(glob);
    }
    builder
        .build()
        .map(Some)
        .map_err(|error| format!("search: could not compile {field}: {error}"))
}

fn extension_matches(path: &Path, extensions: &BTreeSet<String>) -> bool {
    extensions.is_empty()
        || path
            .extension()
            .and_then(|extension| extension.to_str())
            .is_some_and(|extension| extensions.contains(&extension.to_ascii_lowercase()))
}

fn normalize_extension(raw: &str) -> Result<String, String> {
    let extension = raw.trim_start_matches('.').to_ascii_lowercase();
    if extension.is_empty()
        || extension.len() > 32
        || !extension
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
    {
        return Err(format!("search: invalid extension `{raw}`"));
    }
    Ok(extension)
}

fn string_array(arguments: &Value, name: &str) -> Result<Vec<String>, String> {
    let Some(value) = arguments.get(name) else {
        return Ok(Vec::new());
    };
    let array = value
        .as_array()
        .ok_or_else(|| format!("search: `{name}` must be an array of strings"))?;
    if array.len() > MAX_PATTERNS {
        return Err(format!(
            "search: `{name}` is limited to {MAX_PATTERNS} entries"
        ));
    }
    array
        .iter()
        .map(|value| {
            let text = value
                .as_str()
                .ok_or_else(|| format!("search: `{name}` must contain only strings"))?;
            if text.is_empty() || text.len() > MAX_PATTERN_BYTES {
                return Err(format!(
                    "search: `{name}` entries must be 1..={MAX_PATTERN_BYTES} bytes"
                ));
            }
            Ok(text.to_owned())
        })
        .collect()
}

fn string_optional<'a>(arguments: &'a Value, name: &str) -> Result<Option<&'a str>, String> {
    match arguments.get(name) {
        None => Ok(None),
        Some(value) => value
            .as_str()
            .map(Some)
            .ok_or_else(|| format!("search: `{name}` must be a string")),
    }
}

fn bool_optional(arguments: &Value, name: &str) -> Result<Option<bool>, String> {
    match arguments.get(name) {
        None => Ok(None),
        Some(value) => value
            .as_bool()
            .map(Some)
            .ok_or_else(|| format!("search: `{name}` must be a boolean")),
    }
}

fn is_digest(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn is_hard_ignored_dir(name: &str) -> bool {
    matches!(
        name,
        ".git"
            | ".hg"
            | ".svn"
            | ".cache"
            | ".next"
            | ".turbo"
            | "target"
            | "node_modules"
            | "dist"
            | "build"
            | "coverage"
            | "__pycache__"
    )
}

fn truncate_utf8(text: &str, max_bytes: usize) -> (String, bool) {
    if text.len() <= max_bytes {
        return (text.to_owned(), false);
    }
    let mut end = max_bytes;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    (text[..end].to_owned(), true)
}

fn check_cancelled(cancel: &CancelToken) -> Result<(), String> {
    if cancel.is_cancelled() {
        Err("search: cancelled".into())
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(tag: &str) -> (PathBuf, Project) {
        let root = std::env::temp_dir().join(format!(
            "clat-search-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).expect("root");
        (root.clone(), Project::new(root))
    }

    #[test]
    fn regex_glob_extension_and_gitignore_are_composed() {
        let (root, project) = fixture("filters");
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join(".gitignore"), "ignored.rs\n").unwrap();
        std::fs::write(root.join("src/a.rs"), "fn needle_alpha() {}\n").unwrap();
        std::fs::write(root.join("src/b.rs"), "fn needle_beta() {}\n").unwrap();
        std::fs::write(root.join("ignored.rs"), "fn needle_ignored() {}\n").unwrap();
        std::fs::write(root.join("README.md"), "fn needle_readme() {}\n").unwrap();
        let output = execute(
            &json!({
                "query": "fn needle_(alpha|beta)",
                "mode": "regex",
                "include_globs": ["src/**/*.rs"],
                "extensions": ["rs"]
            }),
            &project,
            &CancelToken::new(),
        )
        .expect("search");
        let text = serde_json::to_string(&output).unwrap();
        assert!(text.contains("src/a.rs"));
        assert!(text.contains("src/b.rs"));
        assert!(!text.contains("ignored.rs"));
        assert!(!text.contains("README.md"));
        crate::test_support::cleanup_tree(&root);
    }

    fn set_mtime(path: &Path, secs: u64) {
        let file = std::fs::OpenOptions::new()
            .write(true)
            .open(path)
            .unwrap_or_else(|error| panic!("set_mtime {}: {error}", path.display()));
        file.set_modified(std::time::UNIX_EPOCH + std::time::Duration::from_secs(secs))
            .unwrap();
    }

    #[test]
    fn glob_mode_lists_matching_files_newest_first_with_ignore_discipline() {
        let (root, project) = fixture("glob");
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join(".gitignore"), "ignored.md\n").unwrap();
        std::fs::write(root.join("old.md"), "x").unwrap();
        std::fs::write(root.join("src/new.md"), "x").unwrap();
        std::fs::write(root.join("ignored.md"), "x").unwrap();
        std::fs::write(root.join(".hidden.md"), "x").unwrap();
        std::fs::write(root.join("other.txt"), "x").unwrap();
        set_mtime(&root.join("old.md"), 1_000);
        set_mtime(&root.join("src/new.md"), 2_000);
        set_mtime(&root.join("ignored.md"), 3_000);
        let output = execute(
            &json!({"include_globs": ["**/*.md"]}),
            &project,
            &CancelToken::new(),
        )
        .expect("glob search");
        assert_eq!(output["mode"], "glob");
        assert_eq!(output["total_matches"], 2);
        let paths: Vec<&str> = output["matches"]
            .as_array()
            .unwrap()
            .iter()
            .map(|row| row["path"].as_str().unwrap())
            .collect();
        // 修改时间倒序（Claude Code Glob 同款）；gitignore/隐藏文件不进结果。
        assert_eq!(paths, ["src/new.md", "old.md"]);
        assert_eq!(output["matches"][0]["modified_at"], 2_000);
        crate::test_support::cleanup_tree(&root);
    }

    #[test]
    fn glob_mode_requires_include_globs_when_query_is_omitted() {
        let (root, project) = fixture("glob-args");
        let error = execute(&json!({}), &project, &CancelToken::new())
            .expect_err("glob mode needs patterns");
        assert!(error.contains("include_globs"), "{error}");
        let error = execute(
            &json!({"include_globs": [], "query": ""}),
            &project,
            &CancelToken::new(),
        )
        .expect_err("empty query still rejected");
        assert!(error.contains("cannot be empty"), "{error}");
        crate::test_support::cleanup_tree(&root);
    }

    #[test]
    fn glob_mode_pages_and_invalidates_cursors_on_modification() {
        let (root, project) = fixture("glob-page");
        std::fs::write(root.join("a.md"), "x").unwrap();
        std::fs::write(root.join("b.md"), "x").unwrap();
        set_mtime(&root.join("a.md"), 1_000);
        set_mtime(&root.join("b.md"), 2_000);
        let first = execute(
            &json!({"include_globs": ["*.md"], "max_results": 1}),
            &project,
            &CancelToken::new(),
        )
        .expect("page one");
        assert_eq!(first["matches"][0]["path"], "b.md");
        assert!(first["next_cursor"].is_string(), "paging must continue");
        let cursor = first["next_cursor"].as_str().unwrap().to_owned();
        let second = execute(
            &json!({"include_globs": ["*.md"], "max_results": 1, "cursor": cursor}),
            &project,
            &CancelToken::new(),
        )
        .expect("page two");
        assert_eq!(second["matches"][0]["path"], "a.md");
        assert!(second["next_cursor"].is_null());
        // 快照绑定 mtime：文件一改，旧游标即失效。
        set_mtime(&root.join("a.md"), 4_000);
        let error = execute(
            &json!({"include_globs": ["*.md"], "max_results": 1, "cursor": cursor}),
            &project,
            &CancelToken::new(),
        )
        .expect_err("stale cursor must be rejected");
        assert!(error.contains("invalidated"), "{error}");
        crate::test_support::cleanup_tree(&root);
    }

    #[test]
    fn cursor_pages_stably_and_invalidates_on_content_change() {
        let (root, project) = fixture("cursor");
        std::fs::write(root.join("a.txt"), "needle a\nneedle b\nneedle c\n").unwrap();
        let first = execute(
            &json!({"query": "needle", "max_results": 2}),
            &project,
            &CancelToken::new(),
        )
        .expect("first");
        assert_eq!(first["matches"].as_array().unwrap().len(), 2);
        let cursor = first["next_cursor"].as_str().expect("cursor");
        let second = execute(
            &json!({"query": "needle", "max_results": 2, "cursor": cursor}),
            &project,
            &CancelToken::new(),
        )
        .expect("second");
        assert_eq!(second["matches"].as_array().unwrap().len(), 1);
        assert!(second["next_cursor"].is_null());

        std::fs::write(root.join("a.txt"), "needle changed\n").unwrap();
        let error = execute(
            &json!({"query": "needle", "max_results": 2, "cursor": cursor}),
            &project,
            &CancelToken::new(),
        )
        .expect_err("invalidated");
        assert!(error.contains("searchable files changed"), "{error}");
        crate::test_support::cleanup_tree(&root);
    }

    #[test]
    fn literal_default_invalid_patterns_and_line_truncation_are_bounded() {
        let (root, project) = fixture("bounds");
        std::fs::write(
            root.join("a.txt"),
            format!("prefix NEEDLE {}\n", "x".repeat(MAX_MATCH_LINE_BYTES + 100)),
        )
        .unwrap();
        let output = execute(&json!({"query": "needle"}), &project, &CancelToken::new())
            .expect("literal case-insensitive");
        assert_eq!(output["matches"][0]["text_truncated"], true);
        assert!(output["matches"][0]["text"].as_str().unwrap().len() <= MAX_MATCH_LINE_BYTES);
        assert!(
            execute(
                &json!({"query": "(", "mode": "regex"}),
                &project,
                &CancelToken::new()
            )
            .unwrap_err()
            .contains("invalid regex")
        );
        assert!(
            execute(
                &json!({"query": "x", "include_globs": ["["]}),
                &project,
                &CancelToken::new()
            )
            .unwrap_err()
            .contains("invalid include_globs")
        );
        assert!(
            execute(
                &json!({"query": "x", "unknown": true}),
                &project,
                &CancelToken::new()
            )
            .unwrap_err()
            .contains("unknown argument")
        );
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(root.join("a.txt"), root.join("link.txt")).expect("symlink");
            assert!(
                execute(
                    &json!({"query": "needle", "path": "link.txt"}),
                    &project,
                    &CancelToken::new()
                )
                .unwrap_err()
                .contains("must not be a symlink")
            );
        }
        crate::test_support::cleanup_tree(&root);
    }
}
