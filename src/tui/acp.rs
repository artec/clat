use super::*;
use clat_core::client_ports::acp::Interaction;
use serde_json::{Value, json};

#[derive(Default)]
pub(super) struct UiState {
    epoch: u64,
    pending: bool,
    wake: Option<Instant>,
    refresh_at: Option<Instant>,
    watching: Option<String>,
    target: Option<SessionId>,
}

impl App {
    pub(super) fn handle_acp_command(&mut self, input: &str) -> bool {
        let Some(tail) = input
            .strip_prefix("/acp")
            .filter(|s| s.is_empty() || s.starts_with(' '))
        else {
            return false;
        };
        if self.dsh.is_some() || self.dsh_connect.is_some() {
            return false;
        }
        let Some((method, params, confirmation)) = request(tail.trim(), self.session_id.as_ref())
        else {
            self.flash_message(UiMessage::new("acp.tui_help"));
            return true;
        };
        if confirmation {
            let command = format!("{input} --confirm");
            self.input.insert_str(&command);
            self.flash_message(UiMessage::new("acp.tui_confirmation").arg("command", command));
            return true;
        }
        self.submit_acp(method, params);
        true
    }

    fn submit_acp(&mut self, method: &'static str, params: Value) {
        if self.acp.pending {
            self.flash_message(UiMessage::new("acp.request_pending"));
            return;
        }
        self.acp.watching = None;
        self.acp.refresh_at = None;
        self.request_acp(method, params, false);
    }

    fn request_acp(&mut self, method: &'static str, mut params: Value, refresh: bool) {
        let Some(sender) = self.event_sender.clone() else {
            return;
        };
        self.acp.epoch += 1;
        let epoch = self.acp.epoch;
        if let Some(native) = &self.native {
            let client = native.client.clone();
            let identity = (native.epoch, native.selection);
            if method == "acp.delegations.start" {
                params["expected_selection_generation"] = json!(native.selection);
            }
            self.acp.pending = true;
            thread::spawn(move || {
                let value = client.call(method, &params);
                let _ = sender.send(UiEvent::Worker(WorkerMessage::AcpReply {
                    epoch,
                    identity: Some(identity),
                    refresh,
                    value,
                }));
            });
        } else if let Some(app) = &mut self.application {
            if method == "acp.sessions.remote" {
                let result = app.acp_client_remote(&params, interaction(sender.clone()));
                let locale = self.locale;
                self.acp.pending = true;
                thread::spawn(move || {
                    let value = result
                        .and_then(|run| run())
                        .map_err(|error| locale.render(&error.presentation()));
                    let _ = sender.send(UiEvent::Worker(WorkerMessage::AcpReply {
                        epoch,
                        identity: None,
                        refresh,
                        value,
                    }));
                });
                return;
            }
            let value = app
                .acp_client_call(method, &params, interaction(sender))
                .map_err(|error| self.locale.render(&error.presentation()));
            self.session_id = app.current_session_id();
            self.on_acp_reply(epoch, None, refresh, value);
        }
    }

    pub(super) fn on_acp_reply(
        &mut self,
        epoch: u64,
        identity: Option<(u64, u64)>,
        refresh: bool,
        result: Result<Value, String>,
    ) {
        if epoch != self.acp.epoch {
            return;
        }
        self.acp.pending = false;
        if identity.is_some() && identity != self.native_selection_identity() {
            self.acp.watching = None;
            return;
        }
        if refresh && !self.acp_details_open() {
            return;
        }
        let value = match result {
            Ok(value) => value,
            Err(error) => {
                if refresh && self.acp.watching.is_some() {
                    self.acp.refresh_at = Some(Instant::now() + Duration::from_secs(2));
                }
                self.flash_status(terminal_text(&error));
                return;
            }
        };
        self.acp.watching = value["id"].as_str().and_then(|id| {
            matches!(value["phase"].as_str(), Some("starting" | "running")).then(|| id.to_owned())
        });
        self.acp.refresh_at = self
            .acp
            .watching
            .as_ref()
            .map(|_| Instant::now() + Duration::from_secs(1));
        self.content_view = Some(ContentView::Remote {
            title: "/acp",
            text: terminal_text(&acp_info(value, self.locale)).into(),
        });
        if !refresh {
            self.info_dialog = Some(InfoDialog::new(InfoDialogKind::Remote));
        }
        if !refresh {
            self.acp.wake = Some(Instant::now());
        }
    }

    fn acp_details_open(&self) -> bool {
        self.info_dialog.is_some()
            && matches!(
                &self.content_view,
                Some(ContentView::Remote { title: "/acp", .. })
            )
    }

    pub(super) fn acp_deadline(&self) -> Option<Instant> {
        let continuation = (!self.running && self.native.is_none())
            .then_some(self.acp.wake)
            .flatten();
        self.acp.refresh_at.into_iter().chain(continuation).min()
    }

