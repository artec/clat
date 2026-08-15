//! MCP 客户端会话与工具适配：initialize 握手、tools/list 发现、
//! tools/call 调用，并把远端工具映射为 CLAT 的 [`Tool`] trait。
//!
//! 协议姿态：**只支持 legacy 握手时代**（initialize/initialized，
//! 2024-11-05 … 2025-11-25）。MCP 2.0（2026-07-28）移除了握手并要求
//! 每请求 `_meta` 自描述——在实现 modern envelope 之前，握手失败
//! 一律如实报错，绝不假装协商成功。
//!
//! 资源上限（对抗恶意服务）：分页页数/工具数/cursor 循环/结果大小
//! 均有界，超限隔离该服务器。
//!
//! effect 固定为 [`ToolEffect::Execute`]——远端能力不可静态分类，
//! 一律过权限询问（安全默认）。

use crate::mcp::{McpError, StdioSession};
use crate::project::Project;
use crate::tool::{Tool, ToolDefinition, ToolEffect, ToolError};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::HashSet;
use std::path::Path;
use std::time::Duration;

/// CLAT 使用的协议版本（legacy 握手时代的最新规范）。服务器不支持时
/// 会返回它自己的版本；只有下方白名单中的 legacy 版本会被接受。
pub const PROTOCOL_VERSION: &str = "2025-11-25";

/// CLAT 已验证可按 legacy initialize/initialized 语义处理的版本。
/// 服务器返回 modern 或未知版本时必须拒绝，不能继续发送 legacy
/// notification 冒充协商成功。
const SUPPORTED_LEGACY_VERSIONS: &[&str] =
    &["2024-11-05", "2025-03-26", "2025-06-18", "2025-11-25"];

/// 握手（initialize）超时。
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);
/// tools/list 单页超时。
const LIST_TIMEOUT: Duration = Duration::from_secs(30);
/// tools/call 超时。工具可能合法地运行较久（构建、批处理），
/// 给足余量但必须有界。
const CALL_TIMEOUT: Duration = Duration::from_secs(120);

/// tools/list 分页上限：超过视为服务违约（循环分页或攻击）。
const MAX_LIST_PAGES: usize = 32;
/// 单服务器工具数上限。
const MAX_TOOLS: usize = 512;
/// 单次 tools/call 拼接文本的字节上限。
const MAX_RESULT_BYTES: usize = 1024 * 1024;

/// `~/.clat/mcp.json` 中一个 server 的配置。
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct McpServerConfig {
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub env: std::collections::BTreeMap<String, String>,
}

/// 整个 mcp.json 文档：server 名 → 配置。
pub type McpConfig = std::collections::BTreeMap<String, McpServerConfig>;

/// 一个已握手的服务器会话，持有子进程直到 Drop。
pub struct McpServer {
    name: String,
    session: StdioSession,
    server_version: String,
    /// 握手协商出的协议版本（服务器回显或其支持的最新版）。
    negotiated_version: String,
}

/// tools/list 返回的单个工具描述。
#[derive(Clone, Debug)]
pub struct McpToolInfo {
    pub name: String,
    pub description: String,
    pub input_schema: Value,
}

impl McpServer {
    /// 启动子进程（固定 `cwd`）并完成 legacy initialize 握手。
    pub fn connect(name: &str, config: &McpServerConfig, cwd: &Path) -> Result<Self, McpError> {
        let env: Vec<(String, String)> = config
            .env
            .iter()
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect();
        let session = StdioSession::spawn(&config.command, &config.args, &env, cwd)?;
        let (server_version, negotiated_version) = Self::handshake(&session).map_err(|error| {
            McpError::new(format!(
                "legacy initialize failed ({error}); CLAT 尚未支持无握手的 MCP 2.0"
            ))
        })?;
        Ok(Self {
            name: name.to_owned(),
            session,
            server_version,
            negotiated_version,
        })
    }

