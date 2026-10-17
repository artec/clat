//! NETISO-1（AG-DOGFOOD-7）出站隔离防线测试矩阵。
//!
//! 独立测试二进制：`arm_outbound_guard` 是进程级的，武装窗口内本进程
//! 所有 HTTP 适配器出站都必须命中授权集合——绝不能混进共享 lib 测试
//! 进程（会误伤并行的本地 HTTP provider 测试）。文件内测试经
//! `SERIAL` 互斥串行，环境变量改动也在同一互斥下进行。
//!
//! 全程只用合成配置、假凭据与本地监听器；不读用户 `~/.clat`、环境
//! API Key 或 token；任何断言不打印敏感 query/headers/payload。

use clat_core::test_support::netiso::{
    arm_outbound_guard, effective_utility_route, openai_compatible,
};
use clat_core::{
    BootstrapApplication, CancelToken, FinishReason, Model, ModelConfig, ModelError, ModelEvent,
    ModelEventSink, ModelItem, ModelOptions, ModelProtocol, ModelRequest, ModelResponse, Project,
    ProjectAuthorization, ProviderCredentials, preset_by_id,
};
use std::collections::BTreeSet;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{IpAddr, SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// 武装 + 环境改动都必须串行（进程级状态）。
static SERIAL: Mutex<()> = Mutex::new(());

const FAKE_KEY: &str = "netiso-synthetic-key-0001";
const ANSWER: &str = "NETISO-LOCAL-ANSWER";

// ---------------------------------------------------------------------------
// 本地探针服务器
// ---------------------------------------------------------------------------

enum Reply {
    /// 200 + text/event-stream：逐条写出 data 行后关闭连接。
    Sse(Vec<String>),
    /// 302 + Location：用于重定向负控。
    Redirect(String),
    /// 写出 200 响应头后按住正文（取消传播负控）。
    Hold,
}

struct ProbeInner {
    reply: Reply,
    hits: AtomicUsize,
    requests: Mutex<Vec<String>>,
    auth_headers: Mutex<Vec<String>>,
}

/// 极小 HTTP 探针：记录命中、首行与 Authorization 头，按模式应答。
struct ProbeServer {
    addr: SocketAddr,
    inner: Arc<ProbeInner>,
    stop: Arc<AtomicBool>,
    worker: Option<std::thread::JoinHandle<()>>,
}

impl ProbeServer {
    fn spawn(reply: Reply) -> Self {
        Self::spawn_at(IpAddr::from([127, 0, 0, 1]), reply)
    }

    /// R1-F5：支持字面 IPv6 环回（`[::1]`）探针。
    fn spawn_at(ip: IpAddr, reply: Reply) -> Self {
        let listener = TcpListener::bind((ip, 0)).expect("bind probe listener");
        let addr = listener.local_addr().expect("probe local addr");
        let inner = Arc::new(ProbeInner {
            reply,
            hits: AtomicUsize::new(0),
            requests: Mutex::new(Vec::new()),
            auth_headers: Mutex::new(Vec::new()),
        });
        let stop = Arc::new(AtomicBool::new(false));
        let worker = {
            let inner = Arc::clone(&inner);
            let stop = Arc::clone(&stop);
            std::thread::spawn(move || accept_loop(listener, inner, stop))
        };
        Self {
            addr,
            inner,
            stop,
            worker: Some(worker),
        }
    }

    fn port(&self) -> u16 {
        self.addr.port()
    }

    fn hits(&self) -> usize {
        self.inner.hits.load(Ordering::SeqCst)
    }

    fn auth_headers(&self) -> Vec<String> {
        self.inner.auth_headers.lock().unwrap().clone()
    }

    fn requests(&self) -> Vec<String> {
        self.inner.requests.lock().unwrap().clone()
    }
}

impl Drop for ProbeServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        // 唤醒可能阻塞在 accept 上的线程：连一下自己。
        let _ = TcpStream::connect(self.addr);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

fn accept_loop(listener: TcpListener, inner: Arc<ProbeInner>, stop: Arc<AtomicBool>) {
    listener
        .set_nonblocking(true)
        .expect("probe listener nonblocking");
    while !stop.load(Ordering::Acquire) {
        match listener.accept() {
            Ok((stream, _)) => {
                if let Err(error) = serve_connection(stream, &inner, &stop) {
                    eprintln!("probe serve error: {error}");
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(5));
            }
            Err(_) => return,
        }
    }
}

fn serve_connection(
    stream: TcpStream,
    inner: &ProbeInner,
    stop: &Arc<AtomicBool>,
) -> std::io::Result<()> {
    stream.set_nonblocking(false)?;
    stream.set_read_timeout(Some(Duration::from_secs(5)))?;
    stream.set_write_timeout(Some(Duration::from_secs(5)))?;
    inner.hits.fetch_add(1, Ordering::SeqCst);
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut request_line = String::new();
    reader.read_line(&mut request_line)?;
    let mut authorization = String::new();
    let mut content_length = 0usize;
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line)? == 0 {
            break;
        }
        let trimmed = line.trim_end_matches(['\r', '\n']);
        if trimmed.is_empty() {
            break;
        }
        let (name, value) = trimmed.split_once(':').unwrap_or((trimmed, ""));
        if name.trim().eq_ignore_ascii_case("authorization") {
            authorization = value.trim().to_owned();
        }
        if name.trim().eq_ignore_ascii_case("content-length")
            && let Ok(parsed) = value.trim().parse()
        {
            content_length = parsed;
        }
    }
    let mut body = vec![0u8; content_length.min(1024 * 1024)];
    if !body.is_empty() {
        reader.read_exact(&mut body)?;
    }
    inner
        .requests
        .lock()
        .unwrap()
        .push(request_line.trim().to_owned());
    inner
        .auth_headers
        .lock()
        .unwrap()
        .push(authorization.clone());
    let mut stream = stream;
    match &inner.reply {
        Reply::Sse(lines) => {
            let mut payload = String::new();
            for line in lines {
                payload.push_str(&format!("data: {line}\n\n"));
            }
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                payload.len(),
                payload
            )
        }
        Reply::Redirect(location) => write!(
            stream,
            "HTTP/1.1 302 Found\r\nLocation: {location}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
        ),
        Reply::Hold => {
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\n"
            )?;
            let deadline = Instant::now() + Duration::from_secs(20);
            while !stop.load(Ordering::Acquire) && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(20));
            }
            Ok(())
        }
    }
}

