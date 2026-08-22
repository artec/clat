//! MCP (Model Context Protocol) stdio 传输层。
//!
//! 每行一个 JSON-RPC 2.0 消息（LSP 风格的换行分帧，MCP stdio 传输
//! 的标准格式）。
//!
//! 架构（对抗失控/恶意服务的设计约束）：
//!
//! - **单 reader 线程**独占 stdout，按 id 把响应分发到每请求通道；
//!   乱序、并发响应都能正确路由，未知 id 的迟到响应被丢弃并记录。
//! - **每次调用有超时**：`call` 在 deadline 内收不到响应即失败并
//!   注销 pending 槽位，绝不无限阻塞。
//! - **帧有字节上限**：单行超过 `MAX_FRAME_BYTES` 视为协议违约，
//!   reader 线程结束并使所有 pending 调用失败——无换行洪泛无法
//!   耗尽内存。
//! - **Drop 先关 stdin**（触发服务优雅退出），限时 `try_wait`，
//!   到期 `kill` 再回收；沉默服务不会拖死 CLAT 退出。
//! - 子进程以调用方指定的固定 cwd 启动，绝不继承未受信项目目录。

use crate::model::CancelToken;
use serde_json::{Value, json};
use std::collections::{HashMap, VecDeque};
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// 单帧（一行 JSON-RPC）的最大字节数。超限即协议违约。
pub const MAX_FRAME_BYTES: usize = 4 * 1024 * 1024;

/// Drop 时等待子进程优雅退出的宽限期，之后强杀。
const SHUTDOWN_GRACE: Duration = Duration::from_secs(3);

/// Notification 没有调用方提供的 deadline，仍需给写入设置硬上限。
/// `client.rs` 的挂起回归测试引用它推导壁钟上限（名义 3 倍）。
pub(crate) const NOTIFY_TIMEOUT: Duration = Duration::from_secs(10);
/// HTTP 传输的连接建立上限（逐请求实际截止由调用的 timeout 决定）。
const HTTP_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// 非成功响应的"错误页"正文读取上限：只用于诊断消息，不值得为它
/// 缓冲更大的体。
const HTTP_ERROR_BODY_BYTES: usize = 16 * 1024;
const CANCEL_POLL_INTERVAL: Duration = Duration::from_millis(25);

/// stdio 会话错误。
#[derive(Debug)]
pub struct McpError {
    message: String,
}

impl McpError {
    pub(crate) fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl std::fmt::Display for McpError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for McpError {}

/// 一条 JSON-RPC 请求帧（不含 id，id 由会话分配）。
pub fn request_frame(method: &str, params: Value, id: u64) -> String {
    let mut line = serde_json::to_string(&json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": method,
        "params": params,
    }))
    .unwrap_or_else(|_| "{\"jsonrpc\":\"2.0\"}".into());
    line.push('\n');
    line
}

/// 一条 JSON-RPC notification 帧（无 id，不期待响应）。
pub fn notification_frame(method: &str, params: Value) -> String {
    let mut line = serde_json::to_string(&json!({
        "jsonrpc": "2.0",
        "method": method,
        "params": params,
    }))
    .unwrap_or_else(|_| "{}".into());
    line.push('\n');
    line
}

/// 一条收到帧的归类：对我方请求的响应、服务端发起的请求、或忽略
/// （notification/坏行）。
///
/// 请求判定优先于响应（2026-08-21 修复）：JSON-RPC 里 `method`+`id`
/// 是请求、`id`+`result`/`error` 是响应；旧实现不区分 `method`，服务
/// 端请求会被误当响应路由（id 撞上在途 pending 时错配成 Null 结果，
/// 否则被当坏行丢弃）——sampling/elicitation 的分发前提。
#[derive(Debug)]
pub enum IncomingFrame {
    Response {
        id: u64,
        outcome: Result<Value, String>,
    },
    ServerRequest {
        id: Value,
        method: String,
        params: Value,
    },
    Ignored,
}

pub fn parse_incoming(line: &str) -> IncomingFrame {
    let Ok(value) = serde_json::from_str::<Value>(line.trim()) else {
        return IncomingFrame::Ignored;
    };
    if let Some(method) = value.get("method").and_then(Value::as_str) {
        return match value.get("id") {
            // 服务端请求的 id 按规范可为字符串，响应须原样回显。
            Some(id) => IncomingFrame::ServerRequest {
                id: id.clone(),
                method: method.to_owned(),
                params: value.get("params").cloned().unwrap_or(Value::Null),
            },
            None => IncomingFrame::Ignored,
        };
    }
    match parse_response_value(&value) {
        Some((id, outcome)) => IncomingFrame::Response { id, outcome },
        None => IncomingFrame::Ignored,
    }
}

fn parse_response_value(value: &Value) -> Option<(u64, Result<Value, String>)> {
    let id = value.get("id")?.as_u64()?;
    if let Some(error) = value.get("error") {
        let message = error
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("unknown MCP error");
        let code = error.get("code").and_then(Value::as_i64).unwrap_or(0);
        return Some((id, Err(format!("MCP error {code}: {message}"))));
    }
    Some((id, Ok(value.get("result").cloned().unwrap_or(Value::Null))))
}

/// 解析一行收到的消息，返回 (id, result/error)。notification、服务端
/// 请求与无法解析的行返回 None。（生产路径走 [`parse_incoming`]；
/// 保留给规格推导测试。）
#[cfg(test)]
pub fn parse_response(line: &str) -> Option<(u64, Result<Value, String>)> {
    match parse_incoming(line) {
        IncomingFrame::Response { id, outcome } => Some((id, outcome)),
        _ => None,
    }
}

/// 服务端发起的请求（sampling/elicitation 等）：reader 线程只投递到
/// [`ServerRequestSink`]，处理在上层 dispatcher 线程完成，响应经
/// [`response_frame`] 写回。
pub struct ServerRequest {
    pub id: Value,
    pub method: String,
    pub params: Value,
}

/// reader → dispatcher 的投递通道（有界：洪泛即丢弃并记诊断，绝不
/// 反压 reader）。
pub type ServerRequestSink = mpsc::SyncSender<ServerRequest>;

/// 尽力投递一个服务端请求；返回是否送达（无处理端或队列满 = false）。
pub(crate) fn try_deliver_server_request(
    sink: &Mutex<Option<ServerRequestSink>>,
    request: ServerRequest,
) -> bool {
    match sink.lock() {
        Ok(guard) => match guard.as_ref() {
            Some(sink) => sink.try_send(request).is_ok(),
            None => false,
        },
        Err(_) => false,
    }
}

