//! Native host state is client-local presentation and transport ownership only.
use super::*;
use crate::host::{HostCallError, HostClient, HostEvent, HostEventsInterrupt, decode_host_event};
use serde_json::{Value, json};

mod info;
mod models;
mod permissions;
mod profiles;
mod questions;
mod session_organization;
mod sessions;
#[cfg(all(test, feature = "runtime-tests"))]
mod tests;

fn image_message_label(text: &str, blocks: &[crate::message::ContentBlock]) -> String {
    let mut lines = Vec::new();
    if !text.is_empty() {
        lines.push(text.to_owned());
    }
    for block in blocks {
        if let crate::message::ContentBlock::Image { attachment } = block {
            lines.push(format!(
                "[Image: {} · {}×{}]",
                attachment.display_name.as_deref().unwrap_or("image"),
                attachment.width,
                attachment.height
            ));
        }
    }
    lines.join("\n")
}

pub(super) enum NativeEvent {
    Plugin(u64, u64, u64, super::plugins::PluginReply),
    History(u64, u64, Result<Value, String>),
    Discovery(u64, u64, u64, Result<Value, String>),
    Info(u64, u64, u64, Result<Value, HostCallError>),
    PermissionChanged(u64, u64, Result<Value, String>),
    Connected(u64, HostEventsInterrupt),
    Frame(u64, HostEvent),
    Offline(u64, String),
    Snapshot(u64, u64, Result<Value, String>),
    Submitted(Result<Value, HostCallError>),
    NextTurn(
        u64,
        u64,
        Option<String>,
        String,
        bool,
        Result<Value, String>,
    ),
    ApprovalReply(Result<Value, String>),
    Sessions(u64, u64, Result<Vec<crate::SessionSummary>, String>),
    Renamed(u64, u64, Result<Value, String>),
    Models(u64, u64, Result<crate::host::HostModelChoices, String>),
    ModelChanged(u64, u64, Result<Value, String>),
    PromptSuggestion(u64, u64, u64, Result<Value, String>),
    ThinkingChanged(u64, Result<Value, String>),
    ProfileLoaded(u64, u64, String, Result<Value, String>),
    ProfileSaved(u64, u64, Result<Value, String>),
    QuestionReply(
        u64,
        String,
        crate::interaction::AskAnswer,
        Result<Value, String>,
    ),
    /// CF7（r3 2026-10-14）：run.cancel 的回执——与审批回执分离。
    /// `issued_for` = 发起时所在 run 的本地序号（CF8/r4：旧轮回执不得
    /// 解除新 Run 的在途门）；失败回执按归属解除并提示，成功回执不清
    /// 门（CF7-B/r4：RPC 成功不是 Run 终态，门保持到真实终态事件）。
    CancelReply(u64, Result<Value, String>),
}

pub(super) struct NativeState {
    vendor: crate::ModelVendor,
    context_window: Option<u64>,
    pub(super) client: HostClient,
    pub(super) epoch: u64,
    interrupt: Option<HostEventsInterrupt>,
    pub(super) online: bool,
    pub(super) selection: u64,
    snapshot_request: u64,
    pending: Option<String>,
    /// HIST-TUI-1/T2 + B8-R1 同类：本次在途提交的入史资格（提交时的选择
    /// 世代，空闲态非 slash 消息）。接纳回执（Ok 或 committed 错误）只有
    /// 在宿主选择世代未变（会话未被切换）时才入史——迟到回执不得把旧会
    /// 话提示词写进新会话历史；steering 只能经 durable claim 后由快照补录。
    pending_history: Option<(u64, String)>,
    pub(super) next_turn: Vec<String>,
    pub(super) queue_pending: bool,
    pub(super) queue_retry: Option<(String, String)>,
    pub(super) recall_retry: Option<(String, u64, Option<String>)>,
    pub(super) recalled_queue_items: std::collections::VecDeque<String>,
    pub(super) queue_recovered: Vec<(Option<String>, String)>,
    pending_images: crate::tui::attachments::AttachmentComposer,
    approval_id: Option<String>,
    replay: Vec<crate::session::replay::ReplayEvent>,
    rename_target: Option<(u64, u64, String)>,
    rename_pending: bool,
    models_request: u64,
    models_pending: bool,
    thinking_pending: bool,
    editor_request: u64,
    editor_pending: bool,
    editor_saved_revision: u64,
    questions: questions::NativeQuestions,
    info_request: u64,
    permissions: permissions::NativePermissions,
    /// CF7：run.cancel 在途标志（所在 run 的本地序号）——同一 run 取
    /// 消未结算期间重复 ESC 幂等跳过（C1）；真实终态/新 run 解除。
    /// CF8（r4）：回执按序号归属，旧轮回执不解除新轮。
    pub(super) cancel_inflight: Option<u64>,
    /// CF8（r4）：本连接内 RunStarted 帧递增的本地 run 序号，标识取
    /// 消请求的归属。
    pub(super) run_seq: u64,
}

