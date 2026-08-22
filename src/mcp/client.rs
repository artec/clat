//! MCP 客户端会话与工具适配：支持 legacy initialize 握手和
//! 2026-07-28 modern per-request envelope，并把远端工具映射为 CLAT
//! 的 [`Tool`] trait。
//!
//! 资源上限（对抗恶意服务）：分页页数/工具数/cursor 循环/结果大小
//! 均有界，超限隔离该服务器。
//!
//! 远端 annotations 仅用于细分权限提示，永远不会把 MCP 工具降级成
//! 可自动放行的本地只读能力。

use super::transport::{
    McpError, ServerRequest, ServerRequestSink, StdioSession, WriterRequest, response_frame,
};
use crate::application::{EXIT_JOIN_GRACE, join_with_grace};
use crate::model::CancelToken;
use crate::project::Project;
use crate::tool::{Tool, ToolDefinition, ToolEffect, ToolError};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::HashSet;
use std::path::Path;
use std::sync::Arc;
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
/// W1-16（A2）：单个 server 的 tools/list **总时长帽**——此前逐页
/// 30s×32 页最坏 16 分钟；帽内剩余时间作为每页超时，耗尽即协议违约。
const LIST_TOTAL_BUDGET: Duration = Duration::from_secs(30);
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
/// 服务端请求投递通道容量（reader → dispatcher）：洪泛即丢弃并记
/// 诊断，绝不反压 reader（INV-S3）。
const SERVER_REQUEST_QUEUE: usize = 16;

/// 宿主对"服务端发起请求"（sampling/elicitation）的处理端口。传输
/// 层只负责投递与响应回写；语义（权限门、usage 记账、用户问答）由
/// 实现持有（见 `plugin_host.rs`——传输无关，将来 WASM/WIT 复用）。
pub trait McpServerRequestHandler: Send + Sync {
    /// 处理一个服务端请求：返回结果值，或 JSON-RPC 错误（code, message）。
    fn handle(&self, method: &str, params: Value) -> Result<Value, (i64, String)>;
    /// 在途请求数（>0 时 tools/call 截止延展，INV-S7）。
    fn pending_requests(&self) -> usize {
        0
    }
}

/// dispatcher 回写服务端请求响应的端点：stdio 复用会话 writer 队列，
/// HTTP 独立 POST。放 `Arc<Mutex<Option<…>>>` 里与 shutdown 共享：
/// 关停时先摘除（None），dispatcher 的收尾响应不再占用 writer，
/// session.shutdown 的 writer join 不会被在途响应卡住。
enum Responder {
    Stdio {
        writer: std::sync::mpsc::SyncSender<WriterRequest>,
    },
    Http {
        agent: ureq::Agent,
        url: String,
        headers: Vec<(String, String)>,
        session_id: std::sync::Arc<std::sync::Mutex<Option<String>>>,
    },
}

impl Responder {
    /// 尽力回写一条响应。stdio 侧的有界队列阻塞由 writer 持续排空
    /// 兜底（stdin 已死则通道断开、立即失败）；HTTP 侧失败静默——
    /// 服务器侧超时兜底。
    fn respond(&self, id: &Value, outcome: &Result<Value, (i64, String)>) {
        let frame = response_frame(id, outcome);
        match self {
            Self::Stdio { writer } => {
                let (result, _ignored) = std::sync::mpsc::channel();
                let _ = writer.send(WriterRequest { frame, result });
            }
            Self::Http {
                agent,
                url,
                headers,
                session_id,
            } => {
                let mut request = agent
                    .post(url)
                    .header("Content-Type", "application/json")
                    .header("Accept", "application/json, text/event-stream");
                for (name, value) in headers {
                    request = request.header(name.as_str(), value.as_str());
                }
                if let Some(session_id) = session_id.lock().ok().and_then(|guard| guard.clone()) {
                    request = request.header("Mcp-Session-Id", session_id.as_str());
                }
                let request = request
                    .config()
                    .timeout_global(Some(crate::mcp::transport::NOTIFY_TIMEOUT))
                    .build();
                let _ = request.send(frame.trim_end());
            }
        }
    }
}

fn respond_via(
    responder: &Arc<std::sync::Mutex<Option<Responder>>>,
    id: &Value,
    outcome: &Result<Value, (i64, String)>,
) {
    if let Ok(guard) = responder.lock()
        && let Some(responder) = guard.as_ref()
    {
        responder.respond(id, outcome);
    }
}

/// dispatcher 线程的持有柄：shutdown 序（responder 摘除 → sink 关闭 →
/// 有界 join）的执行者。
struct DispatcherGuard {
    responder: Arc<std::sync::Mutex<Option<Responder>>>,
    join: Option<std::thread::JoinHandle<()>>,
}

impl DispatcherGuard {
    fn stop(&mut self, session: &Session) {
        if let Ok(mut guard) = self.responder.lock() {
            *guard = None;
        }
        session.close_server_requests();
        if let Some(join) = self.join.take() {
            // 有界放弃：卡在等人/等模型上的 dispatcher 由通道断开兜底
            // 退出，不阻塞会话关停。
            let _ = join_with_grace(join, EXIT_JOIN_GRACE, "MCP dispatcher");
        }
    }
}

/// `~/.clat/mcp.json` 中一个 server 的配置。两种传输二选一：
/// - **stdio**（默认）：`command`/`args`/`env` 启动本地子进程；
/// - **远程 Streamable HTTP**（2026-08-19）：`url` + `headers`（如
///   `Authorization: Bearer …`），无子进程。`url` 存在时以其为准。
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct McpServerConfig {
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub headers: std::collections::BTreeMap<String, String>,
    #[serde(default)]
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub env: std::collections::BTreeMap<String, String>,
}

impl McpServerConfig {
    /// 是否为远程 HTTP 传输条目。
    pub fn is_http(&self) -> bool {
        self.url
            .as_deref()
            .is_some_and(|url| !url.trim().is_empty())
    }
}