/// 服务端请求的响应帧（result 或 error；id 原样回显）。
pub fn response_frame(id: &Value, outcome: &Result<Value, (i64, String)>) -> String {
    let body = match outcome {
        Ok(result) => json!({ "jsonrpc": "2.0", "id": id, "result": result }),
        Err((code, message)) => json!({
            "jsonrpc": "2.0",
            "id": id,
            "error": { "code": code, "message": message },
        }),
    };
    let mut line = serde_json::to_string(&body).unwrap_or_else(|_| "{\"jsonrpc\":\"2.0\"}".into());
    line.push('\n');
    line
}

/// 读取一行（到 `\n`），累计字节超过 `cap` 报错。基于 `fill_buf`
/// 实现，不在内存里无限累积无换行的输入。
fn read_capped_line(reader: &mut impl BufRead, cap: usize) -> std::io::Result<Option<String>> {
    let mut line = Vec::new();
    loop {
        let available = reader.fill_buf()?;
        if available.is_empty() {
            // EOF：半行残留也按行交付（宽松），空则报告流结束。
            return if line.is_empty() {
                Ok(None)
            } else {
                Ok(Some(String::from_utf8_lossy(&line).into_owned()))
            };
        }
        let capped = available.len().min(cap.saturating_sub(line.len()).max(1));
        if let Some(newline) = available[..capped].iter().position(|&byte| byte == b'\n') {
            line.extend_from_slice(&available[..=newline]);
            reader.consume(newline + 1);
            return if line.len() > cap {
                Err(std::io::Error::other("frame exceeds byte limit"))
            } else {
                Ok(Some(String::from_utf8_lossy(&line).into_owned()))
            };
        }
        line.extend_from_slice(&available[..capped]);
        let consumed = capped;
        reader.consume(consumed);
        if line.len() > cap {
            return Err(std::io::Error::other("frame exceeds byte limit"));
        }
    }
}

/// 在途请求表：id → 响应回传通道。
type PendingMap = HashMap<u64, std::sync::mpsc::Sender<Result<Value, String>>>;

/// writer 队列条目。`pub(crate)`：client 的 dispatcher 经
/// [`StdioSession::writer_sender`] 写回服务端请求的响应帧。
pub(crate) struct WriterRequest {
    pub(crate) frame: String,
    pub(crate) result: mpsc::Sender<Result<(), String>>,
}

/// 服务端请求在途时的 tools/call 截止延展参数（INV-S7）：elicitation
/// 等人、sampling 等模型的时间不计入基础超时；步长与次数上限防服务
/// 器借永挂请求拖死调用（60s × 10 = 至多 +10 分钟）。
const DEADLINE_EXTENSION_STEP: Duration = Duration::from_secs(60);
const MAX_DEADLINE_EXTENSIONS: usize = 10;
/// 诊断尾缓冲的容量（行）。服务器 stderr 与 reader 异常都进这里，
/// 挂载失败时并入错误消息——既不往终端泼日志（会把 TUI 顶出可视区，
/// 2026-08-20 用户实测报告），也不彻底丢掉排障信息。
const STDERR_TAIL_LINES: usize = 20;
/// 单行诊断的字节上限：npx 进度条等长行截断，防止单行撑爆缓冲。
const STDERR_TAIL_LINE_BYTES: usize = 512;

/// 有界读取 HTTP 响应体（W1-11）：先 `take(cap + 1)` 再读——缓冲上限
/// 在读取前生效，恶意端点无法用数 GB 的"响应体"在长度检查之前把内存
/// 灌满；超限即协议违约错误。删除 `.take()`（回到先读后查的形状）会让
/// `http_body_read_is_bounded_against_an_infinite_source` 挂起/OOM 而红。
fn read_body_capped(body: &mut impl std::io::Read, cap: usize) -> Result<String, std::io::Error> {
    use std::io::Read as _;
    let mut text = String::new();
    body.take(cap as u64 + 1).read_to_string(&mut text)?;
    if text.len() > cap {
        return Err(std::io::Error::other(format!(
            "response body exceeds {cap} bytes"
        )));
    }
    Ok(text)
}

fn push_diagnostic(tail: &Arc<Mutex<VecDeque<String>>>, line: impl Into<String>) {
    let mut line = line.into();
    if line.len() > STDERR_TAIL_LINE_BYTES {
        line.truncate(STDERR_TAIL_LINE_BYTES);
    }
    if let Ok(mut tail) = tail.lock() {
        if tail.len() == STDERR_TAIL_LINES {
            tail.pop_front();
        }
        tail.push_back(line);
    }
}

/// 一个 MCP stdio 服务器子进程会话。
pub struct StdioSession {
    child: Child,
    /// 所有写入交给单独线程串行执行，调用线程只做有 deadline 的等待。
    /// Drop 时丢弃 sender；writer 队列耗尽后关闭 stdin。
    writer: Option<mpsc::SyncSender<WriterRequest>>,
    writer_handle: Option<JoinHandle<()>>,
    reader_handle: Option<JoinHandle<()>>,
    /// stderr 排水线程的 join 句柄（EOF/子进程退出后自然结束）。
    stderr_handle: Option<JoinHandle<()>>,
    /// 每个在途请求的响应回传通道，按 id 注册，由 reader 线程消费。
    pending: Arc<Mutex<PendingMap>>,
    /// 服务端发起请求的投递通道（reader → 上层 dispatcher）；None 或
    /// 队列满时请求被丢弃并记诊断——reader 绝不为服务端请求阻塞。
    server_requests: Arc<Mutex<Option<ServerRequestSink>>>,
    /// 服务器 stderr 与会话异常的有界尾缓冲（诊断用，见
    /// [`push_diagnostic`]）。
    stderr_tail: Arc<Mutex<VecDeque<String>>>,
    next_id: AtomicU64,
}