impl App {
    pub(super) fn native_plugin_context(&self) -> Option<(HostClient, u64, u64)> {
        self.native
            .as_ref()
            .filter(|native| native.online)
            .map(|native| (native.client.clone(), native.epoch, native.selection))
    }

    pub(super) fn native_online(&self) -> Option<bool> {
        self.native.as_ref().map(|native| native.online)
    }

    #[cfg(all(test, feature = "runtime-tests"))]
    pub(super) fn set_native_online_for_snapshot(&mut self, online: bool) {
        self.native.as_mut().expect("native snapshot shell").online = online;
    }

    pub(super) fn native_selection_identity(&self) -> Option<(u64, u64)> {
        self.native
            .as_ref()
            .map(|native| (native.epoch, native.selection))
    }

    pub(super) fn open_native(project: Project, client: HostClient) -> Result<Self, String> {
        let mut app = Self::open_minimal(project, Some(client.storage_root().to_path_buf()))?;
        // Bootstrap was read-only; this shell must never mount a local writer.
        app.bootstrap = None;
        app.trust_prompt = false;
        app.native = Some(NativeState {
            vendor: crate::ModelVendor::Other,
            context_window: None,
            client,
            epoch: 0,
            interrupt: None,
            online: false,
            selection: 0,
            snapshot_request: 0,
            pending: None,
            pending_history: None,
            next_turn: Vec::new(),
            queue_pending: false,
            queue_retry: None,
            recall_retry: None,
            recalled_queue_items: Default::default(),
            queue_recovered: Vec::new(),
            pending_images: Default::default(),
            approval_id: None,
            replay: Vec::new(),
            rename_target: None,
            rename_pending: false,
            models_request: 0,
            models_pending: false,
            thinking_pending: false,
            editor_request: 0,
            editor_pending: false,
            editor_saved_revision: 0,
            questions: questions::NativeQuestions::default(),
            info_request: 0,
            permissions: Default::default(),
            cancel_inflight: None,
            run_seq: 0,
        });
        // The title already carries the host connection marker. Keep the
        // resident status as the project directory and use this only as a
        // transient connection notice.
        app.status = "CLAT host · connecting · Ctrl+C detaches".into();
        Ok(app)
    }

    pub(super) fn start_native(&mut self) {
        self.close_plugin_dialog();
        self.close_native_info();
        self.permission_picker = None;
        if let Some(native) = &mut self.native {
            native.permissions = Default::default();
        }
        let (Some(native), Some(ui)) = (&mut self.native, self.event_sender.clone()) else {
            return;
        };
        native.epoch += 1;
        native.queue_pending = false;
        native.queue_retry = None;
        native.questions = questions::NativeQuestions::default();
        self.pending_ask_user = None;
        native.models_pending = false;
        native.thinking_pending = false;
        native.editor_pending = false;
        native.editor_request += 1;
        self.editor = None;
        self.picker = None;
        native.online = false;
        native.interrupt.take();
        native.approval_id = None;
        self.pending_permission = None;
        let client = native.client.clone();
        let epoch = native.epoch;
        thread::spawn(move || {
            let connected = client
                .events()
                .and_then(|events| Ok((events.interrupt_handle()?, events)));
            let (interrupt, mut events) = match connected {
                Ok(pair) => pair,
                Err(error) => {
                    let _ = ui.send(UiEvent::Native(NativeEvent::Offline(epoch, error)));
                    return;
                }
            };
            if ui
                .send(UiEvent::Native(NativeEvent::Connected(epoch, interrupt)))
                .is_err()
            {
                return;
            }
            loop {
                let next = events
                    .next_frame()
                    .and_then(|(kind, value)| decode_host_event(&kind, value));
                match next {
                    Ok(event) => {
                        if ui
                            .send(UiEvent::Native(NativeEvent::Frame(epoch, event)))
                            .is_err()
                        {
                            break;
                        }
                    }
                    Err(error) => {
                        let _ = ui.send(UiEvent::Native(NativeEvent::Offline(epoch, error)));
                        break;
                    }
                }
            }
        });
        self.refresh_native();
    }