fn sse_chunks(text: &str) -> Vec<String> {
    vec![
        format!(
            "{{\"id\":\"p\",\"object\":\"chat.completion.chunk\",\"choices\":[{{\"index\":0,\
             \"delta\":{{\"content\":\"{text}\"}},\"finish_reason\":null}}]}}"
        ),
        String::from(
            "{\"id\":\"p\",\"object\":\"chat.completion.chunk\",\"choices\":[{\"index\":0,\
             \"delta\":{},\"finish_reason\":\"stop\"}]}",
        ),
    ]
}

// ---------------------------------------------------------------------------
// 模型驱动助手
// ---------------------------------------------------------------------------

fn local_config(port: u16) -> ModelConfig {
    ModelConfig {
        preset: None,
        protocol: ModelProtocol::OpenAiCompatible,
        model: "netiso-synthetic-model".into(),
        endpoint: format!("http://127.0.0.1:{port}"),
        request_path: "/chat/completions".into(),
        ..ModelConfig::default()
    }
}

fn fake_credentials() -> ProviderCredentials {
    let mut credentials = ProviderCredentials::for_protocol(ModelProtocol::OpenAiCompatible);
    credentials.set_value(0, FAKE_KEY.to_owned());
    credentials
}

fn one_allowed(addr: SocketAddr) -> BTreeSet<SocketAddr> {
    BTreeSet::from([addr])
}

fn stream_once(config: &ModelConfig, cancel: &CancelToken) -> Result<ModelResponse, ModelError> {
    let mut model = openai_compatible(config, &fake_credentials())?;
    let items = [ModelItem::user_text("netiso probe")];
    let request = ModelRequest {
        instructions: None,
        items: &items,
        tools: &[],
        options: &ModelOptions::default(),
        cancel,
    };
    let mut sink = RecordingSink::default();
    model.stream(request, &mut sink)
}

#[derive(Default)]
struct RecordingSink {
    deltas: Vec<String>,
}

impl ModelEventSink for RecordingSink {
    fn emit(&mut self, event: ModelEvent) {
        if let ModelEvent::TextDelta { delta } = event {
            self.deltas.push(delta);
        }
    }
}

fn temp_root(tag: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!(
        "clat-netiso-{tag}-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock after epoch")
            .as_nanos()
    ));
    std::fs::create_dir_all(&root).expect("create netiso temp root");
    root
}

fn remove_tree(path: &Path) {
    for _ in 0..100 {
        match std::fs::remove_dir_all(path) {
            Ok(()) => return,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return,
            Err(_) => std::thread::sleep(Duration::from_millis(10)),
        }
    }
    let _ = std::fs::remove_dir_all(path);
}

fn mounted_app(root: &Path) -> clat_core::TrustedProjectApplication {
    let project = root.join("project");
    std::fs::create_dir_all(&project).expect("create project dir");
    BootstrapApplication::open(Project::new(&project), root.join("home/.clat"))
        .expect("open bootstrap")
        .authorize_and_mount(ProjectAuthorization::grant())
        .expect("mount trusted project")
}

// ---------------------------------------------------------------------------
// N2 正控：授权监听器上的真实回环往返
// ---------------------------------------------------------------------------