impl StdioSession {
    /// 启动子进程并建立 stdio 通道。
    ///
    /// `cwd` 是子进程的固定工作目录（MCP 服务器是全局能力，绝不在
    /// 项目目录里启动——未受信项目可用本地文件劫持 `npx` 等查询
    /// cwd 的命令）。调用方负责 `initialize` 握手。
    pub fn spawn(
        command: &str,
        args: &[String],
        env: &[(String, String)],
        cwd: &std::path::Path,
    ) -> Result<Self, McpError> {
        let mut command_builder = Command::new(command);
        command_builder
            .args(args)
            .envs(env.iter().cloned())
            .current_dir(cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            // stderr 管道化并由排水线程消费：直通终端会把 npx 进度/
            // 服务器日志泼进 TUI，把整个界面顶出可视区（2026-08-20
            // 用户实测）；管道不排又会写满阻塞子进程。日志进有界尾
            // 缓冲，挂载失败时并入错误消息（见 push_diagnostic）。
            .stderr(Stdio::piped());
        let mut child = command_builder
            .spawn()
            .map_err(|error| McpError::new(format!("spawn MCP server `{command}`: {error}")))?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| McpError::new("MCP server stdin unavailable"))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| McpError::new("MCP server stdout unavailable"))?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| McpError::new("MCP server stderr unavailable"))?;

        let pending: Arc<Mutex<PendingMap>> = Arc::new(Mutex::new(HashMap::new()));
        let reader_pending = Arc::clone(&pending);
        let server_requests: Arc<Mutex<Option<ServerRequestSink>>> = Arc::new(Mutex::new(None));
        let reader_requests = Arc::clone(&server_requests);
        let stderr_tail: Arc<Mutex<VecDeque<String>>> = Arc::new(Mutex::new(VecDeque::new()));
        let reader_tail = Arc::clone(&stderr_tail);
        let reader_handle = match std::thread::Builder::new().name("mcp-reader".into()).spawn(
            move || {
                let mut reader = BufReader::new(stdout);
                loop {
                    let line = match read_capped_line(&mut reader, MAX_FRAME_BYTES) {
                        Ok(Some(line)) => line,
                        Ok(None) => break,
                        Err(error) => {
                            push_diagnostic(&reader_tail, format!("reader stopping: {error}"));
                            break;
                        }
                    };
                    match parse_incoming(&line) {
                        IncomingFrame::Response { id, outcome } => {
                            let slot = reader_pending
                                .lock()
                                .ok()
                                .and_then(|mut map| map.remove(&id));
                            match slot {
                                Some(sender) => {
                                    let _ = sender.send(outcome);
                                }
                                // 未知 id：超时后被放弃的请求的迟到响应，
                                // 或服务端违反协议。记录但不中断会话——
                                // 进诊断缓冲而非 eprintln（TUI 运行期任何
                                // 终端打印都会毁屏，2026-08-20）。
                                None => push_diagnostic(
                                    &reader_tail,
                                    format!("response for unknown id {id} dropped"),
                                ),
                            }
                        }
                        IncomingFrame::ServerRequest { id, method, params } => {
                            let request = ServerRequest { id, method, params };
                            if !try_deliver_server_request(&reader_requests, request) {
                                push_diagnostic(
                                    &reader_tail,
                                    "server request dropped: no handler installed or queue full",
                                );
                            }
                        }
                        IncomingFrame::Ignored => {}
                    }
                }
                // 流结束：叫醒所有在途调用方，绝不留人挂死。
                if let Ok(mut map) = reader_pending.lock() {
                    for (_, sender) in map.drain() {
                        let _ = sender.send(Err("MCP server closed the connection".into()));
                    }
                }
            },
        ) {
            Ok(handle) => handle,
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(McpError::new(format!("spawn MCP reader thread: {error}")));
            }
        };

        // stderr 排水线程：持续读并截留到尾缓冲。管道不排会被子进程
        // 写满阻塞（cap 沿用帧上限）；EOF/异常时自然结束，shutdown
        // 与 stdout reader 一并 join。
        let drain_tail = Arc::clone(&stderr_tail);
        let stderr_handle =
            match std::thread::Builder::new()
                .name("mcp-stderr".into())
                .spawn(move || {
                    let mut reader = BufReader::new(stderr);
                    loop {
                        match read_capped_line(&mut reader, MAX_FRAME_BYTES) {
                            Ok(Some(line)) if !line.trim().is_empty() => {
                                push_diagnostic(&drain_tail, line);
                            }
                            // 空行跳过；EOF（Ok(None)）结束排水——continue
                            // 会在流结束后忙循环（100% CPU，2026-08-20
                            // 实测抓到）。
                            Ok(None) => break,
                            Ok(Some(_)) => continue,
                            Err(error) => {
                                push_diagnostic(
                                    &drain_tail,
                                    format!("stderr reader stopping: {error}"),
                                );
                                break;
                            }
                        }
                    }
                }) {
                Ok(handle) => handle,
                Err(error) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    let _ = reader_handle.join();
                    return Err(McpError::new(format!("spawn MCP stderr thread: {error}")));
                }
            };

        // 只允许一个待写帧排队；若服务卡住读取，后续调用快速失败，
        // 不能把最多 4 MiB 的帧无限堆进内存。
        let (writer, writer_rx) = mpsc::sync_channel::<WriterRequest>(1);
        let writer_handle =
            match std::thread::Builder::new()
                .name("mcp-writer".into())
                .spawn(move || {
                    let mut stdin = stdin;
                    while let Ok(request) = writer_rx.recv() {
                        let outcome = stdin
                            .write_all(request.frame.as_bytes())
                            .and_then(|_| stdin.flush())
                            .map_err(|error| format!("write to MCP server: {error}"));
                        let failed = outcome.is_err();
                        let _ = request.result.send(outcome);
                        if failed {
                            break;
                        }
                    }
                }) {
                Ok(handle) => handle,
                Err(error) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    let _ = reader_handle.join();
                    return Err(McpError::new(format!("spawn MCP writer thread: {error}")));
                }
            };

        Ok(Self {
            child,
            writer: Some(writer),
            writer_handle: Some(writer_handle),
            reader_handle: Some(reader_handle),
            stderr_handle: Some(stderr_handle),
            pending,
            server_requests,
            stderr_tail,
            next_id: AtomicU64::new(1),
        })
    }

    /// 安装服务端请求的投递通道（client 的 dispatcher 在 connect
    /// 期调用）；probe/无处理端会话不安装，请求被安全丢弃。
    pub fn install_server_requests(&self, sink: ServerRequestSink) {
        if let Ok(mut slot) = self.server_requests.lock() {
            *slot = Some(sink);
        }
    }

    /// 摘除投递通道：通道断开，dispatcher 的 `recv` 返回 Disconnected
    /// 而退出（shutdown 序：先关 sink 再有界 join dispatcher）。
    pub(crate) fn close_server_requests(&self) {
        if let Ok(mut slot) = self.server_requests.lock() {
            *slot = None;
        }
    }

    /// writer 队列发送端的克隆（dispatcher 写回服务端请求的响应）。
    /// 队列容量 1：调用方须容忍短暂阻塞（writer 持续排空兜底）。
    pub(crate) fn writer_sender(&self) -> Option<mpsc::SyncSender<WriterRequest>> {
        self.writer.clone()
    }

    /// 诊断尾缓冲快照（服务器 stderr + 会话异常，最近 [`STDERR_TAIL_LINES`]
    /// 行）。挂载失败时并入错误消息，供用户排障。
    pub fn stderr_tail(&self) -> Vec<String> {
        self.stderr_tail
            .lock()
            .map(|tail| tail.iter().cloned().collect())
            .unwrap_or_default()
    }

    fn send_frame_until(
        &self,
        frame: String,
        deadline: Instant,
        cancel: Option<&CancelToken>,
    ) -> Result<(), McpError> {
        if frame.len() > MAX_FRAME_BYTES {
            return Err(McpError::new(format!(
                "outbound MCP frame exceeds {MAX_FRAME_BYTES} byte limit"
            )));
        }
        let writer = self
            .writer
            .as_ref()
            .ok_or_else(|| McpError::new("MCP session is shutting down"))?;
        let (result_tx, result_rx) = mpsc::channel();
        writer
            .try_send(WriterRequest {
                frame,
                result: result_tx,
            })
            .map_err(|error| match error {
                mpsc::TrySendError::Full(_) => McpError::new("MCP writer is busy"),
                mpsc::TrySendError::Disconnected(_) => McpError::new("MCP writer closed"),
            })?;
        loop {
            if cancel.is_some_and(CancelToken::is_cancelled) {
                return Err(McpError::new("MCP request cancelled while writing"));
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(McpError::new("write to MCP server timed out"));
            }
            let wait = if cancel.is_some() {
                remaining.min(CANCEL_POLL_INTERVAL)
            } else {
                remaining
            };
            match result_rx.recv_timeout(wait) {
                Ok(Ok(())) => return Ok(()),
                Ok(Err(error)) => return Err(McpError::new(error)),
                Err(mpsc::RecvTimeoutError::Timeout) if Instant::now() < deadline => continue,
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    return Err(McpError::new("write to MCP server timed out"));
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    return Err(McpError::new("MCP writer closed"));
                }
            }
        }
    }

    /// 发送请求并在 `timeout` 内等待同 id 响应。超时即失败并注销
    /// pending 槽位（迟到的响应由 reader 丢弃）。
    pub fn call(&self, method: &str, params: Value, timeout: Duration) -> Result<Value, McpError> {
        self.call_inner(method, params, timeout, None, None)
    }

    /// 发送一个可取消请求。取消令牌触发后，调用在一个短轮询周期内
    /// 返回，并尽力发送标准 `notifications/cancelled` 通知；超时仍是
    /// 最终兜底。迟到响应会因 pending 槽已移除而被安全丢弃。
    pub fn call_cancellable(
        &self,
        method: &str,
        params: Value,
        timeout: Duration,
        cancel: &CancelToken,
    ) -> Result<Value, McpError> {
        self.call_inner(method, params, timeout, Some(cancel), None)
    }

    /// 可延展的调用：截止到达时若 `extend` 返回 true（有服务端请求
    /// 在途——elicitation 等人、sampling 等模型），按步延展截止
    /// （INV-S7）；取消令牌仍即时生效。
    pub fn call_extensible(
        &self,
        method: &str,
        params: Value,
        timeout: Duration,
        cancel: &CancelToken,
        extend: Option<std::sync::Arc<dyn Fn() -> bool + Send + Sync>>,
    ) -> Result<Value, McpError> {
        self.call_inner(method, params, timeout, Some(cancel), extend)
    }

    fn call_inner(
        &self,
        method: &str,
        params: Value,
        timeout: Duration,
        cancel: Option<&CancelToken>,
        extend: Option<std::sync::Arc<dyn Fn() -> bool + Send + Sync>>,
    ) -> Result<Value, McpError> {
        if cancel.is_some_and(CancelToken::is_cancelled) {
            return Err(McpError::new(format!("MCP request `{method}` cancelled")));
        }
        let mut deadline = Instant::now() + timeout;
        let mut extensions = 0usize;
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let frame = request_frame(method, params, id);
        let (sender, receiver) = mpsc::channel();
        self.pending
            .lock()
            .map_err(|_| McpError::new("MCP pending map poisoned"))?
            .insert(id, sender);

        // 必须先注册 pending 再写：极速服务可能在 flush 返回前就响应。
        if let Err(error) = self.send_frame_until(frame, deadline, cancel) {
            if let Ok(mut map) = self.pending.lock() {
                map.remove(&id);
            }
            if cancel.is_some_and(CancelToken::is_cancelled) {
                self.try_cancel_request(id, method);
            }
            return Err(error);
        }
        let outcome = loop {
            if cancel.is_some_and(CancelToken::is_cancelled) {
                break Err(mpsc::RecvTimeoutError::Timeout);
            }
            if Instant::now() >= deadline {
                // 截止已到：有服务端请求在途时按步延展；无延展或到
                // 上限才真正超时（INV-S7）。
                let extendable = extensions < MAX_DEADLINE_EXTENSIONS
                    && extend.as_ref().is_some_and(|probe| probe());
                if extendable {
                    extensions += 1;
                    deadline = Instant::now() + DEADLINE_EXTENSION_STEP;
                } else {
                    break Err(mpsc::RecvTimeoutError::Timeout);
                }
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            let wait = if cancel.is_some() {
                remaining.min(CANCEL_POLL_INTERVAL)
            } else {
                remaining
            };
            match receiver.recv_timeout(wait) {
                Err(mpsc::RecvTimeoutError::Timeout) => continue,
                outcome => break outcome,
            }
        };
        // 无论结果如何都注销槽位，防止迟到响应占用。
        if let Ok(mut map) = self.pending.lock() {
            map.remove(&id);
        }
        match outcome {
            Ok(result) => result.map_err(McpError::new),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout)
                if cancel.is_some_and(CancelToken::is_cancelled) =>
            {
                self.try_cancel_request(id, method);
                Err(McpError::new(format!("MCP request `{method}` cancelled")))
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => Err(McpError::new(format!(
                "MCP request `{method}` timed out after {}s",
                timeout.as_secs()
            ))),
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                Err(McpError::new("MCP connection closed"))
            }
        }
    }

    /// 发送 notification，不等待响应。
    pub fn notify(&self, method: &str, params: Value) -> Result<(), McpError> {
        let frame = notification_frame(method, params);
        self.send_frame_until(frame, Instant::now() + NOTIFY_TIMEOUT, None)
    }

    fn try_cancel_request(&self, id: u64, method: &str) {
        let frame = notification_frame(
            "notifications/cancelled",
            json!({
                "requestId": id,
                "reason": format!("CLAT cancelled `{method}`"),
            }),
        );
        if frame.len() > MAX_FRAME_BYTES {
            return;
        }
        let Some(writer) = &self.writer else {
            return;
        };
        let (result, _ignored) = mpsc::channel();
        let _ = writer.try_send(WriterRequest { frame, result });
    }

    /// Idempotently stop accepting calls, wake pending callers, close stdin,
    /// terminate/reap the child within a bounded grace period, then join both
    /// I/O threads. Normal plugin teardown calls this explicitly; `Drop` is a
    /// best-effort fallback only.
    pub fn shutdown(&mut self) -> Result<(), McpError> {
        self.writer.take();
        if let Ok(mut pending) = self.pending.lock() {
            for (_, sender) in pending.drain() {
                let _ = sender.send(Err("MCP session is shutting down".into()));
            }
        }

        let deadline = Instant::now() + SHUTDOWN_GRACE;
        while Instant::now() < deadline {
            match self.child.try_wait() {
                Ok(Some(_)) => break,
                Ok(None) => std::thread::sleep(Duration::from_millis(50)),
                Err(error) => return Err(McpError::new(format!("wait for MCP server: {error}"))),
            }
        }
        if self
            .child
            .try_wait()
            .map_err(|error| McpError::new(format!("query MCP server: {error}")))?
            .is_none()
        {
            self.child
                .kill()
                .map_err(|error| McpError::new(format!("kill MCP server: {error}")))?;
            self.child
                .wait()
                .map_err(|error| McpError::new(format!("reap MCP server: {error}")))?;
        }
        if let Some(handle) = self.writer_handle.take() {
            handle
                .join()
                .map_err(|_| McpError::new("MCP writer thread panicked"))?;
        }
        if let Some(handle) = self.stderr_handle.take() {
            handle
                .join()
                .map_err(|_| McpError::new("MCP stderr thread panicked"))?;
        }
        if let Some(handle) = self.reader_handle.take() {
            handle
                .join()
                .map_err(|_| McpError::new("MCP reader thread panicked"))?;
        }
        Ok(())
    }
}

