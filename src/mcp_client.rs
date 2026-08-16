//! MCP 客户端会话与工具适配：支持 legacy initialize 握手和
//! 2026-07-28 modern per-request envelope，并把远端工具映射为 CLAT
//! 的 [`Tool`] trait。
//!
//! 资源上限（对抗恶意服务）：分页页数/工具数/cursor 循环/结果大小
//! 均有界，超限隔离该服务器。
//!
//! 远端 annotations 仅用于细分权限提示，永远不会把 MCP 工具降级成
//! 可自动放行的本地只读能力。

use crate::mcp::{McpError, StdioSession};
use crate::model::CancelToken;
use crate::project::Project;
use crate::tool::{Tool, ToolDefinition, ToolEffect, ToolError};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::HashSet;
use std::path::Path;
use std::time::Duration;

/// CLAT legacy 握手首选版本。
pub const PROTOCOL_VERSION: &str = "2025-11-25";
/// CLAT 支持的 modern per-request envelope 版本。
pub const MODERN_PROTOCOL_VERSION: &str = "2026-07-28";

/// CLAT 已验证可按 legacy initialize/initialized 语义处理的版本。
/// 服务器返回 modern 或未知版本时必须拒绝，不能继续发送 legacy
/// notification 冒充协商成功。
const SUPPORTED_LEGACY_VERSIONS: &[&str] =
    &["2024-11-05", "2025-03-26", "2025-06-18", "2025-11-25"];

/// 握手（initialize）超时。
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);
/// modern 探测必须短且可回退；legacy 服务可能不认识 discover。
const DISCOVER_TIMEOUT: Duration = Duration::from_secs(3);
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
    era: ProtocolEra,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ProtocolEra {
    Legacy,
    Modern,
}

/// tools/list 返回的单个工具描述。
#[derive(Clone, Debug)]
pub struct McpToolInfo {
    pub name: String,
    pub description: String,
    pub input_schema: Value,
    pub annotations: McpToolAnnotations,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct McpToolAnnotations {
    pub read_only: Option<bool>,
    pub destructive: Option<bool>,
    pub open_world: Option<bool>,
}

impl McpServer {
    /// 启动子进程（固定 `cwd`）并自动协商协议时代。先用一次性会话
    /// 探测 modern `server/discover`；失败后丢弃探测进程并用全新会话
    /// 执行 legacy initialize，避免探测帧污染旧服务器状态机。
    pub fn connect(name: &str, config: &McpServerConfig, cwd: &Path) -> Result<Self, McpError> {
        let env: Vec<(String, String)> = config
            .env
            .iter()
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect();
        let mut probe = StdioSession::spawn(&config.command, &config.args, &env, cwd)?;
        match Self::discover(&probe) {
            Ok(server_version) => Ok(Self {
                name: name.to_owned(),
                session: probe,
                server_version,
                negotiated_version: MODERN_PROTOCOL_VERSION.to_owned(),
                era: ProtocolEra::Modern,
            }),
            Err(modern_error) => {
                let _ = probe.shutdown();
                let mut session = StdioSession::spawn(&config.command, &config.args, &env, cwd)?;
                let (server_version, negotiated_version) = match Self::handshake(&session) {
                    Ok(negotiated) => negotiated,
                    Err(legacy_error) => {
                        let cleanup = session.shutdown().err();
                        let cleanup = cleanup
                            .map(|error| format!("; cleanup: {error}"))
                            .unwrap_or_default();
                        return Err(McpError::new(format!(
                            "MCP negotiation failed: modern discover: {modern_error}; legacy initialize: {legacy_error}{cleanup}"
                        )));
                    }
                };
                Ok(Self {
                    name: name.to_owned(),
                    session,
                    server_version,
                    negotiated_version,
                    era: ProtocolEra::Legacy,
                })
            }
        }
    }

