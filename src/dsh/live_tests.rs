//! D-1v3：真实 Typert 宿主 gated e2e——bunx 固定
//! `@deepseek-ai/dsh@0.2.0-rc.2`，跑 S1–S3 全链：就绪行 token 捕获
//! → cookie 交换 → 能力面探测 → session/list/create/rename → mux 连接
//! + follow 快照。审批/prompt 腿需要已配置的模型供应商，不属本腿。
//!
//! 门控：`CLAT_LIVE_DSH_E2E=1` 且 bunx 可用；两者缺一自跳过（CI 无
//! bunx 静默过）。隔离：`DSH_HOME=<temp>`——绝不触碰负责人 `~/.dsh`
//!（settings-file/src/index.ts:25 实证 DSH_HOME 覆盖语义）。

use crate::dsh::client::{DshClient, exchange_token, probe_typert};
use crate::dsh::mux;
use serde_json::json;
use std::io::Read as _;
use std::sync::Arc;
use std::sync::atomic::AtomicU64;
use std::time::{Duration, Instant};

const READY_TIMEOUT: Duration = Duration::from_secs(45);
const PINNED_DSH_PACKAGE: &str = "@deepseek-ai/dsh@0.2.0-rc.2";

#[test]
#[ignore = "live: set CLAT_LIVE_DSH_E2E=1 with bunx available"]
fn live_typert_host_round_trip() {
    if std::env::var("CLAT_LIVE_DSH_E2E").ok().as_deref() != Some("1") {
        eprintln!("live_typert_host: not armed (set CLAT_LIVE_DSH_E2E=1)");
        return;
    }
    if !bunx_available() {
        eprintln!("live_typert_host: bunx not found — skipping");
        return;
    }

    // 临时家目录 + 空闲端口。
    let home = std::env::temp_dir().join(format!(
        "clat-dsh-live-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos()
    ));
    std::fs::create_dir_all(&home).expect("temp DSH_HOME");
    let scratch = std::net::TcpListener::bind("127.0.0.1:0").expect("scratch port");
    let port = scratch.local_addr().expect("addr").port();
    drop(scratch);

    // 起宿主（进程组，Drop 树级清理）。
    use command_group::CommandGroup as _;
    let mut child = std::process::Command::new("bunx")
        .env("DSH_HOME", &home)
        .args([PINNED_DSH_PACKAGE, "web", "--no-open"])
        .arg("--port")
        .arg(port.to_string())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .stdin(std::process::Stdio::null())
        .group_spawn()
        .expect("spawn the live dsh web host");
    // 就绪行：`dsh web: http://127.0.0.1:P/?token=T`（research §2：
    // launch token 只打印一次）。stdout 句柄先取走，守卫后建（借用
    // 分离）。
    let mut stdout = child.inner().stdout.take().expect("piped stdout");
    let _guard = DropGuard(&mut child, &home);
    let mut line = String::new();
    let token = {
        let deadline = Instant::now() + READY_TIMEOUT;
        let mut token = None;
        while token.is_none() && Instant::now() < deadline {
            line.clear();
            // 有界行读：就绪行之前的输出行逐段消费。
            let mut byte = [0u8; 1];
            loop {
                match stdout.read(&mut byte) {
                    Ok(0) => break,
                    Ok(_) if byte[0] == b'\n' => break,
                    Ok(_) => {
                        line.push(byte[0] as char);
                        if line.len() > 8192 {
                            line.clear();
                        }
                    }
                    Err(_) => break,
                }
            }
            if let Some(start) = line.find("token=") {
                let tail = &line[start + "token=".len()..];
                let candidate: String = tail
                    .chars()
                    .take_while(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '='))
                    .collect();
                if !candidate.is_empty() {
                    token = Some(candidate);
                }
            }
            if token.is_none() {
                std::thread::sleep(Duration::from_millis(100));
            }
        }
        token.expect("the live host printed its launch token")
    };

    // S1：token → cookie → 能力面。
    let cookie = exchange_token(port, &token).expect("live token exchange");
    assert!(cookie.starts_with("dsh-auth-"));
    assert_eq!(cookie.matches('=').count(), 1, "only one cookie name=value");
    let client = DshClient::new(port).with_cookie(&cookie).with_typert_era();
    probe_typert(&client).expect("live capability probe");
    let legacy_describe = client
        .call("host.describe", json!({}))
        .expect_err("0.2.0-rc.2 no longer serves the legacy fingerprint");
    assert_eq!(legacy_describe.code, "http-404");

    // S2：会话面单发（载荷与 run_task 同源：args 包裹在传输缝、参数名
    // = TS 参数名——list(_request)/其余 request）。
    let list = client
        .call("session/list", json!({"_request": {}}))
        .expect("session/list");
    assert!(list.get("items").is_some(), "list shape: {list}");
    let created = client
        .call(
            "session/create",
            json!({"request": {"cwd": home.to_string_lossy(), "agentPreset": "standard"}}),
        )
        .expect("session/create");
    let session = created["sessionId"]
        .as_str()
        .expect("create returns sessionId")
        .to_owned();
    let renamed = client
        .call(
            "session/rename",
            json!({"request": {"sessionId": session, "title": "clat-live-e2e"}}),
        )
        .expect("session/rename");
    assert_eq!(renamed["title"], json!("clat-live-e2e"));

    // S3：mux + follow 快照锚。
    let (events_tx, events_rx) = std::sync::mpsc::sync_channel(16);
    let epoch = Arc::new(AtomicU64::new(1));
    let controller =
        mux::open(port, &cookie, Some(&session), events_tx, 1, &epoch).expect("live mux opens");
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let subscribed = events_rx.try_recv().ok().is_some_and(|event| {
            matches!(
                event,
                crate::dsh::backend::DshEvent::Frame {
                    frame: crate::dsh::frames::DshFrame::Subscribed { .. },
                    ..
                }
            )
        });
        if subscribed {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the live follow snapshot never anchored"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
    drop(controller);
    let (fault_code, heartbeat_interval) = observe_mux_wire(port, &cookie);
    assert!(fault_code.starts_with("gateway/"), "{fault_code}");
    assert!(
        (Duration::from_secs(1)..=Duration::from_secs(4)).contains(&heartbeat_interval),
        "host heartbeat interval: {heartbeat_interval:?}"
    );
    eprintln!(
        "live 0.2.0-rc.2 mux: unknown endpoint={fault_code}, ping interval={heartbeat_interval:?}"
    );
}

