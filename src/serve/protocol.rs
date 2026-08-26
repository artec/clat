//! serve 协议纯层（INV-S5）：RpcResult 编解码、SSE 帧编解码、方法
//! 分派——不碰 `TcpListener`，测试直接喂字节/值断言字节/值，不开端口。
//!
//! 错误模型（§3）：方法不抛业务错，应答恒为 RpcResult；错误码 v1
//! 全集见 [`ErrorCode`]——**新增错误码 = amend**，客户端对未知码按
//! `internal` 处理（[`ErrorCode::parse`]）。方法词汇政策（§4）：
//! 新增方法 = amend；未知方法 → `bad-request`（无 not-implemented
//! 回退，map 只收已实现方法）。

use super::approver;
use super::state::ServeShared;
use crate::application::{ApplicationRunDone, ApplicationRunFailure};
use crate::event::EventSink;
use crate::{ApplicationError, ApplicationRunRequest, RunEvent, SessionId, SteerOutcome};
use serde_json::{Map, Value, json};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::mpsc;

/// `workbench.info.methods` 与实际 dispatch 的同仓能力目录。新增方法
/// 时必须同时进本表；测试钉住全集，避免 PWA 显示不存在的控制面。
pub(crate) const RPC_METHODS: &[&str] = &[
    "workbench.info",
    "session.list",
    "session.info",
    "session.new",
    "session.switch",
    "session.rename",
    "permission.set",
    "command.run",
    "prompt.send",
    "steer.send",
    "run.cancel",
    "approval.respond",
];

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ErrorCode {
    BadRequest,
    Unauthorized,
    Forbidden,
    NotFound,
    Busy,
    NotPending,
    Internal,
}

impl ErrorCode {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::BadRequest => "bad-request",
            Self::Unauthorized => "unauthorized",
            Self::Forbidden => "forbidden",
            Self::NotFound => "not-found",
            Self::Busy => "busy",
            Self::NotPending => "not-pending",
            Self::Internal => "internal",
        }
    }

    /// 未知码按 `internal`（amend 政策的读侧义务）。
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn parse(code: &str) -> Self {
        match code {
            "bad-request" => Self::BadRequest,
            "unauthorized" => Self::Unauthorized,
            "forbidden" => Self::Forbidden,
            "not-found" => Self::NotFound,
            "busy" => Self::Busy,
            "not-pending" => Self::NotPending,
            _ => Self::Internal,
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) struct RpcError {
    pub code: ErrorCode,
    pub message: String,
}

impl RpcError {
    pub(crate) fn bad_request(message: impl Into<String>) -> Self {
        Self {
            code: ErrorCode::BadRequest,
            message: message.into(),
        }
    }
    pub(crate) fn not_found(message: impl Into<String>) -> Self {
        Self {
            code: ErrorCode::NotFound,
            message: message.into(),
        }
    }
    pub(crate) fn busy(message: impl Into<String>) -> Self {
        Self {
            code: ErrorCode::Busy,
            message: message.into(),
        }
    }
    pub(crate) fn not_pending(message: impl Into<String>) -> Self {
        Self {
            code: ErrorCode::NotPending,
            message: message.into(),
        }
    }
    pub(crate) fn internal(message: impl Into<String>) -> Self {
        Self {
            code: ErrorCode::Internal,
            message: message.into(),
        }
    }
}

/// 应答编码（象限②）。
pub(crate) fn rpc_result_json(result: &Result<Value, RpcError>) -> String {
    match result {
        Ok(value) => json!({ "ok": true, "value": value }).to_string(),
        Err(error) => json!({
            "ok": false,
            "error": { "code": error.code.as_str(), "message": error.message },
        })
        .to_string(),
    }
}

/// 应答解码（测试/未来客户端读侧）：保留值或错误；未知码折叠为
/// `internal` 但原始 message 保留。
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) struct ParsedRpcResult {
    pub ok: bool,
    pub value: Option<Value>,
    ///（分类码，原始码字符串， message）。PWA2-04：未知码折叠为
    /// `internal` 分类但**保留原值**——未来客户端可按原码细分
    /// 重试/取消语义，不丢信息。
    pub error: Option<(ErrorCode, String, String)>,
}