    pub(super) fn poll_acp(&mut self) {
        self.refresh_acp_details();
        if self.native.is_some() || self.running || self.run_handle.is_some() {
            return;
        }
        let Some(app) = self.application.as_mut() else {
            return;
        };
        if !app.acp_client_needs_wake() {
            self.acp.wake = None;
            return;
        }
        if self.acp.wake.is_some_and(|wake| wake > Instant::now()) {
            return;
        }
        self.acp.wake = Some(Instant::now() + Duration::from_secs(1));
        let Some(sender) = self.event_sender.clone() else {
            return;
        };
        let ports = interaction(sender.clone());
        let (completion, completed) = mpsc::channel();
        let result = app.acp_client_continue(
            ApplicationRunRequest {
                message: crate::message::PendingMessage::text(""),
                approver: ports.approver,
                asker: ports.asker,
                completion,
                events: Box::new(ChannelEventSink(sender.clone())),
            },
            self.session_id.as_ref(),
        );
        let (handle, session, text) = match result {
            Ok(Some(started)) => started,
            Ok(None) => return,
            Err(error) => {
                self.flash_message(error.presentation());
                return;
            }
        };
        self.acp.target = Some(SessionId::new(session.clone()));
        if self
            .session_id
            .as_ref()
            .is_some_and(|id| id.as_str() == session)
        {
            self.conversation.push_user(text);
            self.conversation_scroll_from_bottom = 0;
        }
        self.run_epoch += 1;
        let epoch = self.run_epoch;
        self.run_handle = Some(handle);
        self.running = true;
        self.terminal_status = None;
        self.terminal_presentation = None;
        self.flash_message(UiMessage::new("acp.handoff_admitted"));
        thread::spawn(move || {
            if let Ok(result) = completed.recv() {
                let _ = sender.send(UiEvent::Worker(WorkerMessage::Done { epoch, result }));
            }
        });
    }

    fn refresh_acp_details(&mut self) {
        if !self.acp_details_open() {
            self.acp.watching = None;
            self.acp.refresh_at = None;
        }
        if !self.acp.pending && self.acp.refresh_at.is_some_and(|at| at <= Instant::now()) {
            self.acp.refresh_at = None;
            if let Some(id) = self.acp.watching.clone() {
                self.request_acp("acp.sessions.status", json!({"id":id}), true);
            }
        }
    }

    pub(super) fn finish_acp_run(&mut self) -> bool {
        let Some(target) = self.acp.target.take() else {
            return false;
        };
        if self.session_id.as_ref() == Some(&target) {
            return false;
        }
        if let Some(handle) = self.run_handle.take() {
            let _ = handle.join();
        }
        self.running = false;
        self.flash_message(UiMessage::new("acp.handoff_admitted"));
        true
    }
}

fn interaction(sender: SyncSender<UiEvent>) -> Interaction {
    Interaction {
        asker: Some(Arc::new(ChannelUserAsker::new(sender.clone()))),
        approver: Arc::new(ChannelApprover::new(sender)),
    }
}

fn request(tail: &str, origin: Option<&SessionId>) -> Option<(&'static str, Value, bool)> {
    let (command, args) = tail.split_once(' ').unwrap_or((tail, ""));
    let (id, rest) = args.split_once(' ').unwrap_or((args, ""));
    let request_id = || uuid::Uuid::new_v4().to_string();
    let confirmed = args.ends_with(" --confirm");
    let request = match command {
        "agents" => ("acp.agents.list", json!({}), false),
        "sessions" => ("acp.sessions.list", json!({}), false),
        "delegations" => (
            "acp.delegations.list",
            json!({"origin":origin?.as_str()}),
            false,
        ),
        "add" => (
            "acp.agents.save",
            json!({"agent":serde_json::from_str::<Value>(args).ok()?}),
            false,
        ),
        "edit" => (
            "acp.agents.save",
            json!({"id":id,"agent":serde_json::from_str::<Value>(rest).ok()?}),
            false,
        ),
        "trust" | "revoke" => (
            "acp.agents.trust",
            json!({
                "id":id,"revision":rest.trim_end_matches(" --confirm"),"allow":command=="trust",
            }),
            !confirmed,
        ),
        "remove" => (
            "acp.agents.remove",
            json!({"id":id,"revision":rest.trim_end_matches(" --confirm")}),
            !confirmed,
        ),
        "new" => (
            "acp.sessions.start",
            json!({"agent":id,"requestId":request_id()}),
            false,
        ),
        "delegate" => (
            "acp.delegations.start",
            json!({"agent":id,"requestId":request_id(),
            "origin":origin.map(SessionId::as_str),"text":rest,"continueWithClat":true,"delegation":true}),
            false,
        ),
        "send" => (
            "acp.sessions.action",
            json!({"id":id,"requestId":request_id(),"action":"prompt","text":rest}),
            false,
        ),
        "status" => ("acp.sessions.status", json!({"id":id}), false),
        "cancel" => ("acp.sessions.cancel", json!({"id":id}), false),
        "close" | "reopen" | "authenticate" | "logout" | "delete" | "clear_permissions" => (
            "acp.sessions.action",
            json!({
                "id":id,"requestId":request_id(),"action":command,"option":(!rest.is_empty()).then_some(rest),
            }),
            command == "delete" && !confirmed,
        ),
        _ => return advanced_request(command, id, rest),
    };
    Some(request)
}

