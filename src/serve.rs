//! `clat serve`：本地 HTTP+SSE 前端（docs/todo/serve-rpc.md，PWA-3）。
//!
//! 给已存在的 Application facade 开一扇网络的窗：四象限 RPC、事件流
//! 即 PWA-1 的 v1 wire 换载体（INV-S2 零转译）、审批回调即 approver
//! 注入点的网络化（INV-S3）。全部新代码落在本前端层，依赖方向
//! serve → core 单向。
//!
//! 不变量（INV-S1…S8，oracle 见设计文档 §10）：
//! - INV-S1 三闸 fail-closed：只绑 127.0.0.1（无 `--host`——安全边界
//!   不是配置项）；每请求 token 精确匹配（不落盘、不进日志）；带
//!   Origin 必属允许集，不发 CORS 头。
//! - INV-S6 单 run 互斥：busy 即拒；每次受理恰一 `prompt.settled`。
//! - INV-S7 背压有界：SSE 每连接 1024 帧，满即断连；run worker 永不
//!   因慢消费者阻塞。
//! - INV-S8 依赖零新增：无 HTTP 栈/异步运行时/嵌入式资产依赖。
//!
//! 本模块不安装进程级信号处理器：关停旗由进程边界（main.rs 的
//! Ctrl-C 处理器）注入（exec 同款纪律）。

pub(crate) mod approver;
mod http;
pub(crate) mod protocol;
pub(crate) mod shapes;
mod sse;
mod state;
#[cfg(test)]
mod tests;
mod web_assets;

use crate::{BootstrapApplication, Project, TrustedProjectApplication};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

/// `clat serve` 的已解析参数。
#[derive(Clone, Debug, Default)]
pub struct ServeArgs {
    /// 绑定端口；0 = OS 自动分配（缺省，打印实际端口）。
    pub port: u16,
    /// 显式 token（脚本/测试）；缺省进程启动时生成 uuid v4。
    pub token: Option<String>,
}

/// 解析 `clat serve` 之后的参数。用法错误返回 `Err(message)`（退出码 2）。
/// 注意：`--host` 落到 unknown option 分支——放宽绑定是产品决策，
/// 不是配置项（INV-S1a，设计文档 §14-5 否决记录）。
pub fn parse_serve_args<I>(args: I) -> Result<ServeArgs, String>
where
    I: IntoIterator<Item = String>,
{
    let mut parsed = ServeArgs::default();
    let mut iter = args.into_iter();
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--port" => {
                let value = iter
                    .next()
                    .ok_or_else(|| "--port requires a number".to_string())?;
                parsed.port = value
                    .parse()
                    .map_err(|_| format!("invalid port: {value}"))?;
            }
            "--token" => {
                let value = iter
                    .next()
                    .ok_or_else(|| "--token requires a value".to_string())?;
                if value.trim().is_empty() {
                    return Err("invalid token: empty".into());
                }
                parsed.token = Some(value);
            }
            "--" => return Err("serve takes no positional arguments".into()),
            other => return Err(format!("unknown option: {other}")),
        }
    }
    Ok(parsed)
}

/// 生产入口（main.rs 挂 `Some("serve")`）。阻塞直到关停旗置位。
pub fn run_serve_with_shutdown(args: ServeArgs, shutdown: Arc<AtomicBool>) -> i32 {
    let project = match Project::current() {
        Ok(project) => project,
        Err(error) => {
            eprintln!("clat: could not determine current project: {error}");
            return 1;
        }
    };
    let handle = match serve_with_with_queue(
        project,
        None,
        args,
        |bootstrap| bootstrap.into_trusted(),
        shutdown,
        state::SUBSCRIBER_QUEUE_FRAMES,
    ) {
        Ok(handle) => handle,
        Err(error) => {
            eprintln!("clat: {error}");
            return 1;
        }
    };
    // 启动横幅（§2.3）：完整入口 URL 只进终端（本机用户可见 = 设计内）。
    println!(
        "clat serve listening on http://127.0.0.1:{}?t={}",
        handle.addr.port(),
        handle.token
    );
    println!("Press Ctrl-C to stop.");
    handle.join();
    0
}

pub struct ServeHandle {
    pub addr: SocketAddr,
    pub token: String,
    shutdown: Arc<AtomicBool>,
    /// 显式应用 close 的结果（accept 线程写入，`join()` 后读取）：
    /// `Some` = 归一成功、显式 close 已执行（Ok/Err 皆算）；
    /// `None` = 归一失败走了降级路径（残留连接/worker 超宽限期），
    /// 应用由 Drop 兜底关闭。
    close_outcome: Arc<Mutex<Option<Result<(), String>>>>,
    join: JoinHandle<()>,
}

impl ServeHandle {
    /// 请求关停（accept 循环检测到后走完整关停序列）。
    pub fn shutdown(&self) {
        self.shutdown.store(true, Ordering::SeqCst);
    }

    /// 阻塞到 serve 完整退出（连接/worker 有界 join、应用显式 close），
    /// 返回显式 close 的结果（语义见 `close_outcome` 字段注释）。
    pub fn join(self) -> Option<Result<(), String>> {
        let Self {
            join,
            close_outcome,
            ..
        } = self;
        let _ = join.join();
        close_outcome
            .lock()
            .expect("serve close outcome lock")
            .clone()
    }