#[test]
fn authorized_loopback_listener_roundtrips_with_fake_credentials() {
    let _serial = SERIAL.lock().unwrap();
    let probe = ProbeServer::spawn(Reply::Sse(sse_chunks(ANSWER)));
    let cancel = CancelToken::new();
    let _arm = arm_outbound_guard(one_allowed(probe.addr));
    let response = stream_once(&local_config(probe.port()), &cancel)
        .expect("authorized listener must be reachable through the guard");
    assert_eq!(response.text, ANSWER);
    assert_eq!(response.finish_reason, FinishReason::Completed);
    assert_eq!(probe.hits(), 1, "exactly one outbound request");
    let auth = probe.auth_headers();
    assert_eq!(
        auth.as_slice(),
        [format!("Bearer {FAKE_KEY}")],
        "the synthetic fake key is the only credential on the wire"
    );
    assert_eq!(
        probe.requests().as_slice(),
        ["POST /chat/completions HTTP/1.1".to_string()],
        "the joined request path is what the adapter really dialed"
    );
}

// ---------------------------------------------------------------------------
// N2/N3 负控：未授权环回端口在连接前被拒
// ---------------------------------------------------------------------------

#[test]
fn unauthorized_loopback_port_is_refused_before_connect() {
    let _serial = SERIAL.lock().unwrap();
    let authorized = ProbeServer::spawn(Reply::Sse(sse_chunks(ANSWER)));
    let trap = ProbeServer::spawn(Reply::Sse(sse_chunks("SHOULD-NOT-ARRIVE")));
    let cancel = CancelToken::new();
    let _arm = arm_outbound_guard(one_allowed(authorized.addr));
    let error = stream_once(&local_config(trap.port()), &cancel)
        .expect_err("an unauthorized loopback port must be refused");
    let message = error.to_string();
    assert!(
        message.contains("test network isolation"),
        "locatable guard reason: {message}"
    );
    assert!(
        message.contains(&format!("127.0.0.1:{}", trap.port())),
        "the rejection must name the refused target: {message}"
    );
    assert!(
        message.contains("not in the authorized test listeners"),
        "{message}"
    );
    assert!(
        !message.contains(FAKE_KEY) && !message.to_lowercase().contains("bearer"),
        "the rejection must not echo credentials: {message}"
    );
    assert_eq!(
        trap.hits(),
        0,
        "nothing may connect to the unauthorized port"
    );
    assert_eq!(authorized.hits(), 0);
}

// ---------------------------------------------------------------------------
// N2/N3 负控：非环回主机在解析前被拒（.invalid 永不解析，撤机制红也
// 无法触达真实厂商）
// ---------------------------------------------------------------------------

#[test]
fn non_loopback_host_is_refused_before_resolution() {
    let _serial = SERIAL.lock().unwrap();
    let probe = ProbeServer::spawn(Reply::Sse(sse_chunks(ANSWER)));
    let cancel = CancelToken::new();
    let _arm = arm_outbound_guard(one_allowed(probe.addr));
    let mut config = local_config(probe.port());
    config.endpoint = "https://vendor.invalid".into();
    config.request_path = "/v1/chat/completions".into();
    let error = stream_once(&config, &cancel)
        .expect_err("a non-loopback host must be refused before resolution");
    let message = error.to_string();
    assert!(message.contains("test network isolation"), "{message}");
    assert!(
        message.contains("not a literal IP") || message.contains("not loopback"),
        "{message}"
    );
    assert_eq!(probe.hits(), 0);
}

// ---------------------------------------------------------------------------
// N1 + N2：第五轮「官方 preset + 本地 endpoint」矛盾配置的重演与拦截
// ---------------------------------------------------------------------------

#[test]
fn preset_reapplication_restores_official_route_and_guard_blocks_it() {
    let _serial = SERIAL.lock().unwrap();
    let root = temp_root("preset-reapply");
    let app = mounted_app(&root);
    let probe = ProbeServer::spawn(Reply::Sse(sse_chunks(ANSWER)));

    // 第五轮历史形态：手工保存的配置同时带官方 preset id 与本地 endpoint。
    let mut hand_written = local_config(probe.port());
    hand_written.preset = Some("deepseek-flash".into());
    hand_written.model = "deepseek-flash".into();
    let deepseek = preset_by_id("deepseek-flash").expect("builtin preset");
    app.save_model_state(&hand_written, &fake_credentials())
        .expect("save hand-written state");

    // N1 冻结证据：model_state() 重应用 preset，生效端点回到官方值。
    let (effective, _) = app.model_state().expect("effective model state");
    assert_eq!(
        effective.endpoint, deepseek.endpoint,
        "preset reapplication restores the official endpoint over the hand-written local route"
    );
    assert_eq!(effective.preset.as_deref(), Some("deepseek-flash"));
    assert_eq!(
        effective.request_path, deepseek.request_path,
        "the request path is part of the preset stamp"
    );
    let view = app.model_settings_view().expect("settings view");
    assert_eq!(view.current.endpoint, deepseek.endpoint);
    assert_eq!(view.current.preset.as_deref(), Some("deepseek-flash"));

    // N2：武装后按「生效路由」出站——防线在连接前拒绝官方端点。
    let hits_before = probe.hits();
    let cancel = CancelToken::new();
    let _arm = arm_outbound_guard(one_allowed(probe.addr));
    let error = stream_once(&effective, &cancel).expect_err(
        "the preset-restored official route must be refused before any outbound request",
    );
    let message = error.to_string();
    assert!(message.contains("test network isolation"), "{message}");
    assert!(
        message.contains("api.deepseek.com"),
        "the refusal must name the restored official host: {message}"
    );
    assert_eq!(
        probe.hits(),
        hits_before,
        "no request may reach any local listener on this leg"
    );
    drop(_arm);
    app.close().expect("close app");
    remove_tree(&root);
}

