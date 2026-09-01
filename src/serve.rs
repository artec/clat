//! `clat serve`：本地 HTTP+SSE 前端（docs/todo/serve-rpc.md，PWA-3）。
//!
//! 给已存在的 Application facade 开一扇网络的窗：四象限 RPC、事件流
//! 即 PWA-1 的 v1 wire 换载体（INV-S2 零转译）、审批回调即 approver
//! 注入点的网络化（INV-S3）。全部新代码落在本前端层，依赖方向
//! serve → core 单向。
//!
//! 不变量（INV-S1…S8，oracle 见设计文档 §10）：
//! - INV-S1 三闸 fail-closed：只绑 127.0.0.1（无 `--host`——安全边界
//!   不是配置项）；API 请求必须精确匹配 Bearer；缺省
//!   token 仅持久化为 `~/.clat/web-token` 0600，不进 URL/日志/journal；
//!   带 Origin 必属允许集，不发 CORS 头。
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
mod token;
mod web_assets;
mod wechat;

use crate::{BootstrapApplication, Project, TrustedProjectApplication};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

/// `clat serve` 的已解析参数。
pub const DEFAULT_SERVE_PORT: u16 = 2691;

/// Terminal control surface for the core-owned WeChat binding/pairing state.
/// QR payloads are rendered directly and never printed as text or persisted.
pub fn run_wechat_command<I>(project: Project, args: I) -> u8
where
    I: IntoIterator<Item = String>,
{
    let words = args.into_iter().collect::<Vec<_>>();
    let action = match words.first().map(String::as_str) {
        Some("bind") => "bind",
        Some("status") => "status",
        Some("pair") => "pair",
        Some("unbind") => "unbind",
        Some(other) => {
            eprintln!("clat: unknown WeChat action: {other}");
            eprintln!("Usage: clat wechat <bind [--replace] | status | pair | unbind --confirm>");
            return 2;
        }
        None => {
            eprintln!("Usage: clat wechat <bind [--replace] | status | pair | unbind --confirm>");
            return 2;
        }
    };
    let valid = match action {
        "bind" => words.len() == 1 || words.as_slice() == ["bind", "--replace"],
        "unbind" => words.as_slice() == ["unbind", "--confirm"],
        _ => words.len() == 1,
    };
    if !valid {
        eprintln!("clat: invalid arguments for `wechat {action}`");
        return 2;
    }
    let bootstrap = match BootstrapApplication::open_default(project) {
        Ok(bootstrap) => bootstrap.with_permission_modes(),
        Err(error) => {
            eprintln!("clat: {error}");
            return 1;
        }
    };
    let application = match bootstrap.into_trusted() {
        Ok(application) => application,
        Err(error) => {
            eprintln!("clat: {error}");
            return 1;
        }
    };
    let result = match action {
        "status" => application.wechat_binding().map(|snapshot| {
            let status = snapshot.status;
            println!(
                "WeChat: {} · {} paired user(s) · {} mapped chat(s)",
                if status.bound { "bound" } else { "not bound" },
                status.paired_users,
                status.mapped_chats
            );
        }),
        "pair" => application.issue_wechat_pairing_code().map(|challenge| {
            println!("WeChat pairing code: {}", challenge.code);
            println!("It expires in 60 minutes and can be used once.");
            println!(
                "Binding does not authorize a user; send `/pair <code>` as a standalone message."
            );
        }),
        "unbind" => application.revoke_wechat_binding().map(|()| {
            println!("WeChat binding, paired users, and chat mappings were removed.");
        }),
        "bind" => run_terminal_wechat_binding(
            &application,
            words.get(1).is_some_and(|word| word == "--replace"),
        ),
        _ => unreachable!(),
    };
    let action_ok = match result {
        Ok(()) => true,
        Err(error) => {
            eprintln!("clat: {error}");
            false
        }
    };
    let close_ok = match application.close() {
        Ok(()) => true,
        Err(error) => {
            eprintln!("clat: could not close project state cleanly: {error}");
            false
        }
    };
    if action_ok && close_ok { 0 } else { 1 }
}

