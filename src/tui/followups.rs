//! Presentation of the shared, ephemeral next-step suggestions.
use super::*;

impl App {
    pub(super) fn clear_followups(&mut self) {
        if self.followup_owner.take().is_some() {
            self.followup_epoch = self.followup_epoch.wrapping_add(1);
            self.conversation.clear_followups();
        }
    }

    pub(super) fn start_local_followups(&mut self) {
        if self.running || self.run_start_pending || self.dsh.is_some() {
            return;
        }
        let Some(session) = self.session_id.clone() else {
            return;
        };
        let Some(sender) = self.event_sender.clone() else {
            return;
        };
        let Some(app) = self.application.as_mut() else {
            return;
        };
        let run = uuid::Uuid::new_v4().to_string();
        app.register_followup_run(&run, true);
        let Ok(Some(prepared)) = app.prepare_followups(&run, false) else {
            return;
        };
        self.followup_epoch = self.followup_epoch.wrapping_add(1);
        self.followup_owner = Some(session.clone());
        let epoch = self.followup_epoch;
        thread::spawn(move || {
            let reply = prepared.generate();
            let _ = sender.send(UiEvent::Followups {
                epoch,
                session,
                native_identity: None,
                reply,
            });
        });
    }

    pub(super) fn restore_local_followups(&mut self) {
        let Some(session) = self.session_id.clone() else {
            return;
        };
        let Some(reply) = self.application.as_ref().map(|app| app.followup_snapshot()) else {
            return;
        };
        if reply.status == "ready" {
            self.followup_owner = Some(session);
            self.conversation.set_followups(reply.items);
        }
    }

    pub(super) fn restore_host_followups(&mut self) {
        self.load_host_followups(String::new(), false);
    }

    pub(super) fn start_host_followups(&mut self, run: String) {
        self.load_host_followups(run, true);
    }

    fn load_host_followups(&mut self, run: String, generate: bool) {
        let Some(native) = self.native.as_ref().filter(|native| native.online) else {
            return;
        };
        let Some(session) = self.session_id.clone() else {
            return;
        };
        let Some(sender) = self.event_sender.clone() else {
            return;
        };
        let client = native.client.clone();
        let identity = (native.epoch, native.selection);
        self.followup_epoch = self.followup_epoch.wrapping_add(1);
        self.followup_owner = Some(session.clone());
        let epoch = self.followup_epoch;
        thread::spawn(move || {
            let params = |action| {
                serde_json::json!({
                    "run_id":run,"action":action,"expected_selection_generation":identity.1
                })
            };
            // The Host publishes settlement before its worker has fully joined.
            let mut reply = client.call(
                "prompt.followups",
                &params(if generate { "generate" } else { "read" }),
            );
            let mut errors = 0;
            for _ in 0..120 {
                let pending = match &reply {
                    Ok(value) => value["status"] == "pending",
                    Err(_) if generate && errors < 4 => {
                        errors += 1;
                        true
                    }
                    Err(_) => false,
                };
                if !pending {
                    break;
                }
                thread::sleep(Duration::from_millis(250));
                reply = client.call(
                    "prompt.followups",
                    &params(if reply.is_err() && generate {
                        "generate"
                    } else {
                        "read"
                    }),
                );
            }
            let Ok(value) = reply else { return };
            let Ok(reply) = serde_json::from_value::<crate::application::FollowupReply>(value)
            else {
                return;
            };
            let _ = sender.send(UiEvent::Followups {
                epoch,
                session,
                native_identity: Some(identity),
                reply,
            });
        });
    }

    pub(super) fn accept_followups(
        &mut self,
        epoch: u64,
        session: SessionId,
        native_identity: Option<(u64, u64)>,
        reply: crate::application::FollowupReply,
    ) {
        if epoch != self.followup_epoch
            || self.running
            || self.run_start_pending
            || self.session_id.as_ref() != Some(&session)
            || self.native_selection_identity() != native_identity
        {
            return;
        }
        if native_identity.is_none()
            && self
                .application
                .as_ref()
                .is_none_or(|app| app.followup_snapshot().run_id != reply.run_id)
        {
            return;
        }
        if reply.status == "ready" {
            self.conversation.set_followups(reply.items);
        }
    }

    pub(super) fn handle_followups_key(&mut self, key: KeyEvent) -> bool {
        let Some(choice) = self.conversation.followups_key(key.code) else {
            if key.code == KeyCode::F(8) {
                self.flash_message(UiMessage::new("tui.no_followups"));
                return true;
            }
            return false;
        };
        if let Some(text) = choice {
            self.input.end();
            if !self.input.text().is_empty() {
                self.input.insert_str("\n\n");
            }
            self.input.insert_str(&text);
        }
        if key.code == KeyCode::F(8) {
            self.conversation_scroll_from_bottom = 0;
        }
        true
    }
}

#[cfg(all(test, feature = "runtime-tests"))]
mod tests {
    use super::*;
    use crate::tui::snapshot_tests::Harness;

    #[test]
    fn followup_choice_appends_to_draft_without_submitting() {
        let mut harness = Harness::trusted("followup-draft", 80, 24);
        let app = &mut harness.app;
        app.input.insert_str("existing draft");
        app.conversation.set_followups(vec!["next step".into()]);
        assert!(app.handle_followups_key(KeyEvent::from(KeyCode::F(8))));
        assert!(app.handle_followups_key(KeyEvent::from(KeyCode::Enter)));
        assert_eq!(app.input.text(), "existing draft\n\nnext step");
        assert!(!app.running);
        assert!(!app.run_start_pending);
    }

    #[test]
    fn local_settings_change_clears_invalidated_suggestions() {
        let mut harness = Harness::trusted("followup-settings", 80, 24);
        let app = &mut harness.app;
        let session = SessionId::new("current");
        app.session_id = Some(session.clone());
        app.followup_owner = Some(session);
        app.conversation.set_followups(vec!["stale".into()]);
        let epoch = app.followup_epoch;
        app.handle_ui_event(UiEvent::Application(ApplicationEvent::ModelsUpdated));
        assert!(app.followup_owner.is_none());
        assert!(app.followup_epoch > epoch);
        assert!(app.handle_followups_key(KeyEvent::from(KeyCode::F(8))));
        assert!(app.status_presentation.is_some());
    }
}
