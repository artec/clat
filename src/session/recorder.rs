//! Dual-stream run production (plan §9): one `RunEvent` stream for
//! frontends (unchanged protocol) and one `SessionEvent` stream into the
//! durable journal, derived from the same run loop. The recorder owns the
//! DSH event vocabulary — turn/step envelope, chunk deltas, assistant
//! message composed from accumulated deltas, tool call/result pairing,
//! and the approval durability barrier.
//!
//! Terminal `RunEvent`s (completed/cancelled/failed) are withheld until
//! `finish()` flushes the closing `step/end` + `turn/end` pair — the UI
//! only learns a run is over once the log is durable (plan §16 stage 4).
//!
//! Barrier order for approved side-effecting tools follows the event
//! catalog §3: `approval/asked` flushed before the human answers, then
//! `approval/decided` + `tool/call` as one atomic flushed group, then the
//! invocation, then `tool/result`.

use crate::event::{EventSink, RunEvent};
use crate::model::ModelEvent;
use crate::permission::{PermissionApprover, PermissionDecision, PermissionRequest};
use crate::session::event::{TurnEndReason, payloads};
use crate::session::run_journal::{NewSessionEvent, RunJournal};
use serde_json::Value;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

/// The canonical `request/header` body the application computes from the
/// real model request inputs (provider, model, sampling/thinking config,
/// resolved system prompt, tool definitions). Built once per run; the
/// recorder publishes it before the first dispatch (catalog §2.7).
#[derive(Clone)]
pub(crate) struct RequestHeaderData {
    pub(crate) header: Value,
    pub(crate) base_system: String,
    pub(crate) dynamic_instructions: Option<Arc<dyn crate::plugins::services::DynamicInstructions>>,
    pub(crate) tool_registry: Option<Arc<crate::tool::ToolRegistry>>,
}

/// Shared between the recorder (which stashes `ToolRequested` calls) and
/// the journaling approver (which runs inside the permission check and
/// journals `approval/decided` + `tool/call` before `ToolStarted` fires).
#[derive(Default)]
struct SharedCore {
    turn: u64,
    step: u64,
    pending: HashMap<String, PendingCall>,
    order: Vec<String>,
    journaled: Vec<String>,
}

impl SharedCore {
    fn stash(&mut self, call_id: String, call: PendingCall) {
        if !self.pending.contains_key(&call_id) {
            self.order.push(call_id.clone());
        }
        self.pending.insert(call_id, call);
    }

    fn take(&mut self, call_id: &str) -> Option<PendingCall> {
        let call = self.pending.remove(call_id);
        if call.is_some() {
            self.order.retain(|id| id != call_id);
        }
        call
    }

    fn get(&self, call_id: &str) -> Option<&PendingCall> {
        self.pending.get(call_id)
    }

    fn is_journaled(&self, call_id: &str) -> bool {
        self.journaled.iter().any(|id| id == call_id)
    }

    /// The most recent stashed call with this tool name that the approver
    /// has not already journaled (policy-level denies carry no call id in
    /// the RunEvent, only the tool name).
    fn unjournaled_by_name(&self, tool: &str) -> Option<String> {
        self.order
            .iter()
            .rev()
            .find(|id| {
                !self.is_journaled(id)
                    && self.pending.get(*id).is_some_and(|call| call.name == tool)
            })
            .cloned()
    }
}

#[derive(Clone)]
struct PendingCall {
    id: String,
    name: String,
    arguments: Value,
    /// Block index used for this call's streaming deltas.
    index: u64,
}

/// The journaling half of the approval barrier.
pub(crate) struct JournalingApprover {
    inner: Arc<dyn PermissionApprover>,
    journal: Arc<dyn RunJournal>,
    shared: Arc<Mutex<SharedCore>>,
}

impl JournalingApprover {
    fn new(
        inner: Arc<dyn PermissionApprover>,
        journal: Arc<dyn RunJournal>,
        shared: Arc<Mutex<SharedCore>>,
    ) -> Self {
        Self {
            inner,
            journal,
            shared,
        }
    }
}

/// The DSH outcome string for a decision (catalog §2.4): a human's "no" is
/// `rejected`; a fail-closed decision from an approver that could not ask
/// anyone is `unavailable`.
fn approval_outcome(decision: &PermissionDecision) -> &'static str {
    match decision {
        PermissionDecision::Allow => "allowed-once",
        PermissionDecision::Deny { .. } | PermissionDecision::Ask { .. } => "rejected",
        PermissionDecision::Unavailable { .. } => "unavailable",
    }
}

impl PermissionApprover for JournalingApprover {
    fn decide(
        &self,
        request: PermissionRequest,
        cancel: &crate::model::CancelToken,
    ) -> PermissionDecision {
        let (turn, step, call) = {
            let shared = self.shared.lock().expect("recorder lock");
            (
                shared.turn,
                shared.step,
                shared.get(&request.call_id).cloned(),
            )
        };
        let approval_id = uuid::Uuid::new_v4().to_string();
        let asked = NewSessionEvent::new(
            "approval/asked",
            payloads::approval_asked(
                &approval_id,
                &request.tool,
                Some(&request.call_id),
                &request.reason,
            ),
        )
        .log_only();
        if let Err(error) = self
            .journal
            .append(asked)
            .and_then(|_| self.journal.flush())
        {
            return PermissionDecision::Deny {
                reason: format!("session journal write failed before approval: {error}"),
            };
        }
        let request_call_id = request.call_id.clone();
        let request_tool = request.tool.clone();
        let decision = self.inner.decide(request, cancel);
        let outcome = approval_outcome(&decision);
        let mut group = vec![
            NewSessionEvent::new(
                "approval/decided",
                payloads::approval_decided(&approval_id, outcome),
            )
            .log_only(),
        ];
        match (&decision, &call) {
            (PermissionDecision::Allow, Some(call)) => group.push(
                NewSessionEvent::new(
                    "tool/call",
                    payloads::tool_call(turn, step, &request_call_id, &call.name, &call.arguments),
                )
                .log_only(),
            ),
            // An allow without a stashed call: nothing to journal yet (the
            // ToolStarted handler writes tool/call when the call arrives).
            (PermissionDecision::Allow, None) => {}
            // Deny path (catalog §3): decided + error tool/result — no
            // tool/call. A result without a call is legal (recovery
            // synthesizes the same shape).
            (_, _) => group.push(
                NewSessionEvent::new(
                    "tool/result",
                    payloads::tool_result(
                        turn,
                        step,
                        &request_call_id,
                        payloads::tool_result_content(&Value::String(format!(
                            "permission denied for tool `{request_tool}`"
                        ))),
                        true,
                    ),
                )
                .append(Vec::new()),
            ),
        }
        if let Err(error) = self
            .journal
            .append_atomic(&group)
            .and_then(|_| self.journal.flush())
        {
            return PermissionDecision::Deny {
                reason: format!("session journal write failed after approval: {error}"),
            };
        }
        if let Ok(mut shared) = self.shared.lock() {
            shared.journaled.push(request_call_id);
        }
        decision
    }
}

/// The dual-stream `EventSink`. One per run; `finish` closes the turn and
/// publishes the withheld terminal event.
pub(crate) struct SessionRecorder {
    journal: Arc<dyn RunJournal>,
    /// FP-09（架构层）：待转发给 frontend sink 的 RunEvent 队列——
    /// sink 不再在 recorder 临界区内执行（`inner` 字段已移除），
    /// 调用方（RecorderHandle）锁外转发。
    forwarded: Vec<RunEvent>,
    /// finish 扣住的终态事件（journal 健康性裁决后发布）——同样由
    /// 调用方锁外转发。
    published: Vec<RunEvent>,
    shared: Arc<Mutex<SharedCore>>,
    provider: String,
    model: String,
    /// Why this run's first dispatch may append a `request/header`
    /// (`initial` / `resume` / `change`), or `None` to suppress: the
    /// catalog (§2.7) appends a header event only when it differs from
    /// the previous one; the application owns that comparison.
    header_reason: Option<&'static str>,
    /// Canonical request/header body from the real model-request inputs.
    request_header: Value,
    base_system: String,
    dynamic_instructions: Option<Arc<dyn crate::plugins::services::DynamicInstructions>>,
    tool_registry: Option<Arc<crate::tool::ToolRegistry>>,
    step_open: bool,
    /// The open step's assistant/message was already journaled.
    message_emitted: bool,
    next_block_index: u64,
    /// State accumulated for the open step's assistant message.
    text: String,
    reasoning: String,
    completed_calls: Vec<crate::tool::ToolCall>,
    chunk_seqs: Vec<u64>,
    /// The step's final token usage (stream-end `ModelEvent::Usage`), landed
    /// on the assistant/message payload (DSH `usage` field). Reset per step.
    stream_usage: Option<crate::model::Usage>,
    /// 插件 sampling 的记账单元（INV-S6）：`ModelResponded` 落账时归并
    /// 进本条 assistant/message 的 usage——journal 侧的唯一记账点；取
    /// 消/失败路径的余量由 run worker 补进终态总量。None = 本 run 无
    /// 桥接（零字节影响）。
    aux_usage: Option<Arc<Mutex<crate::model::Usage>>>,
    /// Provider opaque state to persist on this step's assistant message
    /// (`source.replayState`), carried by `ModelResponded`.
    replay_state: Option<Value>,
    /// Correlates one `llm/retry` with the following
    /// `llm/retry-started`; both events use the same DSH retryId.
    pending_retry_id: Option<String>,
    terminal: Option<RunEvent>,
    journal_error: Option<String>,
    /// B1 花费护栏的预警状态（50%/90% 各恰好一次；hard-stop 在 run.rs）。
    budget: RunBudgetEvents,
}