/// 整个 mcp.json 文档：server 名 → 配置。
pub type McpConfig = std::collections::BTreeMap<String, McpServerConfig>;

/// 一个已握手的服务器会话；stdio 条目持有子进程直到 Drop，HTTP 条目
/// 只持端点与会话 id。
pub struct McpServer {
    name: String,
    session: Session,
    server_version: String,
    /// 握手协商出的协议版本（服务器回显或其支持的最新版）。
    negotiated_version: String,
    era: ProtocolEra,
    /// 服务端请求处理端（sampling/elicitation）；None = 本连接不受理。
    handler: Option<Arc<dyn McpServerRequestHandler>>,
    /// 服务端请求 dispatcher 线程（有 handler 才存在）。
    dispatcher: Option<DispatcherGuard>,
}

/// 建会话所需的传输参数（connect 期克隆两次：探测 + 正式，避免探测
/// 帧污染旧服务器状态机——stdio 侧的既有先例）。
enum Transport {
    Stdio {
        command: String,
        args: Vec<String>,
        env: Vec<(String, String)>,
    },
    Http {
        url: String,
        headers: Vec<(String, String)>,
    },
}

enum Session {
    Stdio(StdioSession),
    Http(super::transport::HttpSession),
}

impl Session {
    fn call(&self, method: &str, params: Value, timeout: Duration) -> Result<Value, McpError> {
        match self {
            Self::Stdio(session) => session.call(method, params, timeout),
            Self::Http(session) => session.call(method, params, timeout),
        }
    }

    fn notify(&self, method: &str, params: Value) -> Result<(), McpError> {
        match self {
            Self::Stdio(session) => session.notify(method, params),
            // HTTP 通知：POST 一条无 id 消息，2xx 即算送达；服务器对
            // 通知不回响应体，非 2xx 报错。
            Self::Http(session) => session.notify(method, params),
        }
    }

    /// 诊断尾缓冲（stdio：服务器 stderr + 会话异常；HTTP：空）。
    fn stderr_tail(&self) -> Vec<String> {
        match self {
            Self::Stdio(session) => session.stderr_tail(),
            Self::Http(_) => Vec::new(),
        }
    }

    fn call_cancellable(
        &self,
        method: &str,
        params: Value,
        timeout: Duration,
        cancel: &CancelToken,
    ) -> Result<Value, McpError> {
        match self {
            Self::Stdio(session) => session.call_cancellable(method, params, timeout, cancel),
            // HTTP 取消只在请求边界生效：发送前检查令牌，在途请求由
            // 截止兜底（见 HttpSession 文档）。
            Self::Http(session) => {
                if cancel.is_cancelled() {
                    return Err(McpError::new(format!("MCP request `{method}` cancelled")));
                }
                session.call(method, params, timeout)
            }
        }
    }

    /// 可延展调用（INV-S7）：stdio 走 call_extensible；HTTP 的单次
    /// POST 无法中途延展截止，回落普通调用（v1 已知限制，stdio 是
    /// sampling/elicitation 的参照路径）。
    fn call_cancellable_extensible(
        &self,
        method: &str,
        params: Value,
        timeout: Duration,
        cancel: &CancelToken,
        extend: Option<Arc<dyn Fn() -> bool + Send + Sync>>,
    ) -> Result<Value, McpError> {
        match self {
            Self::Stdio(session) => {
                session.call_extensible(method, params, timeout, cancel, extend)
            }
            Self::Http(_) => self.call_cancellable(method, params, timeout, cancel),
        }
    }

    fn install_server_requests(&self, sink: ServerRequestSink) {
        match self {
            Self::Stdio(session) => session.install_server_requests(sink),
            Self::Http(session) => session.install_server_requests(sink),
        }
    }

    fn close_server_requests(&self) {
        match self {
            Self::Stdio(session) => session.close_server_requests(),
            Self::Http(session) => session.close_server_requests(),
        }
    }

    fn responder(&self) -> Option<Responder> {
        match self {
            Self::Stdio(session) => session
                .writer_sender()
                .map(|writer| Responder::Stdio { writer }),
            Self::Http(session) => {
                let parts = session.responder_parts();
                Some(Responder::Http {
                    agent: parts.agent,
                    url: parts.url,
                    headers: parts.headers,
                    session_id: parts.session_id,
                })
            }
        }
    }
}

/// 把诊断尾缓冲格式化为错误消息后缀（最近几行；空缓冲零输出）。
pub(crate) fn format_stderr_tail_public(tail: &[String]) -> String {
    format_stderr_tail(tail)
}

fn format_stderr_tail(tail: &[String]) -> String {
    if tail.is_empty() {
        return String::new();
    }
    let recent: Vec<String> = tail
        .iter()
        .rev()
        .take(4)
        .map(|line| crate::redact::redact_secrets(line))
        .collect();
    format!("; server stderr: {}", recent.join(" | "))
}

/// 关停一个会话：stdio 走优雅退出流程，HTTP 无子进程、无需清理。
fn shutdown_session(session: Session) -> Result<(), McpError> {
    match session {
        Session::Stdio(mut session) => session.shutdown(),
        Session::Http(_) => Ok(()),
    }
}