#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn parse_rpc_result(body: &str) -> Result<ParsedRpcResult, String> {
    let parsed: Value =
        serde_json::from_str(body).map_err(|error| format!("not valid JSON: {error}"))?;
    let object = parsed.as_object().ok_or("result is not an object")?;
    let ok = object
        .get("ok")
        .and_then(Value::as_bool)
        .ok_or("missing `ok`")?;
    if ok {
        Ok(ParsedRpcResult {
            ok,
            value: object.get("value").cloned(),
            error: None,
        })
    } else {
        let error = object
            .get("error")
            .and_then(Value::as_object)
            .ok_or("missing `error` object")?;
        let code = error
            .get("code")
            .and_then(Value::as_str)
            .ok_or("missing `error.code`")?;
        let message = error
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        Ok(ParsedRpcResult {
            ok,
            value: None,
            error: Some((ErrorCode::parse(code), code.to_owned(), message)),
        })
    }
}

// —— SSE 帧编解码（§5.1）———————————————————————————————————————————

/// 三行式帧 + 空行：`event:` / `id:` / `data:`。
pub(crate) fn encode_sse_frame(event: &str, seq: u64, data: &str) -> String {
    format!("event: {event}\nid: {seq}\ndata: {data}\n\n")
}

/// 心跳 comment（SSE 规范：`:` 开头的行被客户端忽略）。
pub(crate) fn encode_heartbeat() -> &'static str {
    ": keep-alive\n\n"
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) struct ParsedSseFrame {
    pub event: Option<String>,
    pub id: Option<u64>,
    /// 多行 `data:` 以 `\n` 拼接（SSE 规范）。
    pub data: String,
}

/// 解析一段 SSE 字节流（测试读侧）：comment 行忽略、空行成帧、
/// 无 data 的帧丢弃（规范行为）。
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn parse_sse_frames(input: &str) -> Vec<ParsedSseFrame> {
    let mut frames = Vec::new();
    let mut event: Option<String> = None;
    let mut id: Option<u64> = None;
    let mut data_lines: Vec<String> = Vec::new();
    for line in input.split('\n') {
        let line = line.strip_suffix('\r').unwrap_or(line);
        if line.is_empty() {
            if !data_lines.is_empty() {
                frames.push(ParsedSseFrame {
                    event: event.take(),
                    id: id.take(),
                    data: data_lines.join("\n"),
                });
                data_lines.clear();
            } else {
                event = None;
                id = None;
            }
            continue;
        }
        if let Some(rest) = line.strip_prefix(':') {
            let _ = rest; // comment（心跳）——忽略
        } else if let Some(rest) = line.strip_prefix("event:") {
            event = Some(rest.trim_start().to_owned());
        } else if let Some(rest) = line.strip_prefix("id:") {
            id = rest.trim().parse().ok();
        } else if let Some(rest) = line.strip_prefix("data:") {
            data_lines.push(rest.strip_prefix(' ').unwrap_or(rest).to_owned());
        }
    }
    frames
}

// —— 方法分派（象限①，§4）———————————————————————————————————————————