impl Drop for StdioSession {
    fn drop(&mut self) {
        let _ = self.shutdown();
    }
}

/// HTTP 会话的响应端点零件（服务端请求的回写通道，client 的
/// Responder 组装用）。
pub(crate) struct HttpResponderParts {
    pub(crate) agent: ureq::Agent,
    pub(crate) url: String,
    pub(crate) headers: Vec<(String, String)>,
    pub(crate) session_id: Arc<Mutex<Option<String>>>,
}

/// Streamable HTTP 会话（MCP 远程传输，2026-08-19 为 GLM Coding Plan
/// 专属 MCP 接入）：每次调用向端点 POST 一条 JSON-RPC，响应体是单个
/// JSON 对象或 SSE 流（逐 `data:` 行，每行一条 JSON-RPC 消息）；
/// initialize 响应携带的 `Mcp-Session-Id` 回显到后续请求。无子进程、
/// 无 reader 线程——超时由 HTTP 客户端的全局截止承担（与 stdio 侧
/// "每次调用有 deadline"的约束同源）。取消只能在请求边界生效（发送
/// 前检查令牌），在途请求由截止兜底。
pub struct HttpSession {
    agent: ureq::Agent,
    url: String,
    headers: Vec<(String, String)>,
    /// 会话 id 共享给响应端点（Arc）：dispatcher 回写响应时与主会话
    /// 看到同一个握手后分配的值。
    session_id: Arc<Mutex<Option<String>>>,
    next_id: AtomicU64,
    /// 服务端请求投递通道（POST 响应流中析出的请求 → dispatcher）。
    server_requests: Arc<Mutex<Option<ServerRequestSink>>>,
}