/// 独立原始 WS 探针：验证带 cookie 的 101、逻辑流错误形状和服务端
/// ping 周期。只返回结构数据，不泄露 launch token 或签名 cookie。
fn observe_mux_wire(port: u16, cookie: &str) -> (String, Duration) {
    use crate::dsh::ws::{self, FrameAssembler, WsMessage};
    use std::io::Write as _;
    let mut socket = std::net::TcpStream::connect(("127.0.0.1", port)).expect("mux TCP");
    socket
        .set_read_timeout(Some(Duration::from_secs(10)))
        .expect("probe timeout");
    let key = ws::base64_encode(&ws::uuid_key_bytes());
    let request = format!(
        "GET /api/remote.mux HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\n\
         Upgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Key: {key}\r\n\
         Sec-WebSocket-Version: 13\r\nCookie: {cookie}\r\n\r\n"
    );
    socket.write_all(request.as_bytes()).expect("mux request");
    let mut header = Vec::new();
    let mut byte = [0u8; 1];
    while !header.ends_with(b"\r\n\r\n") {
        socket.read_exact(&mut byte).expect("mux response header");
        header.push(byte[0]);
        assert!(header.len() < 4096, "bounded mux handshake");
    }
    ws::verify_handshake(&String::from_utf8_lossy(&header), &key).expect("101 accept");
    socket
        .write_all(&ws::encode_client_text(
            &json!({"type":"open", "streamId":"bad", "endpoint":"session/notAMethod",
                "payload":{"args":{}}})
            .to_string(),
        ))
        .expect("bad stream open");
    let mut assembler = FrameAssembler::new();
    let mut buffer = [0u8; 4096];
    let mut fault_code = None;
    let mut first_ping = None;
    let mut interval = None;
    let deadline = Instant::now() + Duration::from_secs(10);
    while (fault_code.is_none() || interval.is_none()) && Instant::now() < deadline {
        let read = socket.read(&mut buffer).expect("mux probe read");
        assert!(read > 0, "host closed mux before probe completed");
        for message in assembler.push(&buffer[..read]).expect("WS frame") {
            match message {
                WsMessage::Text(text) => {
                    let frame: serde_json::Value = serde_json::from_str(&text).expect("mux JSON");
                    if frame["type"] == "error" && frame["streamId"] == "bad" {
                        fault_code = frame["error"]["code"].as_str().map(str::to_owned);
                    }
                }
                WsMessage::Ping(payload) => {
                    let now = Instant::now();
                    if let Some(first) = first_ping {
                        interval = Some(now.duration_since(first));
                    } else {
                        first_ping = Some(now);
                    }
                    socket
                        .write_all(&ws::encode_client_pong(&payload))
                        .expect("pong");
                }
                WsMessage::Closed(reason) | WsMessage::Failed(reason) => {
                    panic!("mux probe closed: {reason}")
                }
            }
        }
    }
    (
        fault_code.expect("error code"),
        interval.expect("two pings"),
    )
}

