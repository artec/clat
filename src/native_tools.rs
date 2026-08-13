use crate::project::Project;
use crate::tool::{Tool, ToolDefinition, ToolEffect, ToolError, ToolRegistry};
use serde_json::{Value, json};
use std::fs;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};

const DEFAULT_LIST_DEPTH: usize = 2;
const DEFAULT_LIST_ENTRIES: usize = 200;
const MAX_LIST_DEPTH: usize = 8;
const MAX_LIST_ENTRIES: usize = 2_000;
const DEFAULT_READ_BYTES: usize = 64 * 1024;
const MAX_READ_BYTES: usize = 1024 * 1024;
const DEFAULT_SEARCH_RESULTS: usize = 50;
const MAX_SEARCH_RESULTS: usize = 500;
const DEFAULT_SEARCH_FILES: usize = 20_000;
const MAX_SEARCH_FILE_BYTES: u64 = 1024 * 1024;

pub fn register_native_read_tools(registry: &mut ToolRegistry) {
    registry.register(ListFilesTool);
    registry.register(ReadFileTool);
    registry.register(SearchTool);
}

#[derive(Clone, Copy, Debug, Default)]
pub struct ListFilesTool;

impl Tool for ListFilesTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "list_files".into(),
            description: "List files and directories inside the current project. Paths are project-relative. Use this to understand repository structure before reading individual files.".into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Project-relative directory path. Defaults to '.'"
                    },
                    "max_depth": {
                        "type": "integer",
                        "minimum": 0,
                        "maximum": MAX_LIST_DEPTH,
                        "description": "How many directory levels to descend. Defaults to 2"
                    },
                    "max_entries": {
                        "type": "integer",
                        "minimum": 1,
                        "maximum": MAX_LIST_ENTRIES,
                        "description": "Maximum returned entries. Defaults to 200"
                    }
                },
                "additionalProperties": false
            }),
            effect: ToolEffect::Read,
            strict: false,
        }
    }

    fn invoke(&self, arguments: &Value, project: &Project) -> Result<Value, ToolError> {
        let requested = string_arg(arguments, "path").unwrap_or(".");
        let max_depth = usize_arg(arguments, "max_depth")
            .unwrap_or(DEFAULT_LIST_DEPTH)
            .min(MAX_LIST_DEPTH);
        let max_entries = usize_arg(arguments, "max_entries")
            .unwrap_or(DEFAULT_LIST_ENTRIES)
            .clamp(1, MAX_LIST_ENTRIES);
        let root = project
            .resolve_existing(requested)
            .map_err(|error| tool_io_error("list_files", requested, error))?;

        if !root.is_dir() {
            return Err(ToolError::new(format!(
                "list_files: `{requested}` is not a directory"
            )));
        }

        let mut entries = Vec::new();
        let mut truncated = false;
        walk_directory(
            project,
            &root,
            0,
            max_depth,
            max_entries,
            &mut entries,
            &mut truncated,
        )?;

        Ok(json!({
            "path": requested,
            "entries": entries,
            "truncated": truncated
        }))
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct ReadFileTool;

impl Tool for ReadFileTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "read_file".into(),
            description: "Read UTF-8 text from a project file with line numbers. Paths are project-relative and cannot escape the project root.".into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Project-relative file path"
                    },
                    "start_line": {
                        "type": "integer",
                        "minimum": 1,
                        "description": "First line to return. Defaults to 1"
                    },
                    "end_line": {
                        "type": "integer",
                        "minimum": 1,
                        "description": "Last line to return, inclusive"
                    },
                    "max_bytes": {
                        "type": "integer",
                        "minimum": 1,
                        "maximum": MAX_READ_BYTES,
                        "description": "Maximum UTF-8 bytes returned. Defaults to 65536"
                    }
                },
                "required": ["path"],
                "additionalProperties": false
            }),
            effect: ToolEffect::Read,
            strict: false,
        }
    }

    fn invoke(&self, arguments: &Value, project: &Project) -> Result<Value, ToolError> {
        let requested = required_string_arg(arguments, "path", "read_file")?;
        let start_line = usize_arg(arguments, "start_line").unwrap_or(1).max(1);
        let end_line = usize_arg(arguments, "end_line");
        if end_line.is_some_and(|end| end < start_line) {
            return Err(ToolError::new(
                "read_file: `end_line` must be greater than or equal to `start_line`",
            ));
        }
        let max_bytes = usize_arg(arguments, "max_bytes")
            .unwrap_or(DEFAULT_READ_BYTES)
            .clamp(1, MAX_READ_BYTES);
        let path = project
            .resolve_existing(requested)
            .map_err(|error| tool_io_error("read_file", requested, error))?;

        if !path.is_file() {
            return Err(ToolError::new(format!(
                "read_file: `{requested}` is not a file"
            )));
        }

        let file =
            fs::File::open(&path).map_err(|error| tool_io_error("read_file", requested, error))?;
        let mut content = String::new();
        let mut last_line = start_line.saturating_sub(1);
        let mut truncated = false;

        for (index, line) in BufReader::new(file).lines().enumerate() {
            let line_number = index + 1;
            if line_number < start_line {
                continue;
            }
            if end_line.is_some_and(|end| line_number > end) {
                break;
            }

            let line = line.map_err(|error| tool_io_error("read_file", requested, error))?;
            let formatted = format!("{line_number} | {line}\n");
            if content.len() + formatted.len() > max_bytes {
                truncated = true;
                break;
            }
            content.push_str(&formatted);
            last_line = line_number;
        }

        Ok(json!({
            "path": requested,
            "start_line": start_line,
            "end_line": last_line,
            "content": content,
            "truncated": truncated
        }))
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct SearchTool;

impl Tool for SearchTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "search".into(),
            description: "Search UTF-8 project files for a literal text query and return matching paths, line numbers, and lines. Common generated and dependency directories are skipped.".into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "query": {
                        "type": "string",
                        "description": "Literal text to search for"
                    },
                    "path": {
                        "type": "string",
                        "description": "Project-relative file or directory to search. Defaults to '.'"
                    },
                    "case_sensitive": {
                        "type": "boolean",
                        "description": "Whether matching is case-sensitive. Defaults to false"
                    },
                    "max_results": {
                        "type": "integer",
                        "minimum": 1,
                        "maximum": MAX_SEARCH_RESULTS,
                        "description": "Maximum matches returned. Defaults to 50"
                    }
                },
                "required": ["query"],
                "additionalProperties": false
            }),
            effect: ToolEffect::Read,
            strict: false,
        }
    }

    fn invoke(&self, arguments: &Value, project: &Project) -> Result<Value, ToolError> {
        let query = required_string_arg(arguments, "query", "search")?;
        if query.is_empty() {
            return Err(ToolError::new("search: `query` cannot be empty"));
        }
        let requested = string_arg(arguments, "path").unwrap_or(".");
        let case_sensitive = arguments
            .get("case_sensitive")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let max_results = usize_arg(arguments, "max_results")
            .unwrap_or(DEFAULT_SEARCH_RESULTS)
            .clamp(1, MAX_SEARCH_RESULTS);
        let root = project
            .resolve_existing(requested)
            .map_err(|error| tool_io_error("search", requested, error))?;

        let mut files = Vec::new();
        collect_search_files(&root, &mut files, DEFAULT_SEARCH_FILES)?;
        let mut matches = Vec::new();
        let mut files_searched = 0usize;
        let mut truncated = false;
        let normalized_query = (!case_sensitive).then(|| query.to_lowercase());

        for path in files {
            if matches.len() >= max_results {
                truncated = true;
                break;
            }
            if !is_searchable_file(&path)? {
                continue;
            }

            let bytes = fs::read(&path).map_err(|error| {
                ToolError::new(format!(
                    "search: failed to read `{}`: {error}",
                    path.display()
                ))
            })?;
            if bytes.contains(&0) {
                continue;
            }
            let Ok(text) = std::str::from_utf8(&bytes) else {
                continue;
            };
            files_searched += 1;

            for (index, line) in text.lines().enumerate() {
                let matched = if case_sensitive {
                    line.contains(query)
                } else {
                    line.to_lowercase()
                        .contains(normalized_query.as_deref().unwrap_or_default())
                };
                if !matched {
                    continue;
                }

                let relative = display_project_path(project, &path)?;
                matches.push(json!({
                    "path": relative,
                    "line": index + 1,
                    "text": line
                }));
                if matches.len() >= max_results {
                    truncated = true;
                    break;
                }
            }
        }

        Ok(json!({
            "query": query,
            "path": requested,
            "matches": matches,
            "files_searched": files_searched,
            "truncated": truncated
        }))
    }
}

