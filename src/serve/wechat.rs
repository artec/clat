//! WeChat frontend projection for `clat serve` (MR-3).
//!
//! This module owns no agent semantics or durable session data. It accepts
//! already-authorized iLink deliveries, submits typed messages through the
//! Application facade, injects a fail-closed approver, and projects selected
//! `RunEvent`s into a bounded non-blocking outbox. The iLink poller advances
//! its cursor only after this port returns success, so steering waits for its
//! durable `SteeringApplied` receipt rather than acknowledging a volatile
//! queue entry.

use super::state::ServeShared;
use crate::event::{EventSink, RunEvent};
use crate::im::ilink::{Client, Credentials, InboundMessage};
use crate::message::PendingMessage;
use crate::model::CancelToken;
use crate::permission::{PermissionApprover, PermissionDecision, PermissionRequest};
use crate::{ApplicationRunRequest, PermissionMode, SteerOutcome};
use sha2::{Digest as _, Sha256};
use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

const OUTBOX_CAPACITY: usize = 128;
const OUTBOUND_CHUNK_BYTES: usize = 3_500;
const APPROVAL_TIMEOUT: Duration = Duration::from_secs(10 * 60);
const STEERING_COMMIT_TIMEOUT: Duration = Duration::from_secs(10 * 60);
const WAIT_STEP: Duration = Duration::from_millis(50);
const MAX_INBOUND_IMAGES: usize = 8;
const MAX_INBOUND_ITEMS: usize = 64;
const MAX_INBOUND_TEXT_BYTES: usize = 64 * 1024;
const MAX_REMOTE_ID_BYTES: usize = 1024;
const MAX_CONTEXT_TOKEN_BYTES: usize = 16 * 1024;

#[derive(Clone)]
struct ReplyTarget {
    user_id: String,
    context_token: String,
}

impl ReplyTarget {
    fn from_message(message: &InboundMessage) -> Option<Self> {
        let user_id = message.from_user_id.trim();
        if user_id.is_empty()
            || user_id.len() > MAX_REMOTE_ID_BYTES
            || user_id.chars().any(char::is_control)
            || message.context_token.is_empty()
            || message.context_token.len() > MAX_CONTEXT_TOKEN_BYTES
            || message.context_token.chars().any(char::is_control)
        {
            return None;
        }
        Some(Self {
            user_id: user_id.to_owned(),
            context_token: message.context_token.clone(),
        })
    }
}

enum OutboundItem {
    Text { target: ReplyTarget, text: String },
    Typing { target: ReplyTarget, start: bool },
}

trait OutboundPort: Send + Sync {
    fn text(&self, target: &ReplyTarget, text: String);
    fn typing(&self, target: &ReplyTarget, start: bool);
}

#[derive(Default)]
struct OutboxState {
    items: VecDeque<OutboundItem>,
    dropped: u64,
}

struct BoundedOutbox {
    state: Mutex<OutboxState>,
    ready: Condvar,
}

impl BoundedOutbox {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::new(OutboxState::default()),
            ready: Condvar::new(),
        })
    }

    fn push(&self, item: OutboundItem) {
        let Ok(mut state) = self.state.lock() else {
            return;
        };
        if state.items.len() >= OUTBOX_CAPACITY {
            state.items.pop_front();
            state.dropped = state.dropped.saturating_add(1);
        }
        state.items.push_back(item);
        self.ready.notify_one();
    }

    fn recv(&self, shutdown: &AtomicBool) -> Option<(OutboundItem, u64)> {
        let mut state = self.state.lock().ok()?;
        loop {
            if shutdown.load(Ordering::Acquire) {
                return None;
            }
            if let Some(item) = state.items.pop_front() {
                let dropped = if matches!(item, OutboundItem::Text { .. }) {
                    std::mem::take(&mut state.dropped)
                } else {
                    0
                };
                return Some((item, dropped));
            }
            let waited = self.ready.wait_timeout(state, Duration::from_millis(200));
            let Ok((next, _)) = waited else {
                return None;
            };
            state = next;
        }
    }
}

impl OutboundPort for BoundedOutbox {
    fn text(&self, target: &ReplyTarget, text: String) {
        for chunk in unicode_chunks(&text, OUTBOUND_CHUNK_BYTES) {
            self.push(OutboundItem::Text {
                target: target.clone(),
                text: chunk.to_owned(),
            });
        }
    }

    fn typing(&self, target: &ReplyTarget, start: bool) {
        self.push(OutboundItem::Typing {
            target: target.clone(),
            start,
        });
    }
}

struct PendingApproval {
    user_id: String,
    decision_tx: std::sync::mpsc::Sender<ApprovalAnswer>,
    expires_at: Instant,
}

#[derive(Default)]
struct ApprovalRegistry {
    pending: Mutex<HashMap<String, PendingApproval>>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ApprovalAnswer {
    Allow,
    Always,
    Deny,
}

#[derive(Debug, Eq, PartialEq)]
enum ApprovalWait {
    Answer(ApprovalAnswer),
    Timeout,
    Cancelled,
    Disconnected,
}

impl ApprovalRegistry {
    fn insert(
        &self,
        user_id: String,
        decision_tx: std::sync::mpsc::Sender<ApprovalAnswer>,
    ) -> String {
        let id = uuid::Uuid::new_v4().to_string();
        let mut pending = self.pending.lock().expect("wechat approval lock");
        pending.retain(|_, approval| approval.expires_at > Instant::now());
        pending.insert(
            id.clone(),
            PendingApproval {
                user_id,
                decision_tx,
                expires_at: Instant::now() + APPROVAL_TIMEOUT,
            },
        );
        id
    }

    fn remove(&self, id: &str) {
        self.pending
            .lock()
            .expect("wechat approval lock")
            .remove(id);
    }

    fn take(&self, user_id: &str, id: &str) -> Result<PendingApproval, &'static str> {
        let mut pending = self.pending.lock().expect("wechat approval lock");
        let Some(approval) = pending.get(id) else {
            return Err("审批请求不存在或已过期。");
        };
        if approval.user_id != user_id {
            return Err("审批请求不存在或已过期。");
        }
        if approval.expires_at <= Instant::now() {
            pending.remove(id);
            return Err("审批请求不存在或已过期。");
        }
        Ok(pending.remove(id).expect("checked pending approval"))
    }
}

struct WechatApprover {
    shared: Arc<ServeShared>,
    target: ReplyTarget,
    approvals: Arc<ApprovalRegistry>,
    outbound: Arc<dyn OutboundPort>,
}

impl PermissionApprover for WechatApprover {
    fn decide(&self, request: PermissionRequest, cancel: &CancelToken) -> PermissionDecision {
        let (decision_tx, decision_rx) = std::sync::mpsc::channel();
        let request_id = self
            .approvals
            .insert(self.target.user_id.clone(), decision_tx);
        let tool = bounded_summary(&crate::redact::redact_secrets(&request.tool), 120);
        let reason = bounded_summary(&crate::redact::redact_secrets(&request.reason), 240);
        self.outbound.text(
            &self.target,
            format!(
                "需要审批\nID: {request_id}\n工具: {}\n影响: {:?}\n原因: {reason}\n回复 `/allow {request_id}`、`/deny {request_id}`；仅在明确接受后续同类操作时回复 `/always {request_id}`。",
                tool, request.effect
            ),
        );
        let deadline = Instant::now() + APPROVAL_TIMEOUT;
        let answer = wait_for_approval(&decision_rx, cancel, deadline);
        self.approvals.remove(&request_id);
        match answer {
            ApprovalWait::Answer(ApprovalAnswer::Allow) => {
                self.outbound.text(&self.target, "审批答复已提交。".into());
                PermissionDecision::Allow
            }
            ApprovalWait::Answer(ApprovalAnswer::Always) => {
                let persisted = self
                    .shared
                    .app
                    .lock()
                    .expect("application lock")
                    .set_permission_mode(PermissionMode::FullAccess);
                match persisted {
                    Ok(()) => {
                        self.outbound.text(
                            &self.target,
                            "已允许本次调用，并将当前会话切换为 Full Access。".into(),
                        );
                        PermissionDecision::Allow
                    }
                    Err(error) => {
                        let message = if error.to_string().contains("outcome unknown") {
                            "Full Access 持久化结果不确定；本次调用已默认拒绝，当前会话已停止写入。请重启并在电脑端检查权限档位。"
                        } else {
                            "Full Access 未能持久化；本次调用已默认拒绝，请在电脑端检查后重试。"
                        };
                        self.outbound.text(&self.target, message.into());
                        PermissionDecision::Deny {
                            reason: "Full Access escalation did not commit".into(),
                        }
                    }
                }
            }
            ApprovalWait::Answer(ApprovalAnswer::Deny) => PermissionDecision::Deny {
                reason: "denied from the paired WeChat frontend".into(),
            },
            ApprovalWait::Timeout => {
                self.outbound.text(
                    &self.target,
                    format!("审批 {request_id} 已超时，已默认拒绝。"),
                );
                PermissionDecision::Deny {
                    reason: "approval timeout".into(),
                }
            }
            ApprovalWait::Cancelled => PermissionDecision::Deny {
                reason: "run cancelled".into(),
            },
            ApprovalWait::Disconnected => PermissionDecision::Deny {
                reason: "no permission decision available".into(),
            },
        }
    }
}

fn wait_for_approval(
    receiver: &Receiver<ApprovalAnswer>,
    cancel: &CancelToken,
    deadline: Instant,
) -> ApprovalWait {
    loop {
        match receiver.recv_timeout(WAIT_STEP) {
            Ok(answer) => return ApprovalWait::Answer(answer),
            Err(RecvTimeoutError::Timeout) => {
                if cancel.is_cancelled() {
                    return ApprovalWait::Cancelled;
                }
                if Instant::now() >= deadline {
                    return ApprovalWait::Timeout;
                }
            }
            Err(RecvTimeoutError::Disconnected) => return ApprovalWait::Disconnected,
        }
    }
}