impl SessionRecorder {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        journal: Arc<dyn RunJournal>,
        request_header: RequestHeaderData,
        provider: &str,
        model: &str,
        turn: u64,
        header_reason: Option<&'static str>,
    ) -> Self {
        Self {
            journal,
            forwarded: Vec::new(),
            published: Vec::new(),
            shared: Arc::new(Mutex::new(SharedCore {
                turn,
                // First step is 0 (catalog §3); open_step assigns and
                // post-increments.
                step: 0,
                ..SharedCore::default()
            })),
            provider: provider.into(),
            model: model.into(),
            header_reason,
            request_header: request_header.header,
            base_system: request_header.base_system,
            dynamic_instructions: request_header.dynamic_instructions,
            tool_registry: request_header.tool_registry,
            step_open: false,
            message_emitted: false,
            stream_usage: None,
            aux_usage: None,
            next_block_index: 0,
            text: String::new(),
            reasoning: String::new(),
            completed_calls: Vec::new(),
            chunk_seqs: Vec::new(),
            replay_state: None,
            pending_retry_id: None,
            terminal: None,
            journal_error: None,
            budget: RunBudgetEvents::default(),
        }
    }

    /// The journaling approver sharing this run's call bookkeeping.
    pub(crate) fn approver(&self, inner: Arc<dyn PermissionApprover>) -> JournalingApprover {
        JournalingApprover::new(inner, Arc::clone(&self.journal), Arc::clone(&self.shared))
    }

    /// 挂接插件 sampling 记账单元（run worker 构造 recorder 后立即调
    /// 用；单元由 start_run 创建，宿主桥与 recorder 共享）。
    pub(crate) fn attach_aux_usage(&mut self, cell: Arc<Mutex<crate::model::Usage>>) {
        self.aux_usage = Some(cell);
    }

    /// Build the recorder and its journaling approver in one step: the
    /// caller immediately moves the recorder into shared storage for the
    /// `EventSink` handle, so the approver must be created here, before
    /// that. The UI sink attaches afterwards via `attach_sink`.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn with_approver(
        journal: Arc<dyn RunJournal>,
        inner: Arc<dyn PermissionApprover>,
        request_header: RequestHeaderData,
        provider: &str,
        model: &str,
        turn: u64,
        header_reason: Option<&'static str>,
    ) -> (Self, JournalingApprover) {
        let recorder = Self::new(
            journal,
            request_header,
            provider,
            model,
            turn,
            header_reason,
        );
        let approver = recorder.approver(inner);
        (recorder, approver)
    }

    /// Close the step (if open), close the turn, flush, and prepare the
    /// withheld terminal RunEvent for publication. Returns the journal
    /// error (merged into the run result by the caller) plus the RunEvents
    /// to publish to the frontend — FP-09: publication happens outside the
    /// recorder lock in the caller.
    ///
    /// Audit P1-09: a successful terminal (completed/cancelled) may only
    /// reach the frontend after the closing `turn/end` is durable. When
    /// the final flush fails, the withheld success is replaced by a
    /// `RunFailed` carrying the journal error so the event stream and the
    /// completion channel agree.
    pub(crate) fn finish(&mut self, reason: TurnEndReason) -> (Option<String>, Vec<RunEvent>) {
        // B2（DSH 0.1.1-rc.1 对齐）：aborted 终态的部分产出前缀定稿为
        // `interrupted: true` 的 assistant/message（恢复合成收尾不经过
        // 本路径，语义不变）。
        let interrupted = matches!(reason, TurnEndReason::Aborted { .. });
        self.close_open_step_interrupted(interrupted);
        let turn = self.shared.lock().map(|shared| shared.turn).unwrap_or(0);
        self.append_quietly(NewSessionEvent::new(
            "turn/end",
            payloads::turn_end(turn, &reason),
        ));
        let flush_error = self.journal.flush().err();
        if let Some(error) = flush_error {
            self.journal_error.get_or_insert(error.clone());
        }
        // A success terminal requires the journal to be healthy for the
        // WHOLE turn, not just the final flush: an append that failed
        // mid-run already made the durable log incomplete, and the
        // completion channel will report failure — the event stream must
        // not claim success (audit P1-09, second pass).
        if self.journal_error.is_some() {
            let base = match self.terminal.take() {
                Some(RunEvent::RunFailed { message }) => message,
                _ => String::new(),
            };
            let error = self.journal_error.clone().expect("just checked");
            let message = if base.is_empty() {
                format!("session journal failed: {error}")
            } else {
                format!("{base}; session journal failed: {error}")
            };
            self.published.push(RunEvent::RunFailed { message });
        } else if let Some(terminal) = self.terminal.take() {
            self.published.push(terminal);
        }
        (
            self.journal_error.clone(),
            std::mem::take(&mut self.published),
        )
    }

    /// Emit `step/end` for the open step, keeping any partial assistant
    /// message first: a step cut short by a stream failure still keeps the
    /// partial text the UI already showed (the same guarantee `finish`
    /// gives the whole turn).
    fn close_open_step(&mut self) {
        self.close_open_step_interrupted(false);
    }

    /// [`Self::close_open_step`] 的参数化核：`interrupted` 仅在取消
    /// 终态的部分产出定稿上为真（settled 路径恒假）。
    fn close_open_step_interrupted(&mut self, interrupted: bool) {
        if !self.step_open {
            return;
        }
        if !self.message_emitted && self.has_partial_message() {
            self.assistant_message_interrupted(interrupted);
        }
        let (turn, step) = self.state();
        self.append_quietly(NewSessionEvent::new(
            "step/end",
            payloads::step_end(turn, step),
        ));
        if let Ok(mut shared) = self.shared.lock() {
            shared.step = step + 1;
        }
        self.step_open = false;
    }

    fn append_quietly(&mut self, event: NewSessionEvent) -> Option<u64> {
        if self.journal_error.is_some() {
            return None;
        }
        match self.journal.append(event) {
            Ok(seq) => Some(seq),
            Err(error) => {
                self.journal_error.get_or_insert(error);
                None
            }
        }
    }

    fn flush_quietly(&mut self) {
        if self.journal_error.is_some() {
            return;
        }
        if let Err(error) = self.journal.flush() {
            self.journal_error.get_or_insert(error);
        }
    }

    fn open_step(&mut self) {
        if self.step_open {
            return;
        }
        // `shared.step` is the number of the step now opening (0-based,
        // catalog §3); it advances when the step closes, so every reader
        // between open and close sees the same step number.
        let (turn, step) = {
            let shared = self.shared.lock().expect("recorder lock");
            (shared.turn, shared.step)
        };
        self.append_quietly(NewSessionEvent::new(
            "step/start",
            payloads::step_start(turn, step),
        ));
        if let Some(reason) = self.header_reason.take() {
            let payload = serde_json::json!({
                "header": self.request_header.clone(),
                "reason": reason,
            });
            self.append_quietly(NewSessionEvent::new("request/header", payload));
        }
        self.step_open = true;
        self.message_emitted = false;
        self.text.clear();
        self.reasoning.clear();
        self.completed_calls.clear();
        self.chunk_seqs.clear();
        self.replay_state = None;
        self.stream_usage = None;
        self.pending_retry_id = None;
        self.next_block_index = 0;
    }

    fn refresh_dynamic_instructions(&mut self) {
        let Some(source) = &self.dynamic_instructions else {
            return;
        };
        let snapshot = match source.snapshot() {
            Ok(snapshot) => snapshot,
            Err(error) => {
                self.journal_error
                    .get_or_insert(format!("project instructions failed: {error}"));
                return;
            }
        };
        if crate::plugins::services::apply_instructions_to_header(
            &mut self.request_header,
            &self.base_system,
            snapshot.as_ref(),
        ) {
            self.header_reason = Some("change");
        }
    }

    /// Whether deltas accumulated for the open step have content worth a
    /// partial assistant/message (a step cut short by a stream failure).
    fn has_partial_message(&self) -> bool {
        !self.text.is_empty() || !self.reasoning.is_empty() || !self.chunk_seqs.is_empty()
    }

    fn chunk(&mut self, chunk: Value) {
        let (turn, step) = {
            let shared = self.shared.lock().expect("recorder lock");
            (shared.turn, shared.step)
        };
        let event = NewSessionEvent::new(
            "assistant/chunk",
            payloads::assistant_chunk(turn, step, chunk),
        )
        .log_only();
        if let Some(seq) = self.append_quietly(event) {
            self.chunk_seqs.push(seq);
        }
    }

    /// Streaming deltas of an open step (everything except the retry
    /// meta-events, which journal even between steps).
    fn handle_stream_chunk(&mut self, stream: ModelEvent) {
        match stream {
            ModelEvent::TextDelta { delta } | ModelEvent::RefusalDelta { delta } => {
                self.text.push_str(&delta);
                self.chunk(payloads::chunks::text_delta(0, &delta));
            }
            ModelEvent::ReasoningDelta { delta } | ModelEvent::ReasoningSummaryDelta { delta } => {
                self.reasoning.push_str(&delta);
                self.chunk(payloads::chunks::reasoning_delta(1, &delta));
            }
            ModelEvent::ToolCallStarted { call_id, name } => {
                let index = 2 + self.next_block_index();
                self.chunk(payloads::chunks::tool_call_delta(
                    index,
                    &call_id,
                    name.as_deref(),
                    "",
                ));
                let call = PendingCall {
                    id: call_id.clone(),
                    name: name.clone().unwrap_or_default(),
                    arguments: Value::Object(Default::default()),
                    index,
                };
                if let Ok(mut shared) = self.shared.lock() {
                    shared.stash(call_id.clone(), call);
                }
            }
            ModelEvent::ToolArgumentsDelta { call_id, delta } => {
                let index = self
                    .shared
                    .lock()
                    .map(|shared| shared.get(&call_id).map(|call| call.index).unwrap_or(2))
                    .unwrap_or(2);
                self.chunk(payloads::chunks::tool_call_delta(
                    index, &call_id, None, &delta,
                ));
            }
            ModelEvent::ToolCallCompleted { call } => {
                self.completed_calls.push(call.clone());
            }
            // 流末 usage 暂存：随 ModelResponded 落进本条 assistant/
            // message（DSH usage 字段）。每个 step 重置——重试的失败
            // 尝试不上报 usage，不能让上一次的值渗进本条消息。
            ModelEvent::Usage(usage) => {
                self.stream_usage = Some(usage.clone());
            }
            ModelEvent::ResponseStarted { .. }
            | ModelEvent::ResponseCompleted { .. }
            | ModelEvent::ProviderEvent { .. } => {}
            ModelEvent::RetryScheduled { .. } | ModelEvent::RetryStarted { .. } => {}
        }
    }

    fn next_block_index(&mut self) -> u64 {
        self.next_block_index += 1;
        self.next_block_index - 1
    }

    /// Compose and append the `assistant/message` for the open step.
    fn assistant_message(&mut self) {
        self.assistant_message_interrupted(false);
    }

    /// [`Self::assistant_message`] 的参数化核（B2）：部分产出定稿时
    /// 由取消路径传入 `interrupted: true`。
    fn assistant_message_interrupted(&mut self, interrupted: bool) {
        let (turn, step) = {
            let shared = self.shared.lock().expect("recorder lock");
            (shared.turn, shared.step)
        };
        let mut content = Vec::new();
        if !self.reasoning.is_empty() {
            content.push(payloads::reasoning_block(&self.reasoning));
        }
        if !self.text.is_empty() {
            content.push(payloads::text_block(&self.text));
        }
        for call in &self.completed_calls {
            content.push(payloads::tool_call_block(
                &call.id,
                &call.name,
                &call.arguments,
            ));
        }
        let sources = std::mem::take(&mut self.chunk_seqs);
        // INV-S6：sampling usage 在 assistant/message 落账点归并（DSH
        // 字节形状不变——只是 usage 数值含桥接调用的 token）。空单元
        // 不落 usage，保持零桥接 run 的 journal 字节与从前一致。
        // FP-01：主循环与 aux 分开记账——主循环走预留对账（usage 到达
        // → reconcile 替换预留；无 usage → 预留兑现），aux 照旧 charge。
        let main_usage = self.stream_usage.take();
        let aux_usage = self.take_aux_usage();
        let usage = merged_usage(main_usage.clone(), aux_usage.clone());
        let mut payload = payloads::assistant_message(
            turn,
            step,
            content,
            &self.provider,
            &self.model,
            usage.as_ref(),
        );
        if let Some(replay) = self.replay_state.take() {
            payload = payloads::with_replay_state(payload, &replay);
        }
        if interrupted {
            // B2：可选信封级字段（DSH types.ts:277），跨工具读取时取消
            // 前缀可辨。settled 路径永不写入（B2 反向腿测试钉住）。
            payload["interrupted"] = serde_json::json!(true);
        }
        let event = NewSessionEvent::new("assistant/message", payload).append(sources);
        self.append_quietly(event);
        // B1/FP-01：落账即累计护栏口径（input+output；缓存命中已在
        // input 内，不重复计），跨 50%/90% 各发一次持久化预警。
        let main_spent = match main_usage {
            Some(usage) => Some(usage.input_tokens.saturating_add(usage.output_tokens)),
            None => {
                // 无主循环 usage：预留兑现（若 run.rs 未预留则无操作——
                // recorder 直驱的测试路径等同旧 charge 语义）。
                if let Some(ledger) = &self.budget.ledger {
                    ledger.commit_pending();
                }
                None
            }
        };
        let aux_spent = aux_usage
            .map(|usage| usage.input_tokens.saturating_add(usage.output_tokens))
            .unwrap_or(0);
        if main_spent.is_some() || aux_spent > 0 {
            for event in self.budget.crossing_events(main_spent, aux_spent) {
                self.append_quietly(event);
            }
        }
    }

    /// B1：装入花费护栏（run_lifecycle 于 run 组装时调用）。
    pub(crate) fn set_run_ledger(&mut self, ledger: std::sync::Arc<crate::model::RunSpendLedger>) {
        self.budget = RunBudgetEvents {
            ledger: Some(ledger),
            warned_half: false,
            warned_most: false,
        };
    }

    fn state(&self) -> (u64, u64) {
        let shared = self.shared.lock().expect("recorder lock");
        (shared.turn, shared.step)
    }

    /// 取空 sampling 记账单元；全零（本步无桥接调用）返回 None，避免
    /// 给无 usage 的消息落一个全零 usage 对象。
    fn take_aux_usage(&mut self) -> Option<crate::model::Usage> {
        let cell = self.aux_usage.as_ref()?;
        let drained = cell
            .lock()
            .ok()
            .map(|mut guard| std::mem::take(&mut *guard))?;
        if drained == crate::model::Usage::default() {
            None
        } else {
            Some(drained)
        }
    }
}