fn run_terminal_wechat_binding(
    application: &TrustedProjectApplication,
    replace: bool,
) -> Result<(), crate::ApplicationError> {
    if application.wechat_binding()?.status.bound && !replace {
        return Err(crate::ApplicationError::from_message(
            "WeChat is already bound; use `clat wechat bind --replace` to replace it only after a new QR is confirmed",
        ));
    }
    let (mut binding, content) = crate::im::BindingSession::start()
        .map_err(|error| crate::ApplicationError::from_message(error.to_string()))?;
    let rendered =
        crate::im::qr_terminal(&content).map_err(crate::ApplicationError::from_message)?;
    println!("Scan this QR code in WeChat and confirm the binding:");
    println!("{rendered}");
    println!("Binding the bot does not authorize any WeChat user.");
    let deadline = std::time::Instant::now() + Duration::from_secs(8 * 60);
    let mut verify_code: Option<String> = None;
    while std::time::Instant::now() < deadline {
        let step = binding
            .poll(verify_code.as_deref())
            .map_err(|error| crate::ApplicationError::from_message(error.to_string()))?;
        verify_code = None;
        match step {
            crate::im::BindingStep::Waiting => {}
            crate::im::BindingStep::Scanned => println!("QR scanned; confirm on the phone."),
            crate::im::BindingStep::NeedVerifyCode => {
                use std::io::Write as _;
                print!("Verification code shown by WeChat: ");
                std::io::stdout()
                    .flush()
                    .map_err(|error| crate::ApplicationError::from_message(error.to_string()))?;
                let mut input = String::new();
                std::io::stdin()
                    .read_line(&mut input)
                    .map_err(|error| crate::ApplicationError::from_message(error.to_string()))?;
                let input = input.trim();
                if input.is_empty()
                    || input.len() > 32
                    || !input.bytes().all(|byte| byte.is_ascii_alphanumeric())
                {
                    return Err(crate::ApplicationError::from_message(
                        "verification code must be 1..32 ASCII letters or digits",
                    ));
                }
                verify_code = Some(input.to_owned());
            }
            crate::im::BindingStep::VerifyCodeBlocked => {
                return Err(crate::ApplicationError::from_message(
                    "WeChat blocked the verification code; wait before trying again",
                ));
            }
            crate::im::BindingStep::Expired => {
                return Err(crate::ApplicationError::from_message(
                    "the WeChat QR code expired; run the bind command again",
                ));
            }
            crate::im::BindingStep::AlreadyBound => {
                return Err(crate::ApplicationError::from_message(
                    "this account is already bound and did not return a new credential",
                ));
            }
            crate::im::BindingStep::Confirmed(credentials) => {
                application.replace_wechat_binding(&credentials)?;
                println!("WeChat bot binding confirmed.");
                println!(
                    "Next, run `clat wechat pair` and send the one-time code from the user to authorize."
                );
                return Ok(());
            }
        }
    }
    Err(crate::ApplicationError::from_message(
        "WeChat binding exceeded the 8-minute deadline",
    ))
}

/// Optional IM frontend hosted by the serve process. The enum is deliberately
/// closed while WeChat is the only concrete dogfood backend; a second real
/// backend is the trigger for a more general configuration surface.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ImBackend {
    Wechat,
}

#[derive(Clone, Debug)]
pub struct ServeArgs {
    /// 绑定端口；缺省 2691；0 = OS 自动分配（测试/显式多实例）。
    pub port: u16,
    /// 显式 token（脚本/测试）；缺省读取/创建 `~/.clat/web-token`。
    pub token: Option<String>,
    /// 显式轮换持久 token；与 `--token` 互斥。
    pub rotate_token: bool,
    /// 显式启用的 IM 前端。缺省 None，保证普通 serve 零 IM 路径。
    pub im: Option<ImBackend>,
}