fn walk_directory(
    project: &Project,
    directory: &Path,
    depth: usize,
    max_depth: usize,
    max_entries: usize,
    output: &mut Vec<Value>,
    truncated: &mut bool,
) -> Result<(), ToolError> {
    if depth > max_depth || output.len() >= max_entries {
        *truncated = output.len() >= max_entries;
        return Ok(());
    }

    let mut entries = sorted_entries(directory)?;
    for entry in entries.drain(..) {
        if output.len() >= max_entries {
            *truncated = true;
            return Ok(());
        }

        let file_type = entry.file_type().map_err(|error| {
            ToolError::new(format!("list_files: failed to inspect entry: {error}"))
        })?;
        let name = entry.file_name();
        if file_type.is_dir() && is_ignored_directory(&name.to_string_lossy()) {
            continue;
        }

        let path = entry.path();
        let kind = if file_type.is_dir() {
            "directory"
        } else if file_type.is_file() {
            "file"
        } else if file_type.is_symlink() {
            "symlink"
        } else {
            "other"
        };
        output.push(json!({
            "path": display_project_path(project, &path)?,
            "kind": kind
        }));

        if file_type.is_dir() && depth < max_depth {
            walk_directory(
                project,
                &path,
                depth + 1,
                max_depth,
                max_entries,
                output,
                truncated,
            )?;
        }
    }

    Ok(())
}