/// B1 花费护栏的预警状态：cap 与已发标志。事件名 `clat/budget`，
/// 信封 `ignorable: true`（DSH 读取器按规跳过——CLAT 自产词表外的
/// 唯一事件类型，四道门见 catalog/admission/replay 注记）。
#[derive(Default)]
struct RunBudgetEvents {
    /// F-1（审计）：累计与硬顶都在共享账本（run.rs 检查点同源——
    /// 预警数字与硬停文案唯一仪表）；这里只留一次性预警的已发标志。
    ledger: Option<std::sync::Arc<crate::model::RunSpendLedger>>,
    warned_half: bool,
    warned_most: bool,
}

impl RunBudgetEvents {
    /// 跨阈值检查：返回本步需要新发的预警事件（50% 文案预告重置语义）。
    /// FP-01：主循环 usage 走对账（`Some(actual)` → reconcile 替换预留；
    /// `None` → 调用方已 commit_pending，这里不重复）；aux 照旧 charge。
    fn crossing_events(&mut self, main_spent: Option<u64>, aux_spent: u64) -> Vec<NewSessionEvent> {
        let Some(ledger) = &self.ledger else {
            return Vec::new();
        };
        if let Some(actual) = main_spent {
            ledger.reconcile(actual);
        }
        if aux_spent > 0 {
            ledger.charge(aux_spent);
        }
        let Some(cap) = ledger.cap else {
            return Vec::new();
        };
        let used = ledger.used();
        let mut events = Vec::new();
        if !self.warned_half && used >= cap / 2 {
            self.warned_half = true;
            events.push(
                NewSessionEvent::new(
                    "clat/budget",
                    serde_json::json!({
                        "kind": "warn",
                        "level": 50,
                        "usedTokens": used,
                        "capTokens": cap,
                    }),
                )
                .log_only(),
            );
        }
        // FIX-1/CA-01：`cap * 9` 在大 cap（用户可自定义到 u64::MAX）下
        // 溢出。精确无溢出等价：cap = 10a + b 时 cap*9/10 == 9a + 9b/10
        // （90a 整除 10，两项各自不越 u64）。
        let most = cap / 10 * 9 + cap % 10 * 9 / 10;
        if !self.warned_most && used >= most {
            self.warned_most = true;
            events.push(
                NewSessionEvent::new(
                    "clat/budget",
                    serde_json::json!({
                        "kind": "warn",
                        "level": 90,
                        "usedTokens": used,
                        "capTokens": cap,
                    }),
                )
                .log_only(),
            );
        }
        events
    }
}

/// 流末 usage + sampling 归并：任一侧缺失时保留另一侧。
fn merged_usage(
    streamed: Option<crate::model::Usage>,
    aux: Option<crate::model::Usage>,
) -> Option<crate::model::Usage> {
    match (streamed, aux) {
        (None, None) => None,
        (Some(usage), None) | (None, Some(usage)) => Some(usage),
        (Some(mut total), Some(aux)) => {
            total.add_assign(&aux);
            Some(total)
        }
    }
}

impl SessionRecorder {
    /// FP-09：journal 变换 + 返回待转发事件（终态扣到 `finish`）。
    /// 兼容面：`EventSink::emit` 调它并丢弃返回——recorder 直驱的
    /// 测试不变；需要断言转发面的测试用本方法/`take_forwarded`。
    pub(crate) fn emit_and_forward(&mut self, event: RunEvent) -> Vec<RunEvent> {
        self.record(event);
        std::mem::take(&mut self.forwarded)
    }

    /// 取走累积的待转发事件（`EventSink::emit` 兼容路径累积于此）。
    pub(crate) fn take_forwarded(&mut self) -> Vec<RunEvent> {
        std::mem::take(&mut self.forwarded)
    }

    pub(crate) fn force_terminal_failure(&mut self, message: impl Into<String>) {
        self.terminal = Some(RunEvent::RunFailed {
            message: message.into(),
        });
    }