    pub(super) fn refresh_native(&mut self) {
        let (Some(native), Some(ui)) = (&mut self.native, self.event_sender.clone()) else {
            return;
        };
        let client = native.client.clone();
        native.snapshot_request += 1;
        let (epoch, request) = (native.epoch, native.snapshot_request);
        thread::spawn(move || {
            let value = client.call("workbench.info", &json!({}));
            let _ = ui.send(UiEvent::Native(NativeEvent::Snapshot(
                epoch, request, value,
            )));
        });
    }

    fn native_local_command(&mut self, text: &str) -> bool {
        if HostClient::is_detach_command(text) {
            self.should_quit = true;
            return true;
        }
        if HostClient::is_permission_dialog_command(text) {
            self.open_native_permissions();
            return true;
        }
        if self.open_native_info(text) {
            return true;
        }
        if text == "/model" {
            self.open_native_models();
            return true;
        }
        if text == "/suggest" {
            self.open_native_suggestion();
            return true;
        }
        if text == "/rename" {
            self.open_native_rename();
            return true;
        }
        if text == "/resume" {
            self.open_native_sessions();
            return true;
        }
        self.handle_attachment_command(text)
    }

    pub(super) fn open_native_suggestion(&mut self) {
        let (Some(native), Some(ui)) = (&self.native, self.event_sender.clone()) else {
            return;
        };
        if native.pending.is_some() || !native.online || self.suggestions.pending() || self.running
        {
            self.flash_status("host offline/busy — suggestion kept out of the composer");
            return;
        }
        let client = native.client.clone();
        let (epoch, selection) = (native.epoch, native.selection);
        let (request, _) = self
            .suggestions
            .begin(self.input.generation())
            .expect("idle suggestion slot");
        thread::spawn(move || {
            let result = client.call(
                "prompt.suggest",
                &json!({"expected_selection_generation": selection}),
            );
            let _ = ui.send(UiEvent::Native(NativeEvent::PromptSuggestion(
                epoch,
                selection,
                request,
                result.map_err(|error| error.to_string()),
            )));
        });
        self.flash_status("generating a manual suggestion…");
    }

    pub(super) fn submit_native(&mut self, text: String) {
        if self.native_local_command(text.trim()) {
            return;
        }
        if text.is_empty() && self.attachments.is_empty() {
            return;
        }
        if text == "/cancel" {
            self.cancel_native();
            return;
        }
        let running = self.running;
        let Some(native) = &mut self.native else {
            return;
        };
        if text == "/reconnect" {
            self.start_native();
            return;
        }
        if !native.online || native.pending.is_some() || self.clipboard_image_pending {
            self.input.insert_str(&text);
            self.flash_status(
                "host offline/busy or clipboard pending — draft kept; /reconnect reconnects",
            );
            return;
        }
        let Some(ui) = self.event_sender.clone() else {
            self.input.insert_str(&text);
            return;
        };
        native.pending = Some(text.clone());
        let selection = native.selection;
        // HIST-TUI-1：入史资格在提交时定格（空闲态 + 非 slash 消息，并
        // 记住提交时的选择世代）——running 态提交走 steer.send，接纳 ≠
        // user journal 事件，不入史，只等 durable claim 后由快照
        // recent_inputs 补录。
        native.pending_history = if running || text.starts_with('/') {
            None
        } else {
            Some((selection, text.clone()))
        };
        if !text.starts_with('/') {
            native.pending_images = std::mem::take(&mut self.attachments);
        }
        let images = native.pending_images.paths();
        let client = native.client.clone();
        thread::spawn(move || {
            let result = client.submit_message(&text, &images, running, selection);
            let _ = ui.send(UiEvent::Native(NativeEvent::Submitted(result)));
        });
    }

    fn open_native_sessions(&mut self) {
        let (Some(native), Some(ui)) = (&self.native, self.event_sender.clone()) else {
            return;
        };
        let client = native.client.clone();
        let (epoch, selection) = (native.epoch, native.selection);
        thread::spawn(move || {
            let result = client.sessions();
            let _ = ui.send(UiEvent::Native(NativeEvent::Sessions(
                epoch, selection, result,
            )));
        });
    }

