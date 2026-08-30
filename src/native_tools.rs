//! Built-in native tools: project-scoped `list_files`/`read_file` plus
//! trusted-project `write_file`/`edit_file`/`ask_user`. Process tools live in
//! `plugins/process.rs` over the shared ProcessService.

use crate::CancelToken;
use crate::project::Project;
use crate::tool::{Tool, ToolDefinition, ToolEffect, ToolError};
use serde_json::{Value, json};
use std::fs;
use std::io::BufReader;
use std::path::Path;

const DEFAULT_LIST_DEPTH: usize = 2;
const DEFAULT_LIST_ENTRIES: usize = 200;
const MAX_LIST_DEPTH: usize = 8;
const MAX_LIST_ENTRIES: usize = 2_000;
const DEFAULT_READ_BYTES: usize = 64 * 1024;
const MAX_READ_BYTES: usize = 1024 * 1024;
pub(crate) fn native_read_tools() -> Vec<std::sync::Arc<dyn Tool>> {
    vec![
        std::sync::Arc::new(ListFilesTool),
        std::sync::Arc::new(ReadFileTool),
    ]
}

#[derive(Clone, Copy, Debug, Default)]
pub struct ListFilesTool;

impl Tool for ListFilesTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "list_files".into(),
            description: "List files and directories. Paths are project-relative; absolute directory paths (anywhere on disk) are also accepted. Use this to understand directory structure before reading individual files.".into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Project-relative or absolute directory path. Defaults to '.'"
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

    fn invoke(
        &self,
        arguments: &Value,
        project: &Project,
        cancel: &CancelToken,
    ) -> Result<Value, ToolError> {
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
            WalkLimits {
                depth: max_depth,
                entries: max_entries,
            },
            &mut entries,
            &mut truncated,
            cancel,
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
            description: "Read UTF-8 text from a file with line numbers. Paths are project-relative; absolute paths (anywhere on disk) are also accepted — reads are never confined to the project root.".into(),
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

    fn invoke(
        &self,
        arguments: &Value,
        project: &Project,
        cancel: &CancelToken,
    ) -> Result<Value, ToolError> {
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

        // FP-06（2026-08-22 审计）：有界行读取——超长单行不再先整行
        // 物化（1GiB 无换行文件在 max_bytes=1KiB 时旧实现会先分配
        // 1GiB）。单行超过 max_bytes 即触发截断（该行本身就不可能被
        // 保留）；输出格式与 BufReader::lines 版逐字节一致。
        let mut reader = BufReader::new(file);
        let mut index = 0usize;
        loop {
            check_cancelled(cancel)?;
            let line = match crate::mcp::transport::read_capped_line(&mut reader, max_bytes) {
                Ok(Some(line)) => line,
                Ok(None) => break,
                Err(_) => {
                    truncated = true;
                    break;
                }
            };
            let line_number = index + 1;
            index += 1;
            if line_number < start_line {
                continue;
            }
            if end_line.is_some_and(|end| line_number > end) {
                break;
            }

            let line = line.trim_end_matches(['\r', '\n']);
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

pub(crate) fn native_write_tools(
    scope: crate::permission::WriteScopeSource,
) -> Vec<std::sync::Arc<dyn Tool>> {
    vec![
        std::sync::Arc::new(WriteFileTool {
            scope: scope.clone(),
        }),
        std::sync::Arc::new(EditFileTool { scope }),
    ]
}

const MAX_WRITE_BYTES: usize = 1024 * 1024;

/// 写工具携带写入围栏来源（SR2）：每次 invoke 解析当前档位——FA 开放
/// 绝对路径，RO/PW（与 exec）保持项目根相对路径。
#[derive(Clone, Default)]
pub struct WriteFileTool {
    scope: crate::permission::WriteScopeSource,
}

impl Tool for WriteFileTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "write_file".into(),
            description: "Create or overwrite a file with UTF-8 text. Paths are project-relative (parent directories are created as needed); absolute paths outside the project are only writable under Full Access mode. Prefer edit_file for small changes to existing files.".into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Project-relative file path (absolute only under Full Access mode)"
                    },
                    "content": {
                        "type": "string",
                        "description": "Full file content to write (max 1 MiB)"
                    }
                },
                "required": ["path", "content"],
                "additionalProperties": false
            }),
            effect: ToolEffect::Write,
            strict: false,
        }
    }

    fn invoke(
        &self,
        arguments: &Value,
        project: &Project,
        cancel: &CancelToken,
    ) -> Result<Value, ToolError> {
        check_cancelled(cancel)?;
        let requested = required_string_arg(arguments, "path", "write_file")?;
        let content = required_string_arg(arguments, "content", "write_file")?;
        if content.len() > MAX_WRITE_BYTES {
            // FIX-5/CA-06：不再指向 edit_file——它的结果与目标同受
            // MAX_WRITE_BYTES 约束，指路只会把模型引向另一条失败路径。
            return Err(ToolError::new(format!(
                "write_file: content exceeds {MAX_WRITE_BYTES} bytes — split the write into smaller pieces"
            )));
        }
        // W-INV1：父目录创建、临时文件与 rename 全部绑定到同一个
        // 目标根 capability（项目根，或 FA 绝对路径的目标父目录），
        // 路径竞态不能把后续 I/O 引到目标根之外。
        let target = project
            .writable_target(requested, true, self.scope.resolve())
            .map_err(|error| tool_io_error("write_file", requested, error))?;
        let existed = target
            .atomic_write(content, None)
            .map_err(|error| tool_io_error("write_file", requested, error))?;
        Ok(json!({
            "path": requested,
            "bytes": content.len(),
            "created": !existed
        }))
    }
}

/// 写工具携带写入围栏来源（SR2），同 [`WriteFileTool::default()`]。
#[derive(Clone, Default)]
pub struct EditFileTool {
    scope: crate::permission::WriteScopeSource,
}