struct WechatRunSink {
    shared: Arc<ServeShared>,
    outbound: Arc<dyn OutboundPort>,
    target: ReplyTarget,
}

enum PromptStartOutcome {
    Accepted,
    Duplicate,
    Revoked,
    MappingPending { error: String, claim_active: bool },
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum ChatMappingIntent {
    None,
    Fresh,
    Recovery,
}

impl ChatMappingIntent {
    fn is_some(self) -> bool {
        self != Self::None
    }
}

enum PrepareMessageError {
    Retry(String),
    Reject(String),
}

impl EventSink for WechatRunSink {
    fn emit(&mut self, event: RunEvent) {
        self.shared.fanout_run_event(&event);
        match &event {
            RunEvent::ModelRequested { .. } => self.outbound.typing(&self.target, true),
            RunEvent::ToolStarted { tool, .. } => self.outbound.text(
                &self.target,
                format!("正在运行工具：{}", crate::redact::redact_secrets(tool)),
            ),
            RunEvent::RunCompleted { output, .. } => {
                self.outbound.typing(&self.target, false);
                self.outbound
                    .text(&self.target, crate::redact::redact_secrets(output));
            }
            RunEvent::RunCancelled { .. } => {
                self.outbound.typing(&self.target, false);
                self.outbound.text(&self.target, "运行已停止。".into());
            }
            RunEvent::RunFailed { .. } => {
                self.outbound.typing(&self.target, false);
                self.outbound
                    .text(&self.target, "运行失败，请在电脑端查看详情。".into());
            }
            _ => {}
        }
    }
}

pub(crate) struct WechatBridge {
    shared: Arc<ServeShared>,
    client: Client,
    shutdown: Arc<AtomicBool>,
    outbound: Arc<dyn OutboundPort>,
    approvals: Arc<ApprovalRegistry>,
    armed_new_chats: Mutex<HashMap<String, String>>,
    volatile_chat_sessions: Mutex<HashMap<String, crate::im::WechatChatBinding>>,
}

impl WechatBridge {
    pub(crate) fn spawn(
        shared: Arc<ServeShared>,
        credentials: Credentials,
        shutdown: Arc<AtomicBool>,
    ) -> Result<(Arc<Self>, JoinHandle<()>), String> {
        let outbox = BoundedOutbox::new();
        let client = Client::new();
        let worker = spawn_outbox_worker(
            Arc::clone(&shared),
            client.clone(),
            credentials.clone(),
            Arc::clone(&shutdown),
            Arc::clone(&outbox),
        )?;
        let outbound: Arc<dyn OutboundPort> = outbox;
        Ok((
            Arc::new(Self {
                shared,
                client,
                shutdown,
                outbound,
                approvals: Arc::new(ApprovalRegistry::default()),
                armed_new_chats: Mutex::new(HashMap::new()),
                volatile_chat_sessions: Mutex::new(HashMap::new()),
            }),
            worker,
        ))
    }

    fn handle_authorized(&self, message: &InboundMessage) -> Result<(), String> {
        let Some(target) = ReplyTarget::from_message(message) else {
            return Ok(());
        };
        if !self
            .shared
            .app
            .lock()
            .expect("application lock")
            .is_wechat_user_authorized(&target.user_id)
        {
            return Ok(());
        }
        let chat_id = target.user_id.clone(); // iLink v1 private-chat identity.
        let delivery_id = crate::im::delivery_id(message)?;
        if self
            .shared
            .app
            .lock()
            .expect("application lock")
            .is_wechat_delivery_handled(&delivery_id)
        {
            return Ok(());
        }
        if let Some(text) = standalone_text(message) {
            let is_command = approval_command(text).is_some()
                || matches!(text, "/help" | "/status" | "/new" | "/stop")
                || text.starts_with('/');
            if is_command {
                if let Some((answer, request_id)) = approval_command(text) {
                    self.handle_approval(&target, request_id, answer)?;
                } else {
                    match text {
                        "/help" => self.outbound.text(
                            &target,
                            "命令：/status · /new · /stop · /help。审批仅接受带原 requestId 的 /allow、/deny、/always。".into(),
                        ),
                        "/status" => self.handle_status(&target, &chat_id)?,
                        "/new" => self.handle_new(&target, &chat_id)?,
                        "/stop" => self.handle_stop(&target, &chat_id)?,
                        _ => self
                            .outbound
                            .text(&target, "未知命令。发送 /help 查看可用命令。".into()),
                    }
                }
                self.shared
                    .app
                    .lock()
                    .expect("application lock")
                    .mark_wechat_delivery_handled(&delivery_id)
                    .map_err(|error| error.to_string())?;
                return Ok(());
            }
        }
        self.handle_prompt(message, target, chat_id, delivery_id.clone())?;
        self.shared
            .app
            .lock()
            .expect("application lock")
            .mark_wechat_delivery_handled(&delivery_id)
            .map_err(|error| error.to_string())
    }

    fn handle_approval(
        &self,
        target: &ReplyTarget,
        request_id: &str,
        answer: ApprovalAnswer,
    ) -> Result<(), String> {
        if !self
            .shared
            .app
            .lock()
            .expect("application lock")
            .is_wechat_user_authorized(&target.user_id)
        {
            return Ok(());
        }
        match self.approvals.take(&target.user_id, request_id) {
            Ok(approval) => {
                if approval.decision_tx.send(answer).is_err() {
                    self.outbound
                        .text(target, "审批请求不存在或已过期。".into());
                }
                Ok(())
            }
            Err(message) => {
                self.outbound.text(target, message.into());
                Ok(())
            }
        }
    }

    fn handle_status(&self, target: &ReplyTarget, chat_id: &str) -> Result<(), String> {
        let active = !self.shared.active_run_info().is_null();
        let mapped = self.chat_binding(chat_id, &target.user_id)?;
        let (current, title) = {
            let app = self.shared.app.lock().expect("application lock");
            if !app.is_wechat_user_authorized(&target.user_id) {
                return Ok(());
            }
            let current = app.current_session_id();
            let title = mapped
                .as_ref()
                .filter(|binding| current.as_ref() == Some(&binding.session_id))
                .and_then(|_| app.session_title());
            (current, title)
        };
        let state = match mapped {
            Some(binding) if current.as_ref() == Some(&binding.session_id) => {
                format!(
                    "已映射当前会话{}",
                    title
                        .map(|value| format!("：{}", crate::redact::redact_secrets(&value)))
                        .unwrap_or_default()
                )
            }
            Some(_) => "已映射；下次消息会恢复该会话".into(),
            None => "尚未映射；先发送 /new".into(),
        };
        self.outbound.text(
            target,
            format!(
                "状态：{state}；运行：{}。",
                if active { "进行中" } else { "空闲" }
            ),
        );
        Ok(())
    }

    fn handle_new(&self, target: &ReplyTarget, chat_id: &str) -> Result<(), String> {
        if !self.shared.active_run_info().is_null() {
            self.outbound
                .text(target, "当前有运行进行中，请先 /stop 或等待完成。".into());
            return Ok(());
        }
        {
            let mut app = self.shared.app.lock().expect("application lock");
            if !app.is_wechat_user_authorized(&target.user_id) {
                return Ok(());
            }
            app.new_session().map_err(|error| error.to_string())?;
            app.clear_wechat_chat_binding(&target.user_id, chat_id)
                .map_err(|error| error.to_string())?;
        }
        let mut armed = self
            .armed_new_chats
            .lock()
            .map_err(|_| "wechat new-chat state poisoned")?;
        if armed.len() >= 128 && !armed.contains_key(chat_id) {
            let oldest = armed.keys().next().cloned();
            if let Some(oldest) = oldest {
                armed.remove(&oldest);
            }
        }
        armed.insert(chat_id.to_owned(), target.user_id.clone());
        self.volatile_chat_sessions
            .lock()
            .map_err(|_| "wechat volatile mapping state poisoned")?
            .remove(chat_id);
        self.shared.advance_selection_generation();
        self.outbound.text(
            target,
            "已切换到新会话；发送下一条消息后才会物化并建立持久映射。".into(),
        );
        Ok(())
    }

    fn handle_stop(&self, target: &ReplyTarget, chat_id: &str) -> Result<(), String> {
        let mapping = self.chat_binding(chat_id, &target.user_id)?;
        let owns_active = {
            let app = self.shared.app.lock().expect("application lock");
            if !app.is_wechat_user_authorized(&target.user_id) {
                return Ok(());
            }
            mapping.is_some_and(|binding| {
                binding.user_id == target.user_id
                    && app.current_session_id().as_ref() == Some(&binding.session_id)
            })
        };
        if owns_active && !self.shared.active_run_info().is_null() {
            self.shared.cancel_active_run();
            self.outbound.text(target, "已请求停止当前运行。".into());
        } else {
            self.outbound
                .text(target, "当前没有属于此聊天的活动运行。".into());
        }
        Ok(())
    }

    fn chat_binding(
        &self,
        chat_id: &str,
        user_id: &str,
    ) -> Result<Option<crate::im::WechatChatBinding>, String> {
        let durable = self
            .shared
            .app
            .lock()
            .expect("application lock")
            .wechat_chat_binding(chat_id)
            .filter(|binding| binding.user_id == user_id);
        if durable.is_some() {
            return Ok(durable);
        }
        Ok(self
            .volatile_chat_sessions
            .lock()
            .map_err(|_| "wechat volatile mapping state poisoned")?
            .get(chat_id)
            .filter(|binding| binding.user_id == user_id)
            .cloned())
    }