    fn cancel_native(&mut self) {
        // CANCEL-1（C1）：取消请求是异步 RPC，事件回流要等宿主真正停
        // 止；UI 反馈不得绑定在往返完成上——按键即刻亮"取消中"（flash
        // + phase 行），宿主终态事件到达后由 finish 侧提示覆盖。
        self.cancel_requested = true;
        self.flash_status("cancelling…");
        let (Some(native), Some(ui)) = (&mut self.native, self.event_sender.clone()) else {
            return;
        };
        // CF7（r3，C1）：同一 run 的取消请求尚未结算时，重复 ESC 不
        // 得发出第二个 run.cancel——幂等跳过。Cancelling… 反馈已在屏
        // 上；门保持到**真实终态事件**（CF7-B/r4：成功 RPC 不是终态）；
        // 失败回执按归属解除，失败后的下一次 ESC 是可解释的人工重试。
        let run_seq = native.run_seq;
        if native.cancel_inflight == Some(run_seq) {
            return;
        }
        native.cancel_inflight = Some(run_seq);
        let client = native.client.clone();
        let selection = native.selection;
        thread::spawn(move || {
            let result = client.call(
                "run.cancel",
                &json!({"expected_selection_generation": selection}),
            );
            let _ = ui.send(UiEvent::Native(NativeEvent::CancelReply(run_seq, result)));
        });
    }

    fn native_dialog_event(&mut self, event: NativeEvent) -> Option<NativeEvent> {
        match event {
            NativeEvent::Plugin(epoch, selection, request, reply) => {
                self.plugin_loaded(epoch, selection, request, reply);
            }
            NativeEvent::History(epoch, selection, result) => {
                self.native_history_loaded(epoch, selection, result)
            }
            NativeEvent::Discovery(epoch, selection, request, result) => {
                self.discovery_loaded(epoch, selection, request, result)
            }
            NativeEvent::PermissionChanged(epoch, selection, result) => {
                self.native_permission_changed(epoch, selection, result)
            }
            NativeEvent::Info(epoch, selection, request, result) => {
                self.native_info_loaded(epoch, selection, request, result)
            }
            NativeEvent::QuestionReply(epoch, id, answer, result) => {
                self.native_question_reply(epoch, &id, answer, result)
            }
            NativeEvent::ProfileLoaded(epoch, request, name, result) => {
                self.native_profile_loaded(epoch, request, &name, result)
            }
            NativeEvent::ProfileSaved(epoch, request, result) => {
                self.native_profile_saved(epoch, request, result)
            }
            NativeEvent::ThinkingChanged(epoch, result) => {
                self.native_thinking_changed(epoch, result)
            }
            NativeEvent::Models(epoch, request, result) => {
                self.native_models(epoch, request, result)
            }
            NativeEvent::ModelChanged(epoch, request, result) => {
                self.native_model_changed(epoch, request, result)
            }
            NativeEvent::PromptSuggestion(epoch, selection, request, result) => {
                self.native_prompt_suggestion(epoch, selection, request, result)
            }
            NativeEvent::Renamed(epoch, selection, result) => {
                self.native_renamed(epoch, selection, result)
            }
            other => return Some(other),
        }
        None
    }

    pub(super) fn handle_native_event(&mut self, event: NativeEvent) {
        let Some(event) = self.native_dialog_event(event) else {
            return;
        };
        match event {
            NativeEvent::Sessions(epoch, selection, result)
                if self
                    .native
                    .as_ref()
                    .is_some_and(|n| n.epoch == epoch && n.selection == selection) =>
            {
                match result {
                    Ok(rows) => self.refresh_picker_sessions(rows),
                    Err(error) => self.flash_status(error),
                }
            }
            NativeEvent::Connected(epoch, interrupt)
                if self.native.as_ref().is_some_and(|n| n.epoch == epoch) =>
            {
                self.native.as_mut().unwrap().interrupt = Some(interrupt);
            }
            NativeEvent::Frame(epoch, frame)
                if self.native.as_ref().is_some_and(|n| n.epoch == epoch) =>
            {
                self.native_frame(frame)
            }
            NativeEvent::Offline(epoch, error)
                if self.native.as_ref().is_some_and(|n| n.epoch == epoch) =>
            {
                self.native_offline(error);
            }
            NativeEvent::Snapshot(epoch, request, result)
                if self
                    .native
                    .as_ref()
                    .is_some_and(|n| n.epoch == epoch && n.snapshot_request == request) =>
            {
                match result {
                    Ok(value) => self.native_snapshot(value),
                    Err(error) => self.flash_status(error),
                }
            }
            NativeEvent::Submitted(result) => self.native_submitted(result),
            NativeEvent::NextTurn(epoch, selection, session, text, recall, result) => {
                self.native_next_turn_reply((epoch, selection, session), text, recall, result);
            }
            NativeEvent::ApprovalReply(Err(error)) => self.flash_status(error),
            NativeEvent::CancelReply(issued_for, result) => {
                // CF7-B/r4：成功回执**不解除**在途门——宿主 run.cancel
                // 立即返回，不是 Run 终态；门保持到真实终态事件。
                // CF8/r4：失败回执只在归属匹配时解除（旧轮回执不改当
                // 前取消状态）并给出可解释提示，下一次 ESC 即人工重试。
                let belongs = self
                    .native
                    .as_ref()
                    .is_some_and(|native| native.cancel_inflight == Some(issued_for));
                if belongs && result.is_err() {
                    if let Some(native) = self.native.as_mut() {
                        native.cancel_inflight = None;
                    }
                    if let Err(error) = result {
                        self.flash_status(format!("cancel request failed: {error}"));
                    }
                }
            }
            _ => {}
        }
    }