impl Tool for EditFileTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "edit_file".into(),
            description: "Replace an exact, unique text snippet in a file. old_str must match the file byte-for-byte and appear exactly once; include surrounding lines to disambiguate. Paths are project-relative; absolute paths outside the project are only editable under Full Access mode.".into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Project-relative file path (must exist; absolute only under Full Access mode)"
                    },
                    "old_str": {
                        "type": "string",
                        "description": "Exact text to replace; must be unique in the file"
                    },
                    "new_str": {
                        "type": "string",
                        "description": "Replacement text"
                    }
                },
                "required": ["path", "old_str", "new_str"],
                "additionalProperties": false
            }),
            effect: ToolEffect::Write,
            strict: false,
        }
    }

    fn invoke(
        &self,
        arguments: &Value,
        project: &Project,
        cancel: &CancelToken,
    ) -> Result<Value, ToolError> {
        check_cancelled(cancel)?;
        let requested = required_string_arg(arguments, "path", "edit_file")?;
        let old_str = required_string_arg(arguments, "old_str", "edit_file")?;
        let new_str = required_string_arg(arguments, "new_str", "edit_file")?;
        if old_str.is_empty() {
            return Err(ToolError::new("edit_file: `old_str` must not be empty"));
        }
        if old_str == new_str {
            return Err(ToolError::new(
                "edit_file: `old_str` and `new_str` are identical",
            ));
        }
        // W-INV3：先验证后行动——文件读取与匹配检查全部通过之前，
        // 不触碰目标文件。
        let target = project
            .writable_target(requested, false, self.scope.resolve())
            .map_err(|error| tool_io_error("edit_file", requested, error))?;
        if !target
            .is_file()
            .map_err(|error| tool_io_error("edit_file", requested, error))?
        {
            return Err(ToolError::new(format!(
                "edit_file: `{requested}` is not a file"
            )));
        }
        // RA-04：边界必须落在实际 read 上。stat 只能快拒，不能防住
        // stat 后文件增长；生产读取最多取 MAX_WRITE_BYTES+1，超帽即停。
        let content = target
            .read_to_string_limited(MAX_WRITE_BYTES)
            .map_err(|error| tool_io_error("edit_file", requested, error))?;
        let occurrences = content.match_indices(old_str).count();
        match occurrences {
            0 => {
                return Err(ToolError::new(format!(
                    "edit_file: `old_str` not found in `{requested}` — read the file and copy the exact text"
                )));
            }
            1 => {}
            count => {
                return Err(ToolError::new(format!(
                    "edit_file: `old_str` matches {count} times in `{requested}` — include surrounding lines to make it unique"
                )));
            }
        }
        let updated = content.replacen(old_str, new_str, 1);
        if updated.len() > MAX_WRITE_BYTES {
            return Err(ToolError::new(format!(
                "edit_file: result would exceed {MAX_WRITE_BYTES} bytes"
            )));
        }
        // NWE-06：父目录锁覆盖“快照复查→rename”整个提交区间；
        // 另一个 CLAT 写入者必须串行并基于新快照重试。
        target
            .atomic_write(&updated, Some(&content))
            .map_err(|error| tool_io_error("edit_file", requested, error))?;
        Ok(json!({
            "path": requested,
            "replacements": 1,
            "bytes": updated.len()
        }))
    }
}

#[derive(Clone, Copy)]
struct WalkLimits {
    depth: usize,
    entries: usize,
}

/// Migration characterization only: the historical native-tool tests drive
/// the new ProcessService through this compatibility adapter. Production
/// `run_command` is registered by `builtin.exec_tools`; no second spawn path
/// exists outside tests.
#[cfg(test)]
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct RunCommandTool;

#[cfg(all(test, unix))]
const MAX_COMMAND_OUTPUT_BYTES: usize = 32 * 1024;

#[cfg(test)]
impl Tool for RunCommandTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "run_command".into(),
            description: "test compatibility adapter".into(),
            input_schema: json!({"type": "object"}),
            effect: ToolEffect::Execute,
            strict: true,
        }
    }

    fn invoke(
        &self,
        arguments: &Value,
        project: &Project,
        cancel: &CancelToken,
    ) -> Result<Value, ToolError> {
        let command = required_string_arg(arguments, "command", "run_command")?;
        let timeout = usize_arg(arguments, "timeout_seconds").unwrap_or(120) as u64;
        let sandbox = std::sync::Arc::new(
            crate::sandbox::SandboxService::new(
                project.root().to_path_buf(),
                crate::sandbox::SandboxModeSource::Classic,
            )
            .map_err(ToolError::new)?,
        );
        let service = crate::process::ProcessService::new(project.clone(), sandbox);
        let generation = service
            .bind_run("native-tool-characterization", cancel.clone())
            .map_err(ToolError::new)?;
        let output = service
            .run_compat(
                command,
                std::time::Duration::from_secs(timeout.clamp(1, 600)),
                false,
                crate::sandbox::SandboxRequest::Auto,
            )
            .map_err(ToolError::new)?;
        let cleanup = service.unbind_run(generation);
        let _ = service.close();
        cleanup.map_err(ToolError::new)?;
        if output.cancelled {
            return Err(ToolError::new("run_command: cancelled"));
        }
        Ok(json!({
            "command": command,
            "cwd": project.resolve_existing(".").map_err(|error| ToolError::new(error.to_string()))?.to_string_lossy(),
            "exit_code": output.exit_code,
            "signal": output.signal.as_deref().and_then(|signal| signal.parse::<i64>().ok()),
            "timed_out": output.timed_out,
            "stdout": output.stdout,
            "stderr": output.stderr,
            "stdout_truncated": output.output_truncated || output.stdout_lossy,
            "stderr_truncated": output.output_truncated || output.stderr_lossy,
        }))
    }
}