    pub fn port(&self) -> u16 {
        self.addr.port()
    }

    /// 显式 close 的结果（`join()` 之后读取；语义见字段注释）。
    pub fn close_outcome(&self) -> Option<Result<(), String>> {
        self.close_outcome
            .lock()
            .expect("serve close outcome lock")
            .clone()
    }
}

/// 组装并启动 serve（测试缝：`into_trusted` 注入 TestProvider，与
/// exec 的 `exec_with` 同款）。绑定成功即返回句柄；accept 循环在
/// 后台线程，随 `shutdown` 旗退出。
pub(crate) fn serve_with_with_queue<F>(
    project: Project,
    storage_root: Option<PathBuf>,
    args: ServeArgs,
    into_trusted: F,
    shutdown: Arc<AtomicBool>,
    queue_frames: usize,
) -> Result<ServeHandle, String>
where
    F: FnOnce(BootstrapApplication) -> Result<TrustedProjectApplication, crate::ApplicationError>,
{
    let bootstrap = match storage_root {
        Some(root) => BootstrapApplication::open(project.clone(), root),
        None => BootstrapApplication::open_default(project.clone()),
    };
    let bootstrap = bootstrap.map_err(|error| error.to_string())?;
    let trusted = match bootstrap.is_trusted() {
        Ok(true) => into_trusted(bootstrap).map_err(|error| error.to_string())?,
        Ok(false) => {
            // serve 是常驻服务，不做信任授权交互——信任动作应发生在
            // 发起它的终端上下文（§9）。
            return Err("project is not trusted — trust it once from a terminal \
                 (`clat exec --trust` or open `clat` here), then start serve"
                .into());
        }
        Err(error) => return Err(error.to_string()),
    };
    let token = args
        .token
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    // INV-S1a：只绑 IPv4 loopback。
    let listener = TcpListener::bind(("127.0.0.1", args.port))
        .map_err(|error| format!("could not bind 127.0.0.1:{}: {error}", args.port))?;
    listener
        .set_nonblocking(true)
        .map_err(|error| format!("could not set nonblocking: {error}"))?;
    let addr = listener
        .local_addr()
        .map_err(|error| format!("could not read local addr: {error}"))?;

    let app = Arc::new(Mutex::new(trusted));
    let mut shared = state::ServeShared::new(Arc::clone(&app), token.clone(), addr.port());
    shared.queue_frames = queue_frames;
    let shared = Arc::new(shared);
    shared.spawn_notice_forwarder();

    let accept_shared = Arc::clone(&shared);
    let accept_shutdown = Arc::clone(&shutdown);
    let close_outcome = Arc::new(Mutex::new(None));
    let accept_close_outcome = Arc::clone(&close_outcome);
    let join = std::thread::Builder::new()
        .name("clat-serve-accept".into())
        .spawn(move || {
            accept_loop(
                listener,
                accept_shared,
                app,
                accept_shutdown,
                accept_close_outcome,
            );
        })
        .map_err(|error| format!("could not spawn accept loop: {error}"))?;
    Ok(ServeHandle {
        addr,
        token,
        shutdown,
        close_outcome,
        join,
    })
}

fn accept_loop(
    listener: TcpListener,
    shared: Arc<state::ServeShared>,
    app: Arc<Mutex<TrustedProjectApplication>>,
    shutdown: Arc<AtomicBool>,
    close_outcome: Arc<Mutex<Option<Result<(), String>>>>,
) {
    loop {
        if shutdown.load(Ordering::SeqCst) {
            break;
        }
        match listener.accept() {
            Ok((stream, _)) => {
                let _ = stream.set_nonblocking(false);
                let conn_shared = Arc::clone(&shared);
                if let Ok(handle) = std::thread::Builder::new()
                    .name("clat-serve-conn".into())
                    .spawn(move || handle_connection(stream, conn_shared))
                {
                    shared.register_connection(handle);
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(100));
            }
            Err(_) => break,
        }
    }

    // 关停序列（2026-08-23 修复：归一是有前提的，顺序错了 try_unwrap
    // 结构性必败——close 沦为 Drop 兜底，错误被吞且每次关停误报
    // "could not close application cleanly"）：
    // ①停收新请求；②取消在飞 run、清订阅者；③有界 join 连接与
    //   worker——notice 转发与 settler 各持 `Arc<ServeShared>`（内嵌
    //   app 克隆），join 完才释放；④显式 drop 本线程的 shared。此刻
    //   app 的 Arc 才真正归一，try_unwrap 成功、显式 close 执行且
    //   错误可上报。残留连接/worker（超宽限期）会让归一失败——降级
    //   打印，应用随后由 Drop 兜底关闭（进程即将退出，错误被吞）。
    shared.mark_shutting_down();
    shared.cancel_active_run();
    shared.clear_subscribers();
    shared.drain_connections();
    shared.drain_workers();
    drop(shared);
    match Arc::try_unwrap(app)
        .ok()
        .and_then(|mutex| mutex.into_inner().ok())
    {
        Some(application) => {
            let outcome = application.close().map_err(|error| error.to_string());
            if let Err(error) = &outcome {
                eprintln!("clat: serve: application close failed: {error}");
            }
            *close_outcome.lock().expect("serve close outcome lock") = Some(outcome);
        }
        None => eprintln!("clat: serve: could not close application cleanly"),
    }
}

