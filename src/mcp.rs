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

use serde_json::{Value, json};
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdout, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// 单帧（一行 JSON-RPC）的最大字节数。超限即协议违约。
pub const MAX_FRAME_BYTES: usize = 4 * 1024 * 1024;

/// Drop 时等待子进程优雅退出的宽限期，之后强杀。
const SHUTDOWN_GRACE: Duration = Duration::from_secs(3);

/// Notification 没有调用方提供的 deadline，仍需给写入设置硬上限。
const NOTIFY_TIMEOUT: Duration = Duration::from_secs(10);

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

/// 解析一行收到的消息，返回 (id, result/error)。notification 与无法
/// 解析的行返回 None。
pub fn parse_response(line: &str) -> Option<(u64, Result<Value, String>)> {
    let value: Value = serde_json::from_str(line.trim()).ok()?;
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

/// 读取一行（到 `\n`），累计字节超过 `cap` 报错。基于 `fill_buf`
/// 实现，不在内存里无限累积无换行的输入。
fn read_capped_line(
    reader: &mut BufReader<ChildStdout>,
    cap: usize,
) -> std::io::Result<Option<String>> {
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

struct WriterRequest {
    frame: String,
    result: mpsc::Sender<Result<(), String>>,
}

/// 一个 MCP stdio 服务器子进程会话。
pub struct StdioSession {
    child: Child,
    /// 所有写入交给单独线程串行执行，调用线程只做有 deadline 的等待。
    /// Drop 时丢弃 sender；writer 队列耗尽后关闭 stdin。
    writer: Option<mpsc::SyncSender<WriterRequest>>,
    writer_handle: Option<JoinHandle<()>>,
    /// 每个在途请求的响应回传通道，按 id 注册，由 reader 线程消费。
    pending: Arc<Mutex<PendingMap>>,
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
            // stderr 直通父进程：服务器自身的日志透给用户终端，
            // 不污染 JSON-RPC 通道。
            .stderr(Stdio::inherit());
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

        let pending: Arc<Mutex<PendingMap>> = Arc::new(Mutex::new(HashMap::new()));
        let reader_pending = Arc::clone(&pending);
        if let Err(error) = std::thread::Builder::new()
            .name("mcp-reader".into())
            .spawn(move || {
                let mut reader = BufReader::new(stdout);
                loop {
                    let line = match read_capped_line(&mut reader, MAX_FRAME_BYTES) {
                        Ok(Some(line)) => line,
                        Ok(None) => break,
                        Err(error) => {
                            eprintln!("clat: mcp reader stopping: {error}");
                            break;
                        }
                    };
                    let Some((id, outcome)) = parse_response(&line) else {
                        continue; // notification 或坏行
                    };
                    let slot = reader_pending
                        .lock()
                        .ok()
                        .and_then(|mut map| map.remove(&id));
                    match slot {
                        Some(sender) => {
                            let _ = sender.send(outcome);
                        }
                        // 未知 id：超时后被放弃的请求的迟到响应，
                        // 或服务端违反协议。记录但不中断会话。
                        None => eprintln!("clat: mcp response for unknown id {id} dropped"),
                    }
                }
                // 流结束：叫醒所有在途调用方，绝不留人挂死。
                if let Ok(mut map) = reader_pending.lock() {
                    for (_, sender) in map.drain() {
                        let _ = sender.send(Err("MCP server closed the connection".into()));
                    }
                }
            })
        {
            let _ = child.kill();
            let _ = child.wait();
            return Err(McpError::new(format!("spawn MCP reader thread: {error}")));
        }

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
                    return Err(McpError::new(format!("spawn MCP writer thread: {error}")));
                }
            };

        Ok(Self {
            child,
            writer: Some(writer),
            writer_handle: Some(writer_handle),
            pending,
            next_id: AtomicU64::new(1),
        })
    }

    fn send_frame_until(&self, frame: String, deadline: Instant) -> Result<(), McpError> {
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
        let remaining = deadline.saturating_duration_since(Instant::now());
        match result_rx.recv_timeout(remaining) {
            Ok(Ok(())) => Ok(()),
            Ok(Err(error)) => Err(McpError::new(error)),
            Err(mpsc::RecvTimeoutError::Timeout) => {
                Err(McpError::new("write to MCP server timed out"))
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => Err(McpError::new("MCP writer closed")),
        }
    }

    /// 发送请求并在 `timeout` 内等待同 id 响应。超时即失败并注销
    /// pending 槽位（迟到的响应由 reader 丢弃）。
    pub fn call(&self, method: &str, params: Value, timeout: Duration) -> Result<Value, McpError> {
        let deadline = Instant::now() + timeout;
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let frame = request_frame(method, params, id);
        let (sender, receiver) = mpsc::channel();
        self.pending
            .lock()
            .map_err(|_| McpError::new("MCP pending map poisoned"))?
            .insert(id, sender);

        // 必须先注册 pending 再写：极速服务可能在 flush 返回前就响应。
        if let Err(error) = self.send_frame_until(frame, deadline) {
            if let Ok(mut map) = self.pending.lock() {
                map.remove(&id);
            }
            return Err(error);
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        let outcome = receiver.recv_timeout(remaining);
        // 无论结果如何都注销槽位，防止迟到响应占用。
        if let Ok(mut map) = self.pending.lock() {
            map.remove(&id);
        }
        match outcome {
            Ok(result) => result.map_err(McpError::new),
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
        self.send_frame_until(frame, Instant::now() + NOTIFY_TIMEOUT)
    }
}

impl Drop for StdioSession {
    fn drop(&mut self) {
        // 1) 关闭 writer 队列；writer 退出时销毁 stdin，触发服务优雅退出。
        self.writer.take();
        // 2) 宽限期内轮询 try_wait。
        let deadline = Instant::now() + SHUTDOWN_GRACE;
        while Instant::now() < deadline {
            match self.child.try_wait() {
                Ok(Some(_)) => break,
                Ok(None) => std::thread::sleep(Duration::from_millis(50)),
                Err(_) => break,
            }
        }
        // 3) 若仍存活则强杀并回收；kill 会解除 writer 的阻塞写。
        if self.child.try_wait().ok().flatten().is_none() {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
        if let Some(handle) = self.writer_handle.take() {
            let _ = handle.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