    fn handle_prompt(
        &self,
        message: &InboundMessage,
        target: ReplyTarget,
        chat_id: String,
        delivery_id: String,
    ) -> Result<(), String> {
        let mapped = self.chat_binding(&chat_id, &target.user_id)?;
        let armed = self
            .armed_new_chats
            .lock()
            .map_err(|_| "wechat new-chat state poisoned")?
            .get(&chat_id)
            .is_some_and(|user| user == &target.user_id);
        let pending_mapping = self
            .shared
            .app
            .lock()
            .expect("application lock")
            .pending_wechat_chat_mapping(&delivery_id, &target.user_id, &chat_id);
        if mapped.is_none() && !armed && pending_mapping.is_none() {
            self.outbound.text(
                &target,
                "此聊天尚未映射会话。请先发送 /new，再发送任务。".into(),
            );
            return Ok(());
        }

        let (text, opaque_images, staged_images) = match self.prepare_message(message, &delivery_id)
        {
            Ok(prepared) => prepared,
            Err(PrepareMessageError::Retry(error)) => return Err(error),
            Err(PrepareMessageError::Reject(error)) => {
                self.outbound.text(
                    &target,
                    format!("消息被拒绝：{}", bounded_summary(&error, 300)),
                );
                return Ok(());
            }
        };
        if text.trim().is_empty() && staged_images.is_empty() {
            self.outbound
                .text(&target, "当前消息没有可处理的文本或受支持图片。".into());
            return Ok(());
        }
        let mut pending =
            PendingMessage::from_front_end(text, Some(delivery_id.clone()), opaque_images);
        pending.resolve_staged_attachments(staged_images.clone());
        let incoming_digest = pending.request_digest();
        if pending_mapping
            .as_ref()
            .is_some_and(|mapping| mapping.request_digest != incoming_digest)
        {
            release_staged(&self.shared, &staged_images);
            self.outbound
                .text(&target, "同一投递标识已用于不同消息，已拒绝。".into());
            return Ok(());
        }
        let recovering_mapping_intent = pending_mapping.is_some();
        let recovering_unmapped_delivery = recovering_mapping_intent && mapped.is_none();

        if !self.shared.active_run_info().is_null() {
            if recovering_unmapped_delivery {
                release_staged(&self.shared, &staged_images);
                return Err(
                    "durable chat-mapping recovery waits for the active run to finish".into(),
                );
            }
            return self.steer_or_busy(&target, mapped, pending, staged_images, &delivery_id);
        }

        let rpc_id = format!("wechat-{}", uuid::Uuid::new_v4());
        if !self.shared.try_claim_run(&rpc_id, super::state::now_ms()) {
            release_staged(&self.shared, &staged_images);
            if recovering_unmapped_delivery {
                return Err(
                    "durable chat-mapping recovery waits for the active run to finish".into(),
                );
            }
            self.outbound
                .text(&target, "当前有运行进行中，请稍后重试。".into());
            return Ok(());
        }
        let mapping_intent = if recovering_mapping_intent {
            ChatMappingIntent::Recovery
        } else if mapped.is_none() {
            ChatMappingIntent::Fresh
        } else {
            ChatMappingIntent::None
        };
        if mapping_intent == ChatMappingIntent::Fresh {
            let arm_result = self
                .shared
                .app
                .lock()
                .expect("application lock")
                .arm_wechat_chat_mapping(&delivery_id, &target.user_id, &chat_id, &incoming_digest);
            if let Err(error) = arm_result {
                self.shared.release_run_claim();
                release_staged(&self.shared, &staged_images);
                return Err(format!("could not persist chat-mapping intent: {error}"));
            }
        }
        let result = self.start_prompt(
            &target,
            &chat_id,
            mapped,
            mapping_intent,
            pending,
            &delivery_id,
        );
        release_staged(&self.shared, &staged_images);
        match result {
            Ok(PromptStartOutcome::Accepted | PromptStartOutcome::Duplicate) => Ok(()),
            Ok(PromptStartOutcome::Revoked) => {
                self.shared.release_run_claim();
                self.shared
                    .app
                    .lock()
                    .expect("application lock")
                    .abort_wechat_chat_mapping(&delivery_id, &target.user_id, &chat_id)
                    .map_err(|error| error.to_string())?;
                Ok(())
            }
            Ok(PromptStartOutcome::MappingPending {
                error,
                claim_active,
            }) => {
                if !claim_active {
                    self.shared.release_run_claim();
                }
                self.outbound.text(
                    &target,
                    "消息已提交，但会话映射尚未落盘；正在保留投递等待重试。".into(),
                );
                Err(error)
            }
            Err(error) => {
                self.shared.release_run_claim();
                let abort_error = {
                    let app = self.shared.app.lock().expect("application lock");
                    if !recovering_mapping_intent && app.committed_admission(&delivery_id).is_none()
                    {
                        app.abort_wechat_chat_mapping(&delivery_id, &target.user_id, &chat_id)
                            .err()
                            .map(|error| error.to_string())
                    } else {
                        None
                    }
                };
                self.outbound.text(
                    &target,
                    format!("消息未启动：{}", bounded_summary(&error, 300)),
                );
                if let Some(abort_error) = abort_error {
                    return Err(format!(
                        "{error}; could not clear uncommitted chat-mapping intent: {abort_error}"
                    ));
                }
                if recovering_mapping_intent {
                    Err(error)
                } else {
                    Ok(())
                }
            }
        }
    }

    fn start_prompt(
        &self,
        target: &ReplyTarget,
        chat_id: &str,
        mapped: Option<crate::im::WechatChatBinding>,
        mapping_intent: ChatMappingIntent,
        message: PendingMessage,
        delivery_id: &str,
    ) -> Result<PromptStartOutcome, String> {
        let incoming_digest = message.request_digest();
        let (completion_tx, completion_rx) = std::sync::mpsc::channel();
        let approver: Arc<dyn PermissionApprover> = Arc::new(WechatApprover {
            shared: Arc::clone(&self.shared),
            target: target.clone(),
            approvals: Arc::clone(&self.approvals),
            outbound: Arc::clone(&self.outbound),
        });
        let started = {
            let mut app = self.shared.app.lock().expect("application lock");
            if !app.is_wechat_user_authorized(&target.user_id) {
                return Ok(PromptStartOutcome::Revoked);
            }
            if let Some(binding) = mapped {
                if app.current_session_id().as_ref() != Some(&binding.session_id) {
                    app.switch_session(binding.session_id.clone())
                        .map_err(|error| error.to_string())?;
                    self.shared.advance_selection_generation();
                }
                if app.wechat_chat_binding(chat_id).is_none()
                    && let Err(error) = if mapping_intent.is_some() {
                        app.complete_wechat_chat_mapping(
                            delivery_id,
                            &target.user_id,
                            chat_id,
                            &binding.session_id,
                        )
                    } else {
                        app.bind_wechat_chat(&target.user_id, chat_id, &binding.session_id)
                    }
                {
                    return Ok(PromptStartOutcome::MappingPending {
                        error: error.to_string(),
                        claim_active: false,
                    });
                }
                self.volatile_chat_sessions
                    .lock()
                    .map_err(|_| "wechat volatile mapping state poisoned")?
                    .remove(chat_id);
            } else if mapping_intent.is_some() {
                if mapping_intent == ChatMappingIntent::Recovery
                    && let Some((session_id, record)) = app
                        .find_committed_admission_session(delivery_id)
                        .map_err(|error| error.to_string())?
                {
                    if record
                        .request_digest
                        .as_deref()
                        .is_some_and(|digest| digest != incoming_digest)
                    {
                        return Err("同一投递标识已用于不同消息，已拒绝。".into());
                    }
                    if app.current_session_id().as_ref() != Some(&session_id) {
                        app.switch_session(session_id.clone())
                            .map_err(|error| error.to_string())?;
                        self.shared.advance_selection_generation();
                    }
                    app.complete_wechat_chat_mapping(
                        delivery_id,
                        &target.user_id,
                        chat_id,
                        &session_id,
                    )
                    .map_err(|error| error.to_string())?;
                    drop(app);
                    self.shared.release_run_claim();
                    self.outbound
                        .text(target, "该消息已经处理，无需重复发送。".into());
                    return Ok(PromptStartOutcome::Duplicate);
                }
                if let Some(record) = app.committed_admission(delivery_id) {
                    if record
                        .request_digest
                        .as_deref()
                        .is_some_and(|digest| digest != incoming_digest)
                    {
                        return Err("同一投递标识已用于不同消息，已拒绝。".into());
                    }
                    let session_id = app.current_session_id().ok_or_else(|| {
                        "committed delivery has no restorable active session".to_owned()
                    })?;
                    app.complete_wechat_chat_mapping(
                        delivery_id,
                        &target.user_id,
                        chat_id,
                        &session_id,
                    )
                    .map_err(|error| error.to_string())?;
                    drop(app);
                    self.shared.release_run_claim();
                    self.outbound
                        .text(target, "该消息已经处理，无需重复发送。".into());
                    return Ok(PromptStartOutcome::Duplicate);
                }
                app.new_session().map_err(|error| error.to_string())?;
                self.shared.advance_selection_generation();
            }
            if let Some(record) = app.committed_admission(delivery_id) {
                if record
                    .request_digest
                    .as_deref()
                    .is_none_or(|digest| digest == incoming_digest)
                {
                    drop(app);
                    self.shared.release_run_claim();
                    self.outbound
                        .text(target, "该消息已经处理，无需重复发送。".into());
                    return Ok(PromptStartOutcome::Duplicate);
                }
                return Err("同一投递标识已用于不同消息，已拒绝。".into());
            }
            let handle = app
                .start_run(ApplicationRunRequest {
                    message,
                    approver,
                    asker: None,
                    events: Box::new(WechatRunSink {
                        shared: Arc::clone(&self.shared),
                        outbound: Arc::clone(&self.outbound),
                        target: target.clone(),
                    }),
                    completion: completion_tx,
                })
                .map_err(|error| error.to_string())?;
            let session_id = app
                .current_session_id()
                .ok_or_else(|| "run committed without an active session".to_owned())?;
            let volatile = crate::im::WechatChatBinding {
                user_id: target.user_id.clone(),
                session_id: session_id.clone(),
            };
            let mapping = if mapping_intent.is_some() {
                app.complete_wechat_chat_mapping(delivery_id, &target.user_id, chat_id, &session_id)
            } else {
                app.bind_wechat_chat(&target.user_id, chat_id, &session_id)
            }
            .map_err(|error| error.to_string());
            (handle, mapping, volatile)
        };
        self.outbound.typing(target, true);
        self.shared
            .spawn_settler(format!("wechat:{delivery_id}"), completion_rx, started.0);
        if let Err(error) = started.1 {
            self.volatile_chat_sessions
                .lock()
                .map_err(|_| "wechat volatile mapping state poisoned")?
                .insert(chat_id.to_owned(), started.2);
            // The user message is already committed. Retain the iLink cursor
            // and let replay converge through the committed admission while
            // the process-local active session is still recoverable.
            return Ok(PromptStartOutcome::MappingPending {
                error: format!("could not persist chat/session mapping: {error}"),
                claim_active: true,
            });
        }
        self.armed_new_chats
            .lock()
            .map_err(|_| "wechat new-chat state poisoned")?
            .remove(chat_id);
        self.volatile_chat_sessions
            .lock()
            .map_err(|_| "wechat volatile mapping state poisoned")?
            .remove(chat_id);
        Ok(PromptStartOutcome::Accepted)
    }