/// POST 体（已 JSON 化）→ 方法应答。`params` 必须是对象（空体由
/// HTTP 层归一为 `{}`）；未知字段一律容忍（amend 政策），除
/// `approval.respond.escalate_to`（显式保留字段，见 approver 模块）。
pub(crate) fn dispatch(
    method: &str,
    params: &Value,
    shared: &Arc<ServeShared>,
) -> Result<Value, RpcError> {
    let params = params
        .as_object()
        .ok_or_else(|| RpcError::bad_request("params must be an object"))?;
    match method {
        "workbench.info" => {
            let snapshot = with_app(shared, |app| app.workbench_snapshot()).map_err(app_error)?;
            Ok(super::shapes::workbench_snapshot_json(
                &snapshot,
                shared.active_run_info(),
                RPC_METHODS,
            ))
        }
        "session.list" => {
            let sessions = with_app(shared, |app| app.list_sessions()).map_err(app_error)?;
            Ok(json!({
                "sessions": sessions
                    .iter()
                    .map(super::shapes::session_summary_json)
                    .collect::<Vec<_>>(),
            }))
        }
        "session.info" => {
            let session_id = with_app(shared, |app| app.current_session_id());
            let title = with_app(shared, |app| app.session_title());
            let last_seq = with_app(shared, |app| app.committed_seq());
            Ok(json!({
                "session_id": session_id.map(|id| id.as_str().to_owned()),
                "title": title,
                "last_seq": last_seq,
                "active_run": shared.active_run_info(),
            }))
        }
        "session.new" => {
            with_app(shared, |app| app.new_session()).map_err(app_error)?;
            // 惰性会话（core 事实）：id 在首条 prompt 落 journal 时物化，
            // 详情经 session.info 读取（对账偏差，见 worklist 交付注记）。
            Ok(json!({}))
        }
        "session.switch" => {
            let id = required_str(params, "id")?;
            with_app(shared, |app| app.switch_session(SessionId::new(id)))
                .map_err(session_error)?;
            Ok(json!({}))
        }
        "session.rename" => {
            let id = required_str(params, "id")?;
            let title = required_str(params, "title")?;
            let current = with_app(shared, |app| app.current_session_id());
            if current.as_ref().map(|session| session.as_str()) != Some(id.as_str()) {
                // core 只支持重命名活跃会话（对账偏差）；id 不匹配按
                // 不存在处理，绝不改到别的会话头上。
                return Err(RpcError::not_found(format!(
                    "session {id} is not the active session"
                )));
            }
            match with_app(shared, |app| app.rename_session(&title)).map_err(app_error)? {
                crate::RenameOutcome::Renamed { .. } => Ok(json!({})),
                crate::RenameOutcome::NoSession => Err(RpcError::not_found("no active session")),
                crate::RenameOutcome::Invalid => Err(RpcError::bad_request(
                    "title must be a non-empty single line",
                )),
            }
        }
        "permission.set" => {
            let raw_mode = required_str(params, "mode")?;
            let mode = crate::PermissionMode::from_journal_value(&raw_mode).ok_or_else(|| {
                RpcError::bad_request(
                    "mode must be read-only, workspace-write, or danger-full-access",
                )
            })?;
            if mode == crate::PermissionMode::FullAccess
                && params.get("confirm").and_then(Value::as_str)
                    != Some(crate::PermissionMode::FullAccess.journal_value())
            {
                return Err(RpcError::bad_request(
                    "danger-full-access requires matching `confirm`",
                ));
            }
            with_app(shared, |app| app.set_permission_mode(mode)).map_err(app_error)?;
            Ok(json!({ "mode": mode.journal_value(), "label": mode.to_string() }))
        }
        "command.run" => command_run(params, shared),
        "prompt.send" => prompt_send(params, shared),
        "steer.send" => {
            let text = required_str(params, "text")?;
            if text.trim().is_empty() {
                return Err(RpcError::bad_request("text must not be empty"));
            }
            // MM-1A additive：可选客户端幂等键（additive 可选参数；缺省
            // = 无键提交）。图片 steering 被 core admission fail-closed，
            // `Refused` 如实回 bad-request，不冒充 not_running。
            let client_message_id = optional_str(params, "clientMessageId")?;
            let message =
                crate::message::PendingMessage::from_front_end(text, client_message_id, Vec::new());
            match with_app(shared, |app| app.steer(message)) {
                SteerOutcome::Queued => Ok(json!({ "outcome": "queued" })),
                SteerOutcome::NotRunning => Ok(json!({ "outcome": "not_running" })),
                SteerOutcome::Refused { reason } => Err(RpcError::bad_request(reason)),
            }
        }
        "run.cancel" => {
            // 幂等：无 active run 也 ok（§4）。
            with_app(shared, |app| app.cancel_active_run());
            Ok(json!({}))
        }
        "approval.respond" => approver::respond(shared, params),
        other => Err(RpcError::bad_request(format!("unknown method: {other}"))),
    }
}