impl Transport {
    fn new_session(&self, cwd: &Path) -> Result<Session, McpError> {
        match self {
            Self::Stdio { command, args, env } => Ok(Session::Stdio(StdioSession::spawn(
                command, args, env, cwd,
            )?)),
            Self::Http { url, headers } => Ok(Session::Http(
                super::transport::HttpSession::connect(url, headers)?,
            )),
        }
    }
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
    /// 建立会话（stdio = 子进程、http = 端点）并自动协商协议时代。先
    /// 用一次性会话探测 modern `server/discover`；失败后丢弃探测会话
    /// 并用全新会话执行 legacy initialize，避免探测帧污染旧服务器
    /// 状态机（stdio 侧的既有先例；HTTP 会话无状态，同样流程无害）。
    /// `server_requests` 存在时为最终会话装上服务端请求 dispatcher
    /// （sampling/elicitation；probe 会话永不受理）。
    pub fn connect(
        name: &str,
        config: &McpServerConfig,
        cwd: &Path,
        server_requests: Option<Arc<dyn McpServerRequestHandler>>,
    ) -> Result<Self, McpError> {
        let transport = if config.is_http() {
            Transport::Http {
                url: config.url.clone().unwrap_or_default(),
                headers: config
                    .headers
                    .iter()
                    .map(|(key, value)| (key.clone(), value.clone()))
                    .collect(),
            }
        } else {
            Transport::Stdio {
                command: config.command.clone(),
                args: config.args.clone(),
                env: config
                    .env
                    .iter()
                    .map(|(key, value)| (key.clone(), value.clone()))
                    .collect(),
            }
        };
        let probe = transport.new_session(cwd)?;
        let mut server = match Self::discover(&probe) {
            Ok(server_version) => Self {
                name: name.to_owned(),
                session: probe,
                server_version,
                negotiated_version: MODERN_PROTOCOL_VERSION.to_owned(),
                era: ProtocolEra::Modern,
                handler: None,
                dispatcher: None,
            },
            Err(modern_error) => {
                let _ = shutdown_session(probe);
                let session = transport.new_session(cwd)?;
                let (server_version, negotiated_version) = match Self::handshake(&session) {
                    Ok(negotiated) => negotiated,
                    Err(legacy_error) => {
                        let tail = session.stderr_tail();
                        let cleanup = shutdown_session(session).err();
                        let cleanup = cleanup
                            .map(|error| format!("; cleanup: {error}"))
                            .unwrap_or_default();
                        return Err(McpError::new(format!(
                            "MCP negotiation failed: modern discover: {modern_error}; legacy initialize: {legacy_error}{cleanup}{}",
                            format_stderr_tail(&tail)
                        )));
                    }
                };
                Self {
                    name: name.to_owned(),
                    session,
                    server_version,
                    negotiated_version,
                    era: ProtocolEra::Legacy,
                    handler: None,
                    dispatcher: None,
                }
            }
        };
        if let Some(handler) = server_requests {
            server.start_dispatcher(handler)?;
        }
        Ok(server)
    }

