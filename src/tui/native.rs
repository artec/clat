//! Native host state is client-local presentation and transport ownership only.
use super::*;
use crate::host::{HostCallError, HostClient, HostEvent, HostEventsInterrupt, decode_host_event};
use serde_json::{Value, json};

mod info;
mod models;
mod permissions;
mod profiles;
mod questions;
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
    Info(u64, u64, u64, Result<Value, HostCallError>),
    PermissionChanged(u64, u64, Result<Value, String>),
    Connected(u64, HostEventsInterrupt),
    Frame(u64, HostEvent),
    Offline(u64, String),
    Snapshot(u64, u64, Result<Value, String>),
    Submitted(Result<Value, HostCallError>),
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
}

pub(super) struct NativeState {
    client: HostClient,
    epoch: u64,
    interrupt: Option<HostEventsInterrupt>,
    online: bool,
    selection: u64,
    snapshot_request: u64,
    pending: Option<String>,
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
}

impl App {
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
            client,
            epoch: 0,
            interrupt: None,
            online: false,
            selection: 0,
            snapshot_request: 0,
            pending: None,
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
        });
        // The title already carries the host connection marker. Keep the
        // resident status as the project directory and use this only as a
        // transient connection notice.
        app.status = "CLAT host · connecting · Ctrl+C detaches".into();
        Ok(app)
    }

    pub(super) fn start_native(&mut self) {
        self.close_native_info();
        self.permission_picker = None;
        if let Some(native) = &mut self.native {
            native.permissions = Default::default();
        }
        let (Some(native), Some(ui)) = (&mut self.native, self.event_sender.clone()) else {
            return;
        };
        native.epoch += 1;
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

    fn refresh_native(&mut self) {
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
        if !text.starts_with('/') {
            native.pending_images = std::mem::take(&mut self.attachments);
        }
        let images = native.pending_images.paths();
        let client = native.client.clone();
        let running = self.running;
        let selection = native.selection;
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
        let (Some(native), Some(ui)) = (&self.native, self.event_sender.clone()) else {
            return;
        };
        let client = native.client.clone();
        let selection = native.selection;
        thread::spawn(move || {
            let result = client.call(
                "run.cancel",
                &json!({"expected_selection_generation": selection}),
            );
            let _ = ui.send(UiEvent::Native(NativeEvent::ApprovalReply(result)));
        });
    }

    fn native_dialog_event(&mut self, event: NativeEvent) -> Option<NativeEvent> {
        match event {
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
                    Ok(rows) => {
                        self.session_picker =
                            Some(SessionPicker::new(rows, self.session_id.clone()))
                    }
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
            NativeEvent::ApprovalReply(Err(error)) => self.flash_status(error),
            _ => {}
        }
    }

    fn native_offline(&mut self, error: String) {
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
        let draft = self.native.as_mut().and_then(|n| n.pending.take());
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
            native.permissions.mode = value["permission"]["mode"]
                .as_str()
                .and_then(PermissionMode::from_journal_value);
        }
        self.config.thinking_level =
            serde_json::from_value(value["model"]["thinking_level"].clone()).ok();
        self.session_id = value["session"]["id"]
            .as_str()
            .map(|id| SessionId::new(id.to_owned()));
        self.session_title = value["session"]["title"].as_str().map(str::to_owned);
        self.config.model = value["model"]["model"].as_str().unwrap_or_default().into();
        // preset 也随快照回填：标题按 preset 查显示名（缺了就退回
        // "协议 · 模型名"，attach 壳里会恒走退回分支——2026-09-15
        // 负责人实机：标题多出 "OpenAI Compatible" 前缀）。
        self.config.preset = value["model"]["preset"].as_str().map(str::to_owned);
        if let Ok(protocol) = serde_json::from_value(value["model"]["protocol"].clone()) {
            self.config.protocol = protocol;
        }
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
            "replay.begin" => self.native.as_mut().unwrap().replay.clear(),
            "replay.end" => {
                self.conversation = crate::tui::conversation::ConversationModel::from_replay(
                    &self.native.as_ref().unwrap().replay,
                );
                self.running = false;
            }
            "subscribed" => {
                self.native.as_mut().unwrap().online = true;
                self.native.as_mut().unwrap().selection =
                    payload["selection_generation"].as_u64().unwrap_or(0);
                self.flash_status("attached to CLAT host");
            }
            "prompt.settled" => {
                self.running = false;
                self.phases.finish();
                self.conversation.push_turn_end(
                    payload["outcome"]["type"]
                        .as_str()
                        .unwrap_or("settled")
                        .into(),
                );
                self.refresh_native();
            }
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
            "notice" => self.refresh_native(),
            "approval.requested" => self.native_approval(payload),
            _ => {}
        }
    }
}

impl App {
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
}