/// 每连接处理：读请求 → 三闸 → 路由。任何错误路径都写一个 4xx/5xx
/// JSON 应答再断连（fail-closed，不静默）。
fn handle_connection(mut stream: TcpStream, shared: Arc<state::ServeShared>) {
    let request = match http::read_request(&mut stream) {
        Ok(request) => request,
        Err(http::HttpReadError::TooLarge(part)) => {
            let _ = write_json(
                &mut stream,
                413,
                protocol::rpc_result_json(&Err(protocol::RpcError::bad_request(format!(
                    "{part} exceeds the size limit"
                )))),
            );
            return;
        }
        Err(http::HttpReadError::TimedOut) | Err(http::HttpReadError::Closed) => return,
        Err(http::HttpReadError::BadRequest(reason)) => {
            let _ = write_json(
                &mut stream,
                400,
                protocol::rpc_result_json(&Err(protocol::RpcError::bad_request(reason))),
            );
            return;
        }
        Err(http::HttpReadError::Io) => return,
    };

    // INV-S1c Origin 闸：带头必属允许集（跨站 fetch/form 必带 Origin
    // 且为外域值——此闸对浏览器内恶意页成立）；不带（curl/同源导航）
    // 放行至 token 闸。
    if let Some(origin) = &request.origin {
        let allowed = [
            format!("http://localhost:{}", shared.port),
            format!("http://127.0.0.1:{}", shared.port),
        ];
        if !allowed.contains(origin) {
            let _ = write_json(
                &mut stream,
                403,
                protocol::rpc_result_json(&Err(protocol::RpcError {
                    code: protocol::ErrorCode::Forbidden,
                    message: "origin is not allowed".into(),
                })),
            );
            return;
        }
    }

    // INV-S1b token 闸：Bearer 或 ?t=，精确匹配。PWA2-02：POST API
    // 只认 Authorization 头——query token 是 GET/SSE 的引导通道
    //（EventSource 不能带 header），凭据不应进入会随 URL 泄漏的面
    // （历史/Referer/截图）。
    let provided = http::bearer_token(request.authorization.as_deref())
        .or_else(|| http::query_value(request.query.as_deref(), "t"));
    if provided.as_deref() != Some(shared.token.as_str())
        || (request.method == "POST"
            && http::bearer_token(request.authorization.as_deref()).is_none())
    {
        let _ = write_json(
            &mut stream,
            401,
            protocol::rpc_result_json(&Err(protocol::RpcError {
                code: protocol::ErrorCode::Unauthorized,
                message: if request.method == "POST" {
                    "missing or invalid token (POST requires the Authorization header)"
                } else {
                    "missing or invalid token"
                }
                .into(),
            })),
        );
        return;
    }

    match request.method.as_str() {
        "GET" => match request.path.as_str() {
            "/api/events" => sse::handle(&mut stream, &shared),
            path => match web_assets::asset(path, &shared.token) {
                Some((bytes, content_type)) => {
                    let _ = http::write_response(&mut stream, 200, content_type, &bytes);
                }
                None => {
                    let _ = write_json(
                        &mut stream,
                        404,
                        protocol::rpc_result_json(&Err(protocol::RpcError::not_found(format!(
                            "no such path: {path}"
                        )))),
                    );
                }
            },
        },
        "POST" => match request.path.strip_prefix("/api/") {
            Some(method) if !method.is_empty() => {
                let params: serde_json::Value = if request.body.is_empty() {
                    serde_json::json!({})
                } else {
                    match serde_json::from_slice(&request.body) {
                        Ok(value) => value,
                        Err(error) => {
                            let _ = write_json(
                                &mut stream,
                                400,
                                protocol::rpc_result_json(&Err(protocol::RpcError::bad_request(
                                    format!("params are not valid JSON: {error}"),
                                ))),
                            );
                            return;
                        }
                    }
                };
                let outcome = protocol::dispatch(method, &params, &shared);
                let _ = write_json(&mut stream, 200, protocol::rpc_result_json(&outcome));
            }
            _ => {
                let _ = write_json(
                    &mut stream,
                    404,
                    protocol::rpc_result_json(&Err(protocol::RpcError::not_found(
                        "no such path: /api/",
                    ))),
                );
            }
        },
        // http 层把非 GET/POST 的方法原样上交（parse_head），此处 405。
        _ => {
            let _ = write_json(
                &mut stream,
                405,
                protocol::rpc_result_json(&Err(protocol::RpcError::bad_request(
                    "only GET and POST are supported",
                ))),
            );
        }
    }
}

fn write_json(stream: &mut TcpStream, status: u16, body: String) -> std::io::Result<()> {
    http::write_response(stream, status, "application/json", body.as_bytes())
}
