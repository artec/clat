//! Built-in native tools: project-scoped `list_files`/`read_file` plus
//! trusted-project `write_file`/`edit_file`/`ask_user`. Process tools live in
//! `plugins/process.rs` over the shared ProcessService.

use crate::CancelToken;
use crate::project::Project;
use crate::tool::{Tool, ToolDefinition, ToolEffect, ToolError};
use serde_json::{Value, json};
use std::fs;
use std::path::Path;

const DEFAULT_LIST_DEPTH: usize = 2;
const DEFAULT_LIST_ENTRIES: usize = 200;
const MAX_LIST_DEPTH: usize = 8;
const MAX_LIST_ENTRIES: usize = 2_000;
mod argument_recovery;
mod edit;
mod edit_recovery;
mod export;
mod read;
mod write;

pub use read::ReadFileTool;
pub use write::WriteFileTool;

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

pub(crate) fn native_write_tools(
    scope: crate::permission::WriteScopeSource,
) -> Vec<std::sync::Arc<dyn Tool>> {
    vec![
        std::sync::Arc::new(WriteFileTool {
            scope: scope.clone(),
        }),
        std::sync::Arc::new(EditFileTool { scope }),
        std::sync::Arc::new(export::ExportPatchTool),
    ]
}

const MAX_WRITE_BYTES: usize = 1024 * 1024;