    fn record(&mut self, event: RunEvent) {
        match &event {
            // turn/start + user/message are the application's first durable
            // atomic batch, already written before the run started.
            RunEvent::RunStarted { .. } => {}
            RunEvent::ModelRequested { .. } => {
                self.refresh_dynamic_instructions();
                self.close_open_step();
                self.open_step();
            }
            RunEvent::ModelStream { event: stream, .. } => {
                match stream {
                    ModelEvent::RetryScheduled {
                        retry,
                        max_retries,
                        delay_ms,
                        failure,
                    } => {
                        // FP-01：失败 attempt 已烧掉的保守成本兑现（预留
                        // 保留给同请求的下一次 attempt，最终成功 attempt
                        // 由 ModelResponded 的 reconcile 替换）。
                        if let Some(ledger) = &self.budget.ledger {
                            ledger.commit_retry_attempt();
                        }
                        // llm/retry (catalog §2.3): a retryable failure with
                        // its backoff, durable before the wait.
                        let (turn, step) = self.state();
                        let failure = serde_json::json!({
                            "message": failure.message,
                            "code": failure.code,
                            "providerRetryAfterMs": failure.provider_retry_after_ms,
                        });
                        let retry_id = uuid::Uuid::new_v4().to_string();
                        self.pending_retry_id = Some(retry_id.clone());
                        self.append_quietly(
                            NewSessionEvent::new(
                                "llm/retry",
                                payloads::llm_retry(
                                    &retry_id,
                                    turn,
                                    step,
                                    &self.provider,
                                    *retry,
                                    *max_retries,
                                    *delay_ms,
                                    failure,
                                ),
                            )
                            .log_only(),
                        );
                        self.flush_quietly();
                    }
                    ModelEvent::RetryStarted { retry } => {
                        let (turn, step) = self.state();
                        let retry_id = self
                            .pending_retry_id
                            .take()
                            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
                        self.append_quietly(
                            NewSessionEvent::new(
                                "llm/retry-started",
                                payloads::llm_retry_started(&retry_id, turn, step, *retry),
                            )
                            .log_only(),
                        );
                    }
                    _ if self.step_open => {
                        self.handle_stream_chunk(stream.clone());
                    }
                    _ => {}
                }
            }
            RunEvent::ModelResponded {
                provider_replay, ..
            } => {
                self.replay_state = provider_replay
                    .as_ref()
                    .filter(|replay| !replay.is_null())
                    .cloned();
                self.assistant_message();
                self.message_emitted = true;
            }
            RunEvent::ToolRequested { call } => {
                let index = 2 + self.next_block_index();
                let arguments = self
                    .tool_registry
                    .as_ref()
                    .and_then(|registry| registry.get(&call.name))
                    .map_or_else(
                        || call.arguments.clone(),
                        |tool| tool.journal_arguments(&call.arguments),
                    );
                let pending = PendingCall {
                    id: call.id.clone(),
                    name: call.name.clone(),
                    arguments,
                    index,
                };
                if let Ok(mut shared) = self.shared.lock() {
                    shared.stash(call.id.clone(), pending);
                }
            }
            RunEvent::PermissionChecked { .. } => {}
            RunEvent::PermissionDenied { tool, .. } => {
                let denial = self.shared.lock().ok().and_then(|mut shared| {
                    let call_id = shared.unjournaled_by_name(tool)?;
                    let call = shared.take(&call_id)?;
                    Some((call, shared.turn, shared.step))
                });
                if let Some((call, turn, step)) = denial {
                    // Policy-level denial (no approval round-trip): only the
                    // error tool/result lands durably — no tool/call, the
                    // same shape recovery synthesizes (catalog §3).
                    let result_text = format!("permission denied for tool `{tool}`");
                    let event = NewSessionEvent::new(
                        "tool/result",
                        payloads::tool_result(
                            turn,
                            step,
                            &call.id,
                            payloads::tool_result_content(&Value::String(result_text)),
                            true,
                        ),
                    )
                    .append(Vec::new());
                    self.append_quietly(event);
                    self.flush_quietly();
                }
            }
            RunEvent::ToolStarted { call_id, tool } => {
                let (already, call, turn, step) = match self.shared.lock() {
                    Ok(shared) => {
                        let already = shared.is_journaled(call_id);
                        let call = shared.get(call_id).cloned();
                        (already, call, shared.turn, shared.step)
                    }
                    Err(_) => {
                        self.forwarded.push(event);
                        return;
                    }
                };
                if !already {
                    let arguments = call
                        .as_ref()
                        .map(|pending| pending.arguments.clone())
                        .unwrap_or_else(|| Value::Object(Default::default()));
                    let event = NewSessionEvent::new(
                        "tool/call",
                        payloads::tool_call(turn, step, call_id, tool, &arguments),
                    )
                    .log_only();
                    self.append_quietly(event);
                    // Pre-execution durability barrier: a crash between
                    // `tool/call` and `tool/result` must synthesize an
                    // outcome-unknown result (recovery.rs).
                    self.flush_quietly();
                }
            }
            RunEvent::ToolFinished { result } => {
                let (turn, step) = self.state();
                let output = self
                    .tool_registry
                    .as_ref()
                    .and_then(|registry| registry.get(&result.tool_name))
                    .map_or_else(
                        || result.output.clone(),
                        |tool| tool.journal_output(&result.output),
                    );
                let event = NewSessionEvent::new(
                    "tool/result",
                    payloads::tool_result(
                        turn,
                        step,
                        &result.call_id,
                        payloads::tool_result_content(&output),
                        result.is_error,
                    ),
                )
                .append(Vec::new());
                self.append_quietly(event);
            }
            RunEvent::SteeringApplied {
                message,
                client_message_id,
            } => {
                // In-run steering claimed into the transcript (DSH: plain
                // user/message, no catalog extension). Durability barrier:
                // durable before the consuming model request, and between
                // steps — close the open step so the message lands after the
                // previous step's events and before the next step/start.
                // MM-1A：steering admission 只放行纯文本，`plain_text`
                // 无损；客户端幂等键与 digest 随同一条事件落盘（commit
                // point 证据）。
                self.close_open_step();
                // digest 只随客户端幂等键落盘——无键的合成消息没有
                // "同 key 不同 payload" 的判别对象。
                let digest = client_message_id.as_ref().map(|_| message.request_digest());
                let payload = payloads::admitted_user_message(
                    &uuid::Uuid::new_v4().to_string(),
                    &message.plain_text(),
                    &[],
                    client_message_id.as_deref(),
                    digest.as_deref(),
                );
                let event = NewSessionEvent::new("user/message", payload).append(Vec::new());
                self.append_quietly(event);
                self.flush_quietly();
            }
            RunEvent::RunCompleted { .. } | RunEvent::RunCancelled { .. } => {
                self.terminal = Some(event);
                return;
            }
            RunEvent::RunFailed { message } => {
                // Model/loop failures also fail the journal close below;
                // keep the first journal error authoritative.
                let _ = message;
                self.terminal = Some(event);
                return;
            }
        }
        self.forwarded.push(event);
    }
}