    fn native_offline(&mut self, error: String) {
        self.close_plugin_dialog();
        self.native.as_mut().unwrap().online = false;
        self.native.as_mut().unwrap().approval_id = None;
        self.pending_permission = None;
        self.clear_native_questions();
        self.flash_status(error);
    }

    fn native_prompt_suggestion(
        &mut self,
        epoch: u64,
        selection: u64,
        request: u64,
        result: Result<Value, String>,
    ) {
        let valid = self
            .native
            .as_ref()
            .is_some_and(|native| native.epoch == epoch && native.selection == selection)
            && result.as_ref().map_or(true, |value| {
                value["selection_generation"].as_u64() == Some(selection)
                    && value["session_id"].as_str()
                        == self.session_id.as_ref().map(SessionId::as_str)
            });
        self.finish_suggestion(
            request,
            valid,
            result.and_then(|value| {
                value["text"]
                    .as_str()
                    .map(str::to_owned)
                    .ok_or_else(|| "host returned an empty suggestion".to_owned())
            }),
        );
    }

    pub(super) fn native_clipboard_drafts(
        &self,
    ) -> Option<std::sync::Arc<crate::draft::DraftImageStore>> {
        self.native
            .as_ref()
            .map(|native| native.client.clipboard_drafts())
    }

    fn native_submitted(&mut self, result: Result<Value, HostCallError>) {
        let plugins = result
            .as_ref()
            .is_ok_and(|value| value["kind"] == "plugin_manager");
        let draft = self.native.as_mut().and_then(|n| n.pending.take());
        // HIST-TUI-1/T2 + B8-R1 同类：接纳事实以回执为准（Ok 或 committed
        // 错误均为“已持久入账”），且宿主选择世代未变（提交属于当前会话）
        // 才具备资格入史；未入账失败不污染（草稿走还原路径）。
        let history_entry = self
            .native
            .as_mut()
            .and_then(|n| n.pending_history.take())
            .filter(|_| result.is_ok() || result.as_ref().is_err_and(|error| error.committed()))
            .filter(|(selection_at_submit, _)| {
                self.native
                    .as_ref()
                    .is_some_and(|native| native.selection == *selection_at_submit)
            });
        let images = self
            .native
            .as_mut()
            .map(|n| std::mem::take(&mut n.pending_images));
        if let Some(images) = images {
            if result.as_ref().is_err_and(|error| !error.committed()) {
                self.attachments.restore_before(images);
            } else {
                self.release_core_staged_attachment_paths(images.paths());
            }
        }
        match result {
            Ok(value) => {
                if plugins {
                    self.open_plugin_dialog();
                    return;
                }
                if let Some((_, text)) = history_entry {
                    self.input.remember(text);
                }
                if let Some(text) = value.get("message").and_then(Value::as_str) {
                    self.conversation.push_turn_end(text.into());
                } else if value.get("sessions").is_some() {
                    self.conversation.push_turn_end(value.to_string());
                } else if let Some(status) = value["status"].as_str() {
                    self.flash_status(status);
                }
                self.refresh_native();
            }
            Err(error) => {
                if error.committed() {
                    if let Some((_, text)) = history_entry {
                        self.input.remember(text);
                    }
                    self.flash_status(format!(
                        "{} · message committed; not restored or resent",
                        error
                    ));
                    self.refresh_native();
                    return;
                }
                if let Some(draft) = draft {
                    self.input.prepend_recalled_line(&draft);
                }
                self.flash_status(format!(
                    "{} · draft kept; check host history before resending",
                    error
                ));
            }
        }
    }