/// 写工具携带写入围栏来源（SR2），同 [`WriteFileTool::default()`]。
#[derive(Clone, Default)]
pub struct EditFileTool {
    scope: crate::permission::WriteScopeSource,
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
mod argument_recovery_tests;
#[cfg(test)]
mod edit_tests;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugins::SearchTool;
    use crate::tool::ToolResultTransformer;
    use std::env;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};

    /// fixture 目录名唯一化：纳秒时间戳 + 单调递增计数器——并行
    /// 测试在同一纳秒各建一个 fixture 时纯时间戳会撞名，一方 cleanup
    /// 删掉另一方的根目录。
    static FIXTURE_SEQUENCE: AtomicU64 = AtomicU64::new(0);

    pub(super) fn fixture() -> (PathBuf, Project) {
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

    // ===== READ-1（2026-10-15）判别测试族：有界、可续、完整、可判别 =====

    /// 游标续读驱动器：循环调用 read_file 并回传 next_cursor 与
    /// expected_file_version，直到 has_more=false。返回模型视角的
    /// 全部段（400 段上限防死循环——续读语义要求必然终止）。
    fn read_until_eof(project: &Project, path: &str, extra_args: Value) -> Vec<Value> {
        let mut segments = Vec::new();
        let mut call = serde_json::Map::new();
        call.insert("path".into(), json!(path));
        if let Value::Object(args) = extra_args {
            for (key, value) in args {
                call.insert(key, value);
            }
        }
        loop {
            let output = ReadFileTool
                .invoke(&Value::Object(call.clone()), project, &CancelToken::new())
                .expect("segmented read");
            let has_more = output["has_more"].as_bool().expect("has_more");
            segments.push(output);
            assert!(
                segments.len() <= 400,
                "segmented read must terminate (got {} segments)",
                segments.len()
            );
            if !has_more {
                return segments;
            }
            let last = segments.last().expect("just pushed");
            call.insert(
                "cursor".into(),
                json!(last["next_cursor"].as_str().expect("next_cursor")),
            );
            call.insert(
                "expected_file_version".into(),
                json!(last["file_version"].as_str().expect("file_version")),
            );
        }
    }

    /// 从段显示重建文本：`N | ` 前缀只在行首片出现，行内续片原样；
    /// 每个以 `\n` 结尾的片补一个 `\n`（源 `\r\n` 被规范化）。
    fn reconstruct_text(segments: &[Value]) -> String {
        let mut text = String::new();
        for segment in segments {
            let content = segment["content"].as_str().unwrap_or("");
            let mut rest = content;
            while !rest.is_empty() {
                match rest.split_once('\n') {
                    Some((part, tail)) => {
                        text.push_str(strip_line_prefix(part));
                        text.push('\n');
                        rest = tail;
                    }
                    None => {
                        // 段尾行内截断片：无 `\n`，由下一段续拼。
                        text.push_str(strip_line_prefix(rest));
                        rest = "";
                    }
                }
            }
        }
        text
    }

    fn strip_line_prefix(part: &str) -> &str {
        let digits_end = part.find(" | ").unwrap_or(0);
        if digits_end > 0 && part[..digits_end].chars().all(|ch| ch.is_ascii_digit()) {
            &part[digits_end + 3..]
        } else {
            part
        }
    }

    /// R2 主证据：长文件（CRLF、中文、无尾换行、恰好超限的多行混合）
    /// 按游标续读——不重叠、不遗漏、不循环，最终 has_more=false，重建
    /// 文本与源（规范化后）一致。pre-fix 红：旧实现无游标，模型只能
    /// 重复同一截断窗口或被 pruner 吃掉中段。
    #[test]
    fn read_file_segments_cover_a_long_file_without_gaps_or_overlap() {
        let (root, project) = fixture();
        let path = root.join("long-doc.md");
        let mut source = String::new();
        for index in 0..900 {
            source.push_str(&format!("line-{index:04} 中文内容 emoji 🚀\r\n"));
        }
        // 恰好制造无尾换行的末行 + 超长行（超过单段预算）。
        source.push_str("final line without trailing newline");
        let giant = "g".repeat(20_000);
        source.push_str(&format!("\n{giant}"));
        fs::write(&path, source.as_bytes()).expect("write long doc");

        let segments = read_until_eof(&project, "long-doc.md", json!({}));
        assert!(
            segments.len() >= 3,
            "a long file must arrive as multiple bounded segments"
        );
        // 每段（除末段）都声明 has_more + 游标；末段终止。
        for segment in &segments[..segments.len() - 1] {
            assert_eq!(segment["has_more"], true);
            assert!(segment["next_cursor"].as_str().is_some());
        }
        assert_eq!(segments.last().expect("segments")["has_more"], false);

        // 游标严格单调前进（不重叠、不循环）。
        let mut offsets: Vec<u64> = Vec::new();
        for segment in &segments[..segments.len() - 1] {
            let cursor = segment["next_cursor"].as_str().expect("cursor");
            let byte = cursor
                .strip_prefix("b=")
                .and_then(|rest| rest.split(';').next())
                .and_then(|value| value.parse::<u64>().ok())
                .expect("cursor byte offset");
            offsets.push(byte);
        }
        assert!(
            offsets.windows(2).all(|pair| pair[0] < pair[1]),
            "cursor offsets must strictly increase: {offsets:?}"
        );

        // 重建文本 == 源文本（\r\n 规范化为 \n；无尾换行的末行被显示
        // 规范补 \n；超长单行是最后一行，行内截断片按原样拼回）。
        let normalized = source.replace("\r\n", "\n");
        let normalized = if normalized.ends_with('\n') {
            normalized
        } else {
            format!("{normalized}\n")
        };
        let reconstructed = reconstruct_text(&segments);
        if reconstructed != normalized {
            let mismatch = reconstructed
                .chars()
                .zip(normalized.chars())
                .position(|(left, right)| left != right)
                .unwrap_or_else(|| {
                    reconstructed
                        .chars()
                        .count()
                        .min(normalized.chars().count())
                });
            panic!(
                "segments must reconstruct the file: mismatch at char {mismatch} \
                 (reconstructed {} chars, normalized {} chars, {} segments)",
                reconstructed.chars().count(),
                normalized.chars().count(),
                segments.len()
            );
        }
        crate::test_support::cleanup_tree(&root);
    }

    /// R2 超长单行：pre-fix 红的核心场景——旧实现对超过 max_bytes 的
    /// 单行只返回空 content + truncated，重试同一 start_line 永不前进。
    /// 新语义：首个 start_line 读取就返回行首前缀 + partial_line +
    /// next_cursor；按游标续读覆盖整行（多字节字符不被截断在字节中间，
    /// 无 U+FFFD、无重叠）。
    #[test]
    fn read_file_giant_single_line_progresses_by_byte_cursor() {
        let (root, project) = fixture();
        let path = root.join("giant-utf8-line.txt");
        // ASCII 前缀 + 多字节主体：字符边界切割的判别压力。
        let mut source = String::from("prefix ");
        for index in 0..6_000 {
            source.push_str(&format!("段{index:05}🚀"));
        }
        source.push('\n');
        source.push_str("tail\n");
        fs::write(&path, source.as_bytes()).expect("write giant line");

        let first = ReadFileTool
            .invoke(
                &json!({"path": "giant-utf8-line.txt", "max_bytes": 1024}),
                &project,
                &CancelToken::new(),
            )
            .expect("first segment");
        assert!(
            !first["content"].as_str().unwrap().is_empty(),
            "pre-fix: a giant line returned empty content and never progressed"
        );
        assert_eq!(first["truncated"], true);
        assert_eq!(first["partial_line"], true, "must mark the partial line");
        assert!(first["next_cursor"].as_str().is_some());
        // partial 段不得提供 next_start_line（跳行号会漏行内剩余）。
        assert!(first.get("next_start_line").is_none());

        let segments = read_until_eof(&project, "giant-utf8-line.txt", json!({"max_bytes": 1024}));
        assert!(segments.len() >= 5, "the giant line needs several chunks");
        // 中间段：行内续片无 `N | ` 前缀、标记 partial_line。
        for segment in &segments[..segments.len() - 1] {
            assert!(segment["partial_line"] == true || segment["has_more"] == true);
        }
        // 无 lossy 替换（字符边界完整）。
        for segment in &segments {
            assert!(
                segment.get("invalid_utf8").is_none(),
                "char-boundary chunks must not corrupt multibyte content"
            );
        }
        // 重建：首片带 `1 | ` 前缀，其余行内片原样；拼接 == 源行。
        let mut chunks = String::new();
        for (index, segment) in segments.iter().enumerate() {
            let content = segment["content"].as_str().unwrap();
            if index == 0 {
                chunks.push_str(content.strip_prefix("1 | ").unwrap_or(content));
            } else {
                chunks.push_str(content);
            }
        }
        // 尾部两行（tail\n + 末空行）也在最后一段内；只验证到源第一行
        // 的重建：chunks 以源第一行开头。
        let first_source_line = "prefix 段00000🚀";
        assert!(
            chunks.starts_with(first_source_line),
            "first chunk text must match the source line start"
        );
        // 覆盖证明：所有段的字符总量（去前缀）== 源文件字符数
        //（源两行 + 换行；显示补 \n 与源 \n 对齐）。
        let total: usize = segments
            .iter()
            .map(|segment| segment["content"].as_str().unwrap().chars().count())
            .sum::<usize>()
            - "1 | ".chars().count()
            - "2 | ".chars().count();
        let expected = source.chars().count();
        assert_eq!(
            total, expected,
            "chunks must cover every source char exactly once"
        );
        crate::test_support::cleanup_tree(&root);
    }

    /// R1 主证据：真实 ResultPruner（生产二级裁剪）对每个 read_file 段
    /// 必须是 no-op——结果构造上就低于阈值。对抗输入：引号/反斜杠/
    /// 制表符密集、超长路径、恰好边界尺寸。
    #[test]
    fn read_file_results_survive_the_production_result_pruner() {
        let (root, project) = fixture();
        let dir = root.join("a-very-long-directory-name-for-envelope-pressure");
        fs::create_dir_all(&dir).expect("dir");
        let path = dir.join("escape-heavy-with-quotes-and-tabs-and-backslashes.md");
        let mut source = String::new();
        for index in 0..2_000 {
            source.push_str(&format!("line {index}: \"quoted \\ tab\tend\r\n"));
        }
        fs::write(&path, source.as_bytes()).expect("write escape-heavy doc");

        let segments = read_until_eof(&project, path.to_str().unwrap(), json!({}));
        assert!(segments.len() >= 3);
        let pruner = crate::plugins::ResultPruner;
        for segment in &segments {
            let serialized = serde_json::to_string(segment).expect("serialize");
            assert!(
                serialized.chars().count() <= crate::plugins::THRESHOLD_CHARS,
                "every segment must be constructed under the pruner threshold"
            );
            let mut result = crate::tool::ToolResult {
                call_id: "call-1".into(),
                tool_name: "read_file".into(),
                output: segment.clone(),
                is_error: false,
                blocks: Vec::new(),
                image_parts: Vec::new(),
            };
            pruner.transform_result(&mut result);
            assert_eq!(
                result.output, *segment,
                "the production pruner must never rewrite a read_file segment"
            );
        }
        crate::test_support::cleanup_tree(&root);
    }

    // ===== 审计返工判别（乙方反例 CF1–CF3，2026-10-15 r2）=====

    /// CF1（乙方公开输入）：转义密集超长单行（`"` `\` `\t` ×8000 +
    /// `\n`，24001 字节），默认参数。修复前：首片按裸字符数取片、
    /// 序列化后超帽、最终收缩整片弹出 → **成功返回零进度空段**
    /// （content=""、has_more=true、cursor 不动）。修复后：取片转义
    /// 感知，每个成功且非 EOF 的段有真实内容与前进游标。
    #[test]
    fn read_file_escape_heavy_giant_line_progresses_and_covers() {
        let (root, project) = fixture();
        let path = root.join("escape-heavy.txt");
        let mut source = String::new();
        for _ in 0..8_000 {
            source.push_str("\"\\\t");
        }
        source.push('\n');
        fs::write(&path, source.as_bytes()).expect("write");

        let segments = read_until_eof(&project, "escape-heavy.txt", json!({}));
        assert!(
            segments.len() >= 3,
            "escape-dense giant line must arrive as multiple segments"
        );
        for (index, segment) in segments.iter().enumerate() {
            let has_more = segment["has_more"].as_bool().expect("has_more");
            let content = segment["content"].as_str().expect("content");
            assert!(
                !has_more || !content.is_empty(),
                "CF1: segment {index} returned empty content with has_more=true"
            );
            assert!(
                segment.get("clat_truncated").is_none(),
                "the production pruner must never rewrite segments"
            );
            assert!(
                segment.get("invalid_utf8").is_none(),
                "escape-dense ASCII must not corrupt"
            );
        }
        let reconstructed = reconstruct_text(&segments);
        if reconstructed != source {
            panic!(
                "escape-heavy segments must reconstruct the source: reconstructed \
                 {} chars, expected {} chars, {} segments",
                reconstructed.chars().count(),
                source.chars().count(),
                segments.len()
            );
        }
        crate::test_support::cleanup_tree(&root);
    }

    /// CF2（乙方公开输入）：`abcdef\n` + `max_bytes=7`（schema 最小值
    /// 1 的合法值）。修复前：`budget < 8` 在任何读取前 break → 成功
    /// 空 content + 同游标空转。修复后：任何接受的预算要么取得可解释
    /// 进度（≥1 可见字符），要么显式报出预算不足。
    #[test]
    fn read_file_schema_small_budget_makes_progress_or_fails_explicitly() {
        let (root, project) = fixture();
        fs::write(root.join("small.txt"), b"abcdef\n").expect("write");

        let segments = read_until_eof(&project, "small.txt", json!({"max_bytes": 7}));
        assert!(
            segments.len() >= 2,
            "a 7-byte budget must still split the file"
        );
        for (index, segment) in segments.iter().enumerate() {
            let has_more = segment["has_more"].as_bool().expect("has_more");
            let content = segment["content"].as_str().expect("content");
            assert!(
                !has_more || !content.is_empty(),
                "CF2: segment {index} returned empty content with has_more=true"
            );
        }
        assert_eq!(reconstruct_text(&segments), "abcdef\n");

        // 不可行预算（连一个带行号前缀的字符都放不下）：显式教学错误。
        let error = ReadFileTool
            .invoke(
                &json!({"path": "small.txt", "max_bytes": 1}),
                &project,
                &CancelToken::new(),
            )
            .expect_err("max_bytes=1 cannot fit a numbered prefix");
        assert!(
            error.to_string().contains("too small"),
            "teaching error: {error}"
        );
        crate::test_support::cleanup_tree(&root);
    }

    /// CF3（乙方公开输入）：`"中" × 100 + "\n"`、`max_bytes=32`。修复
    /// 前：预算按字符消费，首段 content 达 85 UTF-8 字节，超出调用者
    /// 显式字节上限。修复后：`max_bytes` 恢复 content 的 UTF-8 字节帽
    /// （含行号前缀），与序列化帽同时生效。
    #[test]
    fn read_file_max_bytes_bounds_content_utf8_bytes() {
        let (root, project) = fixture();
        let path = root.join("cjk.txt");
        let source = format!("{}\n", "中".repeat(100));
        fs::write(&path, source.as_bytes()).expect("write");

        let segments = read_until_eof(&project, "cjk.txt", json!({"max_bytes": 32}));
        assert!(
            segments.len() >= 3,
            "a 32-byte budget must split the CJK line"
        );
        for segment in &segments {
            let content = segment["content"].as_str().expect("content");
            assert!(
                content.len() <= 32,
                "CF3: content is {} UTF-8 bytes, exceeding max_bytes=32",
                content.len()
            );
        }
        assert_eq!(reconstruct_text(&segments), source);
        crate::test_support::cleanup_tree(&root);
    }

    /// CF5（r2 审计公开输入）：四级目录、每级 160 个双引号 + `f.txt`
    /// （请求路径 649 字节、序列化 1291 字符），正文 `"x"×16000 + \n`，
    /// 默认参数。r2 缺陷：信封预留按裸字符计路径、构造预算虚高、首片
    /// 被收缩弹空后空信封低于阈值，循环以**成功零进度段**退出。
    #[test]
    fn read_file_escaped_path_cannot_erase_the_first_piece() {
        let (root, project) = fixture();
        let (path, requested) = crate::test_support::escaped_read_fixture(&root);
        fs::write(&path, format!("{}\n", "x".repeat(16_000))).expect("write");
        assert_eq!(
            project.resolve_existing(&requested).unwrap(),
            path.canonicalize().unwrap()
        );
        assert!(
            serde_json::to_string(&requested).unwrap().chars().count()
                >= requested.chars().count() + 640
        );

        let segments = read_until_eof(&project, &requested, json!({}));
        assert!(
            segments.len() >= 3,
            "the escaped-path file must still arrive as multiple segments"
        );
        for (index, segment) in segments.iter().enumerate() {
            let has_more = segment["has_more"].as_bool().expect("has_more");
            let content = segment["content"].as_str().expect("content");
            assert!(
                !has_more || !content.is_empty(),
                "CF5: segment {index} returned empty content with has_more=true"
            );
        }
        let reconstructed = reconstruct_text(&segments);
        if reconstructed != format!("{}\n", "x".repeat(16_000)) {
            panic!(
                "escaped-path segments must reconstruct the source: {} chars, {} segments",
                reconstructed.chars().count(),
                segments.len()
            );
        }
        crate::test_support::cleanup_tree(&root);
    }

    /// CF6（r2 审计公开输入）：`[0xff; 100] + [0x0a]`、`max_bytes=32`。
    /// r2 缺陷：lossy 替换的 U+FFFD 输出为 3 字节而预算按输入 1 字节
    /// 计，各段 content 输出 33–34 字节超过显式上限。修复后输出字节帽
    /// 与源字节游标解耦，替换文本同样守法。
    #[test]
    fn read_file_lossy_utf8_still_obeys_the_output_byte_cap() {
        let (root, project) = fixture();
        let path = root.join("lossy.bin");
        let mut bytes = vec![0xff; 100];
        bytes.push(0x0a);
        fs::write(&path, &bytes).expect("write");

        let segments = read_until_eof(&project, "lossy.bin", json!({"max_bytes": 32}));
        assert!(
            segments.len() >= 3,
            "a 32-byte cap must split the replacements"
        );
        for (index, segment) in segments.iter().enumerate() {
            let content = segment["content"].as_str().expect("content");
            assert!(
                content.len() <= 32,
                "CF6: segment {index} content is {} output bytes, exceeding max_bytes=32",
                content.len()
            );
            assert_eq!(
                segment["invalid_utf8"], true,
                "lossy decoding must be marked"
            );
        }
        // 重建：100 个替换字符 + 行终止。
        let reconstructed = reconstruct_text(&segments);
        assert_eq!(
            reconstructed.chars().count(),
            101,
            "100 replacement chars plus the newline"
        );
        assert!(reconstructed.starts_with('\u{FFFD}'));
        assert!(reconstructed.ends_with('\n'));
        crate::test_support::cleanup_tree(&root);
    }

    /// CF7（r2 审计公开输入）：`a\r\nb\r\n`、`max_bytes=9`。r2 缺陷：
    /// partial 片发出 `\r`、下一段只看到 `\n`，重建为 `a\r\nb\n`——
    /// CR 归一化依赖分段位置。修复后 CRLF 是单一行终止（跨段边界同样
    /// 归一化），重建恒为完整行归一化结果 `a\nb\n`。
    #[test]
    fn read_file_crlf_normalization_does_not_depend_on_segment_boundary() {
        let (root, project) = fixture();
        fs::write(root.join("crlf.txt"), b"a\r\nb\r\n").expect("write");

        let segments = read_until_eof(&project, "crlf.txt", json!({"max_bytes": 9}));
        assert!(
            segments.len() >= 2,
            "a 9-byte budget must split the two CRLF lines"
        );
        assert_eq!(
            reconstruct_text(&segments),
            "a\nb\n",
            "CRLF normalization must not depend on where segments split"
        );
        // 对照：同文件一次性读完（大预算）也得到相同归一化结果。
        let whole = ReadFileTool
            .invoke(&json!({"path": "crlf.txt"}), &project, &CancelToken::new())
            .expect("whole read");
        assert_eq!(
            whole["content"].as_str().expect("content"),
            "1 | a\n2 | b\n"
        );
        crate::test_support::cleanup_tree(&root);
    }

    /// CF8（r3 审计公开输入）：源字节 `a\r\r\nb\r`——第一个 `\r` 是
    /// 正文，`\r\n` 是行终止，末尾 `\r` 是 EOF 正文。r3 缺陷：扫描层
    /// 已把 CRLF 归一化，`complete_line_piece` 却仍 pop 尾部 CR，正文
    /// CR 随分段预算丢失（默认/32 → `a\nb\n`；9 → `a\r\nb\n`）。修复
    /// 后归一化只作用于真正的 CRLF，任意预算下重建恒为 `a\r\nb\r\n`。
    #[test]
    fn read_file_standalone_cr_is_preserved_independently_of_segmentation() {
        let (root, project) = fixture();
        fs::write(root.join("standalone-cr.txt"), b"a\r\r\nb\r").expect("write");
        let expected = "a\r\nb\r\n";

        for budget in [None, Some(9), Some(32), Some(64), Some(4096)] {
            let args = match budget {
                Some(bytes) => json!({"max_bytes": bytes}),
                None => json!({}),
            };
            let segments = read_until_eof(&project, "standalone-cr.txt", args);
            let label = format!("max_bytes={:?}", budget);
            let reconstructed = reconstruct_text(&segments);
            if reconstructed != expected {
                panic!(
                    "CF8 ({label}): reconstructed {:?} != expected {:?} ({} segments)",
                    reconstructed,
                    expected,
                    segments.len()
                );
            }
        }

        // 对照：一次性整读的正文视图与分段重建一致（同一归一化）。
        let whole = ReadFileTool
            .invoke(
                &json!({"path": "standalone-cr.txt"}),
                &project,
                &CancelToken::new(),
            )
            .expect("whole read");
        assert_eq!(
            whole["content"].as_str().expect("content"),
            "1 | a\r\n2 | b\r\n",
            "whole-read view must keep standalone CRs too"
        );
        crate::test_support::cleanup_tree(&root);
    }

    /// R3：续读之间文件被修改——expected_file_version 必须显式失败并
    /// 指导重启；越界游标同样显式失败。
    #[test]
    fn read_file_version_mismatch_and_stale_cursors_fail_explicitly() {
        let (root, project) = fixture();
        let path = root.join("changing.txt");
        let mut source = String::new();
        for index in 0..300 {
            source.push_str(&format!("original line {index:03}\r\n"));
        }
        fs::write(&path, source.as_bytes()).expect("write");

        let first = ReadFileTool
            .invoke(
                &json!({"path": "changing.txt", "max_bytes": 1024}),
                &project,
                &CancelToken::new(),
            )
            .expect("first read");
        assert_eq!(first["has_more"], true, "the fixture must span segments");
        let version = first["file_version"].as_str().expect("version").to_owned();
        let cursor = first["next_cursor"].as_str().expect("cursor").to_owned();

        // 文件被替换（长度与 mtime 都变）。
        let mut replacement = String::new();
        for index in 0..120 {
            replacement.push_str(&format!("replaced line {index:03} with different width\n"));
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
        fs::write(&path, replacement.as_bytes()).expect("rewrite");

        let error = ReadFileTool
            .invoke(
                &json!({
                    "path": "changing.txt",
                    "cursor": cursor,
                    "expected_file_version": version,
                }),
                &project,
                &CancelToken::new(),
            )
            .expect_err("a changed file must fail the version check");
        assert!(
            error.to_string().contains("changed since the last read"),
            "teaching error: {error}"
        );

        // 不带版本参数的越界游标：显式失败而非空转。
        let error = ReadFileTool
            .invoke(
                &json!({"path": "changing.txt", "cursor": "b=9999;l=9"}),
                &project,
                &CancelToken::new(),
            )
            .expect_err("a stale cursor must fail explicitly");
        assert!(
            error.to_string().contains("past the end of the file"),
            "teaching error: {error}"
        );
        crate::test_support::cleanup_tree(&root);
    }

    /// R4：显式超大 max_bytes 被分段帽压回（content_budget_chars 回报
    /// 实际生效值），续读仍完整覆盖；小 max_bytes 原样尊重。
    #[test]
    fn read_file_large_max_bytes_is_capped_but_still_resumable() {
        let (root, project) = fixture();
        let path = root.join("cap-me.md");
        let mut source = String::new();
        for index in 0..400 {
            source.push_str(&format!("cap line {index:03} with some padding text\r\n"));
        }
        fs::write(&path, source.as_bytes()).expect("write");

        let segments = read_until_eof(&project, "cap-me.md", json!({"max_bytes": 65536}));
        assert!(segments.len() >= 2, "the segment cap must split the file");
        for segment in &segments {
            let budget = segment["content_budget_chars"].as_u64().expect("budget");
            assert!(
                budget <= crate::plugins::THRESHOLD_CHARS as u64,
                "the applied budget must stay under the pruner threshold"
            );
            let content = segment["content"].as_str().expect("content");
            assert!(content.chars().count() <= budget as usize);
        }
        let reconstructed = reconstruct_text(&segments);
        if reconstructed != source.replace("\r\n", "\n") {
            panic!(
                "capped segments must cover the whole file: reconstructed {} chars, \
                 expected {} chars, {} segments",
                reconstructed.chars().count(),
                source.chars().count(),
                segments.len()
            );
        }
        crate::test_support::cleanup_tree(&root);
    }

    /// R2 边界语义：空文件与越过 EOF 的 start_line 都是显式、可判别
    /// 的结果（不返回同一截断片段，不无限空转）。
    #[test]
    fn read_file_empty_and_beyond_eof_have_explicit_semantics() {
        let (root, project) = fixture();
        fs::write(root.join("empty.txt"), b"").expect("empty");

        let output = ReadFileTool
            .invoke(&json!({"path": "empty.txt"}), &project, &CancelToken::new())
            .expect("empty read");
        assert_eq!(output["content"], "");
        assert_eq!(output["has_more"], false);
        assert_eq!(output["truncated"], false);
        assert!(output.get("next_cursor").is_none());

        let output = ReadFileTool
            .invoke(
                &json!({"path": "empty.txt", "start_line": 42}),
                &project,
                &CancelToken::new(),
            )
            .expect("beyond eof read");
        assert_eq!(output["content"], "");
        assert_eq!(output["beyond_eof"], true);
        assert_eq!(output["has_more"], false);
        assert!(output.get("next_cursor").is_none());
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
    #[cfg(unix)]
    fn workspace_write_has_a_project_external_script_target() {
        let (root, project) = fixture();
        let roots = crate::sandbox::roots::WritableRoots::create(project.root()).unwrap();
        *project.roots.write().unwrap() = Some(roots);
        let outside =
            std::env::temp_dir().join(format!("clat-sbx1-red-{}.py", uuid::Uuid::new_v4()));
        let writer = WriteFileTool {
            scope: crate::permission::WriteScopeSource::Shared(std::sync::Arc::new(
                std::sync::RwLock::new(crate::permission::PermissionMode::ProjectWrite),
            )),
        };
        writer
            .invoke(
                &json!({"path": outside, "content": "print(1)"}),
                &project,
                &CancelToken::new(),
            )
            .expect("workspace-write supports temporary scripts outside the project");
        assert_eq!(fs::read_to_string(&outside).unwrap(), "print(1)");
        fs::remove_file(outside).unwrap();
        crate::test_support::cleanup_tree(&root);
    }

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
                &json!({"path": "docs/new/note.md", "content": "second\n", "overwrite": true}),
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
                &json!({"path": "script.sh", "content": "#!/bin/sh\nnew\n", "overwrite": true}),
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
                "export_patch",
                "apply_patch",
                "run_command",
                "ask_user"
            ]
        );
    }
}

#[cfg(test)]
mod write_guard_tests;