    fn steer_or_busy(
        &self,
        target: &ReplyTarget,
        mapped: Option<crate::im::WechatChatBinding>,
        message: PendingMessage,
        staged_images: Vec<std::path::PathBuf>,
        delivery_id: &str,
    ) -> Result<(), String> {
        let digest = message.request_digest();
        match self.shared.pending_steering_retry(delivery_id, &digest) {
            Ok(Some(_)) => {
                release_staged(&self.shared, &staged_images);
                self.outbound
                    .text(target, "该消息已在当前运行中排队。".into());
                return self.wait_for_steering_commit(delivery_id);
            }
            Ok(None) => {}
            Err(_) => {
                release_staged(&self.shared, &staged_images);
                self.outbound
                    .text(target, "同一投递标识已用于不同消息，已拒绝。".into());
                return Ok(());
            }
        }
        let outcome = {
            let app = self.shared.app.lock().expect("application lock");
            if !app.is_wechat_user_authorized(&target.user_id) {
                release_staged(&self.shared, &staged_images);
                return Ok(());
            }
            let owns_current = mapped.as_ref().is_some_and(|binding| {
                app.current_session_id().as_ref() == Some(&binding.session_id)
                    && binding.user_id == target.user_id
            });
            if !owns_current {
                release_staged(&self.shared, &staged_images);
                self.outbound
                    .text(target, "另一会话正在运行；请等待完成后再发送。".into());
                return Ok(());
            }
            if let Some(binding) = mapped.as_ref()
                && app.wechat_chat_binding(&target.user_id).is_none()
            {
                let pending_mapping =
                    app.pending_wechat_chat_mapping(delivery_id, &target.user_id, &target.user_id);
                let repair = if pending_mapping.is_some() {
                    app.complete_wechat_chat_mapping(
                        delivery_id,
                        &target.user_id,
                        &target.user_id,
                        &binding.session_id,
                    )
                } else {
                    app.bind_wechat_chat(&target.user_id, &target.user_id, &binding.session_id)
                };
                if let Err(error) = repair {
                    release_staged(&self.shared, &staged_images);
                    return Err(format!(
                        "could not persist chat/session mapping before replay: {error}"
                    ));
                }
                self.volatile_chat_sessions
                    .lock()
                    .map_err(|_| "wechat volatile mapping state poisoned")?
                    .remove(&target.user_id);
            }
            if let Some(record) = app.committed_admission(delivery_id) {
                release_staged(&self.shared, &staged_images);
                if record
                    .request_digest
                    .as_deref()
                    .is_none_or(|recorded| recorded == digest)
                {
                    self.outbound
                        .text(target, "该消息已经处理，无需重复发送。".into());
                } else {
                    self.outbound
                        .text(target, "同一投递标识已用于不同消息，已拒绝。".into());
                }
                return Ok(());
            }
            app.steer(message)
        };
        release_staged(&self.shared, &staged_images);
        match outcome {
            SteerOutcome::Queued {
                receipt: Some(receipt),
            } => {
                self.shared.remember_pending_steering(
                    delivery_id.to_owned(),
                    digest,
                    (*receipt).clone(),
                    None,
                    Vec::new(),
                );
                self.outbound
                    .text(target, "消息已加入当前运行，等待下一轮处理。".into());
                self.wait_for_steering_commit(delivery_id)
            }
            SteerOutcome::Queued { receipt: None } => {
                Err("steering accepted without an idempotency receipt".into())
            }
            SteerOutcome::NotRunning { .. } => {
                Err("active run sealed before the message was queued".into())
            }
            SteerOutcome::Refused { reason, .. } => {
                self.outbound.text(
                    target,
                    format!("消息未加入运行：{}", bounded_summary(&reason, 300)),
                );
                Ok(())
            }
        }
    }

    fn wait_for_steering_commit(&self, delivery_id: &str) -> Result<(), String> {
        let deadline = Instant::now() + STEERING_COMMIT_TIMEOUT;
        loop {
            if self.shutdown.load(Ordering::Acquire) {
                return Err("shutdown before steering commit".into());
            }
            if self
                .shared
                .app
                .lock()
                .expect("application lock")
                .committed_admission(delivery_id)
                .is_some()
            {
                return Ok(());
            }
            if self.shared.active_run_info().is_null() || Instant::now() >= deadline {
                return Err("steering ended before durable commit".into());
            }
            std::thread::sleep(WAIT_STEP);
        }
    }