fn bunx_available() -> bool {
    std::process::Command::new("bunx")
        .arg("--version")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

/// DV-10/D gated 真实腿：宿主已起、**不使用 launch token**——预置
/// DSH_HOME 凭据单条记录（已知密钥），`ensure_online` 的 401 探测分支
/// 自铸短窗 cookie 直连。真 browser-auth（HMAC/名字/时间窗）是唯一
/// 裁判：铸错任何一项即 401 → 红。判别：撤 minted_online（或撤预置
/// 记录）即红。
#[test]
#[ignore = "live: set CLAT_LIVE_DSH_E2E=1 with bunx available"]
fn live_minted_cookie_connects_to_a_real_host() {
    use crate::dsh::connect::{ConnectFailure, ensure_online};
    if std::env::var("CLAT_LIVE_DSH_E2E").ok().as_deref() != Some("1") {
        eprintln!("live_minted_cookie: not armed (set CLAT_LIVE_DSH_E2E=1)");
        return;
    }
    if !bunx_available() {
        eprintln!("live_minted_cookie: bunx not found — skipping");
        return;
    }
    let home = std::env::temp_dir().join(format!(
        "clat-dsh-live-mint-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos()
    ));
    std::fs::create_dir_all(&home).expect("temp DSH_HOME");
    // 预置凭据单条记录：宿主 initializeSecret 见记录即沿用（凭证侧
    // 幂等），CLAT 与宿主读到同一密钥。
    let secret = [0xa7u8; 32];
    {
        use base64::Engine as _;
        use base64::engine::general_purpose::URL_SAFE_NO_PAD;
        let mut text = String::from("version: 1\nrefs: {}\nrecords:\n");
        text.push_str("  client-connection/browser-session:\n");
        text.push_str("    kind: grant\n");
        text.push_str("    payload:\n");
        text.push_str("      version: 1\n");
        text.push_str("      secret: ");
        text.push_str(&URL_SAFE_NO_PAD.encode(secret));
        text.push('\n');
        std::fs::write(home.join(".credentials.yaml"), text).expect("preset credentials");
        // DSH 的 credentials provider 拒绝 group/other 可读（0600 纪律，
        // credentials-local assertOwnerOnly）——fs::write 默认 0644 会让
        // 宿主拒绝启动。
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(
                home.join(".credentials.yaml"),
                std::fs::Permissions::from_mode(0o600),
            )
            .expect("chmod 600");
        }
    }

    // 起宿主（读同一 DSH_HOME）；就绪即弃 token——本腿只允许走铸
    // cookie 通路。
    use command_group::CommandGroup as _;
    let scratch = std::net::TcpListener::bind("127.0.0.1:0").expect("scratch port");
    let port = scratch.local_addr().expect("addr").port();
    drop(scratch);
    let mut child = std::process::Command::new("bunx")
        .env("DSH_HOME", &home)
        .args([PINNED_DSH_PACKAGE, "web", "--no-open"])
        .arg("--port")
        .arg(port.to_string())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .stdin(std::process::Stdio::null())
        .group_spawn()
        .expect("spawn the live dsh web host");
    let _guard = DropGuard(&mut child, &home);
    // 等宿主**以 401 应答 describe**——那是铸 cookie 分支的精确入口
    //（TCP 能连 ≠ HTTP 已就绪：启动窗口内探测得传输错误会误走 spawn
    // 路，research §5 就绪语义）。
    let deadline = Instant::now() + READY_TIMEOUT;
    loop {
        let auth_ready = DshClient::new(port)
            .probe_describe(port)
            .err()
            .is_some_and(|error| error.code == "http-401");
        if auth_ready {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the live host never started demanding credentials"
        );
        std::thread::sleep(Duration::from_millis(100));
    }

    match ensure_online(port, "/nonexistent/dsh", Some(&home)) {
        Ok(online) => {
            assert_eq!(online.era, crate::dsh::client::DshEra::Typert);
            assert_eq!(online.port, port);
            assert!(
                online
                    .cookie
                    .as_deref()
                    .is_some_and(|c| c.starts_with("dsh-auth-"))
            );
            assert!(online.child.is_none(), "a foreign live host is never owned");
        }
        Err(ConnectFailure::Failed(message)) => {
            panic!("the real host must accept our minted cookie: {message}");
        }
        other => panic!("expected an Online or Failed, got {other:?}"),
    }
}

/// 树级清理 + 临时家目录回收（连失败路径也不留孤儿/垃圾）。
struct DropGuard<'a>(&'a mut command_group::GroupChild, &'a std::path::Path);

impl Drop for DropGuard<'_> {
    fn drop(&mut self) {
        let _ = crate::dsh::connect::terminate_dsh_host(self.0);
        let _ = std::fs::remove_dir_all(self.1);
    }
}