/// Browser-thin slash-command bridge. Commands with an existing dedicated PWA
/// interaction (model/session/permission/title), compaction ownership, or app
/// shutdown are rejected before dispatch; all informational commands and the
/// phase-4 goal continuation remain core-owned.
fn command_run(params: &Map<String, Value>, shared: &Arc<ServeShared>) -> Result<Value, RpcError> {
    let command = required_str(params, "command")?;
    let trimmed = command.trim();
    let token = trimmed
        .strip_prefix('/')
        .and_then(|tail| tail.split_whitespace().next())
        .unwrap_or_default();
    if token.is_empty() {
        return Err(RpcError::bad_request("command must start with /"));
    }
    if matches!(
        token,
        "model" | "resume" | "perm" | "permission" | "rename" | "compact" | "quit" | "exit"
    ) {
        return Err(RpcError::bad_request(format!(
            "/{token} uses a dedicated interactive frontend surface"
        )));
    }
    let words = trimmed.split_whitespace().collect::<Vec<_>>();
    let wants_goal_run = words.first().copied() == Some("/goal")
        && (words.get(1).copied() == Some("run")
            || (words.get(1).copied() == Some("create") && words.contains(&"--run")));
    let claimed_goal_run = wants_goal_run.then(|| uuid::Uuid::new_v4().to_string());
    if let Some(rpc_id) = &claimed_goal_run
        && !shared.try_claim_run(rpc_id, super::state::now_ms())
    {
        return Err(RpcError::busy("another run is already active"));
    }
    let outcome = match with_app(shared, |app| app.dispatch_command(trimmed)) {
        Ok(outcome) => outcome,
        Err(error) => {
            if claimed_goal_run.is_some() {
                shared.release_run_claim();
            }
            return Err(RpcError::bad_request(error.to_string()));
        }
    };
    if !matches!(&outcome, crate::CommandOutcome::StartGoalRun) && claimed_goal_run.is_some() {
        shared.release_run_claim();
    }
    match outcome {
        crate::CommandOutcome::Status(message) => {
            Ok(json!({ "kind": "status", "message": message }))
        }
        crate::CommandOutcome::ShowHelp { commands } => {
            let message = commands
                .iter()
                .map(|info| {
                    let aliases = info
                        .aliases
                        .iter()
                        .map(|alias| format!("/{alias}"))
                        .collect::<Vec<_>>();
                    let names = if aliases.is_empty() {
                        format!("/{}", info.name)
                    } else {
                        format!("/{} ({})", info.name, aliases.join(", "))
                    };
                    format!("{names} — {}", info.description)
                })
                .collect::<Vec<_>>()
                .join("\n");
            Ok(json!({ "kind": "status", "message": message }))
        }
        crate::CommandOutcome::ShowMcpStatus(status) => {
            let mut lines = vec![format!(
                "mcp: {}/{} connected · {} connecting",
                status.connected, status.configured, status.connecting
            )];
            lines.extend(status.servers.iter().map(|server| {
                format!(
                    "{} · {} · {} · {} tools",
                    server.name, server.transport, server.protocol_version, server.tools
                )
            }));
            lines.extend(
                status
                    .failures
                    .iter()
                    .map(|failure| format!("failure: {failure}")),
            );
            Ok(json!({ "kind": "status", "message": lines.join("\n") }))
        }
        crate::CommandOutcome::ShowContext(snapshot) => Ok(json!({
            "kind": "context",
            "context": {
                "estimator": snapshot.estimator,
                "unit": snapshot.unit,
                "base_prompt": snapshot.base_prompt_estimate,
                "project_instructions": snapshot.project_instructions_estimate,
                "plan_policy": snapshot.plan_policy_estimate,
                "skill_catalog": snapshot.skill_catalog_estimate,
                "goal_policy": snapshot.goal_policy_estimate,
                "memory": snapshot.memory_estimate,
                "memory_budget_bytes": snapshot.memory_budget_bytes,
                "tool_schemas": snapshot.tool_schemas_estimate,
                "history": snapshot.history_estimate,
                "output_reserve": snapshot.output_reserve_estimate,
                "input": snapshot.input_estimate,
                "total": snapshot.total_estimate,
                "tools": snapshot.tool_names,
                "skills": snapshot.skill_names,
                "skill_diagnostics": snapshot.skill_diagnostics.iter().map(|item| json!({
                    "source": item.source,
                    "name": item.name,
                    "kind": item.kind,
                    "message": item.message,
                })).collect::<Vec<_>>(),
            }
        })),
        crate::CommandOutcome::StartGoalRun => {
            let rpc_id = claimed_goal_run.ok_or_else(|| {
                RpcError::internal("goal run command reached execution without a run claim")
            })?;
            start_goal_run(shared, rpc_id)
        }
        crate::CommandOutcome::SessionReset => Ok(json!({ "kind": "session_reset" })),
        crate::CommandOutcome::StartModelSelection
        | crate::CommandOutcome::StartSessionSelection { .. }
        | crate::CommandOutcome::StartPermissionModeSelection { .. }
        | crate::CommandOutcome::StartTitleEdit { .. }
        | crate::CommandOutcome::StartCompaction(_)
        | crate::CommandOutcome::QuitRequested => Err(RpcError::bad_request(
            "command requires a dedicated interactive frontend surface",
        )),
    }
}