    fn native_snapshot(&mut self, value: Value) {
        if let Some(native) = &mut self.native {
            native.next_turn = value["next_turn_queue"]
                .as_array()
                .map(|items| {
                    items
                        .iter()
                        .filter_map(|item| item["text"].as_str().map(str::to_owned))
                        .collect()
                })
                .unwrap_or_default();
            native.permissions.mode = value["permission"]["mode"]
                .as_str()
                .and_then(PermissionMode::from_journal_value);
        }
        self.config.thinking_level =
            serde_json::from_value(value["model"]["thinking_level"].clone()).ok();
        let previous_session_id = self.session_id.clone();
        self.session_id = value["session"]["id"]
            .as_str()
            .map(|id| SessionId::new(id.to_owned()));
        // HIST-TUI-1/T3 + B8-R2-1 同类：快照携带的 session.recent_inputs 是
        // 当前会话的权威已提交输入。**会话未变**时走 replace_history（游标
        // 值锚迁移、待还原暂存保留）；**会话变了**（其它客户端切换/重连到
        // 另一会话）时整体 retire——旧会话的浏览暂存不得进入新会话；旧宿
        // 主无此字段时保持现状。
        if let Some(inputs) = value["session"]["recent_inputs"].as_array() {
            let history = inputs
                .iter()
                .filter_map(Value::as_str)
                .filter(|text| !text.trim().is_empty())
                .map(str::to_owned)
                .collect();
            let session_changed = previous_session_id.as_ref() != self.session_id.as_ref();
            if session_changed {
                self.input.retire_history(history);
            } else {
                self.input.replace_history(history);
            }
        }
        self.restore_native_queue_drafts();
        self.session_title = value["session"]["title"].as_str().map(str::to_owned);
        self.config.model = value["model"]["model"].as_str().unwrap_or_default().into();
        // preset 也随快照回填：标题按 preset 查显示名（缺了就退回
        // "协议 · 模型名"，attach 壳里会恒走退回分支——2026-09-15
        // 负责人实机：标题多出 "OpenAI Compatible" 前缀）。
        self.config.preset = value["model"]["preset"].as_str().map(str::to_owned);
        if let Ok(protocol) = serde_json::from_value(value["model"]["protocol"].clone()) {
            self.config.protocol = protocol;
        }
        self.native_telemetry(&value);
    }

    fn native_frame(&mut self, frame: HostEvent) {
        match frame {
            HostEvent::Replay(mut event) => {
                if let crate::session::replay::ReplayEvent::UserMessage {
                    text,
                    content_blocks,
                    ..
                } = &mut event
                {
                    *text = image_message_label(text, content_blocks);
                }
                self.native.as_mut().unwrap().replay.push(event);
            }
            HostEvent::Run(event) => {
                if let RunEvent::RunStarted { message, .. } = &event {
                    self.running = true;
                    // CONT-1（C1）：新 run 开工即清上一 run 的常驻终态。
                    self.terminal_status = None;
                    // CF8（r4）：本地 run 序号推进——此后发起的取消请
                    // 求归属于新 run，旧轮回执不再匹配。
                    if let Some(native) = self.native.as_mut() {
                        native.run_seq += 1;
                    }
                    self.run_usage_acc = Usage::default();
                    self.run_routes_base = Some(self.usage_routes.clone());
                    self.run_route = None;
                    self.conversation
                        .push_user(image_message_label(&message.plain_text(), &message.blocks));
                }
                self.handle_run_event(event);
            }
            HostEvent::Control { kind, payload } => self.native_control(&kind, payload),
        }
    }

    fn native_control(&mut self, kind: &str, payload: Value) {
        match kind {
            "replay.begin" => {
                self.native.as_mut().unwrap().replay.clear();
                self.discovery.reset_transient();
                self.conversation_has_more = payload["has_more"].as_bool().unwrap_or(false);
                self.conversation_history_windowed = self.conversation_has_more;
                self.conversation_history_loading = false;
            }
            "replay.end" => {
                self.conversation = crate::tui::conversation::ConversationModel::from_replay(
                    &self.native.as_ref().unwrap().replay,
                );
                self.running = false;
                // CF1：会话装载/切换完成 = 旧 run 的取消请求不再属于
                // 当前上下文（跨会话/断线重连防线）。
                self.cancel_requested = false;
                if let Some(native) = self.native.as_mut() {
                    native.cancel_inflight = None;
                }
            }
            "subscribed" => {
                self.native.as_mut().unwrap().online = true;
                self.native.as_mut().unwrap().selection =
                    payload["selection_generation"].as_u64().unwrap_or(0);
                self.flash_status("attached to CLAT host");
            }
            "prompt.settled" => self.native_prompt_settled(payload),
            "notice" if payload["kind"] == "selection" => self.start_native(),
            "notice" if payload["kind"] == "compaction" => {
                if let Some(note) = payload["payload"]["note"].as_str() {
                    self.flash_status(note);
                }
                self.refresh_native();
            }
            "notice" if payload["kind"] == "approval_resolved" => {
                let native = self.native.as_mut().unwrap();
                if native.approval_id.as_deref() == payload["payload"]["rpc_id"].as_str() {
                    native.approval_id = None;
                    self.pending_permission = None;
                }
            }
            "notice"
                if matches!(
                    payload["kind"].as_str(),
                    Some("question_requested" | "question_resolved")
                ) =>
            {
                self.native_question_notice(&payload)
            }
            "notice" if payload["kind"] == "next_turn_error" => {
                self.flash_status(format!(
                    "next turn paused: {}",
                    payload["payload"]["message"]
                        .as_str()
                        .unwrap_or("startup failed")
                ));
                self.refresh_native();
            }
            "notice" => self.refresh_native(),
            "approval.requested" => self.native_approval(payload),
            _ => {}
        }
    }