impl EventSink for SessionRecorder {
    fn emit(&mut self, event: RunEvent) {
        self.emit_and_forward(event);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::run_journal::SeqRange;
    use crate::tool::ToolResult;
    use serde_json::json;
    use std::sync::Mutex as StdMutex;

    /// A journal that records appends and can be inspected; `fail_flush_at`
    /// makes the Nth flush fail (1-based) to exercise the terminal barrier.
    struct RecordingJournal {
        events: StdMutex<Vec<NewSessionEvent>>,
        flushes: StdMutex<usize>,
        fail_flush_at: Option<usize>,
        /// Fail every append: models a journal that broke mid-run while the
        /// final flush still succeeds (audit P1-09, second pass).
        fail_appends: bool,
    }

    impl RecordingJournal {
        fn new() -> Arc<Self> {
            Arc::new(Self {
                events: StdMutex::new(Vec::new()),
                flushes: StdMutex::new(0),
                fail_flush_at: None,
                fail_appends: false,
            })
        }

        fn with_failing_flush(fail_flush_at: usize) -> Arc<Self> {
            Arc::new(Self {
                events: StdMutex::new(Vec::new()),
                flushes: StdMutex::new(0),
                fail_flush_at: Some(fail_flush_at),
                fail_appends: false,
            })
        }

        fn with_failing_appends() -> Arc<Self> {
            Arc::new(Self {
                events: StdMutex::new(Vec::new()),
                flushes: StdMutex::new(0),
                fail_flush_at: None,
                fail_appends: true,
            })
        }

        fn events(&self) -> Vec<(String, Value)> {
            self.events
                .lock()
                .unwrap()
                .iter()
                .map(|event| (event.event_type.clone(), event.data.clone()))
                .collect()
        }
    }

    impl RunJournal for RecordingJournal {
        fn append_atomic(&self, events: &[NewSessionEvent]) -> Result<SeqRange, String> {
            if self.fail_appends {
                return Err("injected append failure".into());
            }
            let mut inner = self.events.lock().unwrap();
            for event in events {
                inner.push(event.clone());
            }
            Ok(SeqRange {
                start: 0,
                end_inclusive: 0,
            })
        }
        fn flush(&self) -> Result<(), String> {
            let mut flushes = self.flushes.lock().unwrap();
            *flushes += 1;
            if Some(*flushes) == self.fail_flush_at {
                return Err("injected flush failure".into());
            }
            Ok(())
        }
    }

    fn header_data() -> RequestHeaderData {
        RequestHeaderData {
            header: json!({
                "config": { "provider": "prov", "model": "mdl", "thinking": "high" },
                "system": "you are clat",
                "tools": [{ "name": "read_file" }],
            }),
            base_system: "you are clat".into(),
            dynamic_instructions: None,
            tool_registry: None,
        }
    }

    fn recorder() -> (
        SessionRecorder,
        Arc<RecordingJournal>,
        Arc<StdMutex<Vec<RunEvent>>>,
    ) {
        let journal = RecordingJournal::new();
        // FP-09 起 recorder 不再持有 frontend sink；seen 仅保持 helper
        // 签名兼容（恒空）。需要断言转发面的测试改用
        // emit_and_forward / finish 的返回值。
        let seen = Arc::new(StdMutex::new(Vec::new()));
        let recorder = SessionRecorder::new(
            Arc::clone(&journal) as Arc<dyn RunJournal>,
            header_data(),
            "prov",
            "mdl",
            1,
            Some("initial"),
        );
        (recorder, journal, seen)
    }

    /// S2：steering claim 落 mid-turn `user/message`——surface Append、
    /// 夹在上一条 assistant/message 与下一条之间；写侧先于消费它的下一
    /// step（durability barrier）。
    #[test]
    fn steering_applied_journals_a_mid_turn_user_message() {
        let (mut recorder, journal, _seen) = recorder();
        recorder.emit(RunEvent::ModelRequested {
            turn: 1,
            provider: "p".into(),
            model: "m".into(),
        });
        recorder.emit(RunEvent::ModelStream {
            turn: 1,
            event: ModelEvent::TextDelta {
                delta: "first answer".into(),
            },
        });
        recorder.emit(RunEvent::ModelResponded {
            turn: 1,
            outcome: crate::event::ModelOutcome {
                has_text: true,
                tool_calls: 0,
            },
            finish_reason: crate::model::FinishReason::Completed,
            provider_replay: None,
        });
        recorder.emit(RunEvent::SteeringApplied {
            message: crate::message::MessageContent::text("also run the tests"),
            client_message_id: None,
        });
        recorder.emit(RunEvent::ModelRequested {
            turn: 2,
            provider: "p".into(),
            model: "m".into(),
        });
        recorder.emit(RunEvent::ModelStream {
            turn: 2,
            event: ModelEvent::TextDelta {
                delta: "steering handled".into(),
            },
        });
        recorder.emit(RunEvent::ModelResponded {
            turn: 2,
            outcome: crate::event::ModelOutcome {
                has_text: true,
                tool_calls: 0,
            },
            finish_reason: crate::model::FinishReason::Completed,
            provider_replay: None,
        });
        assert!(recorder.finish(TurnEndReason::Completed).0.is_none());

        let events = journal.events();
        let types: Vec<&str> = events.iter().map(|(kind, _)| kind.as_str()).collect();
        let steering_index = types
            .iter()
            .position(|kind| *kind == "user/message")
            .expect("steering user/message journaled");
        // 夹在两条 assistant/message 之间（第一条 assistant 之前没有任何
        // user/message——recorder 不写首条，那是 prepare_run 的职责）。
        let first_assistant = types
            .iter()
            .position(|kind| *kind == "assistant/message")
            .expect("first assistant");
        let second_assistant = types
            .iter()
            .rposition(|kind| *kind == "assistant/message")
            .expect("second assistant");
        assert!(
            first_assistant < steering_index && steering_index < second_assistant,
            "steering user/message must sit between the two assistant messages: {types:?}"
        );
        // DSH 兼容载荷：role=user、source.kind=user、正文原样。
        let (_, payload) = &events[steering_index];
        assert_eq!(payload["role"], "user");
        assert_eq!(payload["source"]["kind"], "user");
        assert_eq!(payload["content"][0]["text"], "also run the tests");
    }

    #[test]
    fn streamed_usage_lands_on_its_own_step_and_resets_between_steps() {
        // DSH assistant/message.usage（TokenUsage 字段名）：流末 usage 落
        // 在当步的 assistant 消息上；每个 step 重置——上一步的值不得
        // 渗进未上报的下一步（重启还原的状态栏依赖此形状）。
        let (mut recorder, journal, _seen) = recorder();
        recorder.emit(RunEvent::ModelRequested {
            turn: 1,
            provider: "p".into(),
            model: "m".into(),
        });
        recorder.emit(RunEvent::ModelStream {
            turn: 1,
            event: ModelEvent::TextDelta {
                delta: "with usage".into(),
            },
        });
        recorder.emit(RunEvent::ModelStream {
            turn: 1,
            event: ModelEvent::Usage(crate::model::Usage {
                input_tokens: 10,
                output_tokens: 2,
                cached_input_tokens: Some(4),
                reasoning_tokens: Some(1),
            }),
        });
        recorder.emit(RunEvent::ModelResponded {
            turn: 1,
            outcome: crate::event::ModelOutcome {
                has_text: true,
                tool_calls: 0,
            },
            finish_reason: crate::model::FinishReason::Completed,
            provider_replay: None,
        });
        // 第二步不上报 usage：消息不得携带第一步的旧值。
        recorder.emit(RunEvent::ModelRequested {
            turn: 1,
            provider: "p".into(),
            model: "m".into(),
        });
        recorder.emit(RunEvent::ModelStream {
            turn: 1,
            event: ModelEvent::TextDelta {
                delta: "no usage".into(),
            },
        });
        recorder.emit(RunEvent::ModelResponded {
            turn: 1,
            outcome: crate::event::ModelOutcome {
                has_text: true,
                tool_calls: 0,
            },
            finish_reason: crate::model::FinishReason::Completed,
            provider_replay: None,
        });
        assert!(recorder.finish(TurnEndReason::Completed).0.is_none());

        let events = journal.events();
        let messages: Vec<&serde_json::Value> = events
            .iter()
            .filter(|(kind, _)| kind == "assistant/message")
            .map(|(_, payload)| payload)
            .collect();
        assert_eq!(messages.len(), 2);
        let usage = &messages[0]["usage"];
        assert_eq!(usage["inputTokens"], 10);
        assert_eq!(usage["outputTokens"], 2);
        assert_eq!(usage["cacheReadTokens"], 4);
        assert_eq!(usage["reasoningTokens"], 1);
        assert!(
            messages[1].get("usage").is_none(),
            "an unreported step must not inherit the previous step's usage"
        );
    }

    /// INV-S6（sampling 记账，2026-08-21）：桥接 usage 经 aux cell 在
    /// assistant/message 落账点归并进当步 usage——journal 侧的唯一记
    /// 账点，live/replay 平价靠它；落账即清空 cell（余量归 run worker
    /// 的终态补充路径）。空单元不落 usage 对象：零桥接 run 的 journal
    /// 字节与从前一致。
    #[test]
    fn aux_sampling_usage_merges_into_the_step_message_and_empty_cells_write_nothing() {
        let (mut recorder, journal, _seen) = recorder();
        let cell = Arc::new(Mutex::new(crate::model::Usage::default()));
        recorder.attach_aux_usage(Arc::clone(&cell));

        let step = |recorder: &mut SessionRecorder, delta: &str| {
            recorder.emit(RunEvent::ModelRequested {
                turn: 1,
                provider: "p".into(),
                model: "m".into(),
            });
            recorder.emit(RunEvent::ModelStream {
                turn: 1,
                event: ModelEvent::TextDelta {
                    delta: delta.into(),
                },
            });
        };
        let responded = |recorder: &mut SessionRecorder| {
            recorder.emit(RunEvent::ModelResponded {
                turn: 1,
                outcome: crate::event::ModelOutcome {
                    has_text: true,
                    tool_calls: 0,
                },
                finish_reason: crate::model::FinishReason::Completed,
                provider_replay: None,
            });
        };

        // 第一步：模型 usage 10/2 + 桥接 usage 3/1 → 落 13/3。
        step(&mut recorder, "a");
        recorder.emit(RunEvent::ModelStream {
            turn: 1,
            event: ModelEvent::Usage(crate::model::Usage {
                input_tokens: 10,
                output_tokens: 2,
                ..crate::model::Usage::default()
            }),
        });
        cell.lock().unwrap().add_assign(&crate::model::Usage {
            input_tokens: 3,
            output_tokens: 1,
            ..crate::model::Usage::default()
        });
        responded(&mut recorder);

        // 第二步：模型不上报 usage，但桥接有 5/0 → 仍落 usage 5/0
        //（取消余量走 worker 路径，这里是常规归并）。
        step(&mut recorder, "b");
        cell.lock().unwrap().add_assign(&crate::model::Usage {
            input_tokens: 5,
            ..crate::model::Usage::default()
        });
        responded(&mut recorder);

        // 第三步：模型不上报、桥接为零 → 无 usage 字段（字节不变）。
        step(&mut recorder, "c");
        responded(&mut recorder);
        assert!(recorder.finish(TurnEndReason::Completed).0.is_none());

        let events = journal.events();
        let messages: Vec<&serde_json::Value> = events
            .iter()
            .filter(|(kind, _)| kind == "assistant/message")
            .map(|(_, payload)| payload)
            .collect();
        assert_eq!(messages.len(), 3);
        assert_eq!(messages[0]["usage"]["inputTokens"], 13);
        assert_eq!(messages[0]["usage"]["outputTokens"], 3);
        assert_eq!(messages[1]["usage"]["inputTokens"], 5);
        assert_eq!(messages[1]["usage"]["outputTokens"], 0);
        assert!(
            messages[2].get("usage").is_none(),
            "an empty aux cell must not materialize a zero usage object"
        );
        // 落账清空 cell：worker 的终态余量补充只看取消/失败尾巴。
        assert_eq!(
            *cell.lock().unwrap(),
            crate::model::Usage::default(),
            "landed usage must drain the cell"
        );
    }

    #[test]
    fn text_turn_produces_dsh_event_sequence() {
        let (mut recorder, journal, _seen) = recorder();
        recorder.emit(RunEvent::ModelRequested {
            turn: 1,
            provider: "p".into(),
            model: "m".into(),
        });
        recorder.emit(RunEvent::ModelStream {
            turn: 1,
            event: ModelEvent::TextDelta { delta: "He".into() },
        });
        recorder.emit(RunEvent::ModelStream {
            turn: 1,
            event: ModelEvent::TextDelta { delta: "y".into() },
        });
        recorder.emit(RunEvent::ModelResponded {
            turn: 1,
            outcome: crate::event::ModelOutcome {
                has_text: true,
                tool_calls: 0,
            },
            finish_reason: crate::model::FinishReason::Completed,
            provider_replay: None,
        });
        recorder.emit(RunEvent::RunCompleted {
            output: "Hey".into(),
            turns: 1,
            usage: Default::default(),
        });
        let (error, published) = recorder.finish(TurnEndReason::Completed);
        assert!(error.is_none());

        let types: Vec<String> = journal.events().into_iter().map(|(kind, _)| kind).collect();
        let types: Vec<&str> = types.iter().map(String::as_str).collect();
        assert_eq!(
            types,
            vec![
                "step/start",
                "request/header",
                "assistant/chunk",
                "assistant/chunk",
                "assistant/message",
                "step/end",
                "turn/end",
            ]
        );
        let events = journal.events();
        // 首个 step 是 0（pinned catalog），不是 1（审计 P1-14）。
        assert_eq!(events[0].1["step"], json!(0));
        // request/header 记录模型实际看到的配置/system/tools。
        assert_eq!(events[1].1["reason"], json!("initial"));
        assert_eq!(events[1].1["header"]["config"]["model"], json!("mdl"));
        assert_eq!(events[1].1["header"]["system"], json!("you are clat"));
        assert_eq!(
            events[1].1["header"]["tools"][0]["name"],
            json!("read_file")
        );
        let message = &events[4].1;
        assert_eq!(message["message"]["content"][0]["text"], "Hey");
        assert_eq!(message["message"]["source"]["kind"], "model");
        assert_eq!(message["step"], json!(0));
        // The terminal event reached the UI only after the flush in finish.
        let seen = &published;
        assert!(
            seen.iter()
                .any(|event| matches!(event, RunEvent::RunCompleted { .. }))
        );
    }

    #[test]
    fn provider_replay_state_persists_into_the_assistant_message_source() {
        let (mut recorder, journal, _seen) = recorder();
        recorder.emit(RunEvent::ModelRequested {
            turn: 1,
            provider: "p".into(),
            model: "m".into(),
        });
        recorder.emit(RunEvent::ModelResponded {
            turn: 1,
            outcome: crate::event::ModelOutcome {
                has_text: true,
                tool_calls: 0,
            },
            finish_reason: crate::model::FinishReason::Completed,
            provider_replay: Some(json!({ "reasoning": ["item-1", "item-2"] })),
        });
        let _ = recorder.finish(TurnEndReason::Completed);
        let events = journal.events();
        let message = events
            .into_iter()
            .find(|(kind, _)| kind == "assistant/message")
            .expect("assistant message");
        // 修复前：replayState 从不落盘，冷恢复后 OpenAI Responses 的
        // reasoning items 丢失（审计 P1-11 的失败序列）。
        assert_eq!(
            message.1["message"]["source"]["replayState"]["reasoning"],
            json!(["item-1", "item-2"])
        );
    }

    #[test]
    fn terminal_flush_failure_publishes_only_run_failed() {
        let journal = RecordingJournal::with_failing_flush(1);
        let mut recorder = SessionRecorder::new(
            Arc::clone(&journal) as Arc<dyn RunJournal>,
            header_data(),
            "prov",
            "mdl",
            1,
            Some("initial"),
        );
        recorder.emit(RunEvent::ModelRequested {
            turn: 1,
            provider: "p".into(),
            model: "m".into(),
        });
        recorder.emit(RunEvent::RunCompleted {
            output: "done".into(),
            turns: 1,
            usage: Default::default(),
        });
        let (error, published) = recorder.finish(TurnEndReason::Completed);
        let error = error.expect("error");
        assert!(error.contains("injected flush failure"));
        // 修复前：UI 事件流收到 RunCompleted，而 completion 通道携带失败
        // ——两个公开通道互相矛盾（审计 P1-09 的失败序列）。FP-09 起
        // 断言面 = finish 返回的待发布终态（锁外转发由调用方执行）。
        let seen = &published;
        assert!(
            !seen
                .iter()
                .any(|event| matches!(event, RunEvent::RunCompleted { .. })),
            "no success terminal may be published after a failed final flush"
        );
        assert!(
            seen.iter().any(|event| matches!(
                event,
                RunEvent::RunFailed { message } if message.contains("session journal failed")
            )),
            "the journal failure must reach the UI as RunFailed"
        );
    }

    #[test]
    fn mid_run_append_failure_publishes_only_run_failed_even_if_final_flush_succeeds() {
        let journal = RecordingJournal::with_failing_appends();
        let mut recorder = SessionRecorder::new(
            Arc::clone(&journal) as Arc<dyn RunJournal>,
            header_data(),
            "prov",
            "mdl",
            1,
            Some("initial"),
        );
        recorder.emit(RunEvent::ModelRequested {
            turn: 1,
            provider: "p".into(),
            model: "m".into(),
        });
        recorder.emit(RunEvent::RunCompleted {
            output: "done".into(),
            turns: 1,
            usage: Default::default(),
        });
        // 修复前：只检查最终 flush——中途 append 已失败而最终 flush 成功
        // 时仍发布 RunCompleted，completion 通道却报告失败（复审第二轮
        // 发现的半修复）。
        let (error, published) = recorder.finish(TurnEndReason::Completed);
        let error = error.expect("error");
        assert!(error.contains("injected append failure"));
        assert!(
            !published
                .iter()
                .any(|event| matches!(event, RunEvent::RunCompleted { .. })),
            "no success terminal may survive a mid-run journal failure"
        );
        assert!(published.iter().any(|event| matches!(
            event,
            RunEvent::RunFailed { message } if message.contains("session journal failed")
        )));
    }

    #[test]
    fn retry_events_journal_llm_retry_family() {
        let (mut recorder, journal, _seen) = recorder();
        recorder.emit(RunEvent::ModelRequested {
            turn: 1,
            provider: "p".into(),
            model: "m".into(),
        });
        recorder.emit(RunEvent::ModelStream {
            turn: 1,
            event: ModelEvent::RetryScheduled {
                retry: 1,
                max_retries: 3,
                delay_ms: 1000,
                failure: crate::model::RetryFailure {
                    message: "boom".into(),
                    code: "transport".into(),
                    status: None,
                    provider_retry_after_ms: None,
                },
            },
        });
        recorder.emit(RunEvent::ModelStream {
            turn: 1,
            event: ModelEvent::RetryStarted { retry: 1 },
        });
        let _ = recorder.finish(TurnEndReason::Completed);
        let events = journal.events();
        let retry = events
            .iter()
            .find(|(kind, _)| kind == "llm/retry")
            .expect("llm/retry journaled");
        assert_eq!(retry.1["provider"], json!("prov"));
        assert_eq!(retry.1["retry"], json!(1));
        assert_eq!(retry.1["delayMs"], json!(1000));
        assert_eq!(retry.1["failure"]["code"], json!("transport"));
        assert_eq!(retry.1["policyKey"], json!("clat-default"));
        let started = events
            .iter()
            .find(|(kind, _)| kind == "llm/retry-started")
            .expect("llm/retry-started journaled");
        assert_eq!(
            started.1["retryId"], retry.1["retryId"],
            "the scheduled and started facts describe one retry"
        );
    }

    #[test]
    fn tool_calls_pair_results_and_barrier_flushes_before_execution() {
        let (mut recorder, journal, _seen) = recorder();
        recorder.emit(RunEvent::ModelRequested {
            turn: 1,
            provider: "p".into(),
            model: "m".into(),
        });
        recorder.emit(RunEvent::ToolRequested {
            call: crate::tool::ToolCall {
                id: "call-1".into(),
                name: "read_file".into(),
                arguments: json!({"path": "x"}),
            },
        });
        recorder.emit(RunEvent::ToolStarted {
            call_id: "call-1".into(),
            tool: "read_file".into(),
        });
        recorder.emit(RunEvent::ToolFinished {
            result: ToolResult {
                blocks: Vec::new(),
                call_id: "call-1".into(),
                tool_name: "read_file".into(),
                output: json!("body"),
                is_error: false,
            },
        });
        let _ = recorder.finish(TurnEndReason::Completed);

        let events = journal.events();
        let types: Vec<&str> = events.iter().map(|(kind, _)| kind.as_str()).collect();
        assert!(types.contains(&"tool/call"));
        assert!(types.contains(&"tool/result"));
        let result = events
            .iter()
            .find(|(kind, _)| kind == "tool/result")
            .unwrap();
        assert_eq!(result.1["message"]["content"][0]["toolCallId"], "call-1");
        assert_eq!(
            result.1["message"]["content"][0]["content"][0]["text"],
            "body"
        );
    }

    #[test]
    fn approver_barrier_writes_asked_then_decided_with_call() {
        let (mut recorder, journal, _seen) = recorder();
        recorder.emit(RunEvent::ToolRequested {
            call: crate::tool::ToolCall {
                id: "call-9".into(),
                name: "write_file".into(),
                arguments: json!({"path": "y"}),
            },
        });
        let approver = recorder.approver(Arc::new(
            |request: PermissionRequest, _cancel: &crate::model::CancelToken| {
                assert_eq!(request.call_id, "call-9");
                PermissionDecision::Allow
            },
        ));
        let decision = approver.decide(
            PermissionRequest {
                tool: "write_file".into(),
                effect: crate::tool::ToolEffect::Write,
                reason: "side effects".into(),
                arguments: json!({"path": "y"}),
                call_id: "call-9".into(),
            },
            &crate::model::CancelToken::new(),
        );
        assert!(matches!(decision, PermissionDecision::Allow));
        let events = journal.events();
        let types: Vec<&str> = events.iter().map(|(kind, _)| kind.as_str()).collect();
        assert_eq!(
            types,
            vec!["approval/asked", "approval/decided", "tool/call"]
        );
        let decided = &events[1].1;
        assert_eq!(decided["outcome"], "allowed-once");
        let call = &events[2].1;
        assert_eq!(call["callId"], "call-9");
        assert_eq!(call["arguments"], "{\"path\":\"y\"}");
    }

    #[test]
    fn approver_deny_writes_decided_rejected_and_error_result_without_call() {
        let (mut recorder, journal, _seen) = recorder();
        recorder.emit(RunEvent::ToolRequested {
            call: crate::tool::ToolCall {
                id: "call-d".into(),
                name: "write_file".into(),
                arguments: json!({"path": "z"}),
            },
        });
        let approver = recorder.approver(Arc::new(
            |_: PermissionRequest, _cancel: &crate::model::CancelToken| PermissionDecision::Deny {
                reason: "no".into(),
            },
        ));
        let decision = approver.decide(
            PermissionRequest {
                tool: "write_file".into(),
                effect: crate::tool::ToolEffect::Write,
                reason: "side effects".into(),
                arguments: json!({}),
                call_id: "call-d".into(),
            },
            &crate::model::CancelToken::new(),
        );
        assert!(matches!(decision, PermissionDecision::Deny { .. }));
        let events = journal.events();
        let types: Vec<&str> = events.iter().map(|(kind, _)| kind.as_str()).collect();
        assert_eq!(
            types,
            vec!["approval/asked", "approval/decided", "tool/result"],
            "deny path journals no tool/call"
        );
        assert_eq!(events[1].1["outcome"], "rejected");
        assert_eq!(events[2].1["message"]["content"][0]["isError"], json!(true));
    }

    #[test]
    fn fail_closed_approver_maps_to_unavailable() {
        let (mut recorder, journal, _seen) = recorder();
        recorder.emit(RunEvent::ToolRequested {
            call: crate::tool::ToolCall {
                id: "call-u".into(),
                name: "write_file".into(),
                arguments: json!({}),
            },
        });
        let approver = recorder.approver(Arc::new(
            |_: PermissionRequest, _cancel: &crate::model::CancelToken| {
                PermissionDecision::Unavailable {
                    reason: "non-interactive".into(),
                }
            },
        ));
        let decision = approver.decide(
            PermissionRequest {
                tool: "write_file".into(),
                effect: crate::tool::ToolEffect::Write,
                reason: "side effects".into(),
                arguments: json!({}),
                call_id: "call-u".into(),
            },
            &crate::model::CancelToken::new(),
        );
        assert!(matches!(decision, PermissionDecision::Unavailable { .. }));
        let events = journal.events();
        // 修复前：统一写 rejected（审计 P1-14 的失败序列）。
        assert_eq!(events[1].1["outcome"], "unavailable");
    }

    #[test]
    fn policy_denial_journals_only_an_error_result() {
        let (mut recorder, journal, _seen) = recorder();
        recorder.emit(RunEvent::ModelRequested {
            turn: 1,
            provider: "p".into(),
            model: "m".into(),
        });
        recorder.emit(RunEvent::ToolRequested {
            call: crate::tool::ToolCall {
                id: "call-p".into(),
                name: "bash".into(),
                arguments: json!({"cmd": "rm -rf /"}),
            },
        });
        recorder.emit(RunEvent::PermissionDenied {
            tool: "bash".into(),
            reason: "destructive".into(),
        });
        let _ = recorder.finish(TurnEndReason::Completed);
        let events = journal.events();
        let types: Vec<&str> = events.iter().map(|(kind, _)| kind.as_str()).collect();
        // 修复前：这里写入 tool/call + tool/result（审计 P1-14：计划 deny
        // 路径不产生 tool/call）。
        assert_eq!(
            types,
            vec![
                "step/start",
                "request/header",
                "tool/result",
                "step/end",
                "turn/end",
            ]
        );
        let result = events
            .iter()
            .find(|(kind, _)| kind == "tool/result")
            .unwrap();
        assert_eq!(result.1["message"]["content"][0]["isError"], json!(true));
    }

    /// B1：50%/90% 预警各恰好一次、`clat/budget` 信封 ignorable、跨步
    /// 累计不重复发。判别：删除 crossing_events 调用即无事件而红。
    #[test]
    fn budget_warnings_fire_once_per_threshold_and_are_ignorable() {
        let (mut recorder, journal, _seen) = recorder();
        recorder.set_run_ledger(std::sync::Arc::new(crate::model::RunSpendLedger::new(
            Some(1000),
        )));
        let respond_with_usage = |input: u64, output: u64, recorder: &mut SessionRecorder| {
            recorder.emit(RunEvent::ModelRequested {
                turn: 1,
                provider: "p".into(),
                model: "m".into(),
            });
            recorder.emit(RunEvent::ModelStream {
                turn: 1,
                event: ModelEvent::Usage(crate::model::Usage {
                    input_tokens: input,
                    output_tokens: output,
                    ..crate::model::Usage::default()
                }),
            });
            recorder.emit(RunEvent::ModelResponded {
                turn: 1,
                outcome: crate::event::ModelOutcome {
                    has_text: true,
                    tool_calls: 0,
                },
                finish_reason: crate::model::FinishReason::Completed,
                provider_replay: None,
            });
        };
        // 步 1：600/1000 = 60% → 只发 50% 预警。
        respond_with_usage(500, 100, &mut recorder);
        // 步 2：再 400 → 100% → 只发 90% 预警。
        respond_with_usage(350, 50, &mut recorder);
        // 步 3：继续超额 → 无新事件（各阈值恰好一次）。
        respond_with_usage(200, 100, &mut recorder);
        let _ = recorder.finish(TurnEndReason::Completed);

        let budget_events: Vec<_> = journal
            .events()
            .iter()
            .filter(|(event_type, _)| event_type == "clat/budget")
            .cloned()
            .collect();
        assert_eq!(budget_events.len(), 2, "exactly one warn per threshold");
        assert_eq!(budget_events[0].1["level"], serde_json::json!(50));
        assert_eq!(budget_events[1].1["level"], serde_json::json!(90));
        for (_, data) in &budget_events {
            assert_eq!(data["kind"], serde_json::json!("warn"));
        }
        // ignorable 信封由 RecordingJournal 不记录——单独钉住构造形状。
        let probe = NewSessionEvent::new("clat/budget", serde_json::json!({})).log_only();
        assert_eq!(probe.ignorable, Some(true));
        assert_eq!(budget_events[1].1["usedTokens"], serde_json::json!(1000));
    }

    /// FIX-1/CA-01（2026-08-24 审计，pre-fix 红）：对端回 u64::MAX usage
    /// 必须单调落账（触顶）并保持硬停 armed。pre-fix：reconcile 经
    /// `as i64` 记成 -1、`used()` 裁回 0 → 预警不发、exceeds_cap 失效
    /// → 红。
    #[test]
    fn hostile_usage_keeps_the_hard_stop_armed() {
        let (mut recorder, journal, _seen) = recorder();
        let ledger = std::sync::Arc::new(crate::model::RunSpendLedger::new(Some(
            crate::model::RUN_TOKEN_BUDGET_DEFAULT,
        )));
        recorder.set_run_ledger(std::sync::Arc::clone(&ledger));
        recorder.emit(RunEvent::ModelRequested {
            turn: 1,
            provider: "p".into(),
            model: "m".into(),
        });
        recorder.emit(RunEvent::ModelStream {
            turn: 1,
            event: ModelEvent::Usage(crate::model::Usage {
                input_tokens: u64::MAX,
                output_tokens: 0,
                ..crate::model::Usage::default()
            }),
        });
        recorder.emit(RunEvent::ModelResponded {
            turn: 1,
            outcome: crate::event::ModelOutcome {
                has_text: true,
                tool_calls: 0,
            },
            finish_reason: crate::model::FinishReason::Completed,
            provider_replay: None,
        });
        let _ = recorder.finish(TurnEndReason::Completed);

        assert!(
            ledger.exceeds_cap(),
            "a hostile usage report must trip the hard stop, used: {}",
            ledger.used()
        );
        let budget_events: Vec<_> = journal
            .events()
            .iter()
            .filter(|(event_type, _)| event_type == "clat/budget")
            .cloned()
            .collect();
        assert!(
            budget_events
                .iter()
                .any(|(_, data)| data["usedTokens"] == serde_json::json!(u64::MAX)),
            "the warnings must carry the saturated used value: {budget_events:?}"
        );
    }

    /// FIX-1/CA-01（pre-fix 红）：`cap * 9 / 10` 在用户自定义大 cap
    /// （可到 u64::MAX）下溢出 panic。修复后阈值判定无 panic 且语义
    /// 正确：小用量远低于 50% 不预警；巨量落账时 50%/90% 各恰好一次。
    #[test]
    fn budget_thresholds_handle_an_exhausting_cap() {
        let (mut recorder, journal, _seen) = recorder();
        let ledger = std::sync::Arc::new(crate::model::RunSpendLedger::new(Some(u64::MAX)));
        recorder.set_run_ledger(std::sync::Arc::clone(&ledger));
        let respond_with_usage = |input: u64, output: u64, recorder: &mut SessionRecorder| {
            recorder.emit(RunEvent::ModelRequested {
                turn: 1,
                provider: "p".into(),
                model: "m".into(),
            });
            recorder.emit(RunEvent::ModelStream {
                turn: 1,
                event: ModelEvent::Usage(crate::model::Usage {
                    input_tokens: input,
                    output_tokens: output,
                    ..crate::model::Usage::default()
                }),
            });
            recorder.emit(RunEvent::ModelResponded {
                turn: 1,
                outcome: crate::event::ModelOutcome {
                    has_text: true,
                    tool_calls: 0,
                },
                finish_reason: crate::model::FinishReason::Completed,
                provider_replay: None,
            });
        };
        // 小用量：任何非零已耗都远低于 u64::MAX 的 50% → 零预警。
        respond_with_usage(10, 5, &mut recorder);
        assert!(
            !journal
                .events()
                .iter()
                .any(|(event_type, _)| event_type == "clat/budget"),
            "small usage must not warn against an exhausting cap"
        );
        // 巨量：50%/90% 同一步各恰好一次，无 panic。
        respond_with_usage(u64::MAX, 0, &mut recorder);
        let _ = recorder.finish(TurnEndReason::Completed);
        let levels: Vec<_> = journal
            .events()
            .iter()
            .filter(|(event_type, _)| event_type == "clat/budget")
            .map(|(_, data)| data["level"].clone())
            .collect();
        assert_eq!(levels, vec![serde_json::json!(50), serde_json::json!(90)]);
    }

    /// FP-01：retry 的每次 attempt 都计入同一 run 预算——两次
    /// RetryScheduled 各兑现一次预留，最终成功 attempt 的实际 usage
    /// 替换（而非叠加）预留。总账 = 2×预留 + 实际。
    #[test]
    fn budget_counts_failed_retry_attempts_and_reconciles_the_last() {
        let (mut recorder, _journal, _seen) = recorder();
        let ledger = std::sync::Arc::new(crate::model::RunSpendLedger::new(None));
        recorder.set_run_ledger(std::sync::Arc::clone(&ledger));
        // run.rs 的请求前预留（FP-01）。
        ledger.reserve(100);

        recorder.emit(RunEvent::ModelRequested {
            turn: 1,
            provider: "p".into(),
            model: "m".into(),
        });
        for _ in 0..2 {
            recorder.emit(RunEvent::ModelStream {
                turn: 1,
                event: ModelEvent::RetryScheduled {
                    retry: 1,
                    max_retries: 3,
                    delay_ms: 1,
                    failure: crate::model::RetryFailure {
                        message: "transport glitch".into(),
                        code: "E".into(),
                        status: None,
                        provider_retry_after_ms: None,
                    },
                },
            });
        }
        recorder.emit(RunEvent::ModelStream {
            turn: 1,
            event: ModelEvent::Usage(crate::model::Usage {
                input_tokens: 7,
                output_tokens: 3,
                ..crate::model::Usage::default()
            }),
        });
        recorder.emit(RunEvent::ModelResponded {
            turn: 1,
            outcome: crate::event::ModelOutcome {
                has_text: true,
                tool_calls: 0,
            },
            finish_reason: crate::model::FinishReason::Completed,
            provider_replay: None,
        });
        assert_eq!(
            ledger.committed(),
            2 * 100 + 10,
            "two failed attempts keep their reservations; the final attempt is reconciled to actual"
        );
        assert_eq!(ledger.used(), ledger.committed(), "no pending remains");
    }

    /// FP-01（不双算钉子）：usage 到达时对账以实际值**替换**预留——
    /// 落账后的账本恰好等于实际 usage，不含预留残差（若实现错误地把
    /// 实际值叠加在预留上，本测试红）。
    #[test]
    fn budget_reconcile_replaces_the_reservation_no_double_count() {
        let (mut recorder, _journal, _seen) = recorder();
        let ledger = std::sync::Arc::new(crate::model::RunSpendLedger::new(None));
        recorder.set_run_ledger(std::sync::Arc::clone(&ledger));
        // run.rs 的请求前预留（保守值远大于实际 usage）。
        ledger.reserve(4100);

        recorder.emit(RunEvent::ModelRequested {
            turn: 1,
            provider: "p".into(),
            model: "m".into(),
        });
        recorder.emit(RunEvent::ModelStream {
            turn: 1,
            event: ModelEvent::Usage(crate::model::Usage {
                input_tokens: 5,
                output_tokens: 1,
                ..crate::model::Usage::default()
            }),
        });
        recorder.emit(RunEvent::ModelResponded {
            turn: 1,
            outcome: crate::event::ModelOutcome {
                has_text: true,
                tool_calls: 0,
            },
            finish_reason: crate::model::FinishReason::Completed,
            provider_replay: None,
        });
        assert_eq!(
            ledger.committed(),
            6,
            "actual usage replaces the reservation"
        );
        assert_eq!(ledger.used(), 6, "no pending residue remains");
    }

    /// RA-01/CA-01 记账接线腿：adapter 对非法 usage 返回 None 后，
    /// recorder 必须兑现而不是释放请求前的保守预留。删除 commit_pending
    /// 或让 adapter 生成零 Usage，跨层组合都会使硬闸归零。
    #[test]
    fn missing_or_rejected_usage_commits_the_conservative_reservation() {
        let (mut recorder, _journal, _seen) = recorder();
        let ledger = std::sync::Arc::new(crate::model::RunSpendLedger::new(Some(100)));
        recorder.set_run_ledger(std::sync::Arc::clone(&ledger));
        ledger.reserve(100);

        recorder.emit(RunEvent::ModelRequested {
            turn: 1,
            provider: "p".into(),
            model: "m".into(),
        });
        // 没有 ModelEvent::Usage：这是 adapter 拒绝非法 usage 后的生产形状。
        recorder.emit(RunEvent::ModelResponded {
            turn: 1,
            outcome: crate::event::ModelOutcome {
                has_text: false,
                tool_calls: 1,
            },
            finish_reason: crate::model::FinishReason::ToolCalls,
            provider_replay: None,
        });

        assert_eq!(ledger.committed(), 100, "the reservation becomes spent");
        assert_eq!(ledger.used(), 100, "no pending residue remains");
        assert!(ledger.exceeds_cap(), "the next provider request is blocked");
    }

    /// B2（DSH 0.1.1-rc.1 对齐）：流中被取消的轮次把已产出的部分前缀
    /// 定稿为带 `interrupted: true` 的 assistant/message（未派发 tool
    /// calls 不出现）；正常完成路径永不携带。pre-fix 红：无该字段。
    #[test]
    fn cancelled_stream_prefix_finalizes_with_interrupted_flag() {
        let (mut recorder, journal, _seen) = recorder();
        recorder.emit(RunEvent::RunStarted {
            project: "/proj".into(),
            message: crate::message::MessageContent::text("hi"),
            client_message_id: None,
        });
        recorder.emit(RunEvent::ModelRequested {
            turn: 1,
            provider: "p".into(),
            model: "m".into(),
        });
        recorder.emit(RunEvent::ModelStream {
            turn: 1,
            event: ModelEvent::TextDelta {
                delta: "partial answer".into(),
            },
        });
        // 无 ModelResponded：流中被取消。
        let _ = recorder.finish(TurnEndReason::Aborted {
            reason: crate::session::event::TurnEndCancelCause::User,
        });
        let events = journal.events();
        let partial = events
            .iter()
            .find(|(event_type, _)| event_type == "assistant/message")
            .expect("the partial prefix finalizes as an assistant/message");
        assert_eq!(
            partial.1.get("interrupted"),
            Some(&serde_json::json!(true)),
            "the cancelled prefix carries interrupted: true"
        );
        assert_eq!(
            partial.1["message"]["content"][0]["text"],
            serde_json::json!("partial answer")
        );
        let turn_end = events
            .iter()
            .find(|(event_type, _)| event_type == "turn/end")
            .expect("turn/end");
        assert_eq!(turn_end.1["reason"]["kind"], serde_json::json!("aborted"));
    }

    /// B2 反向腿：正常完成的 assistant/message 永不携带 interrupted。
    #[test]
    fn completed_messages_never_carry_the_interrupted_flag() {
        let (mut recorder, journal, _seen) = recorder();
        recorder.emit(RunEvent::ModelRequested {
            turn: 1,
            provider: "p".into(),
            model: "m".into(),
        });
        recorder.emit(RunEvent::ModelStream {
            turn: 1,
            event: ModelEvent::TextDelta {
                delta: "done".into(),
            },
        });
        recorder.emit(RunEvent::ModelResponded {
            turn: 1,
            outcome: crate::event::ModelOutcome {
                has_text: true,
                tool_calls: 0,
            },
            finish_reason: crate::model::FinishReason::Completed,
            provider_replay: None,
        });
        let _ = recorder.finish(TurnEndReason::Completed);
        let events = journal.events();
        let settled = events
            .iter()
            .find(|(event_type, _)| event_type == "assistant/message")
            .expect("settled message");
        assert!(
            settled.1.get("interrupted").is_none(),
            "completed messages never carry interrupted"
        );
    }

    /// F-1（审计）：插件采样（aux cell）与主循环 usage 落进**同一账本**
    /// ——预警含采样、硬停读同一实例，两仪表不再各说各话。判别：拆开
    /// 两个账本（回旧实现）即本测试的 used 断言红。
    #[test]
    fn sampling_usage_lands_in_the_same_ledger_as_the_hard_stop() {
        let (mut recorder, _journal, _seen) = recorder();
        let ledger = std::sync::Arc::new(crate::model::RunSpendLedger::new(Some(1000)));
        recorder.set_run_ledger(std::sync::Arc::clone(&ledger));
        // 桥侧采样烧掉 300（aux cell，INV-S6 落账点归并）。
        let aux = Arc::new(StdMutex::new(crate::model::Usage {
            input_tokens: 280,
            output_tokens: 20,
            ..crate::model::Usage::default()
        }));
        recorder.attach_aux_usage(Arc::clone(&aux));
        // 主循环本步 500 + 100。
        recorder.emit(RunEvent::ModelRequested {
            turn: 1,
            provider: "p".into(),
            model: "m".into(),
        });
        recorder.emit(RunEvent::ModelStream {
            turn: 1,
            event: ModelEvent::Usage(crate::model::Usage {
                input_tokens: 500,
                output_tokens: 100,
                ..crate::model::Usage::default()
            }),
        });
        recorder.emit(RunEvent::ModelResponded {
            turn: 1,
            outcome: crate::event::ModelOutcome {
                has_text: true,
                tool_calls: 0,
            },
            finish_reason: crate::model::FinishReason::Completed,
            provider_replay: None,
        });
        assert_eq!(
            ledger.used(),
            900,
            "stream (600) + sampling aux (300) land in the shared ledger"
        );
        assert!(!ledger.exceeds_cap());
        // 再落一步 200 → 1100 ≥ 1000：硬停判据与 90% 预警同一数字。
        recorder.emit(RunEvent::ModelRequested {
            turn: 2,
            provider: "p".into(),
            model: "m".into(),
        });
        recorder.emit(RunEvent::ModelStream {
            turn: 2,
            event: ModelEvent::Usage(crate::model::Usage {
                input_tokens: 200,
                output_tokens: 0,
                ..crate::model::Usage::default()
            }),
        });
        recorder.emit(RunEvent::ModelResponded {
            turn: 2,
            outcome: crate::event::ModelOutcome {
                has_text: true,
                tool_calls: 0,
            },
            finish_reason: crate::model::FinishReason::Completed,
            provider_replay: None,
        });
        assert_eq!(ledger.used(), 1100);
        assert!(
            ledger.exceeds_cap(),
            "a sampling-heavy run crosses the same cap the hard stop reads"
        );
    }
}