    /// legacy initialize 握手：返回 (serverInfo.version, 协商出的协议版本)。
    fn handshake(session: &StdioSession) -> Result<(String, String), McpError> {
        let result = session.call(
            "initialize",
            json!({
                "protocolVersion": PROTOCOL_VERSION,
                "capabilities": {},
                "clientInfo": {
                    "name": "clat",
                    "version": env!("CARGO_PKG_VERSION"),
                },
            }),
            HANDSHAKE_TIMEOUT,
        )?;
        let server_version = result
            .pointer("/serverInfo/version")
            .and_then(Value::as_str)
            .unwrap_or("unknown")
            .to_owned();
        // 版本协商：服务器回显则一致；否则返回它支持的版本。只接受
        // 明确验证过的 legacy 版本；modern/未知版本不能套用旧信封。
        let negotiated_version = result
            .get("protocolVersion")
            .and_then(Value::as_str)
            .ok_or_else(|| McpError::new("initialize response missing protocolVersion"))?
            .to_owned();
        validate_legacy_version(&negotiated_version)?;
        // 握手收尾：规范要求客户端发送 initialized notification。
        session.notify("notifications/initialized", json!({}))?;
        Ok((server_version, negotiated_version))
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn server_version(&self) -> &str {
        &self.server_version
    }

    /// 握手协商出的协议版本，供将来按版本启用特性开关。
    pub fn negotiated_version(&self) -> &str {
        &self.negotiated_version
    }

    /// 列出远端工具（含 cursor 分页），映射为 CLAT 工具定义。
    /// 页数、工具数、cursor 循环均有界。
    pub fn list_tools(&self) -> Result<Vec<McpToolInfo>, McpError> {
        let mut tools = Vec::new();
        let mut seen_cursors: HashSet<String> = HashSet::new();
        let mut cursor: Option<String> = None;
        for _ in 0..MAX_LIST_PAGES {
            let params = match &cursor {
                Some(cursor) => json!({"cursor": cursor}),
                None => json!({}),
            };
            let result = self.session.call("tools/list", params, LIST_TIMEOUT)?;
            if let Some(list) = result.get("tools").and_then(Value::as_array) {
                for tool in list {
                    let name = tool
                        .get("name")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_owned();
                    if name.is_empty() {
                        continue;
                    }
                    if tools.len() >= MAX_TOOLS {
                        return Err(McpError::new(format!(
                            "server `{}` exposes more than {MAX_TOOLS} tools",
                            self.name
                        )));
                    }
                    tools.push(McpToolInfo {
                        description: tool
                            .get("description")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_owned(),
                        input_schema: tool
                            .get("inputSchema")
                            .cloned()
                            .unwrap_or_else(|| json!({"type": "object"})),
                        name,
                    });
                }
            }
            cursor = result
                .get("nextCursor")
                .and_then(Value::as_str)
                .map(str::to_owned);
            match cursor {
                None => return Ok(tools),
                // 重复 cursor = 服务端分页循环，违约即停。
                Some(ref cursor) if !seen_cursors.insert(cursor.clone()) => {
                    return Err(McpError::new(format!(
                        "server `{}` repeats a pagination cursor",
                        self.name
                    )));
                }
                Some(_) => {}
            }
        }
        Err(McpError::new(format!(
            "server `{}` exceeds {MAX_LIST_PAGES} pagination pages",
            self.name
        )))
    }

    /// 调用远端工具，返回 content 块拼接的文本（CLAT 的输出模型）。
    fn call_tool(&self, name: &str, arguments: &Value) -> Result<Value, McpError> {
        let result = self.session.call(
            "tools/call",
            json!({"name": name, "arguments": arguments}),
            CALL_TIMEOUT,
        )?;
        let is_error = result
            .get("isError")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let mut text = String::new();
        if let Some(content) = result.get("content").and_then(Value::as_array) {
            for block in content {
                let is_text = block.get("type").and_then(Value::as_str) == Some("text");
                let Some(chunk) = block.get("text").and_then(Value::as_str) else {
                    continue;
                };
                if is_text {
                    if !text.is_empty() {
                        text.push('\n');
                    }
                    text.push_str(chunk);
                    if text.len() > MAX_RESULT_BYTES {
                        return Err(McpError::new(format!(
                            "MCP tool `{name}` result exceeds {} bytes",
                            MAX_RESULT_BYTES
                        )));
                    }
                }
            }
        }
        if is_error {
            return Err(McpError::new(if text.is_empty() {
                format!("MCP tool `{name}` failed")
            } else {
                text
            }));
        }
        // 非 text 块（图片/资源等）暂不支持：以占位说明保持输出诚实。
        if text.is_empty() {
            text = format!("(MCP tool `{name}` returned no text content)");
        }
        Ok(Value::String(text))
    }
}

fn validate_legacy_version(version: &str) -> Result<(), McpError> {
    if SUPPORTED_LEGACY_VERSIONS.contains(&version) {
        return Ok(());
    }
    Err(McpError::new(format!(
        "server selected unsupported protocol version {version:?}; supported legacy versions: {}",
        SUPPORTED_LEGACY_VERSIONS.join(", ")
    )))
}

/// 把一个名字段清洗为合法的工具名段：仅保留 [a-zA-Z0-9_]，其余
/// 字符（含 `-`/`.`/空格/斜杠/控制字符）统一替换为 `_`，首尾非
/// 字母数字剥离。保守字符集对所有模型供应商安全，清洗产生的
/// 撞名（`a-b` vs `a.b`）由注册表去重兜底。
fn sanitize_segment(segment: &str) -> String {
    segment
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect::<String>()
        .trim_matches(|c: char| !c.is_ascii_alphanumeric())
        .to_owned()
}

/// MCP 工具映射到 CLAT 的全名：`mcp_{server}_{tool}`。server 与 tool
/// 两段都清洗；总长超过 64 截断为 56 + 稳定短哈希，保证唯一性靠
/// 注册表去重（见 [`register_mcp_tools`]）。空段返回 None（工具被跳过）。
pub fn qualify_tool_name(server: &str, tool: &str) -> Option<String> {
    let server = sanitize_segment(server);
    let tool = sanitize_segment(tool);
    if server.is_empty() || tool.is_empty() {
        return None;
    }
    let qualified = format!("mcp_{server}_{tool}");
    if qualified.len() <= 64 {
        return Some(qualified);
    }
    // 截断 + FNV-1a 短哈希后缀：长名保持确定性且基本不碰撞。
    // 55 + 1 + 8 = 恰好 64。
    let hash = qualified.bytes().fold(0xcbf29ce484222325u64, |acc, byte| {
        (acc ^ byte as u64).wrapping_mul(0x100000001b3)
    });
    let stem = qualified[..55].to_owned();
    Some(format!("{stem}_{:08x}", hash & 0xffff_ffff))
}

/// 把一个远端 MCP 工具适配为 CLAT 工具。
pub struct McpTool {
    server: std::sync::Arc<McpServer>,
    info: McpToolInfo,
    qualified_name: String,
}

impl McpTool {
    pub fn new(server: std::sync::Arc<McpServer>, info: McpToolInfo) -> Self {
        let qualified_name = qualify_tool_name(server.name(), &info.name)
            .unwrap_or_else(|| format!("mcp_unnamed_{}", info.name.len()));
        Self {
            server,
            info,
            qualified_name,
        }
    }
}

impl Tool for McpTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: self.qualified_name.clone(),
            description: format!("[mcp:{}] {}", self.server.name(), self.info.description),
            input_schema: self.info.input_schema.clone(),
            effect: ToolEffect::Execute,
            strict: false,
        }
    }

    fn invoke(&self, arguments: &Value, _project: &Project) -> Result<Value, ToolError> {
        self.server
            .call_tool(&self.info.name, arguments)
            .map_err(|error| ToolError::new(error.to_string()))
    }
}