fn start_goal_run(shared: &Arc<ServeShared>, rpc_id: String) -> Result<Value, RpcError> {
    let (completion_tx, completion_rx) =
        mpsc::channel::<Result<ApplicationRunDone, ApplicationRunFailure>>();
    let started = {
        let mut app = shared.app.lock().expect("application lock");
        app.start_goal_run(ApplicationRunRequest {
            message: crate::message::PendingMessage::text(String::new()),
            asker: None,
            approver: Arc::new(approver::ServeApprover::new(Arc::clone(shared))),
            events: Box::new(FanoutSink {
                shared: Arc::clone(shared),
            }),
            completion: completion_tx,
        })
    };
    match started {
        Ok((handle, _)) => {
            shared.spawn_settler(rpc_id.clone(), completion_rx, handle);
            Ok(json!({ "kind": "goal_run", "prompt_rpc_id": rpc_id }))
        }
        Err(error) => {
            shared.release_run_claim();
            Err(RpcError::internal(format!(
                "could not start goal run: {error}"
            )))
        }
    }
}

/// `prompt.send`：受理即应答（结果在事件流，§5.5）。busy 即拒——
/// run 队列是产品语义，无病历不立（§4/§11-6）。
fn prompt_send(params: &Map<String, Value>, shared: &Arc<ServeShared>) -> Result<Value, RpcError> {
    let text = required_str(params, "text")?;
    if text.trim().is_empty() {
        return Err(RpcError::bad_request("text must not be empty"));
    }
    let attachments = match params.get("attachments") {
        None | Some(Value::Null) => Vec::new(),
        Some(Value::Array(items)) => {
            let mut paths = Vec::new();
            for item in items {
                let path = item
                    .as_str()
                    .ok_or_else(|| RpcError::bad_request("attachments must be strings"))?;
                paths.push(PathBuf::from(path));
            }
            paths
        }
        Some(_) => return Err(RpcError::bad_request("attachments must be an array")),
    };
    // MM-1A additive：可选客户端幂等键——durable user 事件与 receipts
    // 投影携带它。M-02（审查 2026-08-27）：committed 重试在 append 前
    // 拦截——同 key 同 digest 幂等成功（不重复 append，返回原回执）、
    // 同 key 异 digest conflict（bad-request）；判别走 core 的
    // `committed_admission` 生产查询，不在 serve 复刻投影逻辑。
    let client_message_id = optional_str(params, "clientMessageId")?;
    let message =
        crate::message::PendingMessage::from_front_end(text, client_message_id, attachments);
    // 第一道（claim 前）：挡住绝大多数已完成的重复提交。
    if let Some(outcome) = committed_retry_check(shared, &message) {
        return outcome;
    }

    let rpc_id = uuid::Uuid::new_v4().to_string();
    // 先占 run 槽再锁应用（锁纪律：Inner 锁内不调门面；busy 判定与
    // 缓冲归属之间无窗口）。
    if !shared.try_claim_run(&rpc_id, super::state::now_ms()) {
        return Err(RpcError::busy("another run is already active"));
    }
    // 第二道（claim 后复查）：关掉「检查与 append 之间上一个同键 run
    // 恰好完成落盘」的竞态窗口——claim 是串行点，复查之后不再可能
    // 有并发同键 append。
    if let Some(outcome) = committed_retry_check(shared, &message) {
        shared.release_run_claim();
        return outcome;
    }
    let (completion_tx, completion_rx) =
        mpsc::channel::<Result<ApplicationRunDone, ApplicationRunFailure>>();
    let client_key = message.client_message_id.clone();
    let started = {
        let mut app = shared.app.lock().expect("application lock");
        app.start_run(ApplicationRunRequest {
            message,
            asker: None,
            approver: Arc::new(approver::ServeApprover::new(Arc::clone(shared))),
            events: Box::new(FanoutSink {
                shared: Arc::clone(shared),
            }),
            completion: completion_tx,
        })
    };
    match started {
        Ok(handle) => {
            shared.spawn_settler(rpc_id.clone(), completion_rx, handle);
            // M-03：受理即应答携带 committed 回执——start_run 返回时
            // user/message 已 append+flush（prepare 在调用线程执行）、
            // 投影已 fold（flush 同步 fold），此刻 journal 即权威。
            let mut response = json!({ "prompt_rpc_id": rpc_id });
            if let Some(id) = &client_key
                && let Some(record) = with_app(shared, |app| app.committed_admission(id))
            {
                response["receipt"] = super::shapes::admission_receipt_value(&record.receipt);
            }
            Ok(response)
        }
        Err(error) => {
            shared.release_run_claim();
            Err(RpcError::internal(format!("could not start run: {error}")))
        }
    }
}