fn advanced_request(command: &str, id: &str, rest: &str) -> Option<(&'static str, Value, bool)> {
    let request_id = || uuid::Uuid::new_v4().to_string();
    Some(match command {
        "remote" => (
            "acp.sessions.remote",
            json!({"agent":id,"cursor":(!rest.is_empty()).then_some(rest)}),
            false,
        ),
        "import" => (
            "acp.sessions.start",
            json!({"agent":id,"remote":rest,"requestId":request_id()}),
            false,
        ),
        "mode" => (
            "acp.sessions.action",
            json!({"id":id,"action":"mode","option":rest,"requestId":request_id()}),
            false,
        ),
        "config" => {
            let (option, value) = rest.split_once(' ')?;
            (
                "acp.sessions.action",
                json!({"id":id,"action":"config","option":option,
                "value":serde_json::from_str::<Value>(value).ok()?,"requestId":request_id()}),
                false,
            )
        }

        _ => return None,
    })
}

fn acp_info(value: Value, locale: LocaleContext) -> String {
    if let Some(list) = value.as_array().or_else(|| value["sessions"].as_array()) {
        let cursor = value["nextCursor"].as_str();
        if list.is_empty() {
            return locale.text("acp.empty_list").to_owned();
        }
        let mut rows = list
            .iter()
            .map(|row| {
                let identity = row["revision"].as_str().unwrap_or_default();
                let program = row["program"].as_str().unwrap_or_default();
                let mut text = format!(
                    "{}  {}  {}  {}",
                    row["label"]
                        .as_str()
                        .or_else(|| row["title"].as_str())
                        .unwrap_or_default(),
                    row["id"].as_str().unwrap_or_default(),
                    identity,
                    program
                );
                if row["trusted"] == false {
                    text.push_str(&format!("\n{}", locale.text("acp.authority")));
                }
                text
            })
            .collect::<Vec<_>>()
            .join("\n\n");
        if let Some(cursor) = cursor {
            rows.push_str(&format!(
                "\n\n{}",
                locale.render(&UiMessage::new("acp.cursor").arg("cursor", cursor))
            ));
        }
        return rows;
    }
    let phase = value["phase"]
        .as_str()
        .map(|p| locale.text(&format!("acp.phase_{p}")))
        .unwrap_or_default();
    let mut parts = vec![
        format!(
            "{}  {}",
            value["label"].as_str().unwrap_or_default(),
            value["id"].as_str().unwrap_or_default()
        ),
        phase.to_owned(),
    ];
    if let Some(error) = value["error"].as_str() {
        parts.push(error.to_owned());
    }
    if let Some(updates) = value["updates"].as_array() {
        parts.extend(
            updates
                .iter()
                .filter_map(|update| acp_update(update, locale)),
        );
    }
    parts.join("\n\n")
}

fn acp_update(update: &Value, locale: LocaleContext) -> Option<String> {
    let content = &update["content"];
    match update["sessionUpdate"].as_str()? {
        "plan" => {
            let rows = update["entries"]
                .as_array()?
                .iter()
                .filter_map(|entry| {
                    let key = match entry["status"].as_str()? {
                        "in_progress" => "acp.plan_in_progress",
                        "completed" => "acp.plan_completed",
                        _ => "acp.plan_pending",
                    };
                    Some(
                        locale.render(&UiMessage::new(key).arg("item", entry["content"].as_str()?)),
                    )
                })
                .collect::<Vec<_>>()
                .join("\n");
            Some(format!("{}\n{rows}", locale.text("acp.agent_plan")))
        }
        "usage_update" => Some(
            locale.render(
                &UiMessage::new("acp.usage")
                    .arg("used", update["used"].as_u64()?)
                    .arg("size", update["size"].as_u64()?),
            ),
        ),
        _ => content["text"]
            .as_str()
            .or_else(|| content["uri"].as_str())
            .or_else(|| content["resource"]["uri"].as_str())
            .or_else(|| update["title"].as_str())
            .or_else(|| update["message"].as_str())
            .map(str::to_owned)
            .or_else(|| {
                let key = match content["type"].as_str()? {
                    "audio" => "acp.agent_audio",
                    "image" => "acp.agent_image",
                    _ => return None,
                };
                Some(locale.text(key).to_owned())
            }),
    }
}

fn terminal_text(text: &str) -> String {
    text.chars()
        .filter(|c| *c == '\n' || *c == '\t' || !c.is_control())
        .collect()
}