    fn prepare_message(
        &self,
        message: &InboundMessage,
        delivery_id: &str,
    ) -> Result<(String, Vec<std::path::PathBuf>, Vec<std::path::PathBuf>), PrepareMessageError>
    {
        if message.item_list.len() > MAX_INBOUND_ITEMS {
            return Err(PrepareMessageError::Reject(format!(
                "WeChat message contains more than {MAX_INBOUND_ITEMS} items"
            )));
        }
        let mut text = Vec::new();
        let images = message
            .item_list
            .iter()
            .filter_map(|item| item.image_item.as_ref())
            .collect::<Vec<_>>();
        if images.len() > MAX_INBOUND_IMAGES {
            return Err(PrepareMessageError::Reject(format!(
                "WeChat message contains more than {MAX_INBOUND_IMAGES} images"
            )));
        }
        let mut text_bytes = 0usize;
        for item in &message.item_list {
            if let Some(item) = &item.text_item
                && !item.text.trim().is_empty()
            {
                text_bytes = text_bytes
                    .checked_add(item.text.len())
                    .and_then(|total| total.checked_add(usize::from(!text.is_empty())))
                    .ok_or_else(|| {
                        PrepareMessageError::Reject("WeChat text length overflow".into())
                    })?;
                if text_bytes > MAX_INBOUND_TEXT_BYTES {
                    return Err(PrepareMessageError::Reject(format!(
                        "WeChat text exceeds the {MAX_INBOUND_TEXT_BYTES}-byte input limit"
                    )));
                }
                text.push(item.text.clone());
            }
        }
        let mut opaque = Vec::with_capacity(images.len());
        let mut staged = Vec::with_capacity(images.len());
        for (index, image) in images.into_iter().enumerate() {
            let downloaded = match self.client.download_image(image) {
                Ok(downloaded) => downloaded,
                Err(error) => {
                    release_staged(&self.shared, &staged);
                    return Err(classify_media_error(error));
                }
            };
            let digest = Sha256::digest(&downloaded.bytes);
            opaque.push(std::path::PathBuf::from(format!(
                "wechat-{delivery_id}-{index}-{digest:x}.{}",
                downloaded.extension
            )));
            match self
                .shared
                .drafts
                .stage_remote_image(&downloaded.bytes, downloaded.extension)
            {
                Ok(path) => staged.push(path),
                Err(error) => {
                    release_staged(&self.shared, &staged);
                    return Err(PrepareMessageError::Reject(error));
                }
            }
        }
        Ok((text.join("\n"), opaque, staged))
    }
}

impl crate::im::AuthorizedMessageHandler for WechatBridge {
    fn handle(&self, message: &InboundMessage) -> Result<(), String> {
        self.handle_authorized(message)
    }
}

fn spawn_outbox_worker(
    shared: Arc<ServeShared>,
    client: Client,
    credentials: Credentials,
    shutdown: Arc<AtomicBool>,
    outbox: Arc<BoundedOutbox>,
) -> Result<JoinHandle<()>, String> {
    std::thread::Builder::new()
        .name("clat-wechat-outbox".into())
        .spawn(move || {
            let mut typing_tickets = HashMap::<String, String>::new();
            while let Some((item, dropped)) = outbox.recv(&shutdown) {
                let _outbound = shared.wechat_outbound.lock().expect("wechat outbound gate");
                let credential_is_current = shared
                    .app
                    .lock()
                    .expect("application lock")
                    .wechat_credentials()
                    .is_ok_and(|current| current.as_ref() == Some(&credentials));
                if !credential_is_current {
                    break;
                }
                let result: Result<(), crate::im::ilink::Error> = match item {
                    OutboundItem::Text { target, text } => {
                        let text = if dropped == 0 {
                            text
                        } else {
                            format!("[较早状态已丢弃 {dropped} 条]\n{text}")
                        };
                        send_chunks(&client, &credentials, &target, &text)
                    }
                    OutboundItem::Typing { target, start } => {
                        let ticket = if let Some(ticket) = typing_tickets.get(&target.user_id) {
                            Some(ticket.clone())
                        } else {
                            match client.typing_ticket(
                                &credentials,
                                &target.user_id,
                                &target.context_token,
                            ) {
                                Ok(Some(ticket)) => {
                                    if typing_tickets.len() >= 64 {
                                        typing_tickets.clear();
                                    }
                                    typing_tickets.insert(target.user_id.clone(), ticket.clone());
                                    Some(ticket)
                                }
                                Ok(None) => None,
                                Err(error) => {
                                    handle_outbound_error(&shared, &credentials, &error);
                                    None
                                }
                            }
                        };
                        ticket.map_or(Ok(()), |ticket| {
                            client
                                .send_typing(&credentials, &target.user_id, &ticket, start)
                                .map(|_| ())
                        })
                    }
                };
                if let Err(error) = result {
                    handle_outbound_error(&shared, &credentials, &error);
                    eprintln!(
                        "clat: WeChat outbound projection failed: {}",
                        crate::redact::redact_secrets(&error.to_string())
                    );
                }
            }
        })
        .map_err(|error| format!("could not start WeChat outbox: {error}"))
}

fn handle_outbound_error(
    shared: &ServeShared,
    expected: &Credentials,
    error: &crate::im::ilink::Error,
) {
    if matches!(error, crate::im::ilink::Error::InvalidCredential) {
        let app = shared.app.lock().expect("application lock");
        if app
            .wechat_credentials()
            .is_ok_and(|current| current.as_ref() == Some(expected))
        {
            app.cancel_active_run();
            let _ = app.clear_wechat_binding();
        }
    }
}

fn send_chunks(
    client: &Client,
    credentials: &Credentials,
    target: &ReplyTarget,
    text: &str,
) -> Result<(), crate::im::ilink::Error> {
    for (index, chunk) in unicode_chunks(text, OUTBOUND_CHUNK_BYTES)
        .into_iter()
        .enumerate()
    {
        client.send_text(
            credentials,
            &target.user_id,
            &target.context_token,
            &format!("clat-{}-{index}", uuid::Uuid::new_v4()),
            chunk,
        )?;
    }
    Ok(())
}

fn unicode_chunks(text: &str, max_bytes: usize) -> Vec<&str> {
    if text.is_empty() || max_bytes == 0 {
        return Vec::new();
    }
    let mut chunks = Vec::new();
    let mut start = 0;
    while start < text.len() {
        let mut end = (start + max_bytes).min(text.len());
        while end > start && !text.is_char_boundary(end) {
            end -= 1;
        }
        if end == start {
            end = text[start..]
                .char_indices()
                .nth(1)
                .map_or(text.len(), |(offset, _)| start + offset);
        }
        chunks.push(&text[start..end]);
        start = end;
    }
    chunks
}

fn standalone_text(message: &InboundMessage) -> Option<&str> {
    if message.item_list.len() != 1 {
        return None;
    }
    let item = message.item_list.first()?;
    if item.kind != 1 || item.image_item.is_some() {
        return None;
    }
    Some(item.text_item.as_ref()?.text.trim())
}

fn approval_command(text: &str) -> Option<(ApprovalAnswer, &str)> {
    let mut fields = text.split_whitespace();
    let answer = match fields.next()? {
        "/allow" => ApprovalAnswer::Allow,
        "/always" => ApprovalAnswer::Always,
        "/deny" => ApprovalAnswer::Deny,
        _ => return None,
    };
    let id = fields.next()?;
    if fields.next().is_some()
        || id.len() > 128
        || !id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
    {
        return None;
    }
    Some((answer, id))
}

fn bounded_summary(text: &str, max_chars: usize) -> String {
    let mut output = text.chars().take(max_chars).collect::<String>();
    if text.chars().count() > max_chars {
        output.push('…');
    }
    output
}

fn classify_media_error(error: crate::im::ilink::Error) -> PrepareMessageError {
    match error {
        crate::im::ilink::Error::Transport(_) => {
            PrepareMessageError::Retry("WeChat image download transport failure".into())
        }
        crate::im::ilink::Error::InvalidCredential => {
            PrepareMessageError::Retry("WeChat binding credential became invalid".into())
        }
        crate::im::ilink::Error::PollBackoff => {
            PrepareMessageError::Retry("WeChat image endpoint requested backoff".into())
        }
        crate::im::ilink::Error::Http(status)
            if status == 408 || status == 429 || status >= 500 =>
        {
            PrepareMessageError::Retry(format!(
                "WeChat image endpoint returned retryable HTTP {status}"
            ))
        }
        crate::im::ilink::Error::Http(status) => {
            PrepareMessageError::Reject(format!("WeChat image endpoint returned HTTP {status}"))
        }
        crate::im::ilink::Error::ResponseTooLarge => {
            PrepareMessageError::Reject("WeChat image exceeds the transport byte limit".into())
        }
        crate::im::ilink::Error::InvalidOrigin(_) => PrepareMessageError::Reject(
            "WeChat image URL is outside the official media fence".into(),
        ),
        crate::im::ilink::Error::InvalidResponse(_) => {
            PrepareMessageError::Reject("WeChat image metadata or ciphertext is invalid".into())
        }
        crate::im::ilink::Error::Rejected { code, .. } => PrepareMessageError::Reject(format!(
            "WeChat image request was rejected with code {code}"
        )),
    }
}

fn release_staged(shared: &ServeShared, paths: &[std::path::PathBuf]) {
    for path in paths {
        let _ = shared.drafts.release_clipboard_path(path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::im::ilink::{
        Error as IlinkError, ImageItem, InboundItem, MediaRef, Request, Response, TextItem,
        Transport, WireId,
    };
    use crate::test_support::{SharedEvents, SteerGate, TestBehavior, TestProviderPlugin, roots};
    use crate::{BootstrapApplication, Project};

    #[derive(Default)]
    struct RecordingOutbox {
        texts: Mutex<Vec<String>>,
    }

    struct ImageTransport {
        response: Mutex<Option<Response>>,
        requests: Mutex<Vec<Request>>,
    }

    impl Transport for ImageTransport {
        fn execute(&self, request: Request) -> Result<Response, IlinkError> {
            self.requests.lock().unwrap().push(request);
            self.response
                .lock()
                .unwrap()
                .take()
                .ok_or_else(|| IlinkError::Transport("fixture exhausted".into()))
        }
    }

    impl OutboundPort for RecordingOutbox {
        fn text(&self, _target: &ReplyTarget, text: String) {
            self.texts.lock().unwrap().push(text);
        }

        fn typing(&self, _target: &ReplyTarget, _start: bool) {}
    }

    fn message_with_id(text: &str, id: &str) -> InboundMessage {
        InboundMessage {
            message_id: Some(WireId::String(id.into())),
            client_id: Some(WireId::String(format!("client-{id}"))),
            seq: Some(WireId::Number(1.into())),
            from_user_id: "user-a".into(),
            context_token: "context-a".into(),
            item_list: vec![InboundItem {
                kind: 1,
                text_item: Some(TextItem { text: text.into() }),
                image_item: None,
            }],
        }
    }

    fn message(text: &str) -> InboundMessage {
        message_with_id(text, "message-1")
    }

    fn encrypted_png_message(id: &str, key: [u8; 16]) -> (InboundMessage, Arc<ImageTransport>) {
        use aes::cipher::{BlockEncrypt as _, KeyInit as _};

        let mut ciphertext = crate::test_support::png_bytes(8, 8, [4, 5, 6]);
        let padding = 16 - ciphertext.len() % 16;
        ciphertext.extend(std::iter::repeat_n(padding as u8, padding));
        let cipher = aes::Aes128::new_from_slice(&key).unwrap();
        for block in ciphertext.as_chunks_mut::<16>().0 {
            cipher.encrypt_block(block.into());
        }
        let size = ciphertext.len() as u64;
        let transport = Arc::new(ImageTransport {
            response: Mutex::new(Some(Response {
                status: 200,
                body: ciphertext,
            })),
            requests: Mutex::new(Vec::new()),
        });
        let message = InboundMessage {
            message_id: Some(WireId::String(id.into())),
            client_id: Some(WireId::String(format!("client-{id}"))),
            seq: Some(WireId::Number(1.into())),
            from_user_id: "user-a".into(),
            context_token: "context-a".into(),
            item_list: vec![InboundItem {
                kind: 2,
                text_item: None,
                image_item: Some(ImageItem {
                    aeskey: Some(key.iter().map(|byte| format!("{byte:02x}")).collect()),
                    media: Some(MediaRef {
                        full_url: Some(
                            "https://novac2c.cdn.weixin.qq.com/c2c/download?q=opaque".into(),
                        ),
                        ..MediaRef::default()
                    }),
                    mid_size: Some(size),
                }),
            }],
        };
        (message, transport)
    }

    #[test]
    fn commands_are_standalone_exact_and_never_embedded() {
        assert_eq!(standalone_text(&message(" /help ")), Some("/help"));
        assert!(approval_command("/allow 123e4567-e89b").is_some());
        assert!(approval_command("please /allow 123e4567-e89b").is_none());
        assert!(approval_command("/allow 123 extra").is_none());
        let mut mixed = message("/allow 123");
        mixed.item_list.push(InboundItem::default());
        assert_eq!(standalone_text(&mixed), None);
    }

    #[test]
    fn unicode_chunking_is_byte_bounded_lossless_and_char_safe() {
        let text = "你🙂好".repeat(2_000);
        let chunks = unicode_chunks(&text, OUTBOUND_CHUNK_BYTES);
        assert!(chunks.len() > 1);
        assert!(
            chunks
                .iter()
                .all(|chunk| chunk.len() <= OUTBOUND_CHUNK_BYTES)
        );
        assert_eq!(chunks.concat(), text);
    }

    #[test]
    fn outbox_keeps_the_tail_and_counts_drops_without_blocking() {
        let outbox = BoundedOutbox::new();
        let target = ReplyTarget {
            user_id: "u".into(),
            context_token: "c".into(),
        };
        for index in 0..=OUTBOX_CAPACITY {
            outbox.text(&target, format!("message-{index}"));
        }
        let shutdown = AtomicBool::new(false);
        let (item, dropped) = outbox.recv(&shutdown).expect("tail item");
        assert_eq!(dropped, 1);
        assert!(matches!(item, OutboundItem::Text { text, .. } if text == "message-1"));
    }

    #[test]
    fn outbox_drop_notice_survives_typing_until_the_next_text() {
        let outbox = BoundedOutbox::new();
        let target = ReplyTarget {
            user_id: "u".into(),
            context_token: "c".into(),
        };
        outbox.state.lock().unwrap().dropped = 3;
        outbox.typing(&target, true);
        outbox.text(&target, "tail".into());

        let shutdown = AtomicBool::new(false);
        let (typing, dropped) = outbox.recv(&shutdown).expect("typing item");
        assert!(matches!(typing, OutboundItem::Typing { start: true, .. }));
        assert_eq!(dropped, 0, "typing cannot consume the user-visible notice");
        let (text, dropped) = outbox.recv(&shutdown).expect("text item");
        assert!(matches!(text, OutboundItem::Text { text, .. } if text == "tail"));
        assert_eq!(dropped, 3);
    }

    #[test]
    fn outbox_bounds_each_retained_text_item_before_network_io() {
        let outbox = BoundedOutbox::new();
        let target = ReplyTarget {
            user_id: "u".into(),
            context_token: "c".into(),
        };
        outbox.text(&target, "你".repeat(10_000));
        let state = outbox.state.lock().unwrap();
        assert!(state.items.len() > 1);
        assert!(state.items.len() <= OUTBOX_CAPACITY);
        assert!(state.items.iter().all(|item| match item {
            OutboundItem::Text { text, .. } => text.len() <= OUTBOUND_CHUNK_BYTES,
            OutboundItem::Typing { .. } => true,
        }));
    }

    #[test]
    fn outbox_shutdown_does_not_drain_a_network_backlog() {
        let outbox = BoundedOutbox::new();
        let target = ReplyTarget {
            user_id: "u".into(),
            context_token: "c".into(),
        };
        for index in 0..OUTBOX_CAPACITY {
            outbox.text(&target, format!("queued-{index}"));
        }
        let shutdown = AtomicBool::new(true);
        assert!(outbox.recv(&shutdown).is_none());
        assert_eq!(outbox.state.lock().unwrap().items.len(), OUTBOX_CAPACITY);
    }

    #[test]
    fn media_failures_distinguish_retryable_transport_from_poison_input() {
        assert!(matches!(
            classify_media_error(IlinkError::Transport("offline".into())),
            PrepareMessageError::Retry(_)
        ));
        assert!(matches!(
            classify_media_error(IlinkError::Http(503)),
            PrepareMessageError::Retry(_)
        ));
        assert!(matches!(
            classify_media_error(IlinkError::InvalidResponse("bad key".into())),
            PrepareMessageError::Reject(_)
        ));
        assert!(matches!(
            classify_media_error(IlinkError::Http(404)),
            PrepareMessageError::Reject(_)
        ));
    }

    #[test]
    fn approval_registry_is_user_scoped_first_answer_wins() {
        let registry = ApprovalRegistry::default();
        let (tx, rx) = std::sync::mpsc::channel();
        let id = registry.insert("user-a".into(), tx);
        assert!(registry.take("user-b", &id).is_err());
        let approval = registry.take("user-a", &id).expect("owned approval");
        approval.decision_tx.send(ApprovalAnswer::Deny).unwrap();
        assert_eq!(rx.recv().unwrap(), ApprovalAnswer::Deny);
        assert!(registry.take("user-a", &id).is_err());

        let (tx, _rx) = std::sync::mpsc::channel();
        let expired = registry.insert("user-a".into(), tx);
        registry
            .pending
            .lock()
            .unwrap()
            .get_mut(&expired)
            .unwrap()
            .expires_at = Instant::now() - Duration::from_millis(1);
        assert!(registry.take("user-a", &expired).is_err());
    }

    #[test]
    fn approval_wait_timeout_and_cancel_are_fail_closed() {
        let (_tx, rx) = std::sync::mpsc::channel();
        assert!(matches!(
            wait_for_approval(
                &rx,
                &CancelToken::new(),
                Instant::now() - Duration::from_millis(1)
            ),
            ApprovalWait::Timeout
        ));
        let cancel = CancelToken::new();
        cancel.cancel();
        assert!(matches!(
            wait_for_approval(&rx, &cancel, Instant::now() + Duration::from_secs(1)),
            ApprovalWait::Cancelled
        ));
    }

    #[test]
    fn new_prompt_maps_once_and_delivery_replay_is_idempotent() {
        let (storage_root, project_root) = roots("wechat-bridge-idempotent");
        std::fs::create_dir_all(&project_root).unwrap();
        let project = Project::new(&project_root);
        let bootstrap = BootstrapApplication::open(project, storage_root.clone()).unwrap();
        let application = bootstrap
            .with_permission_modes()
            .authorize_and_mount_with_provider(Arc::new(TestProviderPlugin {
                behavior: TestBehavior::Success,
            }))
            .unwrap();
        crate::test_support::configure_test_model(&application);
        application
            .save_wechat_binding(
                &Credentials::new(
                    "test-token".into(),
                    "test-bot".into(),
                    None,
                    "https://ilinkai.weixin.qq.com".into(),
                )
                .unwrap(),
            )
            .unwrap();
        application.set_wechat_allowed_user("user-a", true).unwrap();

        let app = Arc::new(Mutex::new(application));
        let shared = Arc::new(ServeShared::new(Arc::clone(&app), "token".into(), 0));
        let recording = Arc::new(RecordingOutbox::default());
        let outbound: Arc<dyn OutboundPort> = recording.clone();
        let bridge = WechatBridge {
            shared: Arc::clone(&shared),
            client: Client::new(),
            shutdown: Arc::new(AtomicBool::new(false)),
            outbound,
            approvals: Arc::new(ApprovalRegistry::default()),
            armed_new_chats: Mutex::new(HashMap::new()),
            volatile_chat_sessions: Mutex::new(HashMap::new()),
        };

        let new_command = message_with_id("/new", "new-command");
        bridge.handle_authorized(&new_command).unwrap();
        let prompt = message_with_id(
            "请读取 /Users/example/private.txt 和 https://example.invalid/secret",
            "prompt-message",
        );
        bridge.handle_authorized(&prompt).unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while !shared.active_run_info().is_null() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(shared.active_run_info().is_null());
        let delivery_id = crate::im::delivery_id(&prompt).unwrap();
        {
            let application = app.lock().unwrap();
            let admission = application
                .committed_admission(&delivery_id)
                .expect("text prompt committed");
            assert!(
                admission.receipt.attachment_ids.is_empty(),
                "path-looking or ordinary text has no attachment authority"
            );
            let binding = application
                .wechat_chat_binding("user-a")
                .expect("durable chat mapping");
            assert_eq!(binding.user_id, "user-a");
        }

        let projected_before_replay = recording.texts.lock().unwrap().len();
        bridge.handle_authorized(&prompt).unwrap();
        assert_eq!(
            recording.texts.lock().unwrap().len(),
            projected_before_replay,
            "handled delivery replay must be a no-op"
        );
        bridge.handle_authorized(&new_command).unwrap();
        assert!(
            app.lock().unwrap().wechat_chat_binding("user-a").is_some(),
            "replayed /new must not clear the established mapping"
        );
        app.lock()
            .unwrap()
            .set_wechat_allowed_user("user-a", false)
            .unwrap();
        let revoked = message_with_id("still run this", "revoked-delivery");
        let projected_before_revoked = recording.texts.lock().unwrap().len();
        bridge.handle_authorized(&revoked).unwrap();
        assert!(
            app.lock()
                .unwrap()
                .committed_admission(&crate::im::delivery_id(&revoked).unwrap())
                .is_none(),
            "bridge re-checks revocation even if the host had authorized earlier"
        );
        assert_eq!(
            recording.texts.lock().unwrap().len(),
            projected_before_revoked,
            "revoked input receives no reflection"
        );

        shared.mark_shutting_down();
        shared.drain_workers();
        drop(bridge);
        let shared = Arc::try_unwrap(shared).ok().expect("sole shared owner");
        drop(shared);
        let application = Arc::try_unwrap(app)
            .ok()
            .expect("sole application owner")
            .into_inner()
            .unwrap();
        application.close().unwrap();
        crate::test_support::cleanup_tree(&storage_root);
        crate::test_support::cleanup_tree(&project_root);
    }

    #[test]
    fn committed_prompt_repairs_chat_mapping_after_write_failure_and_restart() {
        let (storage_root, project_root) = roots("wechat-bridge-mapping-restart");
        std::fs::create_dir_all(&project_root).unwrap();
        let project = Project::new(&project_root);
        let bootstrap = BootstrapApplication::open(project.clone(), storage_root.clone()).unwrap();
        let application = bootstrap
            .with_permission_modes()
            .authorize_and_mount_with_provider(Arc::new(TestProviderPlugin {
                behavior: TestBehavior::Success,
            }))
            .unwrap();
        crate::test_support::configure_test_model(&application);
        application
            .save_wechat_binding(
                &Credentials::new(
                    "test-token".into(),
                    "test-bot".into(),
                    None,
                    "https://ilinkai.weixin.qq.com".into(),
                )
                .unwrap(),
            )
            .unwrap();
        application.set_wechat_allowed_user("user-a", true).unwrap();

        let app = Arc::new(Mutex::new(application));
        let shared = Arc::new(ServeShared::new(Arc::clone(&app), "token".into(), 0));
        let bridge = WechatBridge {
            shared: Arc::clone(&shared),
            client: Client::new(),
            shutdown: Arc::new(AtomicBool::new(false)),
            outbound: Arc::new(RecordingOutbox::default()),
            approvals: Arc::new(ApprovalRegistry::default()),
            armed_new_chats: Mutex::new(HashMap::new()),
            volatile_chat_sessions: Mutex::new(HashMap::new()),
        };
        bridge
            .handle_authorized(&message_with_id("/new", "mapping-new"))
            .unwrap();
        app.lock()
            .unwrap()
            .inject_next_wechat_chat_mapping_save_failure();
        let prompt = message_with_id("persist this once", "mapping-prompt");
        let delivery_id = crate::im::delivery_id(&prompt).unwrap();
        let error = bridge
            .handle_authorized(&prompt)
            .expect_err("post-admission mapping write must retain the delivery");
        assert!(error.contains("injected im.json chat mapping write failure"));
        let deadline = Instant::now() + Duration::from_secs(5);
        while !shared.active_run_info().is_null() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        let committed_session = {
            let application = app.lock().unwrap();
            assert!(application.committed_admission(&delivery_id).is_some());
            assert!(application.wechat_chat_binding("user-a").is_none());
            application
                .current_session_id()
                .expect("materialized session")
        };

        shared.mark_shutting_down();
        shared.drain_workers();
        drop(bridge);
        let shared = Arc::try_unwrap(shared).ok().expect("sole shared owner");
        drop(shared);
        let application = Arc::try_unwrap(app)
            .ok()
            .expect("sole application owner")
            .into_inner()
            .unwrap();
        application.close().unwrap();

        let bootstrap = BootstrapApplication::open(project, storage_root.clone()).unwrap();
        let application = bootstrap
            .with_permission_modes()
            .authorize_and_mount_with_provider(Arc::new(TestProviderPlugin {
                behavior: TestBehavior::Success,
            }))
            .unwrap();
        crate::test_support::configure_test_model(&application);
        let app = Arc::new(Mutex::new(application));
        let shared = Arc::new(ServeShared::new(Arc::clone(&app), "token".into(), 0));
        let bridge = WechatBridge {
            shared: Arc::clone(&shared),
            client: Client::new(),
            shutdown: Arc::new(AtomicBool::new(false)),
            outbound: Arc::new(RecordingOutbox::default()),
            approvals: Arc::new(ApprovalRegistry::default()),
            armed_new_chats: Mutex::new(HashMap::new()),
            volatile_chat_sessions: Mutex::new(HashMap::new()),
        };

        fn allow_all(
            _request: crate::PermissionRequest,
            _cancel: &crate::model::CancelToken,
        ) -> crate::PermissionDecision {
            crate::PermissionDecision::Allow
        }
        let (completion_tx, completion_rx) = std::sync::mpsc::channel();
        let (drift_handle, drift_session) = {
            let mut application = app.lock().unwrap();
            application
                .new_session()
                .expect("select a different session");
            let handle = application
                .start_run(ApplicationRunRequest {
                    message: PendingMessage::text("materialize selection drift"),
                    approver: Arc::new(allow_all),
                    asker: None,
                    events: Box::new(SharedEvents(Arc::new(Mutex::new(Vec::new())))),
                    completion: completion_tx,
                })
                .expect("start drift run");
            let session = application.current_session_id().expect("drift session");
            (handle, session)
        };
        drift_handle.join().unwrap();
        completion_rx
            .recv()
            .unwrap()
            .expect("selection-drift run succeeds");
        assert_ne!(drift_session, committed_session);

        assert!(shared.try_claim_run("other-client", super::super::state::now_ms()));
        let busy_error = bridge
            .handle_authorized(&prompt)
            .expect_err("mapping recovery must remain retryable while another run owns the slot");
        assert!(busy_error.contains("chat-mapping recovery"));
        {
            let application = app.lock().unwrap();
            assert!(application.wechat_chat_binding("user-a").is_none());
            assert!(
                !application.is_wechat_delivery_handled(&delivery_id),
                "busy recovery must not consume the durable delivery"
            );
        }
        shared.release_run_claim();

        app.lock()
            .unwrap()
            .inject_next_admission_owner_scan_failure();
        let scan_error = bridge
            .handle_authorized(&prompt)
            .expect_err("owner-resolution failure must keep the delivery retryable");
        assert!(scan_error.contains("intentional admission-owner scan failure"));
        {
            let application = app.lock().unwrap();
            assert!(application.wechat_chat_binding("user-a").is_none());
            assert!(
                application
                    .pending_wechat_chat_mapping(&delivery_id, "user-a", "user-a")
                    .is_some(),
                "owner-resolution failure keeps the durable mapping intent"
            );
            assert!(
                !application.is_wechat_delivery_handled(&delivery_id),
                "owner-resolution failure must not consume the delivery"
            );
        }

        bridge
            .handle_authorized(&prompt)
            .expect("restart replay repairs the durable mapping");
        let application = app.lock().unwrap();
        let binding = application
            .wechat_chat_binding("user-a")
            .expect("mapping repaired from the committed delivery");
        assert_eq!(binding.user_id, "user-a");
        assert_eq!(binding.session_id, committed_session);
        assert!(application.is_wechat_delivery_handled(&delivery_id));
        drop(application);

        let sessions = app.lock().unwrap().list_sessions().unwrap();
        let mut admission_sessions = Vec::new();
        for summary in sessions {
            let mut application = app.lock().unwrap();
            application.switch_session(summary.id.clone()).unwrap();
            if application.committed_admission(&delivery_id).is_some() {
                admission_sessions.push(summary.id);
            }
        }
        assert_eq!(
            admission_sessions,
            vec![committed_session],
            "the remote delivery must exist in exactly its original session"
        );

        shared.mark_shutting_down();
        shared.drain_workers();
        drop(bridge);
        let shared = Arc::try_unwrap(shared).ok().expect("sole shared owner");
        drop(shared);
        let application = Arc::try_unwrap(app)
            .ok()
            .expect("sole application owner")
            .into_inner()
            .unwrap();
        application.close().unwrap();
        crate::test_support::cleanup_tree(&storage_root);
        crate::test_support::cleanup_tree(&project_root);
    }

    #[test]
    fn permission_projection_is_summary_only_and_exact_always_escalates_then_resumes() {
        let (storage_root, project_root) = roots("wechat-bridge-approval");
        std::fs::create_dir_all(&project_root).unwrap();
        let project = Project::new(&project_root);
        let bootstrap = BootstrapApplication::open(project, storage_root.clone()).unwrap();
        let application = bootstrap
            .with_permission_modes()
            .authorize_and_mount_with_provider(Arc::new(TestProviderPlugin {
                behavior: TestBehavior::RunCommand,
            }))
            .unwrap();
        crate::test_support::configure_test_model(&application);
        application
            .save_wechat_binding(
                &Credentials::new(
                    "test-token".into(),
                    "test-bot".into(),
                    None,
                    "https://ilinkai.weixin.qq.com".into(),
                )
                .unwrap(),
            )
            .unwrap();
        application.set_wechat_allowed_user("user-a", true).unwrap();

        let app = Arc::new(Mutex::new(application));
        let shared = Arc::new(ServeShared::new(Arc::clone(&app), "token".into(), 0));
        let recording = Arc::new(RecordingOutbox::default());
        let outbound: Arc<dyn OutboundPort> = recording.clone();
        let bridge = WechatBridge {
            shared: Arc::clone(&shared),
            client: Client::new(),
            shutdown: Arc::new(AtomicBool::new(false)),
            outbound,
            approvals: Arc::new(ApprovalRegistry::default()),
            armed_new_chats: Mutex::new(HashMap::new()),
            volatile_chat_sessions: Mutex::new(HashMap::new()),
        };

        bridge
            .handle_authorized(&message_with_id("/new", "command-new"))
            .unwrap();
        bridge
            .handle_authorized(&message_with_id("run it", "prompt-approval"))
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        let approval = loop {
            if let Some(value) = recording
                .texts
                .lock()
                .unwrap()
                .iter()
                .find(|text| text.starts_with("需要审批\nID: "))
                .cloned()
            {
                break value;
            }
            assert!(Instant::now() < deadline, "approval was not projected");
            std::thread::sleep(Duration::from_millis(10));
        };
        assert!(!approval.contains("echo from-serve-test"));
        assert!(!approval.contains("\"command\""));
        let request_id = approval
            .lines()
            .find_map(|line| line.strip_prefix("ID: "))
            .unwrap();
        bridge
            .handle_authorized(&message_with_id(
                &format!("/always {request_id}"),
                "approval-answer",
            ))
            .unwrap();
        while !shared.active_run_info().is_null() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(shared.active_run_info().is_null());
        assert_eq!(
            app.lock().unwrap().permission_mode(),
            PermissionMode::FullAccess,
            "explicit /always uses the durable core session-mode setter"
        );
        assert!(
            recording
                .texts
                .lock()
                .unwrap()
                .iter()
                .any(|text| text == "command attempted")
        );

        shared.mark_shutting_down();
        shared.drain_workers();
        drop(bridge);
        let shared = Arc::try_unwrap(shared).ok().expect("sole shared owner");
        drop(shared);
        let application = Arc::try_unwrap(app)
            .ok()
            .expect("sole application owner")
            .into_inner()
            .unwrap();
        application.close().unwrap();
        crate::test_support::cleanup_tree(&storage_root);
        crate::test_support::cleanup_tree(&project_root);
    }

    #[test]
    fn always_denies_when_full_access_cannot_commit() {
        let (storage_root, project_root) = roots("wechat-bridge-always-persist-failure");
        std::fs::create_dir_all(&project_root).unwrap();
        let project = Project::new(&project_root);
        let bootstrap = BootstrapApplication::open(project, storage_root.clone()).unwrap();
        let application = bootstrap
            .with_permission_modes()
            .authorize_and_mount_with_provider(Arc::new(TestProviderPlugin {
                behavior: TestBehavior::RunCommand,
            }))
            .unwrap();
        crate::test_support::configure_test_model(&application);
        application
            .save_wechat_binding(
                &Credentials::new(
                    "test-token".into(),
                    "test-bot".into(),
                    None,
                    "https://ilinkai.weixin.qq.com".into(),
                )
                .unwrap(),
            )
            .unwrap();
        application.set_wechat_allowed_user("user-a", true).unwrap();

        let app = Arc::new(Mutex::new(application));
        let shared = Arc::new(ServeShared::new(Arc::clone(&app), "token".into(), 0));
        let recording = Arc::new(RecordingOutbox::default());
        let outbound: Arc<dyn OutboundPort> = recording.clone();
        let bridge = WechatBridge {
            shared: Arc::clone(&shared),
            client: Client::new(),
            shutdown: Arc::new(AtomicBool::new(false)),
            outbound,
            approvals: Arc::new(ApprovalRegistry::default()),
            armed_new_chats: Mutex::new(HashMap::new()),
            volatile_chat_sessions: Mutex::new(HashMap::new()),
        };

        bridge
            .handle_authorized(&message_with_id("/new", "failure-new"))
            .unwrap();
        bridge
            .handle_authorized(&message_with_id("run it", "failure-prompt"))
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        let approval = loop {
            if let Some(value) = recording
                .texts
                .lock()
                .unwrap()
                .iter()
                .find(|text| text.starts_with("需要审批\nID: "))
                .cloned()
            {
                break value;
            }
            assert!(Instant::now() < deadline, "approval was not projected");
            std::thread::sleep(Duration::from_millis(10));
        };
        let request_id = approval
            .lines()
            .find_map(|line| line.strip_prefix("ID: "))
            .unwrap();
        app.lock().unwrap().inject_session_persistence_faults(
            crate::session::persistence::FaultHooks {
                fail_batch_write: true,
                ..crate::session::persistence::FaultHooks::default()
            },
        );
        bridge
            .handle_authorized(&message_with_id(
                &format!("/always {request_id}"),
                "failure-answer",
            ))
            .unwrap();
        while !shared.active_run_info().is_null() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(shared.active_run_info().is_null());
        assert_eq!(
            app.lock().unwrap().permission_mode(),
            PermissionMode::ProjectWrite,
            "failed durable escalation must preserve the prior mode"
        );
        let texts = recording.texts.lock().unwrap();
        assert!(
            !texts.iter().any(|text| text == "正在运行工具：run_command"),
            "the gated tool must not start when /always cannot commit"
        );
        assert!(
            texts
                .iter()
                .any(|text| text.contains("Full Access") && text.contains("拒绝")),
            "the paired user must receive an explicit fail-closed result"
        );
        drop(texts);

        shared.mark_shutting_down();
        shared.drain_workers();
        drop(bridge);
        let shared = Arc::try_unwrap(shared).ok().expect("sole shared owner");
        drop(shared);
        let application = Arc::try_unwrap(app)
            .ok()
            .expect("sole application owner")
            .into_inner()
            .unwrap();
        application.close().unwrap();
        crate::test_support::cleanup_tree(&storage_root);
        crate::test_support::cleanup_tree(&project_root);
    }

    #[test]
    fn busy_delivery_uses_core_steering_and_returns_only_after_durable_claim() {
        let (storage_root, project_root) = roots("wechat-bridge-steering");
        std::fs::create_dir_all(&project_root).unwrap();
        let gate = Arc::new(SteerGate::default());
        let project = Project::new(&project_root);
        let bootstrap = BootstrapApplication::open(project, storage_root.clone()).unwrap();
        let application = bootstrap
            .with_permission_modes()
            .authorize_and_mount_with_provider(Arc::new(TestProviderPlugin {
                behavior: TestBehavior::Steer(Arc::clone(&gate)),
            }))
            .unwrap();
        crate::test_support::configure_test_model(&application);
        application
            .save_wechat_binding(
                &Credentials::new(
                    "test-token".into(),
                    "test-bot".into(),
                    None,
                    "https://ilinkai.weixin.qq.com".into(),
                )
                .unwrap(),
            )
            .unwrap();
        application.set_wechat_allowed_user("user-a", true).unwrap();

        let app = Arc::new(Mutex::new(application));
        let shared = Arc::new(ServeShared::new(Arc::clone(&app), "token".into(), 0));
        let recording = Arc::new(RecordingOutbox::default());
        let outbound: Arc<dyn OutboundPort> = recording;
        let bridge = Arc::new(WechatBridge {
            shared: Arc::clone(&shared),
            client: Client::new(),
            shutdown: Arc::new(AtomicBool::new(false)),
            outbound,
            approvals: Arc::new(ApprovalRegistry::default()),
            armed_new_chats: Mutex::new(HashMap::new()),
            volatile_chat_sessions: Mutex::new(HashMap::new()),
        });

        bridge
            .handle_authorized(&message_with_id("/new", "steer-new"))
            .unwrap();
        bridge
            .handle_authorized(&message_with_id("first", "steer-first"))
            .unwrap();
        gate.wait_entered();
        let steered = message_with_id("also run the tests", "steer-second");
        let delivery_id = crate::im::delivery_id(&steered).unwrap();
        let bridge_for_thread = Arc::clone(&bridge);
        let sender = std::thread::spawn(move || bridge_for_thread.handle_authorized(&steered));
        std::thread::sleep(Duration::from_millis(50));
        assert!(
            !sender.is_finished(),
            "cursor must wait for durable steering claim"
        );
        gate.release();
        sender.join().unwrap().unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while !shared.active_run_info().is_null() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(gate.saw_steering.load(Ordering::Acquire));
        assert!(
            app.lock()
                .unwrap()
                .committed_admission(&delivery_id)
                .is_some()
        );

        shared.mark_shutting_down();
        shared.drain_workers();
        drop(bridge);
        let shared = Arc::try_unwrap(shared).ok().expect("sole shared owner");
        drop(shared);
        let application = Arc::try_unwrap(app)
            .ok()
            .expect("sole application owner")
            .into_inner()
            .unwrap();
        application.close().unwrap();
        crate::test_support::cleanup_tree(&storage_root);
        crate::test_support::cleanup_tree(&project_root);
    }

    #[test]
    fn encrypted_wechat_image_enters_the_existing_attachment_admission_path() {
        let (storage_root, project_root) = roots("wechat-bridge-image");
        std::fs::create_dir_all(&project_root).unwrap();
        let project = Project::new(&project_root);
        let bootstrap = BootstrapApplication::open(project, storage_root.clone()).unwrap();
        let application = bootstrap
            .with_permission_modes()
            .authorize_and_mount_with_provider(Arc::new(TestProviderPlugin {
                behavior: TestBehavior::Success,
            }))
            .unwrap();
        crate::test_support::configure_test_model(&application);
        application
            .save_wechat_binding(
                &Credentials::new(
                    "test-token".into(),
                    "test-bot".into(),
                    None,
                    "https://ilinkai.weixin.qq.com".into(),
                )
                .unwrap(),
            )
            .unwrap();
        application.set_wechat_allowed_user("user-a", true).unwrap();

        let app = Arc::new(Mutex::new(application));
        let shared = Arc::new(ServeShared::new(Arc::clone(&app), "token".into(), 0));
        let recording = Arc::new(RecordingOutbox::default());
        let outbound: Arc<dyn OutboundPort> = recording;
        let (image_message, transport) = encrypted_png_message("image-prompt", [0x44; 16]);
        let bridge = WechatBridge {
            shared: Arc::clone(&shared),
            client: Client::with_transport(transport.clone()),
            shutdown: Arc::new(AtomicBool::new(false)),
            outbound,
            approvals: Arc::new(ApprovalRegistry::default()),
            armed_new_chats: Mutex::new(HashMap::new()),
            volatile_chat_sessions: Mutex::new(HashMap::new()),
        };

        bridge
            .handle_authorized(&message_with_id("/new", "image-new"))
            .unwrap();
        bridge.handle_authorized(&image_message).unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while !shared.active_run_info().is_null() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        let delivery_id = crate::im::delivery_id(&image_message).unwrap();
        let record = app
            .lock()
            .unwrap()
            .committed_admission(&delivery_id)
            .expect("image prompt committed");
        assert_eq!(record.receipt.attachment_ids.len(), 1);
        let requests = transport.requests.lock().unwrap();
        assert_eq!(requests.len(), 1);
        assert!(
            requests[0].headers.is_empty(),
            "CDN receives no bot credential"
        );
        assert!(requests[0].body.is_none());

        shared.mark_shutting_down();
        shared.drain_workers();
        drop(bridge);
        let shared = Arc::try_unwrap(shared).ok().expect("sole shared owner");
        drop(shared);
        let application = Arc::try_unwrap(app)
            .ok()
            .expect("sole application owner")
            .into_inner()
            .unwrap();
        application.close().unwrap();
        crate::test_support::cleanup_tree(&storage_root);
        crate::test_support::cleanup_tree(&project_root);
    }
}