    /// 服务端请求 dispatcher：reader/HTTP 响应流只投递（INV-S3），
    /// 本线程串行处理并回写响应。`ping` 直接回空结果；未知方法回
    /// -32601（INV-S4）；handler panic 隔离为 -32603；elicitation 按
    /// 协商版本门控（< 2025-06-18 回 -32601）。
    fn start_dispatcher(
        &mut self,
        handler: Arc<dyn McpServerRequestHandler>,
    ) -> Result<(), McpError> {
        let (sink, receiver) = std::sync::mpsc::sync_channel::<ServerRequest>(SERVER_REQUEST_QUEUE);
        self.session.install_server_requests(sink);
        let responder = Arc::new(std::sync::Mutex::new(self.session.responder()));
        let elicitation_supported = self.elicitation_supported();
        let handler_for_thread = Arc::clone(&handler);
        let responder_for_thread = Arc::clone(&responder);
        let join = match std::thread::Builder::new()
            .name("mcp-dispatch".into())
            .spawn(move || {
                while let Ok(request) = receiver.recv() {
                    if request.method == "ping" {
                        respond_via(&responder_for_thread, &request.id, &Ok(json!({})));
                        continue;
                    }
                    if request.method == "elicitation/create" && !elicitation_supported {
                        respond_via(
                            &responder_for_thread,
                            &request.id,
                            &Err((
                                -32601,
                                "elicitation requires protocol version >= 2025-06-18".to_owned(),
                            )),
                        );
                        continue;
                    }
                    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        handler_for_thread.handle(&request.method, request.params.clone())
                    }))
                    .unwrap_or_else(|_| {
                        Err((
                            -32603,
                            "CLAT host handler panicked while handling a server request".to_owned(),
                        ))
                    });
                    respond_via(&responder_for_thread, &request.id, &outcome);
                }
            }) {
            Ok(join) => join,
            Err(error) => {
                self.session.close_server_requests();
                return Err(McpError::new(format!(
                    "spawn MCP dispatcher thread: {error}"
                )));
            }
        };
        self.handler = Some(handler);
        self.dispatcher = Some(DispatcherGuard {
            responder,
            join: Some(join),
        });
        Ok(())
    }

    /// elicitation 的协议版本门槛（该方法 2025-06-18 引入；版本串是
    /// YYYY-MM-DD，字典序即时间序；modern 恒支持）。
    fn elicitation_supported(&self) -> bool {
        match self.era {
            ProtocolEra::Modern => true,
            ProtocolEra::Legacy => self.negotiated_version.as_str() >= "2025-06-18",
        }
    }

    fn discover(session: &Session) -> Result<String, McpError> {
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
    fn handshake(session: &Session) -> Result<(String, String), McpError> {
        let result = session.call(
            "initialize",
            json!({
                "protocolVersion": PROTOCOL_VERSION,
                "capabilities": client_capabilities(),
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

    /// 诊断尾缓冲快照（stdio 服务器 stderr + 会话异常）。
    pub fn stderr_tail(&self) -> Vec<String> {
        self.session.stderr_tail()
    }

    /// 握手协商出的协议版本，供将来按版本启用特性开关。
    pub fn negotiated_version(&self) -> &str {
        &self.negotiated_version
    }

    pub fn shutdown(mut self) -> Result<(), McpError> {
        // dispatcher 先于会话关停（序：摘响应端 → 关投递通道 → 有界
        // join → 优雅关会话）——保证 session.shutdown 的 writer join
        // 不被在途响应卡住，dispatcher 不悬空。
        if let Some(mut dispatcher) = self.dispatcher.take() {
            dispatcher.stop(&self.session);
        }
        self.handler = None;
        shutdown_session(self.session)
    }

    /// 列出远端工具（含 cursor 分页），映射为 CLAT 工具定义。
    /// 页数、工具数、cursor 循环、**总时长**均有界。
    pub fn list_tools(&self) -> Result<Vec<McpToolInfo>, McpError> {
        self.list_tools_with_budget(LIST_TOTAL_BUDGET)
    }

    /// [`Self::list_tools`] 的可注入预算变体（测试用小帽验证）。
    fn list_tools_with_budget(&self, total_budget: Duration) -> Result<Vec<McpToolInfo>, McpError> {
        let started = std::time::Instant::now();
        let mut tools = Vec::new();
        let mut seen_cursors: HashSet<String> = HashSet::new();
        let mut cursor: Option<String> = None;
        for _ in 0..MAX_LIST_PAGES {
            // W1-16（A2）：总帽先于页超时——剩余时间即本页上限。
            let remaining = total_budget.saturating_sub(started.elapsed());
            if remaining.is_zero() {
                return Err(McpError::new(format!(
                    "server `{}` exceeded the {total_budget:?} tools/list budget \
                     (slow or endless pagination)",
                    self.name
                )));
            }
            let params = match &cursor {
                Some(cursor) => json!({"cursor": cursor}),
                None => json!({}),
            };
            let page_timeout = remaining.min(LIST_TIMEOUT);
            // 页超时被总帽截断时，超时即总帽耗尽——归并为预算错误
            // （文案可分辨，测试据此判别）。
            let result = match self.call("tools/list", params, page_timeout, None) {
                Err(error) if page_timeout < LIST_TIMEOUT => {
                    return Err(McpError::new(format!(
                        "server `{}` exceeded the {total_budget:?} tools/list budget \
                         (slow or endless pagination): {error}",
                        self.name
                    )));
                }
                other => other?,
            };
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
    /// 工具执行期间服务器发起 sampling/elicitation 时，调用截止按
    /// 在途请求数延展（INV-S7：等人/等模型的时间不计入基础超时）。
    fn call_tool(
        &self,
        name: &str,
        arguments: &Value,
        cancel: &CancelToken,
    ) -> Result<Value, McpError> {
        let extend: Option<Arc<dyn Fn() -> bool + Send + Sync>> =
            self.handler.as_ref().map(|handler| {
                let handler = Arc::clone(handler);
                Arc::new(move || handler.pending_requests() > 0)
                    as Arc<dyn Fn() -> bool + Send + Sync>
            });
        let result = self.call_with_extension(
            "tools/call",
            json!({"name": name, "arguments": arguments}),
            CALL_TIMEOUT,
            cancel,
            extend,
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

    /// [`Self::call`] 的可延展形态（tools/call 专用，INV-S7）。
    fn call_with_extension(
        &self,
        method: &str,
        params: Value,
        timeout: Duration,
        cancel: &CancelToken,
        extend: Option<Arc<dyn Fn() -> bool + Send + Sync>>,
    ) -> Result<Value, McpError> {
        let params = match self.era {
            ProtocolEra::Legacy => params,
            ProtocolEra::Modern => modern_params(params),
        };
        let result = self
            .session
            .call_cancellable_extensible(method, params, timeout, cancel, extend)?;
        if self.era == ProtocolEra::Modern {
            validate_modern_result(&result, method)?;
        }
        Ok(result)
    }

    /// 测试专用：跨模块测试直接调用 tools/call（插件桥 Phase 3 的
    /// dsh-adapter e2e 在 plugin_host 测试区复用桥的假件）。
    #[cfg(test)]
    pub(crate) fn call_tool_for_test(
        &self,
        name: &str,
        arguments: &Value,
        cancel: &CancelToken,
    ) -> Result<Value, McpError> {
        self.call_tool(name, arguments, cancel)
    }
}

/// 测试专用：跨模块测试核对 annotations → ToolEffect 推导
/// （插件桥 Phase 3b e2e 断言内置 web_search 落在 Network 档）。
#[cfg(test)]
pub(crate) fn effect_from_annotations_for_test(
    annotations: crate::mcp::client::McpToolAnnotations,
) -> ToolEffect {
    effect_from_annotations(annotations)
}

/// initialize 握手中向服务器声明的客户端能力：sampling（借宿主做
/// 模型调用）与 elicitation（向用户提问）。二者均由宿主桥
/// （plugin_host.rs）受理——不声明则服务器永不发起。
fn client_capabilities() -> Value {
    json!({
        "sampling": {},
        "elicitation": {},
    })
}

fn modern_params(mut params: Value) -> Value {
    let object = params
        .as_object_mut()
        .expect("CLAT MCP request params are always objects");
    object.insert(
        "_meta".into(),
        json!({
            "io.modelcontextprotocol/protocolVersion": MODERN_PROTOCOL_VERSION,
            "io.modelcontextprotocol/clientCapabilities": client_capabilities(),
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
    qualify_prefixed_tool_name("mcp", server, tool)
}

/// [`qualify_tool_name`] 的前缀泛化（wasm 插件共用同一清洗/截断/去重
/// 纪律，INV-W2）：`{prefix}_{a}_{b}`。
pub fn qualify_prefixed_tool_name(prefix: &str, a: &str, b: &str) -> Option<String> {
    let a = sanitize_segment(a);
    let b = sanitize_segment(b);
    if a.is_empty() || b.is_empty() {
        return None;
    }
    let qualified = format!("{prefix}_{a}_{b}");
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

/// GLM Coding Plan 专属 MCP 包（docs.bigmodel.cn/cn/coding-plan/mcp，
/// 2026-08 核验）：激活厂商为 GLM 且配置了 API Key 时随挂载注入，
/// 用户 `mcp.json` 同名条目优先（逃生舱，见 [`merge_vendor_pack`]）。
/// 密钥只进内存合并、绝不写盘。
///
/// - search / reader / zread：远程 Streamable HTTP，`Authorization:
///   Bearer` 头鉴权（webSearchPrime / webReader / search_doc 等工具）；
/// - vision：本地 stdio `npx @z_ai/mcp-server`（@latest 防旧缓存），
///   `Z_AI_API_KEY` 环境变量鉴权（截图转码、OCR、图表分析等 8 工具，
///   需 Node.js ≥ 18）。
pub fn glm_mcp_pack(api_key: &str) -> Vec<(String, McpServerConfig)> {
    let remote = |path: &'static str| {
        let mut headers = std::collections::BTreeMap::new();
        headers.insert("Authorization".to_owned(), format!("Bearer {api_key}"));
        McpServerConfig {
            url: Some(format!("https://open.bigmodel.cn/api/mcp/{path}/mcp")),
            headers,
            ..McpServerConfig::default()
        }
    };
    let mut vision_env = std::collections::BTreeMap::new();
    vision_env.insert("Z_AI_API_KEY".to_owned(), api_key.to_owned());
    vec![
        ("glm-search".to_owned(), remote("web_search_prime")),
        ("glm-reader".to_owned(), remote("web_reader")),
        ("glm-zread".to_owned(), remote("zread")),
        (
            "glm-vision".to_owned(),
            McpServerConfig {
                command: "npx".to_owned(),
                args: vec!["-y".to_owned(), "@z_ai/mcp-server@latest".to_owned()],
                env: vision_env,
                ..McpServerConfig::default()
            },
        ),
    ]
}

/// 把厂商包并入用户配置：**用户 `mcp.json` 同名条目优先**——包只是
/// 拎包入住的默认值，不是覆盖。返回并入的条目数（供状态提示）。
pub fn merge_vendor_pack(config: &mut McpConfig, pack: &[(String, McpServerConfig)]) -> usize {
    let mut merged = 0;
    for (name, server) in pack {
        if !config.contains_key(name) {
            config.insert(name.clone(), server.clone());
            merged += 1;
        }
    }
    merged
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

    /// B6（INV-K1）：stderr 尾部进入失败消息（→ /mcp 面板、exec
    /// stdout、状态闪现）前按密钥形状脱敏；无密行零变化。
    #[test]
    fn stderr_tail_is_redacted_before_reaching_surfaces() {
        let tail = vec![
            "Z_AI_API_KEY=live-abcdef0123456789".to_owned(),
            "server ready on port 9100".to_owned(),
        ];
        let formatted = format_stderr_tail_public(&tail);
        assert!(
            formatted.contains("Z_AI_API_KEY=[REDACTED]"),
            "key must be redacted: {formatted}"
        );
        assert!(
            !formatted.contains("live-abcdef0123456789"),
            "the secret value must not survive: {formatted}"
        );
        assert!(
            formatted.contains("server ready on port 9100"),
            "benign lines pass through unchanged: {formatted}"
        );
        let bearer =
            format_stderr_tail_public(&["Authorization: Bearer sk-abc123def456".to_owned()]);
        assert_eq!(bearer, "; server stderr: Authorization: Bearer [REDACTED]");
    }

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
        let server =
            McpServer::connect("echo", &config, Path::new("/tmp"), None).expect("handshake");
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

    /// 端到端（INV-S3/S4 + 能力声明 + INV-S1）：tools/call 期间服务端
    /// 发起 elicitation/sampling/ping/未知方法——reader 投递、dispatcher
    /// 处理并回写响应，服务器拿到结果继续工具调用。第二个用例用真实
    /// 的空 PluginHostBridge（未安装 run 上下文）验证 no-active-run 的
    /// fail-closed 错误码。
    #[test]
    #[ignore = "spawns a python3 subprocess; run explicitly with --ignored"]
    fn end_to_end_server_requests_flow_through_the_dispatcher() {
        let script = r#"
import json, sys
def send(obj):
    sys.stdout.write(json.dumps(obj) + "\n")
    sys.stdout.flush()
next_server_id = 9000
caps_ok = False
while True:
    line = sys.stdin.readline()
    if not line:
        break
    msg = json.loads(line)
    if "id" not in msg:
        continue
    method = msg.get("method", "")
    if method == "initialize":
        caps = msg.get("params", {}).get("capabilities", {})
        caps_ok = "sampling" in caps and "elicitation" in caps
        send({"jsonrpc": "2.0", "id": msg["id"], "result": {
            "protocolVersion": "2025-11-25",
            "capabilities": {"tools": {}},
            "serverInfo": {"name": "fixture", "version": "1.0"}}})
    elif method == "tools/list":
        send({"jsonrpc": "2.0", "id": msg["id"], "result": {"tools": [
            {"name": "ask", "description": "asks the user",
             "inputSchema": {"type": "object"}}]}})
    elif method == "tools/call":
        name = msg["params"].get("name", "")
        if name == "ask":
            next_server_id += 1
            send({"jsonrpc": "2.0", "id": next_server_id,
                  "method": "elicitation/create",
                  "params": {"message": "pick",
                             "requestedSchema": {"type": "object",
                                 "properties": {"flavor": {"type": "string"}},
                                 "required": ["flavor"]}}})
            reply = json.loads(sys.stdin.readline())
            flavor = reply.get("result", {}).get("content", {}).get("flavor", "?")
            send({"jsonrpc": "2.0", "id": msg["id"], "result": {
                "content": [{"type": "text", "text": "flavor=" + flavor}]}})
        elif name == "sample":
            next_server_id += 1
            send({"jsonrpc": "2.0", "id": next_server_id,
                  "method": "sampling/createMessage",
                  "params": {"messages": [{"role": "user",
                              "content": {"type": "text", "text": "hi"}}],
                             "maxTokens": 16}})
            reply = json.loads(sys.stdin.readline())
            text = reply.get("result", {}).get("content", {}).get("text", "?")
            model = reply.get("result", {}).get("model", "?")
            send({"jsonrpc": "2.0", "id": msg["id"], "result": {
                "content": [{"type": "text", "text": text + "@" + model}]}})
        elif name == "pingseq":
            next_server_id += 1
            send({"jsonrpc": "2.0", "id": next_server_id, "method": "ping"})
            ping_reply = json.loads(sys.stdin.readline())
            next_server_id += 1
            send({"jsonrpc": "2.0", "id": next_server_id,
                  "method": "clat/unknown", "params": {}})
            unknown_reply = json.loads(sys.stdin.readline())
            ok = ping_reply.get("result") == {} and \
                unknown_reply.get("error", {}).get("code") == -32601
            send({"jsonrpc": "2.0", "id": msg["id"], "result": {
                "content": [{"type": "text", "text": "ok" if ok else "bad"}]}})
        elif name == "norun":
            next_server_id += 1
            send({"jsonrpc": "2.0", "id": next_server_id,
                  "method": "elicitation/create",
                  "params": {"message": "m",
                             "requestedSchema": {"type": "object",
                                 "properties": {"a": {"type": "string"}},
                                 "required": ["a"]}}})
            reply = json.loads(sys.stdin.readline())
            code = reply.get("error", {}).get("code")
            send({"jsonrpc": "2.0", "id": msg["id"], "result": {
                "content": [{"type": "text", "text": "code=%s" % code}]}})
        elif name == "caps":
            send({"jsonrpc": "2.0", "id": msg["id"], "result": {
                "content": [{"type": "text", "text": "caps_ok=%d" % caps_ok}]}})
        else:
            send({"jsonrpc": "2.0", "id": msg["id"], "result": {
                "content": [{"type": "text", "text": "?"}]}})
    else:
        send({"jsonrpc": "2.0", "id": msg["id"], "result": {}})
"#;
        let config = McpServerConfig {
            command: "python3".into(),
            args: vec!["-c".into(), script.into()],
            ..Default::default()
        };

        // 受理端：elicitation 回固定答案、sampling 回固定文本。
        struct EchoHandler;
        impl McpServerRequestHandler for EchoHandler {
            fn handle(&self, method: &str, _params: Value) -> Result<Value, (i64, String)> {
                match method {
                    "sampling/createMessage" => Ok(json!({
                        "role": "assistant",
                        "content": {"type": "text", "text": "sampled!"},
                        "model": "fake-model",
                        "stopReason": "endTurn",
                    })),
                    "elicitation/create" => Ok(json!({
                        "action": "accept",
                        "content": {"flavor": "vanilla"},
                    })),
                    other => Err((-32601, format!("CLAT does not implement `{other}`"))),
                }
            }
        }
        let server = McpServer::connect(
            "fixture",
            &config,
            Path::new("/tmp"),
            Some(Arc::new(EchoHandler)),
        )
        .expect("connect");
        let cancel = CancelToken::new();

        // 能力声明（initialize 帧内 sampling + elicitation）。
        assert_eq!(
            server.call_tool("caps", &json!({}), &cancel).expect("caps"),
            json!("caps_ok=1")
        );
        // elicitation：服务器拿到 accept+content 并继续工具调用。
        assert_eq!(
            server.call_tool("ask", &json!({}), &cancel).expect("ask"),
            json!("flavor=vanilla")
        );
        // sampling：服务器拿到文本结果与模型名。
        assert_eq!(
            server
                .call_tool("sample", &json!({}), &cancel)
                .expect("sample"),
            json!("sampled!@fake-model")
        );
        // ping → {}；未知方法 → -32601（INV-S4）。
        assert_eq!(
            server
                .call_tool("pingseq", &json!({}), &cancel)
                .expect("pingseq"),
            json!("ok")
        );
        server.shutdown().expect("shutdown");

        // INV-S1（无免费通道）：真实空桥（无 run 上下文）→ -32000。
        let empty_host = crate::plugin_host::McpHostHandler::new(
            crate::plugin_host::PluginHostBridge::shared(),
            "fixture",
        );
        let server = McpServer::connect(
            "fixture",
            &config,
            Path::new("/tmp"),
            Some(Arc::new(empty_host)),
        )
        .expect("connect");
        assert_eq!(
            server
                .call_tool("norun", &json!({}), &cancel)
                .expect("norun"),
            json!("code=-32000")
        );
        server.shutdown().expect("shutdown");
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
        let server = McpServer::connect("v2", &config, Path::new("/tmp"), None).expect("modern");
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

    /// 远程 Streamable HTTP 传输的端到端（本地 TCP 服务模拟，2026-08-19
    /// 为 GLM Coding Plan MCP 接入）：discover 失败回落 legacy
    /// initialize（响应分配 Mcp-Session-Id）、initialized 通知（202）、
    /// tools/list 服务端校验鉴权头与会话 id 回显，并以 SSE 帧返回工具。
    #[test]
    fn http_transport_negotiates_and_lists_tools_with_session_and_auth() {
        struct Request {
            headers: Vec<String>,
            body: String,
        }
        fn read_request(stream: &mut std::net::TcpStream) -> Request {
            use std::io::Read;
            let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
            let mut bytes = Vec::new();
            let mut chunk = [0_u8; 4096];
            let header_end = loop {
                if let Some(position) = find_double_crlf(&bytes) {
                    break position;
                }
                let read = stream.read(&mut chunk).expect("read request");
                assert!(read > 0, "connection closed before headers ended");
                bytes.extend_from_slice(&chunk[..read]);
            };
            let head = String::from_utf8_lossy(&bytes[..header_end]).into_owned();
            let headers: Vec<String> = head.lines().skip(1).map(str::to_owned).collect();
            let length: usize = headers
                .iter()
                .find(|header| header.to_ascii_lowercase().starts_with("content-length:"))
                .and_then(|header| header.split(':').nth(1))
                .and_then(|value| value.trim().parse().ok())
                .unwrap_or(0);
            while bytes.len() < header_end + 4 + length {
                let read = stream.read(&mut chunk).expect("read body");
                assert!(read > 0, "connection closed before body ended");
                bytes.extend_from_slice(&chunk[..read]);
            }
            Request {
                headers,
                body: String::from_utf8_lossy(&bytes[header_end + 4..header_end + 4 + length])
                    .into_owned(),
            }
        }
        fn find_double_crlf(bytes: &[u8]) -> Option<usize> {
            bytes.windows(4).position(|window| window == b"\r\n\r\n")
        }

        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind mcp http server");
        let address = listener.local_addr().expect("server address");
        let server = std::thread::spawn(move || {
            use std::io::Write;
            for _ in 0..4 {
                let Ok((mut stream, _)) = listener.accept() else {
                    break;
                };
                let request = read_request(&mut stream);
                let body: Value = serde_json::from_str(&request.body).expect("json-rpc body");
                let method = body
                    .get("method")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                let id = body.get("id").cloned().unwrap_or(Value::Null);
                let (status, extra_headers, content): (&str, Vec<&str>, String) = match method {
                    "server/discover" => (
                        "404 Not Found",
                        Vec::new(),
                        format!(
                            r#"{{"jsonrpc":"2.0","id":{id},"error":{{"code":-32601,"message":"no discover"}}}}"#
                        ),
                    ),
                    "initialize" => (
                        "200 OK",
                        vec!["Mcp-Session-Id: sess-7"],
                        format!(
                            r#"{{"jsonrpc":"2.0","id":{id},"result":{{"protocolVersion":"2025-06-18","serverInfo":{{"name":"t","version":"0.1"}}}}}}"#
                        ),
                    ),
                    "notifications/initialized" => ("202 Accepted", Vec::new(), String::new()),
                    "tools/list" => {
                        assert!(
                            request.headers.iter().any(|header| header
                                .eq_ignore_ascii_case("authorization: bearer test-key")),
                            "tools/list must carry the configured bearer auth"
                        );
                        assert!(
                            request.headers.iter().any(|header| header.eq_ignore_ascii_case(
                                "mcp-session-id: sess-7"
                            )),
                            "tools/list must echo the initialize-assigned session id"
                        );
                        let payload = r#"{"tools":[{"name":"webSearchPrime","description":"search the web","inputSchema":{"type":"object"}}]}"#;
                        (
                            "200 OK",
                            vec!["Content-Type: text/event-stream"],
                            format!(
                                "data: {{\"jsonrpc\":\"2.0\",\"id\":{id},\"result\":{payload}}}\n\n"
                            ),
                        )
                    }
                    other => panic!("unexpected method: {other}"),
                };
                let mut response = format!(
                    "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n",
                    content.len()
                );
                for header in extra_headers {
                    response.push_str(header);
                    response.push_str("\r\n");
                }
                response.push_str("\r\n");
                response.push_str(&content);
                stream
                    .write_all(response.as_bytes())
                    .expect("write response");
                let _ = stream.flush();
            }
        });

        let config = McpServerConfig {
            url: Some(format!("http://{address}/mcp")),
            headers: [("Authorization".to_owned(), "Bearer test-key".to_owned())]
                .into_iter()
                .collect(),
            ..McpServerConfig::default()
        };
        let server_session =
            McpServer::connect("glm-search", &config, Path::new("."), None).expect("connect");
        assert_eq!(server_session.negotiated_version(), "2025-06-18");
        let tools = server_session.list_tools().expect("list tools over sse");
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].name, "webSearchPrime");
        assert_eq!(tools[0].description, "search the web");
        let _ = server_session.shutdown();
        server.join().expect("join mcp http server");
    }

    /// 挂起回归（对抗审计 2026-08-19）：initialize 后的
    /// `notifications/initialized` 通知若遇到永不响应的服务器，连接
    /// 必须在 NOTIFY_TIMEOUT（10s）内以错误返回——预修复代码上 HTTP
    /// notify 没有任何截止，挂载线程会无限阻塞（测试表现为超时挂死）。
    #[test]
    fn http_notify_is_bounded_against_silent_servers() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let address = listener.local_addr().expect("address");
        let server = std::thread::spawn(move || {
            use std::io::{Read, Write};
            for request_index in 0..3 {
                let Ok((mut stream, _)) = listener.accept() else {
                    break;
                };
                let mut buffer = [0_u8; 8192];
                let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
                // B4（抖动定位）：单次 read 会与 TCP 分片竞态——headers
                // 先到、body 未齐时 `notifications/initialized` 误走回显
                // 路径，通知瞬间"成功"，connect 假绿。按 Content-Length
                // 读完整再判定。
                let mut text = String::new();
                while let Ok(read) = stream.read(&mut buffer) {
                    if read == 0 {
                        break;
                    }
                    text.push_str(&String::from_utf8_lossy(&buffer[..read]));
                    let Some(headers_end) = text.find("\r\n\r\n") else {
                        continue;
                    };
                    let content_length = text[..headers_end]
                        .lines()
                        .find_map(|line| {
                            let (name, value) = line.split_once(':')?;
                            name.trim()
                                .eq_ignore_ascii_case("content-length")
                                .then(|| value.trim().parse::<usize>().ok())?
                        })
                        .unwrap_or(0);
                    if text.len() >= headers_end + 4 + content_length {
                        break;
                    }
                }
                if text.contains("\"notifications/initialized\"") {
                    // 挂起：收下请求，永不响应。
                    std::thread::sleep(Duration::from_secs(60));
                    continue;
                }
                // 按请求体回显 id（客户端每会话独立计数，硬编码会令
                // 响应永远不匹配——首个版本的空转教训）。
                let body_json: Value = text
                    .split("\r\n\r\n")
                    .nth(1)
                    .and_then(|payload| serde_json::from_str(payload).ok())
                    .unwrap_or(Value::Null);
                let id = body_json.get("id").cloned().unwrap_or(Value::Null);
                let body = if text.contains("\"server/discover\"") {
                    format!(
                        r#"{{"jsonrpc":"2.0","id":{id},"error":{{"code":-32601,"message":"no"}}}}"#
                    )
                } else {
                    format!(
                        r#"{{"jsonrpc":"2.0","id":{id},"result":{{"protocolVersion":"2025-06-18","serverInfo":{{"name":"t","version":"1"}}}}}}"#
                    )
                };
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nMcp-Session-Id: s1\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = request_index;
                let _ = stream.write_all(response.as_bytes());
            }
        });

        let config = McpServerConfig {
            url: Some(format!("http://{address}/mcp")),
            ..McpServerConfig::default()
        };
        let started = std::time::Instant::now();
        let outcome = McpServer::connect("hung", &config, Path::new("."), None);
        let elapsed = started.elapsed();
        // 握手成功、通知被挂起：connect 必须以错误返回且有界。
        assert!(
            outcome.is_err(),
            "a silent server must fail the connect, got ok in {elapsed:?}"
        );
        // 判别边界贴着服务器的 60s 睡眠而不是名义 10s 超时：本测试防
        // 的是"无界挂起/等到服务器睡醒"回归，名义超时 + 5 倍调度拉伸
        // （50s）仍远低于 60s——曾用 20s/30s 上限，全套并行负载下两
        // 次假红（壁钟断言在满载 runner 上会被拉伸 2-3 倍）。
        assert!(
            elapsed < crate::mcp::transport::NOTIFY_TIMEOUT + Duration::from_secs(40),
            "notify must be bounded by NOTIFY_TIMEOUT, took {elapsed:?}"
        );
        // 服务线程在挂起连接上睡眠 60s 后还会阻塞在 accept：测试不
        // join 它——detach 线程随测试进程退出回收，join 反而会把测试
        // 自身变成它要防的挂起。
        drop(server);
    }

    /// mcp.json 的远程条目：`url` + `headers`，`command` 可省略。
    #[test]
    fn parses_remote_http_server_entries() {
        let config = parse_mcp_config(
            r#"{"web-search-prime": {"url": "https://example.test/mcp", "headers": {"Authorization": "Bearer k"}}}"#,
        )
        .expect("parse");
        let server = &config["web-search-prime"];
        assert!(server.is_http());
        assert_eq!(server.url.as_deref(), Some("https://example.test/mcp"));
        assert_eq!(
            server.headers.get("Authorization").map(String::as_str),
            Some("Bearer k")
        );
        assert!(server.command.is_empty());
    }

    /// GLM Coding Plan 四件套：三个远程 Bearer 端点 + 一个本地 stdio
    /// 视觉服务器（npx + 环境变量鉴权），密钥进内存配置。
    #[test]
    fn glm_pack_carries_the_four_official_servers() {
        let pack = glm_mcp_pack("glm-key-1");
        assert_eq!(pack.len(), 4);
        let names: Vec<&str> = pack.iter().map(|(name, _)| name.as_str()).collect();
        assert_eq!(
            names,
            vec!["glm-search", "glm-reader", "glm-zread", "glm-vision"]
        );

        let (search_name, search) = &pack[0];
        assert_eq!(search_name, "glm-search");
        assert_eq!(
            search.url.as_deref(),
            Some("https://open.bigmodel.cn/api/mcp/web_search_prime/mcp")
        );
        assert_eq!(
            search.headers.get("Authorization").map(String::as_str),
            Some("Bearer glm-key-1")
        );
        let (_, reader) = &pack[1];
        assert_eq!(
            reader.url.as_deref(),
            Some("https://open.bigmodel.cn/api/mcp/web_reader/mcp")
        );
        let (_, zread) = &pack[2];
        assert_eq!(
            zread.url.as_deref(),
            Some("https://open.bigmodel.cn/api/mcp/zread/mcp")
        );

        let (_, vision) = &pack[3];
        assert!(!vision.is_http());
        assert_eq!(vision.command, "npx");
        assert!(vision.args.contains(&"@z_ai/mcp-server@latest".to_owned()));
        assert_eq!(
            vision.env.get("Z_AI_API_KEY").map(String::as_str),
            Some("glm-key-1")
        );
    }

    /// 厂商包并入：用户 mcp.json 同名条目优先（逃生舱），不重不漏。
    #[test]
    fn vendor_pack_merge_respects_user_overrides() {
        let mut config: McpConfig = std::collections::BTreeMap::new();
        config.insert(
            "glm-search".to_owned(),
            McpServerConfig {
                url: Some("https://my-proxy.test/mcp".to_owned()),
                ..McpServerConfig::default()
            },
        );
        let merged = merge_vendor_pack(&mut config, &glm_mcp_pack("k"));
        assert_eq!(merged, 3, "only entries without a user override merge");
        assert_eq!(config.len(), 4);
        assert_eq!(
            config["glm-search"].url.as_deref(),
            Some("https://my-proxy.test/mcp"),
            "the user's same-named entry wins"
        );
        assert_eq!(
            config["glm-reader"].url.as_deref(),
            Some("https://open.bigmodel.cn/api/mcp/web_reader/mcp")
        );
    }

    /// W1-16/A2：tools/list **总时长帽**——快页但无限分页（每页唯一
    /// cursor）在小帽下必须以 budget 错误终止，而不是页页叠加。
    /// pre-fix 红：无总帽时走满 32 页报 "exceeds 32 pagination pages"
    /// （不同文案）。
    #[test]
    #[ignore = "spawns a python3 subprocess; run explicitly with --ignored"]
    fn endless_pagination_hits_the_total_list_budget() {
        let script = r#"
import json, sys
counter = 0
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
            "serverInfo": {"name": "pager", "version": "1.0"}}})
    elif method == "tools/list":
        import time
        time.sleep(0.03)
        counter += 1
        send({"jsonrpc": "2.0", "id": msg["id"], "result": {
            "tools": [{"name": "t" + str(counter), "inputSchema": {"type": "object"}}],
            "nextCursor": "page-" + str(counter)}})
    else:
        send({"jsonrpc": "2.0", "id": msg["id"], "result": {}})
"#;
        let config = McpServerConfig {
            command: "python3".into(),
            args: vec!["-u".into(), "-c".into(), script.into()],
            ..Default::default()
        };
        let server = McpServer::connect("pager", &config, std::path::Path::new("/tmp"), None)
            .expect("handshake");
        let error = server
            .list_tools_with_budget(std::time::Duration::from_millis(300))
            .expect_err("the total budget must end endless pagination");
        assert!(
            error.to_string().contains("budget"),
            "the failure must name the total budget: {error}"
        );
    }
}
