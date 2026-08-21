//! clat read 插件（插件桥 Phase 2b 试点：原生读工具的 WASM 移植）。
//!
//! `read_file` / `list_dir`（effect Read）对齐原生读工具的语义子集：
//! 相对项目根寻址（guest 路径 `project/` 前缀）、输出有界；`write_file`
//! （effect Write）是授予矩阵的验证车——Read Only 档下 preopen 只读，
//! 写必须被 capability 边界拒绝。无宿主导入（纯 fs 组件）。

wit_bindgen::generate!({
    path: "../../wit",
    world: "plugin",
});

use crate::exports::clat::plugin::tools::{Definition, Effect, Guest};
use serde::Deserialize;
use std::path::{Path, PathBuf};

/// 单文件读取上限（对齐原生读工具的有界输出纪律）。
const MAX_READ_BYTES: usize = 256 * 1024;

const READ_SCHEMA: &str = r#"{
  "type": "object",
  "properties": {
    "path": { "type": "string", "description": "relative to the project root; /<root>/… addresses an extra granted directory" },
    "offset": { "type": "integer", "description": "byte offset to start reading from" },
    "limit": { "type": "integer", "description": "maximum bytes to read" }
  },
  "required": ["path"]
}"#;

const LIST_SCHEMA: &str = r#"{
  "type": "object",
  "properties": {
    "path": { "type": "string", "description": "directory path relative to the project root" }
  },
  "required": ["path"]
}"#;

const WRITE_SCHEMA: &str = r#"{
  "type": "object",
  "properties": {
    "path": { "type": "string" },
    "content": { "type": "string" }
  },
  "required": ["path", "content"]
}"#;

struct ReadPlugin;

impl Guest for ReadPlugin {
    fn list_tools() -> Vec<Definition> {
        vec![
            Definition {
                name: "read_file".to_owned(),
                description: "Read a file relative to the project root (bounded to 256 KiB)."
                    .to_owned(),
                input_schema: READ_SCHEMA.to_owned(),
                effect: Effect::Read,
            },
            Definition {
                name: "list_dir".to_owned(),
                description: "List directory entries relative to the project root.".to_owned(),
                input_schema: LIST_SCHEMA.to_owned(),
                effect: Effect::Read,
            },
            Definition {
                name: "write_file".to_owned(),
                description: "Write a file relative to the project root (requires a writable \
                              grant; refused under Read Only)."
                    .to_owned(),
                input_schema: WRITE_SCHEMA.to_owned(),
                effect: Effect::Write,
            },
        ]
    }

    fn call(name: String, arguments: String) -> Result<String, String> {
        match name.as_str() {
            "read_file" => read_file(&arguments),
            "list_dir" => list_dir(&arguments),
            "write_file" => write_file(&arguments),
            other => Err(format!("unknown tool `{other}`")),
        }
    }
}

/// guest 寻址（Phase 2b）：宿主把项目根 preopen 为 `project`、额外资
/// 料目录 preopen 为其目录名。规则——相对路径默认以项目根为根
///（`foo` → `project/foo`，`project/foo` 幂等）；`/<根名>/…` 显式以
/// 额外目录为根（`/mydata/x` → `mydata/x`）。
fn guest_path(relative: &str) -> PathBuf {
    let trimmed = relative.trim();
    if let Some(rooted) = trimmed.strip_prefix('/') {
        return PathBuf::from(rooted);
    }
    if trimmed == "project" {
        return PathBuf::from("project");
    }
    if let Some(rest) = trimmed.strip_prefix("project/") {
        return Path::new("project").join(rest);
    }
    Path::new("project").join(trimmed)
}

#[derive(Deserialize)]
struct ReadArgs {
    path: String,
    #[serde(default)]
    offset: Option<usize>,
    #[serde(default)]
    limit: Option<usize>,
}

fn read_file(arguments: &str) -> Result<String, String> {
    let args: ReadArgs =
        serde_json::from_str(arguments).map_err(|error| format!("invalid arguments: {error}"))?;
    let bytes = std::fs::read(guest_path(&args.path))
        .map_err(|error| format!("read `{}`: {error}", args.path))?;
    let offset = args.offset.unwrap_or(0).min(bytes.len());
    let limit = args.limit.unwrap_or(MAX_READ_BYTES).min(MAX_READ_BYTES);
    let window = &bytes[offset..(offset + limit).min(bytes.len())];
    let content = String::from_utf8_lossy(window).into_owned();
    serde_json::to_string(&serde_json::json!({
        "content": content,
        "bytes": window.len(),
        "total_bytes": bytes.len(),
        "truncated": offset + window.len() < bytes.len(),
    }))
    .map_err(|error| format!("serialize: {error}"))
}

#[derive(Deserialize)]
struct ListArgs {
    path: String,
}

fn list_dir(arguments: &str) -> Result<String, String> {
    let args: ListArgs =
        serde_json::from_str(arguments).map_err(|error| format!("invalid arguments: {error}"))?;
    let entries = std::fs::read_dir(guest_path(&args.path))
        .map_err(|error| format!("list `{}`: {error}", args.path))?;
    let mut names: Vec<(String, &str)> = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|error| format!("entry: {error}"))?;
        let kind = match entry.file_type() {
            Ok(kind) if kind.is_dir() => "dir",
            Ok(_) => "file",
            Err(_) => "other",
        };
        names.push((entry.file_name().to_string_lossy().into_owned(), kind));
    }
    names.sort();
    serde_json::to_string(&serde_json::json!({
        "entries": names.into_iter()
            .map(|(name, kind)| serde_json::json!({ "name": name, "kind": kind }))
            .collect::<Vec<_>>(),
    }))
    .map_err(|error| format!("serialize: {error}"))
}

#[derive(Deserialize)]
struct WriteArgs {
    path: String,
    content: String,
}

fn write_file(arguments: &str) -> Result<String, String> {
    let args: WriteArgs =
        serde_json::from_str(arguments).map_err(|error| format!("invalid arguments: {error}"))?;
    let path = guest_path(&args.path);
    // 只在项目根内创建/截断（授予面兜底：RO 档在 open 即被拒）。
    std::fs::write(&path, args.content.as_bytes())
        .map_err(|error| format!("write `{}`: {error}", args.path))?;
    serde_json::to_string(&serde_json::json!({ "bytes": args.content.len() }))
        .map_err(|error| format!("serialize: {error}"))
}

export!(ReadPlugin);