impl HttpSession {
    /// 建立会话（无握手 IO：首次请求才触网）。`headers` 是调用方配置
    /// 的静态头（如 `Authorization: Bearer …`），逐请求附带。
    pub fn connect(url: &str, headers: &[(String, String)]) -> Result<Self, McpError> {
        if url.trim().is_empty() {
            return Err(McpError::new("MCP http server: empty url"));
        }
        // 连接池长存（复用 TLS 会话）；逐请求的实际截止在 call/notify
        // 里以请求级配置覆盖（与 stdio 侧"每次调用有 deadline"的约束
        // 同源——对抗审计 2026-08-19：notify 缺截止会让挂载线程在挂起
        // 的服务器上无限阻塞）。
        let agent = ureq::Agent::config_builder()
            .http_status_as_error(false)
            .build()
            .new_agent();
        Ok(Self {
            agent,
            url: url.trim().to_owned(),
            headers: headers.to_vec(),
            session_id: Arc::new(Mutex::new(None)),
            next_id: AtomicU64::new(1),
            server_requests: Arc::new(Mutex::new(None)),
        })
    }

    /// 安装服务端请求投递通道（与 stdio 侧对称）。
    pub fn install_server_requests(&self, sink: ServerRequestSink) {
        if let Ok(mut slot) = self.server_requests.lock() {
            *slot = Some(sink);
        }
    }

    pub(crate) fn close_server_requests(&self) {
        if let Ok(mut slot) = self.server_requests.lock() {
            *slot = None;
        }
    }

    /// 响应端点零件（client 的 Responder）：独立 POST 回写服务端
    /// 请求的响应；session_id 与主会话共享。
    pub(crate) fn responder_parts(&self) -> HttpResponderParts {
        HttpResponderParts {
            agent: self.agent.clone(),
            url: self.url.clone(),
            headers: self.headers.clone(),
            session_id: Arc::clone(&self.session_id),
        }
    }

    /// 发送一条 notification（无 id、不期待响应）：POST 一条无 id
    /// 消息，2xx 即算送达（服务器对通知不回响应体）。
    pub fn notify(&self, method: &str, params: Value) -> Result<(), McpError> {
        let body = notification_frame(method, params);
        let mut request = self
            .agent
            .post(&self.url)
            .header("Content-Type", "application/json")
            .header("Accept", "application/json, text/event-stream");
        for (name, value) in &self.headers {
            request = request.header(name.as_str(), value.as_str());
        }
        if let Some(session_id) = self.session_id.lock().ok().and_then(|guard| guard.clone()) {
            request = request.header("Mcp-Session-Id", session_id.as_str());
        }
        // 与 stdio 侧的 NOTIFY_TIMEOUT 同值同义：通知不期待响应，但
        // 发送必须有硬上限，挂起的服务器不能拖死挂载线程。
        let request = request
            .config()
            .timeout_global(Some(NOTIFY_TIMEOUT))
            .timeout_connect(Some(NOTIFY_TIMEOUT.min(HTTP_CONNECT_TIMEOUT)))
            .build();
        let response = request
            .send(body.trim_end())
            .map_err(|error| McpError::new(format!("MCP http notify `{method}`: {error}")))?;
        if !response.status().is_success() {
            return Err(McpError::new(format!(
                "MCP http notify `{method}` returned {}",
                response.status()
            )));
        }
        Ok(())
    }

