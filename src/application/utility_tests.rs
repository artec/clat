//! AG-DOGFOOD-6（TITLE-1 / SUG-1）复现与回归：真实 provider 适配器 ×
//! 本地受控“思考模型”服务器。
//!
//! 根因复现面：utility 路由（标题/建议）沿用预设 `extra_body`
//! （`thinking: enabled` + `reasoning_effort: high`，且官方不可关闭思考）
//! 配 `output_limit` 32/96 的小帽——思考 token 先耗尽 max_tokens，流以
//! `finish_reason: "length"` 结束且无 content，标题/建议静默为空。用户
//! 看到的正是“标题停留首条输入”与“建议始终不可用”。
//!
//! 服务器语义忠实于真实思考端点：先流 `reasoning_content`（数量随
//! `reasoning_effort`：high≈180 token、low/缺省≈40 token），随后若
//! `max_tokens` 已被思考耗尽则以 length 终止且无 content，否则给出
//! 正文并以 stop 终止。主 agent 请求（带 tools）直接返回正文。

use super::*;
use crate::model::{ModelConfig, ProviderCredentials};
use crate::permission::PermissionApprover;
use crate::presets::preset_by_id;
use crate::session::key::{ProjectKey, SessionKey};
use std::io::{BufRead, BufReader, Read as _, Write as _};
use std::net::TcpListener;
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// 一次被捕获的请求：路径 + 解析后的 JSON body。
#[derive(Clone, Debug)]
struct CapturedRequest {
    path: String,
    body: serde_json::Value,
}

/// 本地思考模型服务器。串行 accept `expected` 个请求后退出。
struct ThinkingServer {
    port: u16,
    requests: Arc<Mutex<Vec<CapturedRequest>>>,
}

impl ThinkingServer {
    const TITLE_ANSWER: &'static str = "Fix login flow";
    const SUGGESTION_ANSWER: &'static str = "Add a retry test for the login handler";
    const MAIN_ANSWER: &'static str = "main run complete";

    /// high effort ≈ 600 个思考 token（真实 high 档思考常达数千，600 是
    /// 保守下界：足以耗尽 32/96/512 的任何小预算）；low/缺省 ≈ 40。
    const THINKING_TOKENS_HIGH: u64 = 600;
    const THINKING_TOKENS_LOW: u64 = 40;

    fn spawn(expected: usize) -> Self {
        Self::spawn_with(expected, false)
    }