impl Default for ServeArgs {
    fn default() -> Self {
        Self {
            port: DEFAULT_SERVE_PORT,
            token: None,
            rotate_token: false,
            im: None,
        }
    }
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
                token::validate(&value)?;
                parsed.token = Some(value);
            }
            "--rotate-token" => parsed.rotate_token = true,
            "--im" => {
                let value = iter
                    .next()
                    .ok_or_else(|| "--im requires a backend (wechat)".to_string())?;
                let backend = match value.as_str() {
                    "wechat" => ImBackend::Wechat,
                    _ => return Err(format!("unsupported IM backend: {value}")),
                };
                if parsed.im.replace(backend).is_some() {
                    return Err("--im may only be specified once".into());
                }
            }
            "--" => return Err("serve takes no positional arguments".into()),
            other => return Err(format!("unknown option: {other}")),
        }
    }
    if parsed.rotate_token && parsed.token.is_some() {
        return Err("--rotate-token cannot be used with --token".into());
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
        |bootstrap| bootstrap.with_permission_modes().into_trusted(),
        shutdown,
        state::SUBSCRIBER_QUEUE_FRAMES,
    ) {
        Ok(handle) => handle,
        Err(error) => {
            eprintln!("clat: {error}");
            return 1;
        }
    };
    println!(
        "clat serve listening on http://127.0.0.1:{}/",
        handle.addr.port()
    );
    match &handle.token_path {
        Some(path) => println!(
            "Pair this browser once with the token in {}.",
            path.display()
        ),
        None => println!("Pair this browser once with the explicit --token value."),
    }
    println!("Press Ctrl-C to stop.");
    serve_join_exit(handle)
}

/// FIX-3/CA-05：join + 退出码映射（生产与测试共用同一接线——映射
/// 不在生产入口被旁路）。
pub(crate) fn serve_join_exit(handle: ServeHandle) -> i32 {
    serve_exit_code(&handle.join())
}

/// INV-F3-3 退出码语义：**显式 shutdown + accept 线程正常结束 +
/// Application close 成功**三者同时成立才返回 0；accept 线程意外退出/
/// panic、close Err、close_outcome None（归一失败降级）任一发生 →
/// 非零，根因写 stderr。
pub(crate) fn serve_exit_code(exit: &ServeExit) -> i32 {
    let mut clean = true;
    match &exit.accept {
        Ok(()) => {}
        Err(reason) => {
            eprintln!("clat: serve: accept loop ended unexpectedly: {reason}");
            clean = false;
        }
    }
    match &exit.close {
        Some(Ok(())) => {}
        Some(Err(error)) => {
            eprintln!("clat: serve: application close failed: {error}");
            clean = false;
        }
        None => {
            eprintln!("clat: serve: could not close application cleanly");
            clean = false;
        }
    }
    if clean { 0 } else { 1 }
}

/// serve 的完整退出视图（`ServeHandle::join` 的产物）。
pub struct ServeExit {
    /// accept 线程结局：`Ok` = 正常（shutdown 旗位退出）；`Err` = accept
    /// fatal 错误或线程 panic（根因在内，panic 已转译为消息）。
    pub accept: Result<(), String>,
    /// 显式应用 close 的结果（语义同 `ServeHandle::close_outcome` 注释）。
    pub close: Option<Result<(), String>>,
}

pub struct ServeHandle {
    pub addr: SocketAddr,
    pub token: String,
    /// 持久 token 路径；显式 `--token` 时为 None。
    pub token_path: Option<PathBuf>,
    shutdown: Arc<AtomicBool>,
    /// 显式应用 close 的结果（accept 线程写入，`join()` 后读取）：
    /// `Some` = 归一成功、显式 close 已执行（Ok/Err 皆算）；
    /// `None` = 归一失败走了降级路径（残留连接/worker 超宽限期），
    /// 应用由 Drop 兜底关闭。
    close_outcome: Arc<Mutex<Option<Result<(), String>>>>,
    join: JoinHandle<Result<(), String>>,
}

impl ServeHandle {
    /// 请求关停（accept 循环检测到后走完整关停序列）。
    pub fn shutdown(&self) {
        self.shutdown.store(true, Ordering::SeqCst);
    }