    /// 发送一个请求并等待响应。SSE 响应流中的 notification 被跳过，
    /// 只取 id 匹配的响应；流结束仍无匹配按超时语义报错。
    pub fn call(&self, method: &str, params: Value, timeout: Duration) -> Result<Value, McpError> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        // request_frame 自带换行（stdio 分帧），HTTP 体不带尾换行。
        let body = request_frame(method, params, id);
        let mut request = self
            .agent
            .post(&self.url)
            .header("Content-Type", "application/json")
            .header("Accept", "application/json, text/event-stream");
        for (name, value) in &self.headers {
            request = request.header(name.as_str(), value.as_str());
        }
        if let Some(session_id) = self.session_id.lock().ok().and_then(|guard| guard.clone()) {
            request = request.header("Mcp-Session-Id", session_id.as_str());
        }
        let request = request
            .config()
            .timeout_global(Some(timeout))
            .timeout_connect(Some(timeout.min(HTTP_CONNECT_TIMEOUT)))
            .build();
        let mut response = request
            .send(body.trim_end())
            .map_err(|error| McpError::new(format!("MCP http request `{method}`: {error}")))?;
        if !response.status().is_success() {
            let status = response.status();
            // W1-11：错误路径同样有界——恶意端点的"错误页"也能灌内存。
            let text =
                read_body_capped(&mut response.body_mut().as_reader(), HTTP_ERROR_BODY_BYTES)
                    .unwrap_or_default();
            return Err(McpError::new(format!(
                "MCP http request `{method}` returned {status}: {}",
                text.trim()
            )));
        }
        // 握手响应可能分配会话 id：只记录一次，后续请求回显。
        if let Some(session_id) = response
            .headers()
            .get("mcp-session-id")
            .and_then(|value| value.to_str().ok())
            .filter(|value| !value.trim().is_empty())
            .map(str::to_owned)
            && let Ok(mut guard) = self.session_id.lock()
            && guard.is_none()
        {
            *guard = Some(session_id);
        }
        let content_type = response
            .headers()
            .get("content-type")
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default()
            .to_ascii_lowercase();
        let body = read_body_capped(&mut response.body_mut().as_reader(), MAX_FRAME_BYTES)
            .map_err(|error| McpError::new(format!("MCP http body `{method}`: {error}")))?;
        if body.len() > MAX_FRAME_BYTES {
            return Err(McpError::new(format!(
                "MCP http response `{method}` exceeds {MAX_FRAME_BYTES} bytes"
            )));
        }
        let messages = if content_type.contains("text/event-stream") {
            parse_sse_messages(&body)
        } else {
            vec![body.as_str()]
        };
        for message in messages {
            match parse_incoming(message) {
                IncomingFrame::Response {
                    id: response_id,
                    outcome,
                } if response_id == id => return outcome.map_err(McpError::new),
                // POST 响应流里夹带的服务端请求（sampling/elicitation）：
                // 投递给 dispatcher；HTTP 无独立 reader 线程，这是唯一
                // 的到达路径（v1 不支持服务器自发 GET 流推送）。
                IncomingFrame::ServerRequest { id, method, params } => {
                    // POST 响应流里夹带的服务端请求（sampling/elicitation）
                    // 投递给 dispatcher；HTTP 无独立 reader 线程，这是唯一
                    // 到达路径（v1 不支持服务器自发 GET 流推送）。投递失败
                    // 静默丢弃（HTTP 侧无诊断尾缓冲），由服务器侧超时兜底。
                    let request = ServerRequest { id, method, params };
                    let _ = try_deliver_server_request(&self.server_requests, request);
                }
                _ => {}
            }
        }
        Err(McpError::new(format!(
            "MCP http response `{method}`: stream ended without a matching response"
        )))
    }
}