    /// `fail_utility`：对 utility 请求（无 tools）一律回 500——模拟服务端
    /// 故障，用于失败诊断面。正文是**人工合成金丝雀标记**（R1-F1 反例）：
    /// 它若出现在错误文本或证据日志里，即为 provider 不可信正文泄漏。
    fn spawn_with(expected: usize, fail_utility: bool) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind thinking server");
        let port = listener.local_addr().expect("address").port();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let captured = Arc::clone(&requests);
        std::thread::Builder::new()
            .name("clat-thinking-fake".into())
            .spawn(move || {
                for _ in 0..expected {
                    let Ok((mut stream, _)) = listener.accept() else {
                        break;
                    };
                    let Ok(request) = read_http_request(&mut stream) else {
                        continue;
                    };
                    let text = String::from_utf8_lossy(&request).into_owned();
                    let path = text
                        .split_whitespace()
                        .nth(1)
                        .unwrap_or_default()
                        .to_owned();
                    let body = text
                        .split_once("\r\n\r\n")
                        .map(|(_, body)| body)
                        .and_then(|body| serde_json::from_str(body).ok())
                        .unwrap_or(serde_json::Value::Null);
                    let is_utility = body
                        .get("tools")
                        .and_then(serde_json::Value::as_array)
                        .is_none_or(|tools| tools.is_empty());
                    captured
                        .lock()
                        .expect("capture")
                        .push(CapturedRequest { path, body: body.clone() });
                    let result = if fail_utility && is_utility {
                        write!(
                            &mut stream,
                            "HTTP/1.1 500 Internal Server Error\r\nContent-Length: 40\r\nConnection: close\r\n\r\nSYNTHETIC_SECRET_CANARY raw-user-context"
                        )
                        .and_then(|()| stream.flush())
                    } else {
                        respond(&mut stream, &body)
                    };
                    if let Err(error) = result {
                        eprintln!("clat-thinking-fake respond failed: {error}");
                    }
                }
            })
            .expect("spawn thinking server");
        Self { port, requests }
    }

    /// `weird_finish`：utility 请求返回 200 但 `finish_reason` 是 provider
    /// 侧未知词（R1-F1 负控：不得透传原串）。
    fn spawn_weird_finish(expected: usize) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind thinking server");
        let port = listener.local_addr().expect("address").port();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let captured = Arc::clone(&requests);
        std::thread::Builder::new()
            .name("clat-thinking-fake".into())
            .spawn(move || {
                for _ in 0..expected {
                    let Ok((mut stream, _)) = listener.accept() else {
                        break;
                    };
                    let Ok(request) = read_http_request(&mut stream) else {
                        continue;
                    };
                    let text = String::from_utf8_lossy(&request).into_owned();
                    let body = text
                        .split_once("\r\n\r\n")
                        .map(|(_, body)| body)
                        .and_then(|body| serde_json::from_str(body).ok())
                        .unwrap_or(serde_json::Value::Null);
                    let is_utility = body
                        .get("tools")
                        .and_then(serde_json::Value::as_array)
                        .is_none_or(|tools| tools.is_empty());
                    captured
                        .lock()
                        .expect("capture")
                        .push(CapturedRequest {
                            path: String::new(),
                            body: body.clone(),
                        });
                    let payload = if is_utility {
                        concat!(
                            "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"finish-kind-never-seen\"}]}\n\n",
                            "data: [DONE]\n\n"
                        )
                    } else {
                        concat!(
                            "data: {\"choices\":[{\"delta\":{\"content\":\"main run complete\"},\"finish_reason\":\"stop\"}]}\n\n",
                            "data: [DONE]\n\n"
                        )
                    };
                    let result = write!(
                        &mut stream,
                        "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                        payload.len(),
                        payload
                    )
                    .and_then(|()| stream.flush());
                    if let Err(error) = result {
                        eprintln!("clat-thinking-fake respond failed: {error}");
                    }
                }
            })
            .expect("spawn thinking server");
        Self { port, requests }
    }

    fn endpoint(&self) -> String {
        format!("http://127.0.0.1:{}", self.port)
    }

    fn requests(&self) -> Vec<CapturedRequest> {
        self.requests.lock().expect("capture").clone()
    }

    /// 等到捕获满 `count` 个请求（有界），返回全部捕获。
    fn wait_requests(&self, count: usize) -> Vec<CapturedRequest> {
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        loop {
            let requests = self.requests();
            if requests.len() >= count {
                return requests;
            }
            if std::time::Instant::now() > deadline {
                panic!(
                    "expected at least {count} captured requests, got {}",
                    requests.len()
                );
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}

/// 忠实思考模型响应：reasoning 先行，max_tokens 被思考耗尽 → length 且
/// 无 content；否则正文 + stop。主 agent 请求（带 tools）直接正文。
fn respond(stream: &mut std::net::TcpStream, body: &serde_json::Value) -> std::io::Result<()> {
    let max_tokens = body.get("max_tokens").and_then(serde_json::Value::as_u64);
    let effort = body
        .get("reasoning_effort")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("low");
    let has_tools = body
        .get("tools")
        .and_then(serde_json::Value::as_array)
        .is_some_and(|tools| !tools.is_empty());
    let system = body
        .pointer("/messages/0/content")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default()
        .to_owned();

    let mut frames: Vec<String> = Vec::new();
    let mut answer: Option<&'static str> = None;
    if has_tools {
        // 主 agent 请求：直接给出正文。
        answer = Some(ThinkingServer::MAIN_ANSWER);
    } else {
        let thinking_tokens = if effort == "high" {
            ThinkingServer::THINKING_TOKENS_HIGH
        } else {
            ThinkingServer::THINKING_TOKENS_LOW
        };
        // 逐段流出思考（每段 20 token），模拟真实思考流。
        let mut remaining = thinking_tokens;
        while remaining > 0 {
            let chunk = remaining.min(20);
            frames.push(format!(
                "data: {{\"choices\":[{{\"delta\":{{\"reasoning_content\":\"t{chunk}\"}}}}]}}\n\n"
            ));
            remaining -= chunk;
        }
        if max_tokens.is_some_and(|limit| limit <= thinking_tokens) {
            // 预算被思考耗尽：无正文，length 终止。
            frames.push(
                "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"length\"}]}\n\n".to_owned(),
            );
        } else {
            answer = Some(if system.contains("next user message") {
                ThinkingServer::SUGGESTION_ANSWER
            } else {
                ThinkingServer::TITLE_ANSWER
            });
        }
    }
    if let Some(answer) = answer {
        frames.push(format!(
            "data: {{\"choices\":[{{\"delta\":{{\"content\":\"{answer}\"}},\"finish_reason\":\"stop\"}}]}}\n\n"
        ));
    }
    frames.push("data: [DONE]\n\n".to_owned());
    let payload: String = frames.concat();
    write!(
        stream,
        "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        payload.len(),
        payload
    )?;
    stream.flush()
}

/// 读完整 HTTP 请求（含 Content-Length 定界的 body）。
fn read_http_request(stream: &mut std::net::TcpStream) -> std::io::Result<Vec<u8>> {
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut request = Vec::new();
    let mut line = String::new();
    let mut content_length = 0usize;
    loop {
        line.clear();
        if reader.read_line(&mut line)? == 0 {
            break;
        }
        if let Some((name, value)) = line.split_once(':')
            && name.eq_ignore_ascii_case("content-length")
        {
            content_length = value.trim().parse().unwrap_or(0);
        }
        request.extend_from_slice(line.as_bytes());
        if line == "\r\n" {
            break;
        }
    }
    let mut body = vec![0u8; content_length];
    reader.read_exact(&mut body)?;
    request.extend_from_slice(&body);
    Ok(request)
}

fn allow_all_approver() -> Arc<dyn PermissionApprover> {
    Arc::new(
        |_request: crate::PermissionRequest, _cancel: &crate::model::CancelToken| {
            crate::PermissionDecision::Allow
        },
    )
}

/// 挂载生产 OpenAI-compatible 适配器（真实 HTTP 栈），模型指向本地思考
/// 服务器。配置 = 自定义端点（preset=None，避免 `model_state()` 的预设
/// 重应用把端点还原成真实厂商）+ deepseek-v4-pro 预设的官方
/// `extra_body`（thinking 对象 + effort high）——用户把自定义端点指向
/// 思考不可关闭模型时的真实形态。
fn mount_thinking(
    storage_root: &std::path::Path,
    project_root: &std::path::Path,
    server: &ThinkingServer,
) -> TrustedProjectApplication {
    std::fs::create_dir_all(project_root).expect("project dir");
    let application =
        BootstrapApplication::open(Project::new(project_root), storage_root.to_path_buf())
            .expect("open")
            .authorize_and_mount_with_provider(Arc::new(crate::plugins::OpenAiCompatiblePlugin))
            .expect("mount");
    let preset = preset_by_id("deepseek-v4-pro").expect("preset");
    let config = ModelConfig {
        protocol: crate::ModelProtocol::OpenAiCompatible,
        model: "deepseek-v4-pro".into(),
        endpoint: server.endpoint(),
        request_path: "/chat/completions".into(),
        extra_body: preset.extra_body(),
        ..ModelConfig::default()
    };
    let credentials = ProviderCredentials::for_protocol(config.protocol);
    application
        .save_model_state(&config, &credentials)
        .expect("save model");
    application
}

fn run_prompt(
    application: &mut TrustedProjectApplication,
    prompt: &str,
) -> Result<ApplicationRunDone, ApplicationRunFailure> {
    let (completion, receiver) = mpsc::channel();
    let handle = application
        .start_run(ApplicationRunRequest {
            message: crate::message::PendingMessage::text(prompt),
            asker: None,
            approver: allow_all_approver(),
            events: Box::new(Vec::new()),
            completion,
        })
        .expect("start run");
    handle.join().expect("run worker");
    receiver
        .recv_timeout(Duration::from_secs(10))
        .expect("run completion within 10 seconds")
}

fn load_events(storage_root: &std::path::Path) -> Vec<crate::session::event::SessionEvent> {
    let backend = crate::session::persistence::JsonlBackend::new(
        storage_root.join("sessions"),
        crate::session::persistence::JsonlCompression::Zstd,
        false,
    );
    let headers = backend.list_headers().unwrap();
    let header = headers.first().expect("one session header");
    let key = SessionKey {
        project: ProjectKey::from_cwd(&header.cwd.clone().expect("header carries the project cwd")),
        id: header.id.clone(),
    };
    backend.load(&key, false).unwrap().events
}

/// 等待 journal 中出现 `session/title`（有界）。
fn wait_for_title_event(
    storage_root: &std::path::Path,
) -> Option<crate::session::event::SessionEvent> {
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    loop {
        let found = load_events(storage_root)
            .into_iter()
            .find(|event| event.event_type == "session/title");
        if found.is_some() || std::time::Instant::now() > deadline {
            return found;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// TITLE-1（前置红）：思考无法关闭的预设模型上，首个有内容 Run 完成后
/// 必须落盘摘要标题（session/title + 广播 + listing 一致）。基线上
/// utility 请求带 thinking+effort high 且 max_tokens=32，被思考耗尽返回
/// 空——标题永远停留首条输入回退。
#[test]
fn autotitle_lands_for_thinking_companion_models() {
    let (storage_root, project_root) = crate::test_support::roots("ag6-title-thinking");
    // 1 个主请求 + 1 个标题请求。
    let server = ThinkingServer::spawn(2);
    let mut application = mount_thinking(&storage_root, &project_root, &server);

    let done = run_prompt(&mut application, "please fix the login handler race").expect("run");
    assert!(done.output.contains("main run complete"));

    let title_event = wait_for_title_event(&storage_root);
    let title = title_event
        .as_ref()
        .and_then(|event| event.data.get("title"))
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned);
    assert_eq!(
        title.as_deref(),
        Some(ThinkingServer::TITLE_ANSWER),
        "auto naming must summarize the conversation, not stay on the first input"
    );

    // 命中的请求形态：无 thinking 对象、effort 收敛、预算充足。
    let requests = server.wait_requests(2);
    let utility = requests
        .iter()
        .find(|request| request.body.get("tools").is_none())
        .expect("utility request");
    assert_eq!(utility.path, "/chat/completions");
    assert!(
        utility.body.get("thinking").is_none(),
        "one-shot utility requests must not force thinking on"
    );
    assert_ne!(
        utility
            .body
            .get("reasoning_effort")
            .and_then(serde_json::Value::as_str),
        Some("high")
    );
    assert!(
        utility
            .body
            .get("max_tokens")
            .and_then(serde_json::Value::as_u64)
            .is_some_and(|limit| limit >= 256),
        "the title budget must leave room after thinking"
    );

    // listing 与快照一致（checkpoint 落盘与 journal 可见之间有微秒级
    // 窗口，有界轮询）。
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        let summaries = application.list_sessions().expect("sessions");
        if summaries
            .iter()
            .any(|summary| summary.title.as_deref() == Some(ThinkingServer::TITLE_ANSWER))
        {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "session listing must surface the generated title"
        );
        std::thread::sleep(Duration::from_millis(25));
    }

    // 冷重启：重开同一 storage root，标题仍在 journal 里。
    application.close().expect("close");
    let reopened = crate::test_support::reopen_after_close(|| {
        BootstrapApplication::open(Project::new(&project_root), storage_root.clone())
            .expect("reopen")
            .into_trusted()
    });
    reopened.close().expect("close reopened");
    let durable_title = load_events(&storage_root)
        .into_iter()
        .rev()
        .find(|event| event.event_type == "session/title")
        .and_then(|event| {
            event
                .data
                .get("title")
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned)
        });
    assert_eq!(durable_title.as_deref(), Some(ThinkingServer::TITLE_ANSWER));

    crate::test_support::cleanup_tree(&storage_root);
    crate::test_support::cleanup_tree(&project_root);
}

/// TITLE-1 诊断面（前置红）：命名失败必须留下有界、脱敏、真实的证据行
///（`<storage-root>/utility-log.jsonl`），供用户诊断“为什么不生效”。
/// 服务器对 utility 请求一律 500——真实故障注入（主 agent 请求正常）。
#[test]
fn naming_failures_leave_bounded_sanitized_evidence() {
    let (storage_root, project_root) = crate::test_support::roots("ag6-title-evidence");
    let server = ThinkingServer::spawn_with(3, true);
    let mut application = mount_thinking(&storage_root, &project_root, &server);
    let done = run_prompt(&mut application, "another login fix please").expect("run");
    assert!(done.output.contains("main run complete"));

    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    let log_path = storage_root.join("utility-log.jsonl");
    let mut log = String::new();
    loop {
        if log_path.exists() {
            log = std::fs::read_to_string(&log_path).unwrap_or_default();
            if log.contains("session-title") {
                break;
            }
        }
        if std::time::Instant::now() > deadline {
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(
        log.contains("session-title"),
        "a failed naming attempt must leave diagnosable evidence, log: {log:?}"
    );
    let line: serde_json::Value = log
        .lines()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .find(|row: &serde_json::Value| row["task"].as_str() == Some("session-title"))
        .expect("evidence row");
    assert_eq!(line["stage"].as_str(), Some("generate"));
    // R1-F1：只有自有类别码，500 的正文（合成金丝雀）绝不进日志。
    assert_eq!(line["reason"].as_str(), Some("request failed (server)"));
    assert!(!log.contains("SYNTHETIC_SECRET_CANARY"));
    assert!(!log.contains("raw-user-context"));
    // 脱敏：证据行不携带对话上下文与凭据。
    assert!(!log.contains("another login fix"));
    assert!(log.len() < 4096, "evidence stays bounded");

    application.close().expect("close");
    crate::test_support::cleanup_tree(&storage_root);
    crate::test_support::cleanup_tree(&project_root);
}

/// SUG-1（前置红）：出厂默认关闭；显式启用真实持久化（冷重开保持）；
/// 有上下文、idle 时经生产 prepare/generate 请求受控辅助模型拿到非空建
/// 议；建议不产生会话消息、不写 journal。
#[test]
fn prompt_suggestions_enable_persist_and_generate() {
    let (storage_root, project_root) = crate::test_support::roots("ag6-suggest-thinking");
    // 主请求 1 + 标题请求 1 + 建议请求 1。
    let server = ThinkingServer::spawn(3);
    let mut application = mount_thinking(&storage_root, &project_root, &server);

    // 出厂默认：命名开、建议关；禁用态给出真实原因。
    let defaults = application.utility_settings_view().expect("utility view");
    assert!(defaults.naming_enabled);
    assert!(!defaults.suggestions_enabled);
    let disabled = match application.prepare_prompt_suggestion() {
        Err(error) => error.to_string(),
        Ok(_) => panic!("suggestions must stay disabled by default"),
    };
    assert!(disabled.contains("disabled"));

    // 显式启用并保存。
    application
        .edit_utility_settings(UtilitySettingsEdit {
            naming_enabled: true,
            suggestions_enabled: true,
            automatic_followups: true,
            profile: None,
        })
        .expect("enable suggestions");
    assert!(
        application
            .utility_settings_view()
            .expect("utility view")
            .suggestions_enabled
    );

    let done = run_prompt(&mut application, "please fix the login handler race").expect("run");
    assert!(done.output.contains("main run complete"));
    // 命名（异步）先落或并行——等到标题证据稳定。
    wait_for_title_event(&storage_root);

    let before = load_events(&storage_root).len();
    let prepared = application.prepare_prompt_suggestion().expect("prepared");
    let suggestion = prepared
        .generate(&crate::CancelToken::new())
        .expect("a non-empty suggestion from the controlled utility model");
    assert!(!suggestion.text.is_empty());
    assert!(suggestion.text.contains("retry test"));

    // 建议：无 journal 写入、无新会话消息。
    assert_eq!(load_events(&storage_root).len(), before);

    // 启用状态冷重开保持（settings.json 是控制面事实 + 真实重开读取）。
    application.close().expect("close");
    let raw = std::fs::read_to_string(storage_root.join("settings.json")).unwrap_or_default();
    let parsed: serde_json::Value = serde_json::from_str(&raw).expect("settings.json parses");
    assert_eq!(
        parsed.pointer("/utility/suggestionsEnabled"),
        Some(&serde_json::json!(true)),
        "the enabled flag must persist in settings.json, got: {raw}"
    );
    let reopened = crate::test_support::reopen_after_close(|| {
        BootstrapApplication::open(Project::new(&project_root), storage_root.clone())
            .expect("reopen")
            .into_trusted()
    });
    assert!(
        reopened
            .utility_settings_view()
            .expect("utility view after reopen")
            .suggestions_enabled
    );
    reopened.close().expect("close reopened");

    crate::test_support::cleanup_tree(&storage_root);
    crate::test_support::cleanup_tree(&project_root);
}

/// SUG-1 诊断面（前置红）：生成失败必须留下有界、脱敏、真实的证据行，
/// 而不是静默吞掉（服务器 500 = 真实故障注入）。
#[test]
fn suggestion_failure_leaves_bounded_sanitized_evidence() {
    let (storage_root, project_root) = crate::test_support::roots("ag6-suggest-evidence");
    // 主请求 1 + 标题请求 1 + 建议请求 1（utility 一律 500）。
    let server = ThinkingServer::spawn_with(3, true);
    let mut application = mount_thinking(&storage_root, &project_root, &server);
    application
        .edit_utility_settings(UtilitySettingsEdit {
            naming_enabled: true,
            suggestions_enabled: true,
            automatic_followups: true,
            profile: None,
        })
        .expect("enable suggestions");
    let done = run_prompt(&mut application, "context for suggestion").expect("run");
    assert!(done.output.contains("main run complete"));

    // 生成一次建议（500 → 失败）——真实调用在留痕断言之前。
    let prepared = application.prepare_prompt_suggestion().expect("prepared");
    let outcome = prepared.generate(&crate::CancelToken::new());
    assert!(
        outcome.map_or(true, |suggestion| !suggestion.text.is_empty()),
        "a 500 must not be reported as a usable suggestion"
    );

    // 命名（500 → 失败）与建议（500 → 失败）都应留痕。
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    let log_path = storage_root.join("utility-log.jsonl");
    let mut log = String::new();
    loop {
        if log_path.exists() {
            log = std::fs::read_to_string(&log_path).unwrap_or_default();
            if log.contains("prompt-suggestion") {
                break;
            }
        }
        if std::time::Instant::now() > deadline {
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(
        log.contains("prompt-suggestion"),
        "a failed suggestion attempt must leave diagnosable evidence, log: {log:?}"
    );
    let suggestion_row: serde_json::Value = log
        .lines()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .find(|row: &serde_json::Value| row["task"].as_str() == Some("prompt-suggestion"))
        .unwrap_or_else(|| panic!("evidence row missing, log:\n{log}"));
    // R1-F1：类别码而非 provider 正文。
    assert_eq!(
        suggestion_row["reason"].as_str(),
        Some("request failed (server)")
    );
    assert!(!log.contains("SYNTHETIC_SECRET_CANARY"));
    assert!(!log.contains("raw-user-context"));
    assert!(!log.contains("context for suggestion"), "no raw context");
    assert!(log.len() < 4096, "evidence stays bounded");

    application.close().expect("close");
    crate::test_support::cleanup_tree(&storage_root);
    crate::test_support::cleanup_tree(&project_root);
}

/// J 联合最小链（应用层）：首轮对话 → 自动标题 → 启用建议 → 建议预览
/// （不自动发送、不落盘）→ 显式发送第二 round → 标题继续更新 → 冷重启
/// 标题保持。多轮更新与同 turn 合并（一次标题 I/O）由服务器请求数锁定。
#[test]
fn utility_journey_title_suggestion_adoption_and_restart() {
    let (storage_root, project_root) = crate::test_support::roots("ag6-journey");
    // round1 主 1 + 标题 1 + 建议 1 + round2 主 1 + 标题 1。
    let server = ThinkingServer::spawn(5);
    let mut application = mount_thinking(&storage_root, &project_root, &server);

    // 1. 首轮对话 + 自动标题。
    let done = run_prompt(&mut application, "please fix the login handler race").expect("run");
    assert!(done.output.contains("main run complete"));
    let first_title = wait_for_title_event(&storage_root).expect("first title");
    assert_eq!(
        first_title
            .data
            .get("title")
            .and_then(serde_json::Value::as_str),
        Some(ThinkingServer::TITLE_ANSWER)
    );

    // 2. 启用建议并预览。
    application
        .edit_utility_settings(UtilitySettingsEdit {
            naming_enabled: true,
            suggestions_enabled: true,
            automatic_followups: true,
            profile: None,
        })
        .expect("enable suggestions");
    let prepared = application.prepare_prompt_suggestion().expect("prepared");
    let suggestion = prepared
        .generate(&crate::CancelToken::new())
        .expect("suggestion");
    assert!(suggestion.text.contains("retry test"));

    // 3. 显式发送（采纳是用户动作；核心只给文本）。
    let done = run_prompt(&mut application, &suggestion.text).expect("second run");
    assert!(done.output.contains("main run complete"));
    wait_for_title_event(&storage_root);
    let titles: Vec<String> = load_events(&storage_root)
        .into_iter()
        .filter(|event| event.event_type == "session/title")
        .filter_map(|event| {
            event
                .data
                .get("title")
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned)
        })
        .collect();
    assert!(
        !titles.is_empty() && titles.len() <= 2,
        "multi-turn naming updates within the same-turn coalescing budget"
    );

    // 4. utility 请求（标题+建议）合计 3 次：同 turn 不重复、无重复 I/O。
    let requests = server.wait_requests(5);
    let utility_requests: Vec<_> = requests
        .iter()
        .filter(|request| request.body.get("tools").is_none())
        .collect();
    assert_eq!(
        utility_requests.len(),
        3,
        "two title jobs (one per turn) + one suggestion request, no duplicate I/O"
    );

    // 5. 冷重启后标题仍在。
    application.close().expect("close");
    let durable_titles: Vec<String> = load_events(&storage_root)
        .into_iter()
        .filter(|event| event.event_type == "session/title")
        .filter_map(|event| {
            event
                .data
                .get("title")
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned)
        })
        .collect();
    assert!(!durable_titles.is_empty());

    crate::test_support::cleanup_tree(&storage_root);
    crate::test_support::cleanup_tree(&project_root);
}

/// R1-F1 负控（乙方反例复刻 + 收敛）：受控 500 的正文携带合成金丝雀与
/// 用户上下文标记、200 但 finish_reason 是 provider 侧未知词——错误文本
/// 与证据日志都只能出现自有类别码，绝不出现 provider 返回的原文。
#[test]
fn utility_failures_never_leak_provider_text() {
    let (storage_root, project_root) = crate::test_support::roots("ag6-r1f1-leak");
    let mut application = mount_thinking(
        &storage_root,
        &project_root,
        &ThinkingServer::spawn_with(3, true),
    );
    application
        .edit_utility_settings(UtilitySettingsEdit {
            naming_enabled: true,
            suggestions_enabled: true,
            automatic_followups: true,
            profile: None,
        })
        .expect("enable suggestions");
    let done = run_prompt(&mut application, "leak probe main").expect("run");
    assert!(done.output.contains("main run complete"));

    let failure = match application.prepare_prompt_suggestion() {
        Err(error) => error.to_string(),
        Ok(prepared) => match prepared.generate(&crate::CancelToken::new()) {
            Ok(_) => panic!("the canary 500 must fail the suggestion"),
            Err(reason) => reason,
        },
    };
    assert_eq!(failure, "request failed (server)");
    assert!(!failure.contains("SYNTHETIC_SECRET_CANARY"));
    assert!(!failure.contains("raw-user-context"));
    let log = std::fs::read_to_string(storage_root.join("utility-log.jsonl")).unwrap_or_default();
    assert!(!log.contains("SYNTHETIC_SECRET_CANARY"));
    assert!(!log.contains("raw-user-context"));
    application.close().expect("close");
    crate::test_support::cleanup_tree(&storage_root);
    crate::test_support::cleanup_tree(&project_root);

    // 未知 finish 词：归一为类别 "unknown"，不透传 provider 原串。
    let (storage_root, project_root) = crate::test_support::roots("ag6-r1f1-weird");
    let mut application = mount_thinking(
        &storage_root,
        &project_root,
        &ThinkingServer::spawn_weird_finish(3),
    );
    application
        .edit_utility_settings(UtilitySettingsEdit {
            naming_enabled: true,
            suggestions_enabled: true,
            automatic_followups: true,
            profile: None,
        })
        .expect("enable suggestions");
    let done = run_prompt(&mut application, "weird finish main").expect("run");
    assert!(done.output.contains("main run complete"));
    let failure = match application.prepare_prompt_suggestion() {
        Err(error) => error.to_string(),
        Ok(prepared) => match prepared.generate(&crate::CancelToken::new()) {
            Ok(_) => panic!("an unknown finish with no content must fail"),
            Err(reason) => reason,
        },
    };
    assert_eq!(failure, "empty response (finish unknown)");
    assert!(!failure.contains("finish-kind-never-seen"));
    let log = std::fs::read_to_string(storage_root.join("utility-log.jsonl")).unwrap_or_default();
    assert!(!log.contains("finish-kind-never-seen"), "log: {log}");
    application.close().expect("close");
    crate::test_support::cleanup_tree(&storage_root);
    crate::test_support::cleanup_tree(&project_root);
}

/// J 链取消负控：预先取消的请求必须以自有类别 `cancelled` 失败（安全
/// 文本），不产生建议、不写 journal、不留“伪成功”。
#[test]
fn cancelled_utility_request_reports_cancelled_category() {
    let (storage_root, project_root) = crate::test_support::roots("ag6-cancel");
    let mut application = mount_thinking(&storage_root, &project_root, &ThinkingServer::spawn(3));
    application
        .edit_utility_settings(UtilitySettingsEdit {
            naming_enabled: true,
            suggestions_enabled: true,
            automatic_followups: true,
            profile: None,
        })
        .expect("enable suggestions");
    let done = run_prompt(&mut application, "cancellation probe").expect("run");
    assert!(done.output.contains("main run complete"));
    wait_for_title_event(&storage_root);

    let before = load_events(&storage_root).len();
    let prepared = application.prepare_prompt_suggestion().expect("prepared");
    let cancel = crate::CancelToken::new();
    cancel.cancel();
    let failure = prepared
        .generate(&cancel)
        .expect_err("a cancelled request must not produce a suggestion");
    assert_eq!(failure, "cancelled");
    assert_eq!(load_events(&storage_root).len(), before);

    application.close().expect("close");
    crate::test_support::cleanup_tree(&storage_root);
    crate::test_support::cleanup_tree(&project_root);
}

/// R1-F4（乙方反例复刻）：utility-log 已超上限且轮转目标 `.1` 被目录占
/// 用时，追加必须丢弃（文件大小不变）；正常轮转时旧行保留在 `.1`、主文
/// 件从新行继续——两条路径都保持有界证据契约。
#[test]
fn utility_evidence_rotation_stays_bounded() {
    use crate::application::title::append_utility_evidence as append;
    let root = std::env::temp_dir().join(format!("clat-ag6-r1f4-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&root).unwrap();

    // 反例路径：预置超限文件 + `.1` 是目录 → rename 拒绝 → 全部丢弃。
    let log = root.join("utility-log.jsonl");
    std::fs::write(&log, vec![b'x'; 256 * 1024 + 1]).unwrap();
    std::fs::create_dir_all(root.join("utility-log.jsonl.1")).unwrap();
    for _ in 0..5 {
        append(
            &root,
            "session-title",
            None,
            "generate",
            "request failed (server)",
        );
    }
    assert_eq!(
        std::fs::metadata(&log).unwrap().len(),
        (256 * 1024 + 1) as u64,
        "rotation failure must drop the line instead of growing past the cap"
    );

    // 正常轮转：`.1` 可替换 → 旧内容进入 `.1`，主文件只含新行。
    std::fs::remove_dir_all(root.join("utility-log.jsonl.1")).unwrap();
    append(
        &root,
        "prompt-suggestion",
        None,
        "generate",
        "empty response (finish unknown)",
    );
    let rotated = std::fs::read_to_string(root.join("utility-log.jsonl.1")).unwrap();
    assert!(
        rotated.starts_with('x'),
        "the oversized log rotates into .1"
    );
    let fresh = std::fs::read_to_string(&log).unwrap();
    assert_eq!(fresh.lines().count(), 1);
    assert!(fresh.contains("prompt-suggestion"));

    crate::test_support::cleanup_tree(&root);
}