/// 解析 mcp.json 文档。
pub fn parse_mcp_config(text: &str) -> Result<McpConfig, String> {
    if text.trim().is_empty() {
        return Ok(McpConfig::default());
    }
    serde_json::from_str(text).map_err(|error| format!("invalid mcp.json: {error}"))
}

/// 从 CLAT 存储根（如 `~/.clat`）读取 mcp.json；文件不存在视为空
/// 配置（MCP 是可选能力）。
pub fn load_mcp_config(root: &Path) -> Result<McpConfig, String> {
    let path = root.join("mcp.json");
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(McpConfig::default());
        }
        Err(error) => return Err(format!("read {}: {error}", path.display())),
    };
    parse_mcp_config(&text)
}

/// 按 mcp.json 连接全部服务器并把工具注册进注册表。
///
/// - 子进程以 `cwd`（调用方传入 `~/.clat`）为固定工作目录；
/// - 同名工具（跨服务器清洗后碰撞，或单服务器内 `a-b`/`a.b` 重名）
///   只注册第一个，其余记入失败提示——绝不静默路由到错误实现；
/// - 单个服务器失败只跳过并收集为提示信息，不拖垮 CLAT 启动。
///
/// 返回 (成功的服务器数, 失败提示)。
pub fn register_mcp_tools(
    registry: &mut crate::tool::ToolRegistry,
    config: &McpConfig,
    cwd: &Path,
) -> (usize, Vec<String>) {
    let mut connected = 0;
    let mut failures = Vec::new();
    let mut seen_names: HashSet<String> = HashSet::new();
    for (name, server_config) in config {
        if server_config.command.trim().is_empty() {
            failures.push(format!("mcp `{name}`: empty command"));
            continue;
        }
        match McpServer::connect(name, server_config, cwd) {
            Ok(server) => match server.list_tools() {
                Ok(tools) => {
                    let server = std::sync::Arc::new(server);
                    for info in tools {
                        let tool = McpTool::new(server.clone(), info);
                        let qualified = tool.definition().name;
                        if !seen_names.insert(qualified.clone()) {
                            failures.push(format!(
                                "mcp `{name}`: tool `{}` collides with an existing tool, skipped",
                                tool.definition().name
                            ));
                            continue;
                        }
                        registry.register(tool);
                    }
                    connected += 1;
                }
                Err(error) => failures.push(format!("mcp `{name}`: {error}")),
            },
            Err(error) => failures.push(format!("mcp `{name}`: {error}")),
        }
    }
    (connected, failures)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_mcp_config_documents() {
        let config = parse_mcp_config(
            r#"{
                "filesystem": {
                    "command": "npx",
                    "args": ["-y", "@modelcontextprotocol/server-filesystem", "/tmp"]
                },
                "memory": {
                    "command": "mcp-memory",
                    "env": {"STORE": "/data"}
                }
            }"#,
        )
        .expect("config");
        assert_eq!(config.len(), 2);
        assert_eq!(config["filesystem"].command, "npx");
        assert_eq!(config["filesystem"].args.len(), 3);
        assert!(config["filesystem"].env.is_empty());
        assert_eq!(config["memory"].env["STORE"], "/data");

        // 空文档与坏文档。
        assert!(parse_mcp_config("").unwrap().is_empty());
        assert!(parse_mcp_config("{ nope").is_err());
    }

    #[test]
    fn rejects_modern_and_unknown_protocol_versions() {
        for version in ["2026-07-28", "2099-01-01", "draft"] {
            let error = validate_legacy_version(version).unwrap_err();
            assert!(error.to_string().contains("unsupported protocol version"));
        }
        for version in SUPPORTED_LEGACY_VERSIONS {
            validate_legacy_version(version).expect("known legacy version");
        }
    }

    /// 工具名规则（A-08）：白名单清洗、非法字符归一、空段拒绝、
    /// 长名截断加哈希、易碰撞对保留可区分性。
    #[test]
    fn tool_names_are_sanitized_deduplicatable_and_bounded() {
        assert_eq!(
            qualify_tool_name("fs", "read-file").as_deref(),
            Some("mcp_fs_read_file")
        );
        // 非法字符（空格、斜杠、控制字符）统一替换为 _。
        assert_eq!(
            qualify_tool_name("my server", "a b").as_deref(),
            Some("mcp_my_server_a_b")
        );
        // 前导/尾随非字母数字被剥离；剥空的段拒绝注册。
        assert_eq!(
            qualify_tool_name("--fs--", "tool").as_deref(),
            Some("mcp_fs_tool")
        );
        assert_eq!(qualify_tool_name("///", "tool"), None);
        assert_eq!(qualify_tool_name("fs", ""), None);
        // 长名截断到 64 内且保持确定性。
        let long = qualify_tool_name("s", &"x".repeat(200)).unwrap();
        assert!(long.len() <= 64, "{}", long.len());
        assert_eq!(long, qualify_tool_name("s", &"x".repeat(200)).unwrap());
        // `a-b` 与 `a.b` 清洗后同名——由注册表去重兜底（见 register）。
        assert_eq!(
            qualify_tool_name("fs", "a-b"),
            qualify_tool_name("fs", "a.b")
        );
    }

    /// 端到端 legacy 链路：initialize 握手（版本协商）→ tools/list →
    /// tools/call。需 python3，`cargo test -- --ignored` 显式跑。
    #[test]
    #[ignore = "spawns a python3 subprocess; run explicitly with --ignored"]
    fn end_to_end_handshake_list_and_call() {
        let script = r#"
import json, sys
def send(obj):
    sys.stdout.write(json.dumps(obj) + "\n")
    sys.stdout.flush()
for line in sys.stdin:
    msg = json.loads(line)
    if "id" not in msg:
        continue
    method = msg.get("method", "")
    if method == "initialize":
        send({"jsonrpc": "2.0", "id": msg["id"], "result": {
            "protocolVersion": "2025-06-18",
            "capabilities": {"tools": {}},
            "serverInfo": {"name": "echo", "version": "1.0"}}})
    elif method == "tools/list":
        send({"jsonrpc": "2.0", "id": msg["id"], "result": {"tools": [
            {"name": "echo", "description": "echoes text",
             "inputSchema": {"type": "object", "properties": {"text": {"type": "string"}}}}]}})
    elif method == "tools/call":
        text = msg["params"]["arguments"].get("text", "")
        send({"jsonrpc": "2.0", "id": msg["id"], "result": {
            "content": [{"type": "text", "text": "echo: " + text}]}})
    else:
        send({"jsonrpc": "2.0", "id": msg["id"], "result": {}})
"#;
        let config = McpServerConfig {
            command: "python3".into(),
            args: vec!["-c".into(), script.into()],
            ..Default::default()
        };
        let server = McpServer::connect("echo", &config, Path::new("/tmp")).expect("handshake");
        assert_eq!(server.server_version(), "1.0");
        // 版本协商：服务器只支持旧版时返回其版本，客户端接受并继续。
        assert_eq!(server.negotiated_version(), "2025-06-18");

        let tools = server.list_tools().expect("tools");
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].name, "echo");

        let output = server
            .call_tool("echo", &json!({"text": "hello"}))
            .expect("call");
        assert_eq!(output, json!("echo: hello"));
    }

    /// 对 initialize 报 method not found 的服务（MCP 2.0 形态）：
    /// 必须如实报错并说明 CLAT 尚不支持，绝不静默"协商成功"。
    #[test]
    #[ignore = "spawns a python3 subprocess; run explicitly with --ignored"]
    fn handshake_rejection_is_reported_not_masked() {
        let script = r#"
import json, sys
def send(obj):
    sys.stdout.write(json.dumps(obj) + "\n")
    sys.stdout.flush()
for line in sys.stdin:
    msg = json.loads(line)
    if "id" not in msg:
        continue
    if msg.get("method", "") == "initialize":
        send({"jsonrpc": "2.0", "id": msg["id"],
              "error": {"code": -32601, "message": "method not found: initialize"}})
    else:
        send({"jsonrpc": "2.0", "id": msg["id"], "result": {"tools": []}})
"#;
        let config = McpServerConfig {
            command: "python3".into(),
            args: vec!["-c".into(), script.into()],
            ..Default::default()
        };
        let outcome = McpServer::connect("v2", &config, Path::new("/tmp"));
        let error = outcome.err().expect("must fail honestly");
        assert!(error.to_string().contains("MCP 2.0"), "{error}");
    }
}