fn walk_directory(
    project: &Project,
    directory: &Path,
    depth: usize,
    limits: WalkLimits,
    output: &mut Vec<Value>,
    truncated: &mut bool,
    cancel: &CancelToken,
) -> Result<(), ToolError> {
    check_cancelled(cancel)?;
    if depth > limits.depth || output.len() >= limits.entries {
        *truncated = output.len() >= limits.entries;
        return Ok(());
    }

    let (entries, dir_overflow) = sorted_entries(directory)?;
    if dir_overflow {
        *truncated = true;
    }
    for entry in entries {
        if output.len() >= limits.entries {
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

        if file_type.is_dir() && depth < limits.depth {
            walk_directory(project, &path, depth + 1, limits, output, truncated, cancel)?;
        }
    }

    Ok(())
}

fn check_cancelled(cancel: &CancelToken) -> Result<(), ToolError> {
    if cancel.is_cancelled() {
        Err(ToolError::new("tool invocation cancelled"))
    } else {
        Ok(())
    }
}

/// FP-06：单目录读取帽——巨型目录不再无界 collect + sort 全量 entry
///（百万 entry 会先吃满内存再谈 max_entries）。超帽即截断（仅保留
/// 前 `MAX_DIR_ENTRIES` 项并排序），溢出信号由调用方并入 `truncated`。
/// 帽值远大于 list 的 max_entries 上限（2000），正常目录零影响。
const MAX_DIR_ENTRIES: usize = 10_000;

fn sorted_entries(path: &Path) -> Result<(Vec<fs::DirEntry>, bool), ToolError> {
    let directory = fs::read_dir(path)
        .map_err(|error| ToolError::new(format!("failed to read `{}`: {error}", path.display())))?;
    let mut entries = Vec::new();
    let mut overflow = false;
    for entry in directory {
        if entries.len() >= MAX_DIR_ENTRIES {
            overflow = true;
            break;
        }
        entries.push(
            entry.map_err(|error| {
                ToolError::new(format!("failed to read directory entry: {error}"))
            })?,
        );
    }
    entries.sort_by_key(|entry| entry.file_name());
    Ok((entries, overflow))
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

/// 展示路径：项目内用相对路径；项目外（SR1 绝对读口）用绝对路径——
/// 读不再被围在项目根内，展示也不再把整个工具调用报错。
fn display_project_path(project: &Project, path: &Path) -> Result<String, ToolError> {
    let root = project
        .resolve_existing(".")
        .map_err(|error| ToolError::new(format!("cannot resolve project root: {error}")))?;
    match path.strip_prefix(&root) {
        Ok(relative) if relative.as_os_str().is_empty() => Ok(".".into()),
        Ok(relative) => Ok(relative.to_string_lossy().replace('\\', "/")),
        Err(_) => Ok(path.to_string_lossy().replace('\\', "/")),
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

/// ask-user 工具族：一次安装、按 run 换前端实现的交互工具。
pub(crate) fn native_interaction_tools(
    slot: std::sync::Arc<crate::interaction::AskUserSlot>,
) -> Vec<std::sync::Arc<dyn Tool>> {
    vec![std::sync::Arc::new(AskUserTool { slot })]
}

/// 模型向用户提问的端口工具（DSH `ask_user_question` 的 CLAT 单问版）。
/// 阻塞等待前端应答；无前端（headless）时结构化报错，run 不中断。
/// 效果为 Pure：提问本身就是人机交互，不叠加权限审批。
pub struct AskUserTool {
    slot: std::sync::Arc<crate::interaction::AskUserSlot>,
}

impl Tool for AskUserTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "ask_user".into(),
            description: "Ask the human user a single question and wait for the answer. Use it only for user-owned choices (preferences, approvals, direction) or genuine ambiguity you cannot resolve from the repository; never for facts you can look up yourself. Provide 2-4 short options when the choice is enumerable; free-text answers are always available.".into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "question": {
                        "type": "string",
                        "description": "The question to show the user, self-contained"
                    },
                    "options": {
                        "type": "array",
                        "description": "Selectable answers; omit for open-ended questions",
                        "items": {
                            "type": "object",
                            "properties": {
                                "label": { "type": "string", "description": "Short answer returned to the model" },
                                "description": { "type": "string", "description": "Optional context shown to the human" }
                            },
                            "required": ["label"]
                        }
                    },
                    "allow_custom": {
                        "type": "boolean",
                        "description": "Whether the user may type a custom answer (default true)"
                    }
                },
                "required": ["question"]
            }),
            effect: ToolEffect::Pure,
            strict: false,
        }
    }

    fn invoke(
        &self,
        arguments: &Value,
        _project: &Project,
        cancel: &CancelToken,
    ) -> Result<Value, ToolError> {
        let question = arguments
            .get("question")
            .and_then(Value::as_str)
            .filter(|text| !text.trim().is_empty())
            .ok_or_else(|| ToolError::new("ask_user: `question` must be a non-empty string"))?
            .to_owned();
        let mut options = Vec::new();
        if let Some(list) = arguments.get("options").and_then(Value::as_array) {
            for option in list {
                let label = option
                    .get("label")
                    .and_then(Value::as_str)
                    .filter(|text| !text.trim().is_empty())
                    .ok_or_else(|| {
                        ToolError::new("ask_user: every option needs a non-empty `label`")
                    })?
                    .to_owned();
                options.push(crate::interaction::AskOption {
                    label,
                    description: option
                        .get("description")
                        .and_then(Value::as_str)
                        .map(str::to_owned),
                });
            }
        }
        let allow_custom = arguments
            .get("allow_custom")
            .and_then(Value::as_bool)
            .unwrap_or(true);

        let Some(asker) = self.slot.asker() else {
            return Err(ToolError::new(
                "ask_user: no interactive frontend is attached (headless run); proceed without asking or state what you would have asked",
            ));
        };
        match asker.ask(
            crate::interaction::AskQuestion {
                question,
                options,
                allow_custom,
            },
            cancel,
        ) {
            crate::interaction::AskAnswer::Selected(label)
            | crate::interaction::AskAnswer::Custom(label) => Ok(json!({ "answer": label })),
            crate::interaction::AskAnswer::Declined => Err(ToolError::new(
                "ask_user: the user declined to answer; proceed with your best judgment or stop",
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugins::SearchTool;
    use std::env;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};

    /// fixture 目录名唯一化：纳秒时间戳 + 单调递增计数器——并行
    /// 测试在同一纳秒各建一个 fixture 时纯时间戳会撞名，一方 cleanup
    /// 删掉另一方的根目录。
    static FIXTURE_SEQUENCE: AtomicU64 = AtomicU64::new(0);

    fn fixture() -> (PathBuf, Project) {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time")
            .as_nanos();
        let sequence = FIXTURE_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let root = env::temp_dir().join(format!("clat-native-tools-test-{unique}-{sequence}"));
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
            .invoke(
                &json!({"path": ".", "max_depth": 3}),
                &project,
                &CancelToken::new(),
            )
            .expect("list");
        let serialized = output.to_string();

        assert!(serialized.contains("README.md"));
        assert!(serialized.contains("src/main.rs"));
        assert!(!serialized.contains("node_modules"));

        crate::test_support::cleanup_tree(&root);
    }

    #[test]
    fn reads_line_ranges_with_line_numbers() {
        let (root, project) = fixture();
        let output = ReadFileTool
            .invoke(
                &json!({"path": "src/main.rs", "start_line": 2, "end_line": 2}),
                &project,
                &CancelToken::new(),
            )
            .expect("read");

        assert_eq!(output["start_line"], 2);
        assert_eq!(output["end_line"], 2);
        assert_eq!(output["content"], "2 |     println!(\"hello\");\n");

        crate::test_support::cleanup_tree(&root);
    }

    #[test]
    fn searches_text_files_and_skips_dependency_directories() {
        let (root, project) = fixture();
        let output = SearchTool
            .invoke(&json!({"query": "hello"}), &project, &CancelToken::new())
            .expect("search");
        let matches = output["matches"].as_array().expect("matches");

        assert_eq!(matches.len(), 2);
        assert!(matches.iter().any(|item| item["path"] == "README.md"));
        assert!(matches.iter().any(|item| item["path"] == "src/main.rs"));

        crate::test_support::cleanup_tree(&root);
    }

    #[test]
    fn read_file_rejects_parent_traversal() {
        let (root, project) = fixture();
        let error = ReadFileTool
            .invoke(
                &json!({"path": "../secret.txt"}),
                &project,
                &CancelToken::new(),
            )
            .expect_err("must reject");

        assert!(error.to_string().contains("parent traversal"));
        crate::test_support::cleanup_tree(&root);
    }

    /// W-INV2/W-INV1：写文件新建、覆盖、父目录物化，均在项目内；
    /// SR1（读自由，对齐 DSH「every mode permits reading」）：三读工具
    /// 接受任意绝对路径（全档位一致，含项目外与 symlink 解析）；项目
    /// 相对路径的旧纪律不变（`..` 仍拒）。pre-fix 上绝对路径在
    /// `resolve_existing` 被拒，绝对读断言必红。
    /// FP-06：超长单行（8MiB 无换行）在 max_bytes=1KiB 下——截断、
    /// 输出有界、不物化整行。判别力说明：输出与旧实现等价（截断标志
    /// 两版都为 true），边界在**内存**而非输出——有界形状由 mcp 的
    /// read_capped_line 测试族钉住，此处钉行为契约（truncated + 有界
    /// content + 正常文件零变化——后者由 reads_line_ranges 钉住）。
    #[test]
    fn read_file_bounded_against_a_giant_single_line() {
        let (root, project) = fixture();
        let giant = root.join("giant-single-line.txt");
        let mut line = String::with_capacity(8 * 1024 * 1024);
        std::iter::repeat_n(b'x', 8 * 1024 * 1024).for_each(|byte| line.push(byte as char));
        fs::write(&giant, &line).expect("giant");
        let output = ReadFileTool
            .invoke(
                &json!({"path": "giant-single-line.txt", "max_bytes": 1024}),
                &project,
                &CancelToken::new(),
            )
            .expect("bounded read");
        assert_eq!(output["truncated"], true);
        let content = output["content"].as_str().expect("content");
        assert!(
            content.len() <= 1024,
            "content stays within the budget: {}",
            content.len()
        );
        crate::test_support::cleanup_tree(&root);
    }

    /// FP-06：目录帽——sorted_entries 对超帽目录只收 MAX_DIR_ENTRIES
    /// 项并报告溢出（判别面在助手返回的 overflow 位：旧实现无界
    /// collect，输出层面等价，边界在内存——结构性判别）。
    #[test]
    fn directory_entry_collection_is_capped() {
        let root = fixture().0;
        let dir = root.join("huge-dir");
        fs::create_dir_all(&dir).expect("dir");
        for index in 0..(MAX_DIR_ENTRIES + 100) {
            fs::write(dir.join(format!("f{index:05}")), b"").expect("file");
        }
        let (entries, overflow) = sorted_entries(&dir).expect("sorted");
        assert!(overflow, "directories past the cap report overflow");
        assert_eq!(
            entries.len(),
            MAX_DIR_ENTRIES,
            "only cap-many entries are ever collected"
        );
        // 正常目录零影响：无溢出、全量排序。
        let (small, overflow) = sorted_entries(&root).expect("root");
        assert!(!overflow);
        assert!(small.len() < MAX_DIR_ENTRIES);
        crate::test_support::cleanup_tree(&root);
    }

    #[test]
    fn read_tools_accept_absolute_paths_beyond_the_project_root() {
        let (root, project) = fixture();
        let outside_dir = root.parent().expect("parent").join("clat-sr1-outside");
        let _ = fs::remove_dir_all(&outside_dir);
        fs::create_dir_all(&outside_dir).expect("outside dir");
        let outside_file = outside_dir.join("notes.txt");
        fs::write(&outside_file, "outside content\nsecond line\n").expect("file");
        let absolute = outside_file.to_str().expect("utf8 path").to_owned();

        // read_file：绝对路径 + 行号内容。
        let output = ReadFileTool
            .invoke(
                &json!({"path": absolute, "max_bytes": 65536}),
                &project,
                &CancelToken::new(),
            )
            .expect("absolute read");
        assert!(
            output["content"]
                .as_str()
                .unwrap()
                .contains("outside content")
        );

        // list_files：绝对目录。
        let output = ListFilesTool
            .invoke(
                &json!({"path": outside_dir.to_str().expect("utf8 path")}),
                &project,
                &CancelToken::new(),
            )
            .expect("absolute list");
        assert!(
            output["entries"]
                .as_array()
                .expect("entries")
                .iter()
                .any(|entry| entry["path"].as_str().unwrap().contains("notes.txt"))
        );

        // search：绝对目录内检索；项目外匹配以绝对路径展示。
        let output = SearchTool
            .invoke(
                &json!({"query": "outside", "path": outside_dir.to_str().expect("utf8 path")}),
                &project,
                &CancelToken::new(),
            )
            .expect("absolute search");
        let matches = output["matches"].as_array().expect("matches");
        assert_eq!(matches.len(), 1);
        assert!(
            Path::new(matches[0]["path"].as_str().unwrap()).is_absolute(),
            "outside matches display as absolute paths"
        );

        // 相对路径纪律不变：`..` 穿越/根外仍拒。
        ReadFileTool
            .invoke(
                &json!({"path": "../escape.txt"}),
                &project,
                &CancelToken::new(),
            )
            .expect_err("relative traversal stays rejected");

        fs::remove_dir_all(&outside_dir).ok();
        crate::test_support::cleanup_tree(&root);
    }

    /// SR2/SR3（写分档）：默认围栏（PW，也是 exec 的固定档）拒绝绝对
    /// 路径且错误点名档位；FA（共享 cell）开放绝对写并保持原子纪律
    ///（无临时残留）；同一 cell 热降档后下一次写重新被围。pre-fix 上
    /// 绝对路径一律被拒（无 FA 分支），FA 断言必红。
    #[test]
    fn write_scope_gates_absolute_paths_by_mode() {
        use crate::permission::PermissionMode;
        let (root, project) = fixture();
        let outside_dir = root.parent().expect("parent").join("clat-sr2-outside");
        let _ = fs::remove_dir_all(&outside_dir);
        fs::create_dir_all(&outside_dir).expect("outside dir");
        let target = outside_dir.join("out.md");
        let absolute = target.to_str().expect("utf8 path").to_owned();

        // 默认围栏：绝对路径拒绝，错误指向 Full Access。
        let error = WriteFileTool::default()
            .invoke(
                &json!({"path": absolute, "content": "no\n"}),
                &project,
                &CancelToken::new(),
            )
            .expect_err("the default fence rejects absolute paths");
        assert!(
            error.to_string().contains("Full Access"),
            "the error names the mode: {error}"
        );
        assert!(!target.exists());

        // FA（共享 cell）：绝对写成功 + 原子纪律（无 .clat-tmp 残留）。
        let cell = std::sync::Arc::new(std::sync::RwLock::new(PermissionMode::FullAccess));
        let writer = WriteFileTool {
            scope: crate::permission::WriteScopeSource::Shared(std::sync::Arc::clone(&cell)),
        };
        writer
            .invoke(
                &json!({"path": absolute, "content": "fa\n"}),
                &project,
                &CancelToken::new(),
            )
            .expect("full access writes outside the project");
        assert_eq!(fs::read_to_string(&target).unwrap(), "fa\n");
        let leftovers: Vec<_> = fs::read_dir(&outside_dir)
            .unwrap()
            .filter_map(Result::ok)
            .filter(|entry| entry.file_name().to_string_lossy().starts_with(".clat-tmp"))
            .collect();
        assert!(
            leftovers.is_empty(),
            "atomic discipline holds outside the project"
        );

        // 同一 cell 热降档 PW：下一次写重新被围，内容不动。
        *cell.write().expect("mode lock") = PermissionMode::ProjectWrite;
        writer
            .invoke(
                &json!({"path": absolute, "content": "again\n"}),
                &project,
                &CancelToken::new(),
            )
            .expect_err("downgrade re-fences absolute paths");
        assert_eq!(
            fs::read_to_string(&target).unwrap(),
            "fa\n",
            "content untouched by the rejected write"
        );

        // edit_file 走同一围栏：FA 下编辑绝对路径成功。
        *cell.write().expect("mode lock") = PermissionMode::FullAccess;
        let editor = EditFileTool {
            scope: crate::permission::WriteScopeSource::Shared(cell),
        };
        editor
            .invoke(
                &json!({"path": absolute, "old_str": "fa", "new_str": "full access"}),
                &project,
                &CancelToken::new(),
            )
            .expect("full access edits outside the project");
        assert_eq!(fs::read_to_string(&target).unwrap(), "full access\n");

        fs::remove_dir_all(&outside_dir).ok();
        crate::test_support::cleanup_tree(&root);
    }

    /// 失败路径（穿越）不产生任何文件。
    #[test]
    fn write_file_creates_overwrites_and_never_escapes() {
        let (root, project) = fixture();

        // 新建（父目录不存在 → 物化）。
        let output = WriteFileTool::default()
            .invoke(
                &json!({"path": "docs/new/note.md", "content": "first\n"}),
                &project,
                &CancelToken::new(),
            )
            .expect("create");
        assert_eq!(output["created"], true);
        assert_eq!(
            fs::read_to_string(root.join("docs/new/note.md")).unwrap(),
            "first\n"
        );

        // 覆盖。
        let output = WriteFileTool::default()
            .invoke(
                &json!({"path": "docs/new/note.md", "content": "second\n"}),
                &project,
                &CancelToken::new(),
            )
            .expect("overwrite");
        assert_eq!(output["created"], false);
        assert_eq!(
            fs::read_to_string(root.join("docs/new/note.md")).unwrap(),
            "second\n"
        );

        // 穿越：拒绝，且根外无新文件、无临时文件残留。
        let outside = root.parent().expect("parent").join("clat-write-escape.txt");
        let _ = fs::remove_file(&outside);
        WriteFileTool::default()
            .invoke(
                &json!({"path": "../clat-write-escape.txt", "content": "pwn"}),
                &project,
                &CancelToken::new(),
            )
            .expect_err("must reject traversal");
        assert!(
            !outside.exists(),
            "nothing may be created outside the project"
        );
        let leftovers: Vec<_> = fs::read_dir(root.join("docs/new"))
            .unwrap()
            .filter_map(Result::ok)
            .filter(|entry| entry.file_name().to_string_lossy().starts_with(".clat-tmp"))
            .collect();
        assert!(leftovers.is_empty(), "no temp files may survive a failure");

        // 超限：拒绝，目标文件保持旧内容。
        let oversized = "x".repeat(MAX_WRITE_BYTES + 1);
        let error = WriteFileTool::default()
            .invoke(
                &json!({"path": "docs/new/note.md", "content": oversized}),
                &project,
                &CancelToken::new(),
            )
            .expect_err("must reject oversized");
        assert!(error.to_string().contains("exceeds"));
        assert_eq!(
            fs::read_to_string(root.join("docs/new/note.md")).unwrap(),
            "second\n"
        );

        crate::test_support::cleanup_tree(&root);
    }

    /// RA-04：超帽目标由实际读取边界拒绝，错误必须诚实且目标不变。
    /// cap+1 的分配上界由 project::bounded_read_helpers_stop_at_cap_plus_one
    /// 直接钉住，不能再依赖可竞态的 metadata.len()。
    #[test]
    fn edit_file_rejects_an_oversized_target_before_reading_it() {
        let (root, project) = fixture();
        let path = root.join("big.txt");
        fs::write(&path, "x".repeat(2 * 1024 * 1024)).expect("big file");
        let error = EditFileTool::default()
            .invoke(
                &json!({"path": "big.txt", "old_str": "x", "new_str": "y"}),
                &project,
                &CancelToken::new(),
            )
            .expect_err("an over-cap target must be rejected without reading it");
        let message = error.to_string();
        assert!(
            message.contains("exceeds") && message.contains("file cap"),
            "the error must state the file cap honestly: {message}"
        );
        assert_eq!(
            fs::read(&path).unwrap().len(),
            2 * 1024 * 1024,
            "the target bytes are untouched"
        );
        crate::test_support::cleanup_tree(&root);
    }

    /// FIX-5/CA-06（pre-fix 红）：write_file 超限文案不得把模型指向
    /// 另一个同样不支持大结果的工具（edit_file 的结果与目标同受
    /// MAX_WRITE_BYTES 约束）。
    #[test]
    fn write_file_over_limit_advice_does_not_point_to_edit_file() {
        let (root, project) = fixture();
        let error = WriteFileTool::default()
            .invoke(
                &json!({
                    "path": "big.txt",
                    "content": "x".repeat(MAX_WRITE_BYTES + 1)
                }),
                &project,
                &CancelToken::new(),
            )
            .expect_err("over limit");
        let message = error.to_string();
        assert!(
            !message.contains("edit_file"),
            "advice must not point at a tool with the same cap: {message}"
        );
        assert!(
            message.contains("split"),
            "advice should say how to proceed: {message}"
        );
        crate::test_support::cleanup_tree(&root);
    }

    /// W-INV3：edit_file 拒绝歧义与不匹配，且失败时文件字节不变。
    #[test]
    fn edit_file_requires_a_unique_exact_match() {
        let (root, project) = fixture();
        fs::write(root.join("code.txt"), "alpha\nbeta\nalpha\n").expect("file");
        let path = json!("code.txt");

        // 多处匹配：拒绝 + 文件不变。
        let error = EditFileTool::default()
            .invoke(
                &json!({"path": path, "old_str": "alpha", "new_str": "gamma"}),
                &project,
                &CancelToken::new(),
            )
            .expect_err("must reject ambiguous");
        assert!(error.to_string().contains("matches 2 times"));
        assert_eq!(
            fs::read_to_string(root.join("code.txt")).unwrap(),
            "alpha\nbeta\nalpha\n"
        );

        // 无匹配：拒绝 + 文件不变。
        EditFileTool::default()
            .invoke(
                &json!({"path": path, "old_str": "delta", "new_str": "gamma"}),
                &project,
                &CancelToken::new(),
            )
            .expect_err("must reject missing");
        assert_eq!(
            fs::read_to_string(root.join("code.txt")).unwrap(),
            "alpha\nbeta\nalpha\n"
        );

        // old == new：拒绝。
        EditFileTool::default()
            .invoke(
                &json!({"path": path, "old_str": "beta", "new_str": "beta"}),
                &project,
                &CancelToken::new(),
            )
            .expect_err("must reject no-op");

        // 唯一匹配：应用。
        EditFileTool::default()
            .invoke(
                &json!({"path": path, "old_str": "beta", "new_str": "beta2"}),
                &project,
                &CancelToken::new(),
            )
            .expect("apply");
        assert_eq!(
            fs::read_to_string(root.join("code.txt")).unwrap(),
            "alpha\nbeta2\nalpha\n"
        );

        crate::test_support::cleanup_tree(&root);
    }

    /// C-INV1：命令在 canonical 项目根执行。
    #[test]
    #[cfg(unix)]
    fn run_command_executes_in_the_project_root() {
        let (root, project) = fixture();
        let output = RunCommandTool
            .invoke(&json!({"command": "pwd"}), &project, &CancelToken::new())
            .expect("run");
        let canonical = root.canonicalize().unwrap();
        assert_eq!(
            output["cwd"].as_str().map(str::to_owned),
            Some(canonical.to_string_lossy().into_owned())
        );
        assert_eq!(
            output["stdout"].as_str().unwrap().trim(),
            canonical.to_string_lossy()
        );
        assert_eq!(output["exit_code"], 0);
        crate::test_support::cleanup_tree(&root);
    }

    /// C-INV2/NWE-02：超时终止整个进程组——后台/后代进程在工具
    /// 返回后不得继续产生副作用（审计动态复现：旧实现只杀 shell，
    /// 后台进程在返回后写出 marker）。
    #[test]
    #[cfg(unix)]
    fn run_command_timeout_kills_the_whole_process_tree() {
        let (root, project) = fixture();
        let marker = root.join("orphan-marker");
        let output = RunCommandTool
            .invoke(
                &json!({
                    "command": "(sleep 2; printf survived > orphan-marker) >/dev/null 2>&1 & wait",
                    "timeout_seconds": 1
                }),
                &project,
                &CancelToken::new(),
            )
            .expect("run");
        assert_eq!(output["timed_out"], true);
        // 后台进程本应在工具返回 ~1s 后写 marker；等过它应有的
        // 存活期，断言进程组终止覆盖了它。
        std::thread::sleep(std::time::Duration::from_millis(2500));
        assert!(
            !marker.exists(),
            "descendants must not outlive the tool call (NWE-02)"
        );
        crate::test_support::cleanup_tree(&root);
    }

    /// NWE-02 回归：leader shell 收到 TERM 后可能先退出，而后代明确
    /// 忽略 TERM。清理不能把 leader 退出误判为整组结束，宽限后仍须
    /// 向原进程组发送 KILL。旧实现会让 marker 在工具返回后出现。
    #[test]
    #[cfg(unix)]
    fn run_command_kills_term_ignoring_descendants_after_leader_exits() {
        let (root, project) = fixture();
        let marker = root.join("ignored-term-marker");
        let output = RunCommandTool
            .invoke(
                &json!({
                    "command": "(trap '' TERM; sleep 3; printf survived > ignored-term-marker) >/dev/null 2>&1 & wait",
                    "timeout_seconds": 1
                }),
                &project,
                &CancelToken::new(),
            )
            .expect("run");
        assert_eq!(output["timed_out"], true);
        std::thread::sleep(std::time::Duration::from_millis(1500));
        assert!(
            !marker.exists(),
            "TERM-ignoring descendants must receive the final group KILL"
        );
        crate::test_support::cleanup_tree(&root);
    }

    /// FP-03（2026-08-22 审计，前置红）：leader shell 正常先退出、后台
    /// 后代继承持有 stdout/stderr 管道——旧实现在 leader 退出即停全部
    /// 监控、无界 join reader，`sleep 30 &` 这类日常命令即可把工具挂
    /// 死整整 30s。修复后：leader 退出 + 1s drain 宽限耗尽 → 整组终止
    /// → 有限返回，且 timed_out=false（leader 已按期完成）。
    /// 看门狗（线程 + recv_timeout）让红阶段干净失败而非挂死测试器。
    #[test]
    #[cfg(unix)]
    fn run_command_bounded_when_background_descendants_hold_the_pipes() {
        let (root, project) = fixture();
        let (tx, rx) = std::sync::mpsc::channel();
        let handle = {
            let project = project.clone();
            std::thread::spawn(move || {
                let result = RunCommandTool.invoke(
                    &json!({"command": "sleep 30 & exit 0", "timeout_seconds": 30}),
                    &project,
                    &CancelToken::new(),
                );
                let _ = tx.send(result);
            })
        };
        let output = rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .expect("the tool must return within the watchdog window")
            .expect("leader exited 0; only lingering pipes were reclaimed");
        assert_eq!(output["exit_code"], 0, "the leader completed on time");
        assert_eq!(
            output["timed_out"], false,
            "leader completion is not a timeout"
        );
        handle.join().expect("worker");
        // 后台 sleep 已被整组终止：等过它的存活期，不得有 marker。
        let marker = root.join("lingering-marker");
        let _ = RunCommandTool.invoke(
            &json!({
                "command": "(sleep 3; printf x > lingering-marker) & exit 0",
                "timeout_seconds": 30
            }),
            &project,
            &CancelToken::new(),
        );
        std::thread::sleep(std::time::Duration::from_millis(1500));
        assert!(
            !marker.exists(),
            "pipe-holding descendants must be group-terminated after the drain grace"
        );
        crate::test_support::cleanup_tree(&root);
    }

    /// FP-03（前置红）：同一状态（leader 已死、后代持管道）下，Esc 取
    /// 消必须仍然有效——旧实现的取消监控随 leader 退出一起停了。
    #[test]
    #[cfg(unix)]
    fn run_command_cancel_still_works_after_leader_exits() {
        let (root, project) = fixture();
        let cancel = CancelToken::new();
        let (tx, rx) = std::sync::mpsc::channel();
        let handle = {
            let project = project.clone();
            let cancel = cancel.clone();
            std::thread::spawn(move || {
                let result = RunCommandTool.invoke(
                    &json!({"command": "sleep 30 & exit 0", "timeout_seconds": 30}),
                    &project,
                    &cancel,
                );
                let _ = tx.send(result);
            })
        };
        std::thread::sleep(std::time::Duration::from_millis(200));
        cancel.cancel();
        let error = rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .expect("cancel must be honoured even after the leader exited")
            .expect_err("cancelled runs return an error");
        assert!(error.to_string().contains("cancelled"), "error: {error}");
        handle.join().expect("worker");
        // 取消同样整组终止后代。
        let marker = root.join("cancel-marker");
        let cancel2 = CancelToken::new();
        let cancel2_for_worker = cancel2.clone();
        let (tx2, rx2) = std::sync::mpsc::channel();
        let project2 = project.clone();
        std::thread::spawn(move || {
            let result = RunCommandTool.invoke(
                &json!({
                    "command": "(sleep 3; printf x > cancel-marker) & exit 0",
                    "timeout_seconds": 30
                }),
                &project2,
                &cancel2_for_worker,
            );
            let _ = tx2.send(result);
        });
        std::thread::sleep(std::time::Duration::from_millis(200));
        cancel2.cancel();
        rx2.recv_timeout(std::time::Duration::from_secs(5))
            .expect("second leg bounded")
            .expect_err("cancelled");
        std::thread::sleep(std::time::Duration::from_millis(1500));
        assert!(!marker.exists(), "cancelled groups must not leave writers");
        crate::test_support::cleanup_tree(&root);
    }

    /// C-INV2：超时必然终止（kill），且整体耗时贴近超时值而非命令
    /// 长度；终止契约（NWE-07）：exit_code 为 null、signal 有值。
    #[test]
    #[cfg(unix)]
    fn run_command_times_out_and_kills_the_child() {
        let (root, project) = fixture();
        let started = std::time::Instant::now();
        let output = RunCommandTool
            .invoke(
                &json!({"command": "sleep 30", "timeout_seconds": 1}),
                &project,
                &CancelToken::new(),
            )
            .expect("run returns a timeout result, not an error");
        assert_eq!(output["timed_out"], true);
        assert!(
            output["exit_code"].is_null(),
            "signal-terminated child has no exit code"
        );
        assert!(
            output["signal"].as_i64().is_some(),
            "signal-terminated child reports its signal"
        );
        assert!(started.elapsed() < std::time::Duration::from_secs(5));
        crate::test_support::cleanup_tree(&root);
    }

    /// C-INV2/NWE-02：取消同样终止整个进程组。
    #[test]
    #[cfg(unix)]
    fn run_command_cancellation_kills_the_process_tree() {
        let (root, project) = fixture();
        let marker = root.join("orphan-marker");
        let cancel = CancelToken::new();
        let token = cancel.clone();
        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(100));
            token.cancel();
        });
        let started = std::time::Instant::now();
        let error = RunCommandTool
            .invoke(
                &json!({"command": "(sleep 2; printf survived > orphan-marker) >/dev/null 2>&1 & wait"}),
                &project,
                &cancel,
            )
            .expect_err("cancelled run must fail");
        assert!(error.to_string().contains("cancelled"));
        assert!(started.elapsed() < std::time::Duration::from_secs(5));
        std::thread::sleep(std::time::Duration::from_millis(2500));
        assert!(!marker.exists(), "cancel must kill descendants too");
        crate::test_support::cleanup_tree(&root);
    }

    /// C-INV2/NWE-03：超限输出**不得**改变命令语义——命令正常退出
    /// （exit 0）、输出之后的动作照常执行（marker 产生）、保留字节
    /// 恰为上限且标记截断。旧实现关闭管道使命令死于 SIGPIPE。
    #[test]
    #[cfg(unix)]
    fn run_command_output_is_bounded() {
        let (root, project) = fixture();
        let output = RunCommandTool
            .invoke(
                &json!({
                    "command": "head -c 100000 /dev/zero | tr '\\0' 'x'; printf done > bounded-marker"
                }),
                &project,
                &CancelToken::new(),
            )
            .expect("run");
        assert_eq!(
            output["exit_code"], 0,
            "truncation must not kill the command"
        );
        assert_eq!(output["signal"], serde_json::Value::Null);
        assert_eq!(output["stdout_truncated"], true);
        assert_eq!(
            output["stdout"].as_str().unwrap().len(),
            MAX_COMMAND_OUTPUT_BYTES
        );
        assert!(
            root.join("bounded-marker").exists(),
            "actions after the overflowed output must still run (NWE-03)"
        );
        crate::test_support::cleanup_tree(&root);
    }

    /// NWE-05：覆盖保留既有文件的权限位（可执行脚本不丢执行位）。
    #[test]
    #[cfg(unix)]
    fn overwrites_preserve_existing_file_permissions() {
        use std::os::unix::fs::PermissionsExt;
        let (root, project) = fixture();
        let script = root.join("script.sh");
        fs::write(&script, "#!/bin/sh\nold\n").expect("script");
        fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).expect("mode");

        WriteFileTool::default()
            .invoke(
                &json!({"path": "script.sh", "content": "#!/bin/sh\nnew\n"}),
                &project,
                &CancelToken::new(),
            )
            .expect("overwrite");
        let mode = fs::metadata(&script).unwrap().permissions().mode();
        assert_eq!(
            mode & 0o777,
            0o755,
            "write_file keeps the exec bit (NWE-05)"
        );

        EditFileTool::default()
            .invoke(
                &json!({"path": "script.sh", "old_str": "new", "new_str": "edited"}),
                &project,
                &CancelToken::new(),
            )
            .expect("edit");
        let mode = fs::metadata(&script).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o755, "edit_file keeps the exec bit (NWE-05)");

        // 新建文件不受影响（umask 语义）。
        WriteFileTool::default()
            .invoke(
                &json!({"path": "fresh.txt", "content": "x"}),
                &project,
                &CancelToken::new(),
            )
            .expect("create");
        let mode = fs::metadata(root.join("fresh.txt"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o644);
        crate::test_support::cleanup_tree(&root);
    }

    /// NWE-06：编辑基于过期快照提交时必须冲突失败，不得静默覆盖
    /// 并行修改。通过 atomic_write 的快照参数直接驱动（edit_file 内
    /// 部读取无法确定性注入并发窗口，守卫本身在此锁定）。
    #[test]
    fn stale_snapshots_conflict_instead_of_overwriting() {
        let (root, project) = fixture();
        let file = root.join("concurrent.txt");
        fs::write(&file, "original\n").expect("file");
        let target = project
            .writable_target(
                "concurrent.txt",
                false,
                crate::permission::WriteScope::ProjectRoot,
            )
            .expect("target");

        // "并行修改者"在读取快照之后改了文件。
        fs::write(&file, "parallel-writer\n").expect("parallel write");
        // 基于过期快照（"original"）的提交：冲突失败，并行修改者
        // 的内容原样保留。
        let error = target
            .atomic_write("stale-edit\n", Some("original\n"))
            .expect_err("conflict");
        assert!(error.to_string().contains("changed while editing"));
        assert_eq!(
            fs::read_to_string(&file).unwrap(),
            "parallel-writer\n",
            "parallel changes must never be silently overwritten (NWE-06)"
        );

        // 基于当前快照的提交：正常应用。
        target
            .atomic_write("my-edit\n", Some("parallel-writer\n"))
            .expect("commit");
        assert_eq!(fs::read_to_string(&file).unwrap(), "my-edit\n");

        crate::test_support::cleanup_tree(&root);
    }

    /// P-INV1：写/执行工具必然落入 Ask——权限系统对新工具自动生效。
    #[test]
    fn write_and_execute_tools_always_require_approval() {
        use crate::permission::{PermissionDecision, PermissionPolicy, SafeByDefault};
        use crate::tool::ToolCall;
        let project = Project::new("/tmp/clat-nowhere");
        for definition in [
            WriteFileTool::default().definition(),
            EditFileTool::default().definition(),
            RunCommandTool.definition(),
        ] {
            let call = ToolCall {
                id: "t".into(),
                name: definition.name.clone(),
                arguments: json!({}),
            };
            assert!(
                matches!(
                    SafeByDefault.check(&project, &definition, &call),
                    PermissionDecision::Ask { .. }
                ),
                "{} must ask, never auto-allow",
                definition.name
            );
        }
    }

    struct YesAsker;

    impl crate::interaction::UserAsker for YesAsker {
        fn ask(
            &self,
            question: crate::interaction::AskQuestion,
            _cancel: &CancelToken,
        ) -> crate::interaction::AskAnswer {
            assert_eq!(question.question, "ship it?");
            assert_eq!(question.options.len(), 2);
            assert!(question.allow_custom);
            crate::interaction::AskAnswer::Selected("yes".into())
        }
    }

    struct DecliningAsker;

    impl crate::interaction::UserAsker for DecliningAsker {
        fn ask(
            &self,
            _question: crate::interaction::AskQuestion,
            _cancel: &CancelToken,
        ) -> crate::interaction::AskAnswer {
            crate::interaction::AskAnswer::Declined
        }
    }

    /// S8/答案形状：无前端结构化报错；已安装回传所选标签；拒绝变错误
    /// 结果（run 继续）；参数校验先行。
    #[test]
    fn ask_user_tool_degrades_without_a_frontend_and_returns_answers() {
        let slot = crate::interaction::AskUserSlot::shared();
        let tool = AskUserTool {
            slot: std::sync::Arc::clone(&slot),
        };
        let project = Project::new(".");
        let cancel = CancelToken::new();
        let arguments = json!({
            "question": "ship it?",
            "options": [{"label": "yes"}, {"label": "no"}],
        });

        let headless = tool
            .invoke(&arguments, &project, &cancel)
            .expect_err("no frontend installed");
        assert!(
            headless.to_string().contains("no interactive frontend"),
            "{}",
            headless
        );

        slot.install(Some(std::sync::Arc::new(YesAsker)));
        assert_eq!(
            tool.invoke(&arguments, &project, &cancel).unwrap(),
            json!({ "answer": "yes" })
        );

        slot.install(Some(std::sync::Arc::new(DecliningAsker)));
        let declined = tool
            .invoke(&arguments, &project, &cancel)
            .expect_err("declined is an error result");
        assert!(declined.to_string().contains("declined"), "{declined}");

        slot.install(Some(std::sync::Arc::new(YesAsker)));
        assert!(
            tool.invoke(&json!({"question": "   "}), &project, &cancel)
                .is_err(),
            "blank questions are rejected before the frontend"
        );
        assert!(
            tool.invoke(
                &json!({"question": "q", "options": [{}]}),
                &project,
                &cancel
            )
            .is_err(),
            "option without label is rejected before the frontend"
        );

        slot.install(None);
    }

    /// Stage-0 characterization: model-visible tool order is part of the
    /// request/cache surface and must survive registry/plugin migration.
    #[test]
    fn native_tool_definition_order_is_stable() {
        let definitions = native_read_tools()
            .into_iter()
            .chain(std::iter::once(
                std::sync::Arc::new(crate::plugins::SearchTool) as std::sync::Arc<dyn Tool>,
            ))
            .chain(native_write_tools(
                crate::permission::WriteScopeSource::default(),
            ))
            .chain(std::iter::once(
                std::sync::Arc::new(crate::plugins::ApplyPatchTool::default())
                    as std::sync::Arc<dyn Tool>,
            ))
            .chain(std::iter::once(
                std::sync::Arc::new(RunCommandTool) as std::sync::Arc<dyn Tool>
            ))
            .chain(native_interaction_tools(
                crate::interaction::AskUserSlot::shared(),
            ))
            .map(|tool| tool.definition().name)
            .collect::<Vec<_>>();
        assert_eq!(
            definitions,
            [
                "list_files",
                "read_file",
                "search",
                "write_file",
                "edit_file",
                "apply_patch",
                "run_command",
                "ask_user"
            ]
        );
    }
}