// ---------------------------------------------------------------------------
// N1 负控：合法用户路径不被防线语义改写
// ---------------------------------------------------------------------------

#[test]
fn legitimate_preset_and_custom_routes_keep_their_sources() {
    let _serial = SERIAL.lock().unwrap();
    let root = temp_root("legit-routes");
    let app = mounted_app(&root);
    let probe = ProbeServer::spawn(Reply::Sse(sse_chunks(ANSWER)));

    // 负控 a：用户真选了官方预设——生效端点来自预设（合法）。
    let mut chosen = ModelConfig::default();
    let deepseek = preset_by_id("deepseek-flash").expect("builtin preset");
    deepseek.apply(&mut chosen);
    app.save_model_state(&chosen, &fake_credentials())
        .expect("save preset route");
    let (effective, _) = app.model_state().expect("preset state");
    assert_eq!(effective.endpoint, deepseek.endpoint);
    assert_eq!(effective.preset.as_deref(), Some("deepseek-flash"));

    // 负控 b：用户 Custom（preset=None）——自定义路由原样保留。
    let custom = local_config(probe.port());
    app.save_model_state(&custom, &fake_credentials())
        .expect("save custom route");
    let (effective, _) = app.model_state().expect("custom state");
    assert_eq!(effective.endpoint, custom.endpoint);
    assert_eq!(
        effective.preset, None,
        "custom routes must not carry a preset stamp"
    );
    assert_eq!(
        effective.request_path, custom.request_path,
        "custom request path survives reload"
    );

    // 负控 c：Workbench Profile 编辑路径显式落 preset=None（既有产品语义）。
    app.edit_model_profile(clat_core::application::ModelProfileEdit {
        name: "isolated-custom".into(),
        protocol: ModelProtocol::OpenAiCompatible,
        model: "netiso-profile-model".into(),
        endpoint: format!("http://127.0.0.1:{}", probe.port()),
        request_path: "/chat/completions".into(),
        api_key: Some(FAKE_KEY.to_owned()),
        limits: None,
        tuning: None,
        auth: None,
        extra_headers: None,
        extra_body: None,
    })
    .expect("edit model profile");
    let profile = app
        .load_model_profile("isolated-custom")
        .expect("load profile")
        .expect("profile exists");
    assert_eq!(
        profile.0.preset, None,
        "profile edits mark the route custom"
    );
    assert_eq!(
        profile.0.endpoint,
        format!("http://127.0.0.1:{}", probe.port())
    );
    app.close().expect("close app");
    remove_tree(&root);
}

// ---------------------------------------------------------------------------
// N3 负控：武装时不跟随重定向（ureq 默认跟随最多 10 跳）
// ---------------------------------------------------------------------------

#[test]
fn redirects_are_not_followed_under_the_guard() {
    let _serial = SERIAL.lock().unwrap();
    let trap = ProbeServer::spawn(Reply::Sse(sse_chunks("REDIRECT-TARGET")));
    let entry = ProbeServer::spawn(Reply::Redirect(format!(
        "http://127.0.0.1:{}/exfiltrate",
        trap.port()
    )));
    let cancel = CancelToken::new();
    let _arm = arm_outbound_guard(one_allowed(entry.addr));
    let error = stream_once(&local_config(entry.port()), &cancel)
        .expect_err("a redirect must not be followed under the guard");
    let message = error.to_string();
    assert!(
        message.contains("302"),
        "the unfollowed redirect status must surface: {message}"
    );
    assert_eq!(trap.hits(), 0, "the redirect target must not be contacted");
    assert_eq!(entry.hits(), 1);
}

// ---------------------------------------------------------------------------
// N3 负控：武装时忽略继承的代理环境（ureq 默认读 ALL_PROXY 等）
// ---------------------------------------------------------------------------