    fn discover(session: &StdioSession) -> Result<String, McpError> {
        let result = session.call(
            "server/discover",
            modern_params(json!({})),
            DISCOVER_TIMEOUT,
        )?;
        validate_modern_result(&result, "server/discover")?;
        let versions = result
            .get("supportedVersions")
            .and_then(Value::as_array)
            .ok_or_else(|| McpError::new("server/discover missing supportedVersions"))?;
        if !versions
            .iter()
            .any(|version| version.as_str() == Some(MODERN_PROTOCOL_VERSION))
        {
            return Err(McpError::new(format!(
                "server/discover does not offer {MODERN_PROTOCOL_VERSION}"
            )));
        }
        if !result.get("capabilities").is_some_and(Value::is_object) {
            return Err(McpError::new("server/discover missing capabilities"));
        }
        Ok(result
            .get("_meta")
            .and_then(|meta| meta.get("io.modelcontextprotocol/serverInfo"))
            .and_then(|info| info.get("version"))
            .and_then(Value::as_str)
            .unwrap_or("unknown")
            .to_owned())
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

    pub fn shutdown(mut self) -> Result<(), McpError> {
        self.session.shutdown()
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
            let result = self.call("tools/list", params, LIST_TIMEOUT, None)?;
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
                        annotations: parse_annotations(tool.get("annotations")),
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
    fn call_tool(
        &self,
        name: &str,
        arguments: &Value,
        cancel: &CancelToken,
    ) -> Result<Value, McpError> {
        let result = self.call(
            "tools/call",
            json!({"name": name, "arguments": arguments}),
            CALL_TIMEOUT,
            Some(cancel),
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

    fn call(
        &self,
        method: &str,
        params: Value,
        timeout: Duration,
        cancel: Option<&CancelToken>,
    ) -> Result<Value, McpError> {
        let params = match self.era {
            ProtocolEra::Legacy => params,
            ProtocolEra::Modern => modern_params(params),
        };
        let result = match cancel {
            Some(cancel) => self
                .session
                .call_cancellable(method, params, timeout, cancel)?,
            None => self.session.call(method, params, timeout)?,
        };
        if self.era == ProtocolEra::Modern {
            validate_modern_result(&result, method)?;
        }
        Ok(result)
    }
}

fn modern_params(mut params: Value) -> Value {
    let object = params
        .as_object_mut()
        .expect("CLAT MCP request params are always objects");
    object.insert(
        "_meta".into(),
        json!({
            "io.modelcontextprotocol/protocolVersion": MODERN_PROTOCOL_VERSION,
            "io.modelcontextprotocol/clientCapabilities": {},
            "io.modelcontextprotocol/clientInfo": {
                "name": "clat",
                "version": env!("CARGO_PKG_VERSION"),
            }
        }),
    );
    params
}

fn validate_modern_result(result: &Value, method: &str) -> Result<(), McpError> {
    match result.get("resultType").and_then(Value::as_str) {
        Some("complete") => Ok(()),
        Some("input_required") => Err(McpError::new(format!(
            "MCP request `{method}` requires multi-round-trip input, which CLAT does not support yet"
        ))),
        Some(other) => Err(McpError::new(format!(
            "MCP request `{method}` returned unsupported resultType {other:?}"
        ))),
        None => Err(McpError::new(format!(
            "modern MCP response for `{method}` missing resultType"
        ))),
    }
}

fn parse_annotations(value: Option<&Value>) -> McpToolAnnotations {
    McpToolAnnotations {
        read_only: value
            .and_then(|value| value.get("readOnlyHint"))
            .and_then(Value::as_bool),
        destructive: value
            .and_then(|value| value.get("destructiveHint"))
            .and_then(Value::as_bool),
        open_world: value
            .and_then(|value| value.get("openWorldHint"))
            .and_then(Value::as_bool),
    }
}

fn effect_from_annotations(annotations: McpToolAnnotations) -> ToolEffect {
    let read_only = annotations.read_only.unwrap_or(false);
    let open_world = annotations.open_world.unwrap_or(true);
    if read_only {
        if open_world {
            ToolEffect::Network
        } else {
            ToolEffect::ExternalRead
        }
    } else if annotations.destructive.unwrap_or(true) {
        ToolEffect::Destructive
    } else if open_world {
        ToolEffect::Network
    } else {
        ToolEffect::Write
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
/// 注册表去重。空段返回 None（工具被跳过）。
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
    server: std::sync::Weak<McpServer>,
    server_name: String,
    info: McpToolInfo,
    qualified_name: String,
}

impl McpTool {
    pub fn new(server: &std::sync::Arc<McpServer>, info: McpToolInfo) -> Self {
        let qualified_name = qualify_tool_name(server.name(), &info.name)
            .unwrap_or_else(|| format!("mcp_unnamed_{}", info.name.len()));
        Self {
            server: std::sync::Arc::downgrade(server),
            server_name: server.name().to_owned(),
            info,
            qualified_name,
        }
    }
}

impl Tool for McpTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: self.qualified_name.clone(),
            description: format!("[mcp:{}] {}", self.server_name, self.info.description),
            input_schema: self.info.input_schema.clone(),
            effect: effect_from_annotations(self.info.annotations),
            strict: false,
        }
    }

    fn invoke(
        &self,
        arguments: &Value,
        _project: &Project,
        cancel: &CancelToken,
    ) -> Result<Value, ToolError> {
        self.server
            .upgrade()
            .ok_or_else(|| ToolError::new("MCP server is shutting down"))?
            .call_tool(&self.info.name, arguments, cancel)
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

    #[test]
    fn modern_envelope_and_tool_effects_are_conservative() {
        let params = modern_params(json!({"cursor": "next"}));
        let meta = &params["_meta"];
        assert_eq!(
            meta["io.modelcontextprotocol/protocolVersion"],
            MODERN_PROTOCOL_VERSION
        );
        assert!(meta["io.modelcontextprotocol/clientCapabilities"].is_object());
        assert_eq!(meta["io.modelcontextprotocol/clientInfo"]["name"], "clat");

        assert_eq!(
            effect_from_annotations(McpToolAnnotations {
                read_only: Some(true),
                destructive: None,
                open_world: Some(false),
            }),
            ToolEffect::ExternalRead
        );
        assert_eq!(
            effect_from_annotations(McpToolAnnotations {
                read_only: Some(true),
                destructive: None,
                open_world: None,
            }),
            ToolEffect::Network
        );
        assert_eq!(
            effect_from_annotations(McpToolAnnotations {
                read_only: Some(false),
                destructive: Some(false),
                open_world: Some(false),
            }),
            ToolEffect::Write
        );
        // Missing annotations use the MCP defaults and remain destructive;
        // no remote hint can produce the auto-allowed native Read effect.
        assert_eq!(
            effect_from_annotations(McpToolAnnotations::default()),
            ToolEffect::Destructive
        );
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
            .call_tool("echo", &json!({"text": "hello"}), &CancelToken::new())
            .expect("call");
        assert_eq!(output, json!("echo: hello"));
    }

    /// 严格 modern 链路：discover → tools/list → tools/call；所有请求
    /// 必须携带 2026-07-28 的 per-request `_meta`，且不发送 initialize。
    #[test]
    #[ignore = "spawns a python3 subprocess; run explicitly with --ignored"]
    fn end_to_end_modern_discover_list_and_call() {
        let script = r#"
import json, sys
def send(obj):
    sys.stdout.write(json.dumps(obj) + "\n")
    sys.stdout.flush()
stage = 0
for line in sys.stdin:
    msg = json.loads(line)
    if "id" not in msg:
        continue
    method = msg.get("method", "")
    meta = msg.get("params", {}).get("_meta", {})
    valid_meta = (
        meta.get("io.modelcontextprotocol/protocolVersion") == "2026-07-28" and
        isinstance(meta.get("io.modelcontextprotocol/clientCapabilities"), dict) and
        meta.get("io.modelcontextprotocol/clientInfo", {}).get("name") == "clat")
    if not valid_meta:
        send({"jsonrpc": "2.0", "id": msg["id"],
              "error": {"code": -32602, "message": "missing modern envelope"}})
    elif method == "server/discover" and stage == 0:
        stage = 1
        send({"jsonrpc": "2.0", "id": msg["id"], "result": {
            "resultType": "complete",
            "supportedVersions": ["2026-07-28"],
            "capabilities": {"tools": {}},
            "_meta": {"io.modelcontextprotocol/serverInfo": {
                "name": "strict-modern", "version": "2.0"}}}})
    elif method == "tools/list" and stage == 1:
        stage = 2
        send({"jsonrpc": "2.0", "id": msg["id"], "result": {
            "resultType": "complete", "tools": [{
                "name": "echo", "description": "echoes text",
                "annotations": {"readOnlyHint": True, "openWorldHint": False},
                "inputSchema": {"type": "object"}}]}})
    elif method == "tools/call" and stage == 2:
        stage = 3
        text = msg["params"]["arguments"].get("text", "")
        send({"jsonrpc": "2.0", "id": msg["id"], "result": {
            "resultType": "complete",
            "content": [{"type": "text", "text": "modern: " + text}]}})
    else:
        send({"jsonrpc": "2.0", "id": msg["id"],
              "error": {"code": -32600, "message": "wrong method order"}})
"#;
        let config = McpServerConfig {
            command: "python3".into(),
            args: vec!["-c".into(), script.into()],
            ..Default::default()
        };
        let server = McpServer::connect("v2", &config, Path::new("/tmp")).expect("modern");
        assert_eq!(server.negotiated_version(), MODERN_PROTOCOL_VERSION);
        assert_eq!(server.server_version(), "2.0");
        let tools = server.list_tools().expect("tools");
        assert_eq!(tools.len(), 1);
        assert_eq!(
            effect_from_annotations(tools[0].annotations),
            ToolEffect::ExternalRead
        );
        let output = server
            .call_tool("echo", &json!({"text": "hello"}), &CancelToken::new())
            .expect("call");
        assert_eq!(output, json!("modern: hello"));
    }
}