fn collect_search_files(
    path: &Path,
    output: &mut Vec<PathBuf>,
    max_files: usize,
) -> Result<(), ToolError> {
    if output.len() >= max_files {
        return Ok(());
    }
    if path.is_file() {
        output.push(path.to_path_buf());
        return Ok(());
    }
    if !path.is_dir() {
        return Ok(());
    }

    for entry in sorted_entries(path)? {
        if output.len() >= max_files {
            break;
        }
        let file_type = entry
            .file_type()
            .map_err(|error| ToolError::new(format!("search: failed to inspect entry: {error}")))?;
        let name = entry.file_name();
        if file_type.is_dir() {
            if is_ignored_directory(&name.to_string_lossy()) {
                continue;
            }
            collect_search_files(&entry.path(), output, max_files)?;
        } else if file_type.is_file() {
            output.push(entry.path());
        }
    }
    Ok(())
}

fn sorted_entries(path: &Path) -> Result<Vec<fs::DirEntry>, ToolError> {
    let mut entries = fs::read_dir(path)
        .map_err(|error| ToolError::new(format!("failed to read `{}`: {error}", path.display())))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| ToolError::new(format!("failed to read directory entry: {error}")))?;
    entries.sort_by_key(|entry| entry.file_name());
    Ok(entries)
}

fn is_ignored_directory(name: &str) -> bool {
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
    )
}