    /// 阻塞到 serve 完整退出（连接/worker 有界 join、应用显式 close），
    /// 返回完整退出视图（accept 结局 + 显式 close 结果）。
    pub fn join(self) -> ServeExit {
        let Self {
            join,
            close_outcome,
            ..
        } = self;
        // FIX-3/CA-05：accept 线程的 panic 不再被吞——转译为根因。
        let accept = match join.join() {
            Ok(outcome) => outcome,
            Err(panic) => Err(format!(
                "accept thread panicked: {}",
                panic
                    .downcast_ref::<&str>()
                    .map(|message| (*message).to_owned())
                    .or_else(|| panic.downcast_ref::<String>().cloned())
                    .unwrap_or_else(|| "unknown panic payload".to_owned())
            )),
        };
        let close = close_outcome
            .lock()
            .expect("serve close outcome lock")
            .clone();
        ServeExit { accept, close }
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
    let storage_root = bootstrap.storage_root().to_path_buf();
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
    let wechat_credentials = if matches!(args.im, Some(ImBackend::Wechat)) {
        Some(
            trusted
                .wechat_binding()
                .map_err(|error| error.to_string())?
                .credentials
                .ok_or_else(|| {
                    "WeChat IM is not configured yet; complete QR binding before starting with `--im wechat`"
                        .to_owned()
                })?,
        )
    } else {
        None
    };
    // INV-S1a：只绑 IPv4 loopback。
    let listener = TcpListener::bind(("127.0.0.1", args.port))
        .map_err(|error| format!("could not bind 127.0.0.1:{}: {error}", args.port))?;
    listener
        .set_nonblocking(true)
        .map_err(|error| format!("could not set nonblocking: {error}"))?;
    let addr = listener
        .local_addr()
        .map_err(|error| format!("could not read local addr: {error}"))?;
    // 端口成功占用后才读取/轮换凭据，避免 bind 失败却先撤销浏览器中
    // 已配对的 Bearer。显式 --token 是临时覆盖，resolve 不碰持久文件。
    let resolved = token::resolve(&storage_root, args.token, args.rotate_token)?;
    let token = resolved.value;
    let token_path = resolved.path;

    let app = Arc::new(Mutex::new(trusted));
    let mut shared = state::ServeShared::new(Arc::clone(&app), token.clone(), addr.port());
    shared.queue_frames = queue_frames;
    let shared = Arc::new(shared);
    shared.spawn_notice_forwarder();
    if let Some(credentials) = wechat_credentials {
        let (bridge, outbox_worker) = wechat::WechatBridge::spawn(
            Arc::clone(&shared),
            credentials.clone(),
            Arc::clone(&shutdown),
        )?;
        let handler: Arc<dyn crate::im::AuthorizedMessageHandler> = bridge;
        let poller = match crate::im::spawn_wechat_host(
            Arc::clone(&app),
            credentials,
            Arc::clone(&shutdown),
            handler,
        ) {
            Ok(worker) => worker,
            Err(error) => {
                shutdown.store(true, Ordering::Release);
                let _ = outbox_worker.join();
                return Err(error);
            }
        };
        shared.register_worker(outbox_worker);
        shared.register_worker(poller);
    }

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
            )
        })
        .map_err(|error| format!("could not spawn accept loop: {error}"))?;
    Ok(ServeHandle {
        addr,
        token,
        token_path,
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
) -> Result<(), String> {
    let mut fatal: Option<String> = None;
    loop {
        if shutdown.load(Ordering::SeqCst) {
            break;
        }
        match listener.accept() {
            Ok((mut stream, _)) => {
                let _ = stream.set_nonblocking(false);
                let Some(connection_permit) = shared.try_connection_permit() else {
                    let _ = http::write_response(
                        &mut stream,
                        503,
                        "application/json",
                        br#"{"ok":false,"error":{"code":"busy","message":"server connection limit reached"}}"#,
                    );
                    continue;
                };
                let conn_shared = Arc::clone(&shared);
                if let Ok(handle) = std::thread::Builder::new()
                    .name("clat-serve-conn".into())
                    .spawn(move || {
                        let _permit = connection_permit;
                        handle_connection(stream, conn_shared);
                    })
                {
                    shared.register_connection(handle);
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(100));
            }
            // FIX-3/CA-05（INV-F3-4）：fatal accept 错误记录根因、仍走
            // 完整关停序列（不静默消失）；退出码由结果映射判定非零。
            Err(error) => {
                fatal = Some(format!("accept failed: {error}"));
                break;
            }
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
    match fatal {
        Some(reason) => Err(reason),
        None => Ok(()),
    }
}

/// 每连接处理：读请求 → 三闸 → 路由。任何错误路径都写一个 4xx/5xx
/// JSON 应答再断连（fail-closed，不静默）。
fn handle_connection(mut stream: TcpStream, shared: Arc<state::ServeShared>) {
    let mut request = match http::read_request_head(&mut stream) {
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

    // DNS-rebinding fence: loopback bind alone is not enough when a hostile
    // hostname resolves to 127.0.0.1. The browser-visible authority must be
    // one of the two exact origins we advertise, including the bound port.
    let allowed_hosts = [
        format!("localhost:{}", shared.port),
        format!("127.0.0.1:{}", shared.port),
    ];
    if !allowed_hosts.contains(&request.host.to_ascii_lowercase()) {
        let _ = write_json(
            &mut stream,
            403,
            protocol::rpc_result_json(&Err(protocol::RpcError {
                code: protocol::ErrorCode::Forbidden,
                message: "host is not allowed".into(),
                receipt: None,
            })),
        );
        return;
    }

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
                    receipt: None,
                })),
            );
            return;
        }
    }

    // 静态 shell 是无凭据引导面：它不含 token、不暴露 API 数据，允许
    // PWA 从干净 URL 冷启动并呈现一次配对页。Origin 闸仍先执行。
    if request.method == "GET"
        && let Some((bytes, content_type)) = web_assets::asset(&request.path)
    {
        let _ = http::write_response_with_headers(
            &mut stream,
            200,
            content_type,
            &bytes,
            &[
                (
                    "Content-Security-Policy",
                    "default-src 'self'; img-src 'self' blob:; connect-src 'self' https://pi.at.cn; base-uri 'none'; object-src 'none'; frame-ancestors 'none'; form-action 'self'",
                ),
                ("Cache-Control", "no-store"),
                ("Referrer-Policy", "no-referrer"),
                ("X-Content-Type-Options", "nosniff"),
            ],
        );
        return;
    }

    // 浏览器一次配对验证：只校验 Bearer、不回显 token。前端成功后把
    // token 保存在当前 origin（包含端口）的 localStorage，后续仍只发
    // Authorization。token 从不进入 URL 或 Cookie。
    if request.method == "POST" && request.path == "/auth" {
        if http::bearer_token(request.authorization.as_deref()).as_deref()
            != Some(shared.token.as_str())
        {
            write_unauthorized(&mut stream);
            return;
        }
        if request.content_length != 0 {
            let _ = write_json(
                &mut stream,
                400,
                protocol::rpc_result_json(&Err(protocol::RpcError::bad_request(
                    "auth request body must be empty",
                ))),
            );
            return;
        }
        let body = br#"{"ok":true}"#;
        let _ = http::write_response_with_headers(
            &mut stream,
            200,
            "application/json",
            body,
            &[("Cache-Control", "no-store")],
        );
        return;
    }

    // INV-S1b token 闸：非引导请求只认 Bearer，精确匹配；query token
    // 与 Cookie 均不具有鉴权语义。
    let provided = http::bearer_token(request.authorization.as_deref());
    if provided.as_deref() != Some(shared.token.as_str()) {
        write_unauthorized(&mut stream);
        return;
    }

    // Image bytes never travel in the replay/SSE protocol. The sole browser
    // read route accepts an opaque attachment id, resolves it through the
    // active session's durable content reachability fence, then streams a
    // no-follow bounded core reader in 64KiB pieces. No filesystem path is a
    // web capability.
    if request.method == "GET"
        && let Some(attachment_id) = attachment_read_id(&request.path)
    {
        let Some(_download_permit) = shared.try_attachment_download_permit() else {
            let _ = write_json(
                &mut stream,
                503,
                protocol::rpc_result_json(&Err(protocol::RpcError::busy(
                    "attachment download limit reached",
                ))),
            );
            return;
        };
        let result = {
            let app = shared.app.lock().expect("application lock");
            app.open_current_attachment(attachment_id)
        };
        match result {
            Ok(mut image)
                if matches!(
                    image.descriptor.media_type.as_str(),
                    "image/png" | "image/jpeg"
                ) =>
            {
                let _ = stream.set_write_timeout(Some(Duration::from_secs(15)));
                let _ = http::write_file_response_with_headers(
                    &mut stream,
                    200,
                    &image.descriptor.media_type,
                    &mut image.file,
                    image.bytes,
                    &[
                        ("Cache-Control", "no-store"),
                        ("X-Content-Type-Options", "nosniff"),
                        ("Referrer-Policy", "no-referrer"),
                    ],
                );
            }
            _ => {
                let _ = write_json(
                    &mut stream,
                    404,
                    protocol::rpc_result_json(&Err(protocol::RpcError::not_found(
                        "image is unavailable in the active session",
                    ))),
                );
            }
        }
        return;
    }

    // Raw single-image ingress is deliberately outside JSON RPC: no Base64
    // expansion and no whole-body Vec allocation. Only a server-minted scope
    // id is accepted, then core owns the private create-new staging writer.
    if request.method == "POST"
        && let Some(scope_id) = draft_upload_scope(&request.path)
    {
        let Some(media_type) = request.content_type.as_deref().map(str::trim) else {
            let _ = write_json(
                &mut stream,
                400,
                protocol::rpc_result_json(&Err(protocol::RpcError::bad_request(
                    "image upload requires Content-Type: image/png or image/jpeg",
                ))),
            );
            return;
        };
        if !matches!(media_type, "image/png" | "image/jpeg") {
            let _ = write_json(
                &mut stream,
                400,
                protocol::rpc_result_json(&Err(protocol::RpcError::bad_request(
                    "image upload requires Content-Type: image/png or image/jpeg",
                ))),
            );
            return;
        }
        if request.content_length == 0
            || request.content_length as u64 > crate::media::MAX_ATTACHMENT_BYTES
        {
            let _ = write_json(
                &mut stream,
                413,
                protocol::rpc_result_json(&Err(protocol::RpcError::bad_request(format!(
                    "image upload must be 1..={} bytes",
                    crate::media::MAX_ATTACHMENT_BYTES
                )))),
            );
            return;
        }
        let Some(_permit) = shared.try_upload_permit() else {
            let _ = write_json(
                &mut stream,
                429,
                protocol::rpc_result_json(&Err(protocol::RpcError::busy(
                    "too many image uploads are active",
                ))),
            );
            return;
        };
        let mut writer = match shared.drafts.begin_upload(
            scope_id,
            shared.selection_generation(),
            shared.token_generation(),
            request.content_length as u64,
            media_type,
            request.display_name.as_deref(),
        ) {
            Ok(writer) => writer,
            Err(error) => {
                let _ = write_json(
                    &mut stream,
                    400,
                    protocol::rpc_result_json(&Err(protocol::RpcError::bad_request(error))),
                );
                return;
            }
        };
        match http::read_body_into(
            &mut stream,
            &mut request,
            crate::media::MAX_ATTACHMENT_BYTES as usize,
            &mut writer,
        ) {
            Ok(()) => {}
            Err(http::HttpReadError::TooLarge(_)) => {
                let _ = write_json(
                    &mut stream,
                    413,
                    protocol::rpc_result_json(&Err(protocol::RpcError::bad_request(
                        "image upload exceeds the size limit",
                    ))),
                );
                return;
            }
            Err(http::HttpReadError::BadRequest(reason)) => {
                let _ = write_json(
                    &mut stream,
                    400,
                    protocol::rpc_result_json(&Err(protocol::RpcError::bad_request(reason))),
                );
                return;
            }
            Err(http::HttpReadError::TimedOut | http::HttpReadError::Closed) => return,
            Err(http::HttpReadError::Io) => return,
        }
        match writer.finish() {
            Ok(uploaded) => {
                let _ = write_json(
                    &mut stream,
                    200,
                    protocol::rpc_result_json(&Ok(serde_json::json!({
                        "uploadId": uploaded.upload_id,
                        "bytes": uploaded.bytes,
                    }))),
                );
            }
            Err(error) => {
                let _ = write_json(
                    &mut stream,
                    400,
                    protocol::rpc_result_json(&Err(protocol::RpcError::bad_request(error))),
                );
            }
        }
        return;
    }

    let rpc_method = request.path.strip_prefix("/api/").map(str::to_owned);
    match request.method.as_str() {
        "GET" => match request.path.as_str() {
            "/api/events" => sse::handle(&mut stream, &shared),
            path => {
                let _ = write_json(
                    &mut stream,
                    404,
                    protocol::rpc_result_json(&Err(protocol::RpcError::not_found(format!(
                        "no such path: {path}"
                    )))),
                );
            }
        },
        "POST" => match rpc_method.as_deref() {
            Some(method) if protocol::RPC_METHODS.contains(&method) => {
                if !request.content_type.as_deref().is_some_and(|content_type| {
                    content_type
                        .split(';')
                        .next()
                        .is_some_and(|value| value.trim().eq_ignore_ascii_case("application/json"))
                }) {
                    let _ = write_json(
                        &mut stream,
                        400,
                        protocol::rpc_result_json(&Err(protocol::RpcError::bad_request(
                            "RPC requests require Content-Type: application/json",
                        ))),
                    );
                    return;
                }
                let body = match http::read_body(&mut stream, &mut request, http::MAX_BODY_BYTES) {
                    Ok(body) => body,
                    Err(http::HttpReadError::TooLarge(part)) => {
                        let _ = write_json(
                            &mut stream,
                            413,
                            protocol::rpc_result_json(&Err(protocol::RpcError::bad_request(
                                format!("{part} exceeds the size limit"),
                            ))),
                        );
                        return;
                    }
                    Err(http::HttpReadError::BadRequest(reason)) => {
                        let _ = write_json(
                            &mut stream,
                            400,
                            protocol::rpc_result_json(&Err(protocol::RpcError::bad_request(
                                reason,
                            ))),
                        );
                        return;
                    }
                    Err(http::HttpReadError::TimedOut | http::HttpReadError::Closed) => return,
                    Err(http::HttpReadError::Io) => return,
                };
                let params: serde_json::Value = if body.is_empty() {
                    serde_json::json!({})
                } else {
                    match serde_json::from_slice(&body) {
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
            Some(method) if !method.is_empty() => {
                let _ = write_json(
                    &mut stream,
                    200,
                    protocol::rpc_result_json(&Err(protocol::RpcError::bad_request(format!(
                        "unknown RPC method: {method}"
                    )))),
                );
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

fn draft_upload_scope(path: &str) -> Option<&str> {
    let rest = path.strip_prefix("/api/drafts/")?;
    let scope_id = rest.strip_suffix("/images")?;
    (!scope_id.is_empty() && !scope_id.contains('/')).then_some(scope_id)
}

fn attachment_read_id(path: &str) -> Option<&str> {
    let attachment_id = path.strip_prefix("/api/attachments/")?;
    (!attachment_id.is_empty() && !attachment_id.contains('/')).then_some(attachment_id)
}

fn write_unauthorized(stream: &mut TcpStream) {
    let _ = write_json(
        stream,
        401,
        protocol::rpc_result_json(&Err(protocol::RpcError {
            code: protocol::ErrorCode::Unauthorized,
            message: "missing or invalid authentication".into(),
            receipt: None,
        })),
    );
}

fn write_json(stream: &mut TcpStream, status: u16, body: String) -> std::io::Result<()> {
    http::write_response(stream, status, "application/json", body.as_bytes())
}