    /// `prompt.settled` 控制帧：宿主侧 run 结算（拒绝/失败路径可能没有
    /// 对应的 RunCancelled 事件到达，同样是 run 终态）。从 `native_control`
    /// 拆出（BELL-HOST-1 修复同批：不延长既有巨函数，形状预算只紧不松）。
    ///
    /// F3（返工 r2/r3/r4）：去重记忆 = 转录中以该 id 为主键的终态卡——
    /// 不可遗忘且天然有界（转录无删除路径，与会话同生命周期），不受固定
    /// 窗口淘汰（CF-R3-2：任意历史距离的迟到重放都拦）。不同 id 逐一处
    /// 理；Null/缺失 id（旧宿主语义）不做归属去重。
    fn native_prompt_settled(&mut self, payload: Value) {
        let rpc_id = payload["prompt_rpc_id"].as_str().map(str::to_owned);
        let duplicate = rpc_id
            .as_deref()
            .is_some_and(|id| self.conversation.has_settled_card(id));
        if duplicate {
            self.refresh_native();
            return;
        }
        // BELL-HOST-1：结算铃的归属快照——本 TUI 把这个 run 视为
        // running（RunStarted 先于 settled 到达同一条 SSE 流，顺序有保
        // 证）。切会话/重连后的 replay.end 已把 running 复位，旧会话的
        // 迟到结算不会乱响。
        let was_running = self.running;
        self.running = false;
        // CF1：宿主侧 prompt 结算同样是 run 终态。
        self.cancel_requested = false;
        if let Some(native) = self.native.as_mut() {
            native.cancel_inflight = None;
        }
        self.phases.finish();
        // RUNERR-1：settled 帧的完整 outcome（含 failed 的真实原因）进转
        // 录，与 standalone/回放同词汇——不再只剩 `failed`。终态卡携带
        // prompt_rpc_id 主键（F3/CF-R3-2 的去重记忆）。
        let notice = crate::tui::conversation::settled_outcome_notice(&payload);
        match rpc_id {
            Some(id) => self.conversation.push_settled_turn_end(notice, id),
            None => self.conversation.push_turn_end(notice),
        }
        // CONT-1（C1/C2）：attached 腿与 standalone 同款——run 真实终态常
        // 驻状态栏；存在可观察未完成待办时给出诚实交代。todo 事实经既有
        // workflow.details RPC（不加新 wire 面）。
        let outcome_type = payload["outcome"]["type"].as_str().unwrap_or("").to_owned();
        // BELL-HOST-1（AG-DOGFOOD-10，2026-10-19）：attached 腿 run 终态响
        // 铃——与 standalone（run_events.rs `finish_run`）/ dsh
        // （dsh_events.rs `turn/end`）同语义：completed/failed 各一声，
        // cancelled（用户主动取消）不响。焦点三态与 CLAT_NO_BELL/
        // CLAT_BELL_COMMAND 由 notify/should_ring 单点裁决；同 RPC 重复帧
        // 已被上面的去重门拦截，实际结算至多一响。AFK 语义：提醒“本次
        // Run 已结束/需人工处理”，不代言业务目标验收成功。
        if was_running && matches!(outcome_type.as_str(), "completed" | "failed") {
            self.notify();
        }
        self.apply_settled_accounting(outcome_type);
        self.refresh_native();
    }
}