/// 解析 SSE 体的 `data:` 载荷行（跨行 payload 不支持——JSON-RPC 消息
/// 是单行 JSON，MCP 规范亦如此分帧）。
pub fn parse_sse_messages(body: &str) -> Vec<&str> {
    body.lines()
        .map(str::trim)
        .filter(|line| line.starts_with("data:"))
        .map(|line| line["data:".len()..].trim_start())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// W1-11：HTTP 响应体读取必须有界——无限数据源下 `read_body_capped`
    /// 必须终止并报超限（先 `take` 后读）。若有人删掉 `.take()` 回到
    /// "无界读入再查长度"的旧形状，本测试会因试图缓冲无限输入而
    /// 挂起/OOM，即为红。
    #[test]
    fn http_body_read_is_bounded_against_an_infinite_source() {
        struct InfiniteBody;
        impl std::io::Read for InfiniteBody {
            fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
                buf.fill(b'a');
                Ok(buf.len())
            }
        }
        let result = read_body_capped(&mut InfiniteBody, MAX_FRAME_BYTES);
        let error = result.expect_err("an infinite body must be rejected, not buffered");
        assert!(
            error.to_string().contains("exceeds"),
            "the failure must name the bound: {error}"
        );
    }

    /// 有界读取的正常路径：cap 内的正文原样返回。
    #[test]
    fn http_body_read_returns_bodies_within_the_cap() {
        let body = "{\"jsonrpc\":\"2.0\",\"id\":1}";
        let read = read_body_capped(&mut std::io::Cursor::new(body.as_bytes()), MAX_FRAME_BYTES)
            .expect("small body reads fine");
        assert_eq!(read, body);
        // cap 恰好等于体长：不视为超限。
        let exact = read_body_capped(&mut std::io::Cursor::new(body.as_bytes()), body.len())
            .expect("exact-cap body is within the bound");
        assert_eq!(exact, body);
        // cap + 1 边界：超一个字节即拒。
        let over = read_body_capped(&mut std::io::Cursor::new(body.as_bytes()), body.len() - 1);
        assert!(over.is_err(), "one byte past the cap must be rejected");
    }

    /// 诊断缓冲：超长行按字节截断、容量满后环形淘汰最旧行。
    #[test]
    fn diagnostics_tail_caps_lines_and_length() {
        let tail: Arc<Mutex<VecDeque<String>>> = Arc::new(Mutex::new(VecDeque::new()));
        push_diagnostic(&tail, "x".repeat(1000));
        {
            let guard = tail.lock().unwrap();
            assert_eq!(guard.len(), 1);
            assert_eq!(guard[0].len(), STDERR_TAIL_LINE_BYTES);
        }
        for index in 0..(STDERR_TAIL_LINES + 5) {
            push_diagnostic(&tail, format!("line-{index}"));
        }
        let guard = tail.lock().unwrap();
        assert_eq!(guard.len(), STDERR_TAIL_LINES);
        assert_eq!(guard.front().unwrap(), "line-5");
        assert_eq!(
            guard.back().unwrap(),
            &format!("line-{}", STDERR_TAIL_LINES + 4)
        );
    }

    /// SSE 体解析：只取 `data:` 载荷行，`event:`/注释行/空行跳过。
    #[test]
    fn parses_sse_data_payloads_only() {
        let body = "event: message\ndata: {\"jsonrpc\":\"2.0\",\"id\":1}\n\n: keep-alive comment\ndata: {\"jsonrpc\":\"2.0\",\"id\":2}\n\n";
        assert_eq!(
            parse_sse_messages(body),
            vec![
                "{\"jsonrpc\":\"2.0\",\"id\":1}",
                "{\"jsonrpc\":\"2.0\",\"id\":2}"
            ]
        );
        assert!(parse_sse_messages("").is_empty());
    }

    #[test]
    fn frames_are_single_line_json_rpc() {
        let request = request_frame("tools/list", json!({}), 7);
        assert!(request.ends_with('\n'));
        assert_eq!(request.matches('\n').count(), 1);
        let value: Value = serde_json::from_str(request.trim()).unwrap();
        assert_eq!(value["jsonrpc"], "2.0");
        assert_eq!(value["id"], 7);
        assert_eq!(value["method"], "tools/list");

        let notification = notification_frame("notifications/initialized", json!({}));
        let value: Value = serde_json::from_str(notification.trim()).unwrap();
        assert!(value.get("id").is_none());
    }

    #[test]
    fn parses_results_errors_and_ignores_notifications() {
        let (id, result) =
            parse_response(r#"{"jsonrpc":"2.0","id":3,"result":{"tools":[]}}"#).unwrap();
        assert_eq!(id, 3);
        assert_eq!(result.unwrap()["tools"], serde_json::json!([]));

        let (id, error) = parse_response(
            r#"{"jsonrpc":"2.0","id":4,"error":{"code":-32601,"message":"method not found"}}"#,
        )
        .unwrap();
        assert_eq!(id, 4);
        assert!(error.unwrap_err().contains("-32601"));

        // notification（无 id）与坏行被忽略。
        assert!(parse_response(r#"{"jsonrpc":"2.0","method":"progress"}"#).is_none());
        assert!(parse_response("not json").is_none());
        assert!(parse_response("").is_none());
    }

    /// 帧分类（2026-08-21 修复的规格）：`method`+`id` 是服务端请求——
    /// pre-fix 实现把这类帧当响应解析（id 撞上在途 pending 时错配成
    /// Null 结果），本测试在旧代码上红。
    #[test]
    fn incoming_frames_classify_requests_responses_and_notifications() {
        let response = r#"{"jsonrpc":"2.0","id":7,"result":{"ok":true}}"#;
        match parse_incoming(response) {
            IncomingFrame::Response { id, outcome } => {
                assert_eq!(id, 7);
                assert_eq!(outcome.unwrap()["ok"], serde_json::json!(true));
            }
            other => panic!("expected a response frame, got {other:?}"),
        }

        // 服务端请求：数字 id 与字符串 id 都合法，method/params 原样。
        let request = r#"{"jsonrpc":"2.0","id":5,"method":"sampling/createMessage","params":{"maxTokens":16}}"#;
        match parse_incoming(request) {
            IncomingFrame::ServerRequest { id, method, params } => {
                assert_eq!(id, serde_json::json!(5));
                assert_eq!(method, "sampling/createMessage");
                assert_eq!(params["maxTokens"], serde_json::json!(16));
            }
            other => panic!("expected a server request, got {other:?}"),
        }
        let string_id =
            r#"{"jsonrpc":"2.0","id":"req-1","method":"elicitation/create","params":{}}"#;
        match parse_incoming(string_id) {
            IncomingFrame::ServerRequest { id, method, .. } => {
                assert_eq!(id, serde_json::json!("req-1"));
                assert_eq!(method, "elicitation/create");
            }
            other => panic!("expected a server request, got {other:?}"),
        }

        // notification（method 无 id）与坏行被忽略。
        assert!(matches!(
            parse_incoming(r#"{"jsonrpc":"2.0","method":"notifications/progress"}"#),
            IncomingFrame::Ignored
        ));
        assert!(matches!(parse_incoming("garbage"), IncomingFrame::Ignored));

        // 关键回归：请求帧绝不能被 parse_response 当作响应。
        assert!(parse_response(request).is_none());
    }

    /// 响应帧构造：result 与 error 两种形态都是单行 JSON-RPC，id 原样
    /// 回显（含字符串 id）。
    #[test]
    fn response_frames_are_single_line_and_echo_the_id() {
        let ok = response_frame(
            &serde_json::json!(9),
            &Ok(serde_json::json!({"done": true})),
        );
        assert!(ok.ends_with('\n'));
        assert_eq!(ok.matches('\n').count(), 1);
        let value: Value = serde_json::from_str(ok.trim()).unwrap();
        assert_eq!(value["jsonrpc"], "2.0");
        assert_eq!(value["id"], 9);
        assert_eq!(value["result"]["done"], true);
        assert!(value.get("error").is_none());

        let error = response_frame(
            &serde_json::json!("req-1"),
            &Err((-32601, "method not found".to_owned())),
        );
        let value: Value = serde_json::from_str(error.trim()).unwrap();
        assert_eq!(value["id"], "req-1");
        assert_eq!(value["error"]["code"], -32601);
        assert_eq!(value["error"]["message"], "method not found");
    }

    /// stderr 截留（2026-08-20 修复回归）：子进程的 stderr 必须被
    /// 排水线程收进尾缓冲——既不直通终端（会把 TUI 顶出可视区），
    /// 也不因管道无人读而阻塞子进程。
    #[test]
    #[ignore = "spawns a python3 subprocess; run explicitly with --ignored"]
    fn server_stderr_is_captured_not_inherited() {
        let script = r#"
import sys, time
print('noise line 1', file=sys.stderr, flush=True)
print('noise line 2', file=sys.stderr, flush=True)
time.sleep(300)
"#;
        let session = StdioSession::spawn(
            "python3",
            &["-c".to_owned(), script.to_owned()],
            &[],
            std::path::Path::new("/tmp"),
        )
        .expect("spawn");
        // 排水是异步的：轮询等待两条噪声进入尾缓冲（有界预算）。
        let mut seen = Vec::new();
        for _ in 0..100 {
            seen = session.stderr_tail();
            if seen.iter().filter(|line| line.contains("noise")).count() >= 2 {
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(
            seen.iter().any(|line| line.contains("noise line 1")),
            "stderr must be captured into the tail: {seen:?}"
        );
        assert!(
            seen.iter().any(|line| line.contains("noise line 2")),
            "the drain thread must keep reading: {seen:?}"
        );
        drop(session);
    }

    /// 沉默服务（不输出任何行、也不退出）：调用必须在超时内失败，
    /// 会话 Drop 必须在宽限期内回收子进程——不挂死。
    #[test]
    #[ignore = "spawns a python3 subprocess; run explicitly with --ignored"]
    fn silent_server_times_out_and_drop_reclaims() {
        let started = Instant::now();
        let session = StdioSession::spawn(
            "python3",
            &[
                "-c".to_owned(),
                "import sys, time; time.sleep(300)".to_owned(),
            ],
            &[],
            std::path::Path::new("/tmp"),
        )
        .expect("spawn");
        let outcome = session.call("initialize", json!({}), Duration::from_secs(2));
        assert!(outcome.is_err());
        assert!(outcome.unwrap_err().to_string().contains("timed out"));
        // Drop 在宽限期（3s）+ 少量余量内完成。
        drop(session);
        assert!(started.elapsed() < Duration::from_secs(8), "drop hung");
    }

    /// 半行后 EOF（服务写出无换行片段即退出）：宽松交付该行，随后的
    /// 调用立即收到连接关闭而不是挂死。
    #[test]
    #[ignore = "spawns a python3 subprocess; run explicitly with --ignored"]
    fn eof_after_partial_line_fails_pending_calls() {
        let script = "import sys; sys.stdout.write('{\"jsonrpc\":\"2.0\"'); sys.stdout.flush()";
        let session = StdioSession::spawn(
            "python3",
            &["-c".to_owned(), script.to_owned()],
            &[],
            std::path::Path::new("/tmp"),
        )
        .expect("spawn");
        let outcome = session.call("tools/list", json!({}), Duration::from_secs(5));
        // 半行 JSON 解析失败属 notification/坏行 → reader 循环读下一条
        // 遇 EOF → pending 收到 closed。
        assert!(outcome.is_err());
    }

    /// 服务不读取 stdin 时，管道写满会阻塞 writer；call 的 deadline
    /// 必须覆盖写入阶段，不能等 write_all 返回后才开始计时。
    #[test]
    #[ignore = "spawns a python3 subprocess; run explicitly with --ignored"]
    fn blocked_stdin_write_respects_call_deadline() {
        let session = StdioSession::spawn(
            "python3",
            &["-c".to_owned(), "import time; time.sleep(300)".to_owned()],
            &[],
            std::path::Path::new("/tmp"),
        )
        .expect("spawn");
        let started = Instant::now();
        let outcome = session.call(
            "tools/call",
            json!({"payload": "x".repeat(2 * 1024 * 1024)}),
            Duration::from_millis(250),
        );
        assert!(outcome.is_err());
        assert!(started.elapsed() < Duration::from_secs(2));
    }

    /// INV-S7（等人不死）：服务端请求在途时（oracle = true），调用
    /// 截止按步延展——基础超时 300ms 挡不住 1.2s 才到的响应；oracle
    /// = false 时同一延迟必须如约超时。pre-fix（无延展）第一段红。
    #[test]
    #[ignore = "spawns a python3 subprocess; run explicitly with --ignored"]
    fn pending_server_requests_extend_the_call_deadline() {
        let script = r#"
import json, sys, time
def send(obj):
    sys.stdout.write(json.dumps(obj) + "\n")
    sys.stdout.flush()
for line in sys.stdin:
    msg = json.loads(line)
    if "id" not in msg:
        continue
    if msg.get("method") == "tools/call":
        time.sleep(1.2)
        send({"jsonrpc": "2.0", "id": msg["id"],
              "result": {"content": [{"type": "text", "text": "done"}]}})
    else:
        send({"jsonrpc": "2.0", "id": msg["id"],
              "error": {"code": -32601, "message": "no"}})
"#;
        let session = StdioSession::spawn(
            "python3",
            &["-c".to_owned(), script.to_owned()],
            &[],
            std::path::Path::new("/tmp"),
        )
        .expect("spawn");
        let cancel = CancelToken::new();

        let extend = Arc::new(|| true) as Arc<dyn Fn() -> bool + Send + Sync>;
        let outcome = session.call_extensible(
            "tools/call",
            json!({"name": "slow"}),
            Duration::from_millis(300),
            &cancel,
            Some(extend),
        );
        assert_eq!(
            outcome.expect("extended call succeeds").get("content"),
            Some(&json!([{"type": "text", "text": "done"}])),
            "a pending server request must push the deadline forward"
        );

        let idle = Arc::new(|| false) as Arc<dyn Fn() -> bool + Send + Sync>;
        let outcome = session.call_extensible(
            "tools/call",
            json!({"name": "slow"}),
            Duration::from_millis(300),
            &cancel,
            Some(idle),
        );
        let error = outcome.expect_err("unextended call must time out");
        assert!(error.to_string().contains("timed out"), "{error}");
    }

    /// 取消令牌必须打断正在等待的 MCP 调用，并尽力把标准 cancellation
    /// notification 发给仍在处理请求的服务端。
    #[test]
    #[ignore = "spawns a python3 subprocess; run explicitly with --ignored"]
    fn cancellation_interrupts_call_and_notifies_server() {
        let marker = std::env::temp_dir().join(format!(
            "clat-mcp-cancel-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        ));
        let script = r#"
import json, sys
pending = None
for line in sys.stdin:
    msg = json.loads(line)
    if msg.get("method") == "tools/call" and "id" in msg:
        pending = msg["id"]
    elif msg.get("method") == "notifications/cancelled":
        if msg.get("params", {}).get("requestId") == pending:
            with open(sys.argv[1], "w") as marker:
                marker.write("cancelled")
            break
"#;
        let session = StdioSession::spawn(
            "python3",
            &[
                "-c".to_owned(),
                script.to_owned(),
                marker.to_string_lossy().into_owned(),
            ],
            &[],
            std::path::Path::new("/tmp"),
        )
        .expect("spawn");
        let cancel = CancelToken::new();
        let trigger = cancel.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(100));
            trigger.cancel();
        });
        let started = Instant::now();
        let error = session
            .call_cancellable(
                "tools/call",
                json!({"name": "slow"}),
                Duration::from_secs(10),
                &cancel,
            )
            .expect_err("cancelled call");
        assert!(error.to_string().contains("cancel"), "{error}");
        assert!(started.elapsed() < Duration::from_secs(1));

        for _ in 0..20 {
            if marker.exists() {
                break;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        assert_eq!(
            std::fs::read_to_string(&marker).expect("server observed cancellation"),
            "cancelled"
        );
        let _ = std::fs::remove_file(marker);
    }
}