fn is_searchable_file(path: &Path) -> Result<bool, ToolError> {
    let metadata = fs::metadata(path).map_err(|error| {
        ToolError::new(format!(
            "search: failed to stat `{}`: {error}",
            path.display()
        ))
    })?;
    Ok(metadata.is_file() && metadata.len() <= MAX_SEARCH_FILE_BYTES)
}

fn display_project_path(project: &Project, path: &Path) -> Result<String, ToolError> {
    let root = project
        .resolve_existing(".")
        .map_err(|error| ToolError::new(format!("cannot resolve project root: {error}")))?;
    let relative = path
        .strip_prefix(&root)
        .map_err(|_| ToolError::new("path escaped project root"))?;
    if relative.as_os_str().is_empty() {
        Ok(".".into())
    } else {
        Ok(relative.to_string_lossy().replace('\\', "/"))
    }
}

fn string_arg<'a>(arguments: &'a Value, name: &str) -> Option<&'a str> {
    arguments.get(name).and_then(Value::as_str)
}

fn required_string_arg<'a>(
    arguments: &'a Value,
    name: &str,
    tool: &str,
) -> Result<&'a str, ToolError> {
    string_arg(arguments, name)
        .ok_or_else(|| ToolError::new(format!("{tool}: `{name}` must be a string")))
}

fn usize_arg(arguments: &Value, name: &str) -> Option<usize> {
    arguments
        .get(name)
        .and_then(Value::as_u64)
        .and_then(|value| usize::try_from(value).ok())
}

fn tool_io_error(tool: &str, path: &str, error: std::io::Error) -> ToolError {
    ToolError::new(format!("{tool}: cannot access `{path}`: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::env;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn fixture() -> (PathBuf, Project) {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time")
            .as_nanos();
        let root = env::temp_dir().join(format!("clat-native-tools-test-{unique}"));
        fs::create_dir_all(root.join("src")).expect("src");
        fs::create_dir_all(root.join("node_modules/pkg")).expect("ignored");
        fs::write(root.join("README.md"), "# Demo\nhello world\n").expect("readme");
        fs::write(
            root.join("src/main.rs"),
            "fn main() {\n    println!(\"hello\");\n}\n",
        )
        .expect("main");
        fs::write(root.join("node_modules/pkg/index.js"), "hello ignored").expect("ignored file");
        let project = Project::new(&root);
        (root, project)
    }

    #[test]
    fn lists_project_files_and_skips_dependency_directories() {
        let (root, project) = fixture();
        let output = ListFilesTool
            .invoke(&json!({"path": ".", "max_depth": 3}), &project)
            .expect("list");
        let serialized = output.to_string();

        assert!(serialized.contains("README.md"));
        assert!(serialized.contains("src/main.rs"));
        assert!(!serialized.contains("node_modules"));

        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn reads_line_ranges_with_line_numbers() {
        let (root, project) = fixture();
        let output = ReadFileTool
            .invoke(
                &json!({"path": "src/main.rs", "start_line": 2, "end_line": 2}),
                &project,
            )
            .expect("read");

        assert_eq!(output["start_line"], 2);
        assert_eq!(output["end_line"], 2);
        assert_eq!(output["content"], "2 |     println!(\"hello\");\n");

        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn searches_text_files_and_skips_dependency_directories() {
        let (root, project) = fixture();
        let output = SearchTool
            .invoke(&json!({"query": "hello"}), &project)
            .expect("search");
        let matches = output["matches"].as_array().expect("matches");

        assert_eq!(matches.len(), 2);
        assert!(matches.iter().any(|item| item["path"] == "README.md"));
        assert!(matches.iter().any(|item| item["path"] == "src/main.rs"));

        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn read_file_rejects_parent_traversal() {
        let (root, project) = fixture();
        let error = ReadFileTool
            .invoke(&json!({"path": "../secret.txt"}), &project)
            .expect_err("must reject");

        assert!(error.to_string().contains("parent traversal"));
        fs::remove_dir_all(root).expect("cleanup");
    }
}