/// M-02：committed 重试的幂等/conflict 判定。返回 `Some(outcome)` =
/// 命中已提交键（调用方直接应答，不再 append）；`None` = 正常接纳。
/// digest 缺失（本仓库写入方在有键时恒写 digest；缺失只可能来自
/// 篡改/异常日志）按"无法证伪即同一提交"处理——键归属客户端，威胁
/// 模型是事故性重试而非键盗用。
fn committed_retry_check(
    shared: &Arc<ServeShared>,
    message: &crate::message::PendingMessage,
) -> Option<Result<Value, RpcError>> {
    let client_message_id = message.client_message_id.as_ref()?;
    let record = {
        let app = shared.app.lock().expect("application lock");
        app.committed_admission(client_message_id)
    }?;
    let incoming = message.request_digest();
    if let Some(recorded) = record.request_digest.as_deref()
        && recorded != incoming
    {
        return Some(Err(RpcError::bad_request(
            "clientMessageId is already committed with a different payload",
        )));
    }
    Some(Ok(json!({
        "kind": "receipt",
        "duplicate": true,
        "receipt": super::shapes::admission_receipt_value(&record.receipt),
    })))
}

fn with_app<T>(
    shared: &Arc<ServeShared>,
    call: impl FnOnce(&mut crate::TrustedProjectApplication) -> T,
) -> T {
    let mut app = shared.app.lock().expect("application lock");
    call(&mut app)
}

fn app_error(error: ApplicationError) -> RpcError {
    RpcError::internal(error.to_string())
}

/// 会话类操作：不存在的会话 id → `not-found`（§3 错误模型）。core 的
/// ApplicationError 是字符串新类型——这里按已核实的消息约定
/// （"… does not exist in this project"，trusted.rs switch_session）
/// 判定；其余落 internal。
fn session_error(error: ApplicationError) -> RpcError {
    if error.to_string().contains("does not exist") {
        RpcError::not_found(error.to_string())
    } else {
        app_error(error)
    }
}

fn required_str(params: &Map<String, Value>, field: &str) -> Result<String, RpcError> {
    params
        .get(field)
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| RpcError::bad_request(format!("missing field: {field}")))
}

/// MM-1A：可选字符串参数——缺席/Null = None；出现但非字符串/空/
/// 超界 = bad-request（幂等键不吞畸形值）。
fn optional_str(params: &Map<String, Value>, field: &str) -> Result<Option<String>, RpcError> {
    match params.get(field) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(value)) if !value.is_empty() && value.len() <= 256 => {
            Ok(Some(value.clone()))
        }
        Some(_) => Err(RpcError::bad_request(format!(
            "field `{field}` must be a non-empty string of at most 256 bytes"
        ))),
    }
}

/// 实时族 sink（§5.2）：emit → 零转译过线。挂在 run worker 上，
/// `try_send` 非阻塞（INV-S7）。
struct FanoutSink {
    shared: Arc<ServeShared>,
}