impl App {
    fn native_telemetry(&mut self, value: &Value) {
        let Some(native) = self.native.as_mut() else {
            return;
        };
        native.vendor = match value["model"]["vendor"].as_str() {
            Some("DeepSeek") => crate::ModelVendor::DeepSeek,
            Some("Glm") => crate::ModelVendor::Glm,
            Some("Kimi") => crate::ModelVendor::Kimi,
            Some("Qwen") => crate::ModelVendor::Qwen,
            Some("Tencent") => crate::ModelVendor::Tencent,
            _ => crate::ModelVendor::Other,
        };
        native.context_window = value["model"]["max_context_tokens"].as_u64().or_else(|| {
            value["model"]["preset"]
                .as_str()
                .and_then(crate::presets::preset_by_id)
                .map(|preset| preset.context_window as u64)
        });
        self.balance = value["telemetry"]["monitor"].as_str().map(str::to_owned);
        self.usage_routes.clear();
        if let Some(usage) = telemetry_usage(&value["telemetry"]["route_usage"]) {
            self.usage_routes.insert(
                crate::model::model_route_key(
                    &self.config.protocol.to_string(),
                    &self.config.model,
                ),
                usage,
            );
        }
        self.last_turn_usage = telemetry_usage(&value["telemetry"]["last_request_usage"]);
    }

    pub(super) fn native_status_segments(&self) -> Vec<String> {
        let Some(native) = self.native.as_ref() else {
            return Vec::new();
        };
        status_telemetry_segments(
            native.vendor,
            &self.balance,
            current_route_usage(&self.usage_routes, &self.config),
            self.last_turn_usage.as_ref(),
            native.context_window,
        )
    }

    fn native_approval(&mut self, payload: Value) {
        let (id, request) = match crate::host::decode_host_approval(&payload) {
            Ok(request) => request,
            Err(error) => {
                self.flash_status(error);
                return;
            }
        };
        let (Some(native), Some(ui)) = (&mut self.native, self.event_sender.clone()) else {
            return;
        };
        if native.approval_id.as_ref() == Some(&id) {
            return;
        }
        native.approval_id = Some(id.clone());
        let client = native.client.clone();
        let (decision_tx, decisions) = mpsc::channel();
        self.handle_worker_message(WorkerMessage::PermissionRequest {
            request,
            decision_tx,
        });
        thread::spawn(move || {
            // A closed dialog/client is not a denial on behalf of other clients.
            let Ok(decision) = decisions.recv_timeout(Duration::from_secs(600)) else {
                return;
            };
            let decision = if matches!(decision, PermissionDecision::Allow) {
                "allow"
            } else {
                "deny"
            };
            let result = client.call(
                "approval.respond",
                &json!({"rpcId":id, "decision":decision}),
            );
            let _ = ui.send(UiEvent::Native(NativeEvent::ApprovalReply(result)));
        });
    }

    /// CONT-1（C1/C2）：attached 腿的 settled 交代。completed/cancelled 时
    /// 经既有 `workflow.details` RPC 读 todo 投影：存在未完成项 → 转录
    /// 诚实交代行（RunCompleted ≠ 业务目标已核验）+ 常驻终态带未完成数；
    /// 读不到投影或无未完成项 → 常驻终态不带待办注记，不编造。failed 的
    /// 原因已由 settled_outcome_notice 进转录，这里只设常驻终态。
    fn apply_settled_accounting(&mut self, outcome_type: String) {
        let todos = match outcome_type.as_str() {
            "completed" | "cancelled" => self.native.as_ref().and_then(|native| {
                native
                    .client
                    .call("workflow.details", &json!({}))
                    .ok()
                    .and_then(|details| crate::tui::run_events::summarize_open_todos(&details))
            }),
            _ => None,
        };
        match (outcome_type.as_str(), todos) {
            ("completed", Some((open, total, sample))) => {
                self.conversation.push_turn_end(format!(
                    "run completed — {open} of {total} todos unfinished (task completion not verified): {sample}"
                ));
                self.set_terminal_status(format!(
                    "completed · {open} todos unfinished — send a message to continue"
                ));
            }
            ("completed", None) => {
                self.set_terminal_status("completed — send a message to continue");
            }
            ("cancelled", Some((open, _, _))) => {
                self.set_terminal_status(format!("cancelled · {open} todos unfinished"));
            }
            ("cancelled", None) => {
                self.set_terminal_status("cancelled");
            }
            ("failed", _) => {
                self.set_terminal_status("run failed — see the failure notice above");
            }
            _ => {}
        }
    }
}

fn telemetry_usage(value: &Value) -> Option<Usage> {
    Some(Usage {
        input_tokens: value["input_tokens"].as_u64()?,
        output_tokens: value["output_tokens"].as_u64()?,
        cached_input_tokens: value["cached_input_tokens"].as_u64(),
        reasoning_tokens: value["reasoning_tokens"].as_u64(),
    })
}