struct EnvRestore(Vec<(&'static str, Option<String>)>);

impl EnvRestore {
    fn set(vars: &[(&'static str, &str)]) -> Self {
        let mut saved = Vec::new();
        for (key, value) in vars {
            // SAFETY：SERIAL 互斥下本测试进程内无并发环境读写；这些
            // 变量只影响本进程 ureq agent 的代理选择。
            let old = std::env::var(key).ok();
            unsafe { std::env::set_var(key, value) };
            saved.push((*key, old));
        }
        EnvRestore(saved)
    }
}

impl Drop for EnvRestore {
    fn drop(&mut self) {
        for (key, old) in self.0.drain(..) {
            // SAFETY：同上，串行测试内的环境恢复。
            match old {
                Some(value) => unsafe { std::env::set_var(key, value) },
                None => unsafe { std::env::remove_var(key) },
            }
        }
    }
}

#[test]
fn inherited_proxy_environment_is_ignored_under_the_guard() {
    let _serial = SERIAL.lock().unwrap();
    let probe = ProbeServer::spawn(Reply::Sse(sse_chunks(ANSWER)));
    let proxy_trap = ProbeServer::spawn(Reply::Sse(sse_chunks("PROXY-HIT")));
    let _env = EnvRestore::set(&[
        (
            "ALL_PROXY",
            &format!("http://127.0.0.1:{}", proxy_trap.port()),
        ),
        (
            "HTTPS_PROXY",
            &format!("http://127.0.0.1:{}", proxy_trap.port()),
        ),
        (
            "HTTP_PROXY",
            &format!("http://127.0.0.1:{}", proxy_trap.port()),
        ),
        ("NO_PROXY", ""),
    ]);
    let cancel = CancelToken::new();
    let _arm = arm_outbound_guard(one_allowed(probe.addr));
    // agent 在武装后构建：必须以 proxy(None) 覆盖环境代理。
    let response =
        stream_once(&local_config(probe.port()), &cancel).expect("direct loopback request");
    assert_eq!(response.text, ANSWER);
    assert_eq!(probe.hits(), 1, "the request goes directly to the listener");
    assert_eq!(
        proxy_trap.hits(),
        0,
        "the inherited proxy must never be contacted under the guard"
    );
}

// ---------------------------------------------------------------------------
// N2：离线脚本模型不受武装影响（不强制绑定 HTTP）
// ---------------------------------------------------------------------------

struct OfflineScripted;

impl Model for OfflineScripted {
    fn provider(&self) -> &str {
        "offline-scripted"
    }

    fn model_id(&self) -> &str {
        "offline-scripted"
    }

    fn stream(
        &mut self,
        _request: ModelRequest<'_>,
        _events: &mut dyn ModelEventSink,
    ) -> Result<ModelResponse, ModelError> {
        Ok(ModelResponse {
            text: ANSWER.to_owned(),
            tool_calls: Vec::new(),
            finish_reason: FinishReason::Completed,
            usage: None,
            provider_response_id: None,
            provider_state: Vec::new(),
            reasoning: None,
        })
    }
}

#[test]
fn offline_scripted_model_is_unaffected_by_the_armed_guard() {
    let _serial = SERIAL.lock().unwrap();
    let probe = ProbeServer::spawn(Reply::Sse(sse_chunks(ANSWER)));
    let _arm = arm_outbound_guard(one_allowed(probe.addr));
    let mut model = OfflineScripted;
    let items = [ModelItem::user_text("offline")];
    let cancel = CancelToken::new();
    let request = ModelRequest {
        instructions: None,
        items: &items,
        tools: &[],
        options: &ModelOptions::default(),
        cancel: &cancel,
    };
    let response = model
        .stream(request, &mut RecordingSink::default())
        .expect("offline scripted models must not require an HTTP listener");
    assert_eq!(response.text, ANSWER);
    assert_eq!(probe.hits(), 0);
}

// ---------------------------------------------------------------------------
// N4：utility companion 的生效路由走同一道出站闸门
// ---------------------------------------------------------------------------

#[test]
fn utility_companion_effective_route_hits_the_same_gate() {
    let _serial = SERIAL.lock().unwrap();
    let probe = ProbeServer::spawn(Reply::Sse(sse_chunks(ANSWER)));

    // 主路由 = 官方 deepseek-v4-pro（companion = deepseek-flash）。
    let mut primary = ModelConfig::default();
    let pro = preset_by_id("deepseek-v4-pro").expect("builtin preset");
    pro.apply(&mut primary);
    let companion = effective_utility_route(&primary);
    assert_eq!(
        companion.endpoint, pro.endpoint,
        "the companion rides the same official vendor endpoint"
    );
    assert_ne!(companion.model, primary.model);

    let cancel = CancelToken::new();
    let _arm = arm_outbound_guard(one_allowed(probe.addr));
    let error = stream_once(&companion, &cancel)
        .expect_err("the companion's official route must hit the outbound gate");
    assert!(
        error.to_string().contains("api.deepseek.com"),
        "same gate, same locatable reason: {error}"
    );
    assert_eq!(probe.hits(), 0);

    // 负控：主路由 Custom 本地时，utility 默认路由跟随主路由（本地）。
    let custom = local_config(probe.port());
    let follows = effective_utility_route(&custom);
    assert_eq!(follows.endpoint, custom.endpoint);
    let response =
        stream_once(&follows, &cancel).expect("a local custom utility route is authorized");
    assert_eq!(response.text, ANSWER);
    assert_eq!(probe.hits(), 1);
}

// ---------------------------------------------------------------------------
// N4：取消传播期间隔离不放宽
// ---------------------------------------------------------------------------

#[test]
fn cancel_during_isolated_stream_stays_local() {
    let _serial = SERIAL.lock().unwrap();
    let probe = ProbeServer::spawn(Reply::Hold);
    let trap = ProbeServer::spawn(Reply::Sse(sse_chunks("MUST-NOT-ARRIVE")));
    let cancel = CancelToken::new();
    let _arm = arm_outbound_guard(BTreeSet::from([probe.addr]));
    let stream_cancel = cancel.clone();
    let (tx, rx) = std::sync::mpsc::channel();
    let config = local_config(probe.port());
    std::thread::spawn(move || {
        let _ = tx.send(stream_once(&config, &stream_cancel));
    });
    let deadline = Instant::now() + Duration::from_secs(10);
    while probe.hits() == 0 {
        assert!(Instant::now() < deadline, "probe never saw the request");
        std::thread::sleep(Duration::from_millis(10));
    }
    cancel.cancel();
    let result = rx
        .recv_timeout(Duration::from_secs(20))
        .expect("stream returns after cancel");
    let response = result.expect("cancelled stream is a Cancelled response");
    assert_eq!(response.finish_reason, FinishReason::Cancelled);
    assert_eq!(trap.hits(), 0, "cancellation must not widen isolation");
}

// ---------------------------------------------------------------------------
// N4：拒绝 URL userinfo / 非 http scheme（分类负控，不出站）
// ---------------------------------------------------------------------------

#[test]
fn userinfo_and_non_http_schemes_are_refused() {
    let _serial = SERIAL.lock().unwrap();
    let probe = ProbeServer::spawn(Reply::Sse(sse_chunks(ANSWER)));
    let cancel = CancelToken::new();
    let _arm = arm_outbound_guard(one_allowed(probe.addr));
    let mut with_userinfo = local_config(probe.port());
    with_userinfo.endpoint = format!("http://user:secret@127.0.0.1:{}", probe.port());
    let error = stream_once(&with_userinfo, &cancel).expect_err("URL userinfo must be refused");
    assert!(error.to_string().contains("userinfo"), "{}", error);
    let mut ftp = local_config(probe.port());
    ftp.endpoint = format!("ftp://127.0.0.1:{}", probe.port());
    let error = stream_once(&ftp, &cancel).expect_err("non-http schemes must be refused");
    assert!(error.to_string().contains("not http/https"), "{}", error);
    assert_eq!(probe.hits(), 0);
}

// ---------------------------------------------------------------------------
// R1-F1：武装晚于构造的适配器在 send 时点被传输层约束
// ---------------------------------------------------------------------------

#[test]
fn existing_adapter_hardens_redirects_when_armed_before_send() {
    let _serial = SERIAL.lock().unwrap();
    let trap = ProbeServer::spawn(Reply::Sse(sse_chunks("EXFILTRATED")));
    let entry = ProbeServer::spawn(Reply::Redirect(format!(
        "http://127.0.0.1:{}/exfiltrate",
        trap.port()
    )));
    // 反例形态：适配器在未武装时创建（R1-F1 的时序），随后才武装。
    let cancel = CancelToken::new();
    let config = local_config(entry.port());
    let mut pre_created =
        openai_compatible(&config, &fake_credentials()).expect("build pre-armed adapter");
    let _arm = arm_outbound_guard(one_allowed(entry.addr));
    let items = [ModelItem::user_text("pre-created adapter")];
    let request = ModelRequest {
        instructions: None,
        items: &items,
        tools: &[],
        options: &ModelOptions::default(),
        cancel: &cancel,
    };
    let error = pre_created
        .stream(request, &mut RecordingSink::default())
        .expect_err("a pre-existing adapter must not follow redirects once armed");
    assert!(
        error.to_string().contains("302"),
        "the unfollowed redirect must surface: {error}"
    );
    assert_eq!(
        trap.hits(),
        0,
        "the unauthorized redirect target must never be contacted"
    );
    assert_eq!(entry.hits(), 1);
}

#[test]
fn existing_adapter_clears_inherited_proxy_when_armed_before_send() {
    let _serial = SERIAL.lock().unwrap();
    let probe = ProbeServer::spawn(Reply::Sse(sse_chunks(ANSWER)));
    let proxy_trap = ProbeServer::spawn(Reply::Sse(sse_chunks("PROXIED")));
    let _env = EnvRestore::set(&[
        (
            "HTTP_PROXY",
            &format!("http://127.0.0.1:{}", proxy_trap.port()),
        ),
        (
            "HTTPS_PROXY",
            &format!("http://127.0.0.1:{}", proxy_trap.port()),
        ),
        (
            "ALL_PROXY",
            &format!("http://127.0.0.1:{}", proxy_trap.port()),
        ),
        ("NO_PROXY", ""),
    ]);
    let cancel = CancelToken::new();
    // 适配器在未武装时创建：agent 已携带环境代理配置。
    let config = local_config(probe.port());
    let mut pre_created =
        openai_compatible(&config, &fake_credentials()).expect("build pre-armed adapter");
    let _arm = arm_outbound_guard(one_allowed(probe.addr));
    let items = [ModelItem::user_text("pre-created adapter")];
    let request = ModelRequest {
        instructions: None,
        items: &items,
        tools: &[],
        options: &ModelOptions::default(),
        cancel: &cancel,
    };
    let response = pre_created
        .stream(request, &mut RecordingSink::default())
        .expect("send-time hardening must rebuild the agent without the inherited proxy");
    assert_eq!(response.text, ANSWER);
    assert_eq!(probe.hits(), 1, "the request goes directly to the listener");
    assert_eq!(
        proxy_trap.hits(),
        0,
        "an agent built before arming must not keep the inherited proxy"
    );
}

// ---------------------------------------------------------------------------
// R1-F3：作用域栈——令牌释放不得解除其它仍有效力的隔离
// ---------------------------------------------------------------------------

#[test]
fn nested_arm_drop_restores_the_still_live_outer_scope() {
    let _serial = SERIAL.lock().unwrap();
    let authorized = ProbeServer::spawn(Reply::Sse(sse_chunks(ANSWER)));
    let trap = ProbeServer::spawn(Reply::Sse(sse_chunks("ESCAPED")));
    let cancel = CancelToken::new();
    let outer = arm_outbound_guard(one_allowed(authorized.addr));
    {
        let _inner = arm_outbound_guard(one_allowed(authorized.addr));
        // 内层存活期间：未授权目标被拒。
        assert!(stream_once(&local_config(trap.port()), &cancel).is_err());
        assert_eq!(trap.hits(), 0);
    }
    // 内层令牌 Drop 后，外层仍有效：隔离不得静默消失（R1-F3）。
    assert!(
        stream_once(&local_config(trap.port()), &cancel).is_err(),
        "dropping the inner token must not disable the still-live outer scope"
    );
    assert_eq!(trap.hits(), 0);
    // 外层语义完整：授权监听器照常可达。
    let response = stream_once(&local_config(authorized.port()), &cancel)
        .expect("the outer scope keeps authorizing its listener");
    assert_eq!(response.text, ANSWER);
    drop(outer);
    // 全部释放后恢复生产直通。
    let response = stream_once(&local_config(trap.port()), &cancel)
        .expect("disarm restores pass-through production semantics");
    assert_eq!(response.text, "ESCAPED");
}

#[test]
fn interleaved_drop_keeps_the_still_live_scope() {
    let _serial = SERIAL.lock().unwrap();
    let only_a = ProbeServer::spawn(Reply::Sse(sse_chunks(ANSWER)));
    let only_b = ProbeServer::spawn(Reply::Sse(sse_chunks(ANSWER)));
    let cancel = CancelToken::new();
    let outer = arm_outbound_guard(one_allowed(only_a.addr));
    let inner = arm_outbound_guard(one_allowed(only_b.addr));
    // 先释放外层：内层仍存活，隔离继续以内层集合生效。
    drop(outer);
    assert!(
        stream_once(&local_config(only_a.port()), &cancel).is_err(),
        "the still-live inner scope governs after the outer token drops"
    );
    let response = stream_once(&local_config(only_b.port()), &cancel)
        .expect("the inner scope still authorizes its listener");
    assert_eq!(response.text, ANSWER);
    drop(inner);
    let response = stream_once(&local_config(only_a.port()), &cancel)
        .expect("full release restores pass-through production semantics");
    assert_eq!(response.text, ANSWER);
}

// ---------------------------------------------------------------------------
// R1-F4：诊断脱敏——包括无法解析的 URL
// ---------------------------------------------------------------------------

#[test]
fn diagnostics_never_echo_sensitive_path_or_query() {
    let _serial = SERIAL.lock().unwrap();
    let probe = ProbeServer::spawn(Reply::Sse(sse_chunks(ANSWER)));
    let cancel = CancelToken::new();
    let _arm = arm_outbound_guard(one_allowed(probe.addr));
    // 解析失败的 URL：原始输入携带合成敏感标记，拒绝理由不得回显。
    let mut malformed = local_config(probe.port());
    malformed.endpoint = "not a valid url/SYNTHETIC_PRIVATE_PATH".into();
    malformed.request_path = "/x?q=SYNTHETIC_SECRET_QUERY".into();
    let error = stream_once(&malformed, &cancel).expect_err("malformed URLs must be refused");
    let message = error.to_string();
    assert!(
        !message.contains("SYNTHETIC_PRIVATE_PATH") && !message.contains("SYNTHETIC_SECRET_QUERY"),
        "diagnostics must stay redacted for unparseable URLs: {message}"
    );
    assert!(message.contains("redacted"), "{message}");
    // 解析成功但未授权：携带 query 的路径同样不得回显（用未授权端口 1，
    // 拒绝发生在连接前，无需真实监听器）。
    let mut with_query = local_config(probe.port());
    with_query.endpoint = "http://127.0.0.1:1".into();
    with_query.request_path = "/chat/completions?token=SYNTHETIC_SECRET_QUERY&x=1".to_string();
    let error = stream_once(&with_query, &cancel)
        .expect_err("the wrong port must be refused regardless of the path");
    assert!(
        !error.to_string().contains("SYNTHETIC_SECRET_QUERY"),
        "rejections must not echo the query: {error}"
    );
    assert_eq!(probe.hits(), 0);
}

// ---------------------------------------------------------------------------
// R1-F5：授权的字面 IPv6 环回监听器可用
// ---------------------------------------------------------------------------

#[test]
fn authorized_literal_ipv6_loopback_is_usable() {
    let _serial = SERIAL.lock().unwrap();
    let probe = ProbeServer::spawn_at(
        IpAddr::from([0, 0, 0, 0, 0, 0, 0, 1]),
        Reply::Sse(sse_chunks(ANSWER)),
    );
    let unauthorized = ProbeServer::spawn_at(
        IpAddr::from([0, 0, 0, 0, 0, 0, 0, 1]),
        Reply::Sse(sse_chunks("OTHER")),
    );
    let cancel = CancelToken::new();
    let _arm = arm_outbound_guard(one_allowed(probe.addr));
    let mut config = local_config(probe.port());
    config.endpoint = format!("http://[::1]:{}", probe.port());
    let response =
        stream_once(&config, &cancel).expect("an authorized literal [::1] listener must be usable");
    assert_eq!(response.text, ANSWER);
    // 负控：同主机未授权端口仍拒绝；授权集合里的 IPv4 端口不被 IPv6 冒用。
    let mut wrong = local_config(probe.port());
    wrong.endpoint = format!("http://[::1]:{}", unauthorized.port());
    assert!(stream_once(&wrong, &cancel).is_err());
    assert_eq!(unauthorized.hits(), 0);
    // 负控：非字面环回主机名仍拒绝。
    let mut domain = local_config(probe.port());
    domain.endpoint = format!("http://localhost:{}", probe.port());
    let error = stream_once(&domain, &cancel).expect_err("DNS names stay refused");
    assert!(error.to_string().contains("not a literal IP"), "{error}");
}

// ---------------------------------------------------------------------------
// R2-F2：解除全部隔离作用域后，同一实例恢复普通生产语义
// ---------------------------------------------------------------------------

#[test]
fn adapter_restores_normal_transport_when_all_scopes_release() {
    let _serial = SERIAL.lock().unwrap();
    let target = ProbeServer::spawn(Reply::Sse(sse_chunks("RESTORED-NORMAL")));
    let entry = ProbeServer::spawn(Reply::Redirect(format!(
        "http://127.0.0.1:{}/exfiltrate",
        target.port()
    )));
    let config = local_config(entry.port());
    // 同一实例跨越隔离生命周期：未武装创建 → 武装（受限）→ 解除（恢复）。
    let mut model = openai_compatible(&config, &fake_credentials()).expect("build adapter once");
    let stream = |model: &mut Box<dyn Model>, cancel: &CancelToken| {
        let items = [ModelItem::user_text("lifecycle probe")];
        let request = ModelRequest {
            instructions: None,
            items: &items,
            tools: &[],
            options: &ModelOptions::default(),
            cancel,
        };
        model.stream(request, &mut RecordingSink::default())
    };
    {
        let _arm = arm_outbound_guard(one_allowed(entry.addr));
        let cancel = CancelToken::new();
        let error = stream(&mut model, &cancel)
            .expect_err("while armed the same instance must not follow the redirect");
        assert!(
            error.to_string().contains("302"),
            "the unfollowed redirect must surface: {error}"
        );
        assert_eq!(target.hits(), 0);
    }
    // 最后一个作用域解除：同一实例必须恢复普通重定向跟随语义
    // （fresh 未武装实例的正控蕴含在同一实例上，隔离不永久残留）。
    let cancel = CancelToken::new();
    let response = stream(&mut model, &cancel)
        .expect("after the last scope releases, the same instance restores normal transport");
    assert_eq!(
        response.text, "RESTORED-NORMAL",
        "normal redirect-following semantics must be restored on the same instance"
    );
    assert_eq!(target.hits(), 1, "the redirect target is reachable again");
    assert_eq!(entry.hits(), 2, "each send dialed the entry first");
}

// 环境一致性自检：武装令牌 Drop 后必须解除武装（下一个测试的未武装
// 生产语义不受污染）。
#[test]
fn arming_is_scoped_and_releases_on_drop() {
    let _serial = SERIAL.lock().unwrap();
    let probe = ProbeServer::spawn(Reply::Sse(sse_chunks(ANSWER)));
    {
        let _arm = arm_outbound_guard(one_allowed(SocketAddr::new(
            IpAddr::from([127, 0, 0, 1]),
            1,
        )));
        let cancel = CancelToken::new();
        // 端口 1 不在真实监听器内，但武装集合含 127.0.0.1:1——授权外的
        // 探针端口必须被拒。
        assert!(stream_once(&local_config(probe.port()), &cancel).is_err());
    }
    // Drop 后未武装：防线直通（生产语义），本地往返成功。
    let cancel = CancelToken::new();
    let response = stream_once(&local_config(probe.port()), &cancel)
        .expect("disarm restores pass-through production semantics");
    assert_eq!(response.text, ANSWER);
}