impl EventSink for FanoutSink {
    fn emit(&mut self, event: RunEvent) {
        self.shared.fanout_run_event(&event);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// M-04（审查 2026-08-27）：`optional_str` 解析层的直接判别——
    /// 缺省/Null 合法放行，非字符串/空串/超长 fail-closed（幂等键
    /// 不吞畸形值；admission 侧的等价校验是第二道，不是替身）。
    #[test]
    fn optional_str_admits_absent_and_rejects_malformed_values() {
        let params = |value: Option<Value>| {
            let mut map = serde_json::Map::new();
            if let Some(value) = value {
                map.insert("clientMessageId".into(), value);
            }
            map
        };
        // 缺席 / Null：None。
        assert_eq!(
            optional_str(&params(None), "clientMessageId").expect("absent is fine"),
            None
        );
        assert_eq!(
            optional_str(&params(Some(Value::Null)), "clientMessageId").expect("null is fine"),
            None
        );
        // 合法值：Some。
        assert_eq!(
            optional_str(
                &params(Some(Value::String("key-1".into()))),
                "clientMessageId"
            )
            .expect("valid id"),
            Some("key-1".to_owned())
        );
        // 畸形：非字符串 / 空串 / 超过 256 字节。
        assert!(optional_str(&params(Some(json!(7))), "clientMessageId").is_err());
        assert!(optional_str(&params(Some(json!(""))), "clientMessageId").is_err());
        assert!(
            optional_str(&params(Some(json!("x".repeat(257)))), "clientMessageId").is_err(),
            "an over-long id must be rejected at the parse layer"
        );
        // 边界值本身合法。
        assert_eq!(
            optional_str(&params(Some(json!("x".repeat(256)))), "clientMessageId")
                .expect("the bound itself is valid"),
            Some("x".repeat(256))
        );
    }

    #[test]
    fn rpc_result_roundtrips_and_unknown_codes_fold_to_internal() {
        let ok = rpc_result_json(&Ok(json!({"value_a": 1})));
        assert_eq!(ok, r#"{"ok":true,"value":{"value_a":1}}"#);
        let parsed = parse_rpc_result(&ok).expect("parse");
        assert!(parsed.ok);
        assert_eq!(parsed.value, Some(json!({"value_a": 1})));

        for code in [
            "bad-request",
            "unauthorized",
            "forbidden",
            "not-found",
            "busy",
            "not-pending",
            "internal",
        ] {
            let body = rpc_result_json(&Err(RpcError {
                code: ErrorCode::parse(code),
                message: format!("{code} message"),
            }));
            let parsed = parse_rpc_result(&body).expect("parse");
            assert!(!parsed.ok);
            let (parsed_code, raw_code, message) = parsed.error.expect("error triple");
            assert_eq!(parsed_code.as_str(), code);
            assert_eq!(raw_code, code);
            assert_eq!(message, format!("{code} message"));
        }

        // 未知码（未来 amend 新增）→ internal 分类，原值保留（PWA2-04）。
        let future = r#"{"ok":false,"error":{"code":"rate-limited","message":"slow down"}}"#;
        let parsed = parse_rpc_result(future).expect("parse");
        let (code, raw_code, message) = parsed.error.expect("error triple");
        assert_eq!(code, ErrorCode::Internal);
        assert_eq!(raw_code, "rate-limited");
        assert_eq!(message, "slow down");
    }

    #[test]
    fn sse_frames_encode_and_parse_with_multiline_data_and_heartbeats() {
        let frame = encode_sse_frame("event", 7, r#"{"v":1,"event":{"type":"run_started"}}"#);
        assert_eq!(
            frame,
            "event: event\nid: 7\ndata: {\"v\":1,\"event\":{\"type\":\"run_started\"}}\n\n"
        );
        let parsed = parse_sse_frames(&frame);
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].event.as_deref(), Some("event"));
        assert_eq!(parsed[0].id, Some(7));
        assert_eq!(parsed[0].data, r#"{"v":1,"event":{"type":"run_started"}}"#);

        // 多行 data 以 \n 拼接；comment 心跳忽略；无 data 的帧丢弃。
        let stream = ": keep-alive\n\n\
                      event: replay\n\
                      data: line one\n\
                      data: line two\n\
                      id: 2\n\n";
        let parsed = parse_sse_frames(stream);
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].event.as_deref(), Some("replay"));
        assert_eq!(parsed[0].id, Some(2));
        assert_eq!(parsed[0].data, "line one\nline two");

        assert_eq!(encode_heartbeat(), ": keep-alive\n\n");
        assert!(parse_sse_frames(": keep-alive\n\n").is_empty());
    }
}
