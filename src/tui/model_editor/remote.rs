//! Remote forms carry only fields accepted by the write-only host contract.
use super::*;

impl ModelEditor {
    pub(super) fn remote_rows(&self) -> Vec<RowKind> {
        use RowKind::*;
        if self.preset.is_some() {
            return vec![ApiKey, Save, Cancel];
        }
        let mut rows = vec![Name, Model, Endpoint, ApiKey, Protocol, RequestPath];
        if self.remote_limits {
            rows.extend([ContextWindow, OutputLimit, SpendBudget]);
        }
        let advanced = [
            (self.remote_tuning.is_some(), vec![Temperature, Parallel]),
            (
                self.remote_tuning.is_some() && self.remote_body != Some(true),
                vec![Thinking],
            ),
            (self.remote_auth.is_some(), vec![AuthHeader, AuthPrefix]),
            (self.remote_headers.is_some(), vec![ExtraHeaders]),
            (self.remote_body.is_some(), vec![ExtraBody]),
        ];
        if advanced.iter().any(|(enabled, _)| *enabled) {
            rows.push(Advanced);
            if self.show_advanced {
                rows.extend(
                    advanced
                        .into_iter()
                        .filter(|(enabled, _)| *enabled)
                        .flat_map(|(_, rows)| rows),
                );
            }
        }
        rows.extend([Save, Cancel]);
        rows
    }

    pub(super) fn editor_title(&self) -> String {
        match self.preset.filter(|_| self.remote) {
            Some(preset) => format!("/model · {} · API key", preset.name),
            None => "/model".into(),
        }
    }

    pub(super) fn editor_footer(&self) -> &'static str {
        if self.is_remote_preset() {
            "Enter edit/confirm · Ctrl+S save and activate · Esc back"
        } else if self.remote {
            "↑↓ select · Enter edit/confirm · ←/→ cycle · Ctrl+D clear · Ctrl+S save · Esc back"
        } else {
            "↑↓ select · Enter edit · ←/→ cycle · Ctrl+D clear · Ctrl+S save · Esc cancel"
        }
    }

    pub(super) fn key_summary(&self) -> String {
        if self.remote && !self.remote_key_edited {
            "host key hidden · unchanged".into()
        } else if self.remote && self.credentials.value(0).is_none_or(str::is_empty) {
            "clear on save".into()
        } else {
            self.credentials.masked_value(0)
        }
    }

    pub(crate) fn remote_revision(&self) -> u64 {
        self.remote_revision
    }

    pub(crate) fn is_remote_preset(&self) -> bool {
        self.remote && self.preset.is_some()
    }

    pub(crate) fn remote_preset_key(preset: &'static ModelPreset) -> Self {
        let config = ModelConfig::default();
        let mut editor = Self::new_with_descriptors(
            &config,
            ProviderCredentials::for_protocol(config.protocol),
            vec![],
        );
        editor.remote = true;
        editor.apply_preset_and_focus_key(preset);
        editor
    }

    pub(crate) fn remote_profile(name: &str, route: &Value) -> Result<Self, String> {
        if route["route_redacted"] == true {
            return Err("profile route contains hidden fields; use the standalone editor".into());
        }
        let mut config = if route.is_null() {
            ModelConfig::default()
        } else {
            ModelConfig {
                protocol: serde_json::from_value(route["protocol"].clone())
                    .map_err(|_| "invalid host profile protocol")?,
                model: route["model"].as_str().ok_or("invalid host model")?.into(),
                endpoint: route["endpoint"]
                    .as_str()
                    .ok_or("invalid host endpoint")?
                    .into(),
                request_path: route["request_path"]
                    .as_str()
                    .ok_or("invalid host path")?
                    .into(),
                ..Default::default()
            }
        };
        let limits = &route["limits"];
        if limits.is_object() {
            config.output_limit = serde_json::from_value(limits["output_limit"].clone())
                .map_err(|_| "invalid host output limit")?;
            config.max_context_tokens =
                serde_json::from_value(limits["max_context_tokens"].clone())
                    .map_err(|_| "invalid host context limit")?;
            config.run_token_budget = serde_json::from_value(limits["run_token_budget"].clone())
                .map_err(|_| "invalid host run budget")?;
        }
        Self::read_remote_tuning(&mut config, &route["tuning"])?;
        let credentials = ProviderCredentials::for_protocol(config.protocol);
        let mut editor = Self::for_profile(name, &config, credentials, vec![]);
        editor.remote = true;
        editor.remote_limits = route.is_null() || limits.is_object();
        if route.is_null() || route["extra_body_edit_supported"] == true {
            editor.remote_body = Some(false);
            editor.extra_body.clear();
        }
        if route.is_null() || route["extra_headers_edit_supported"] == true {
            editor.remote_headers = Some(false);
            editor.extra_headers.clear();
        }
        if route.is_null() || route["auth_edit_supported"] == true {
            editor.remote_auth = Some(serde_json::json!({}));
            editor.auth_header.clear();
            editor.auth_prefix.clear();
        }
        if route.is_null() || route["tuning"].is_object() {
            editor.remote_tuning = Some(serde_json::json!({
                "values":editor.remote_tuning_values()?, "route":editor.remote_route_identity()
            }));
        }
        Ok(editor)
    }

    pub(super) fn remote_save_action(&mut self) -> EditorAction {
        if let Some(preset) = self.preset {
            let mut params = serde_json::json!({"id": preset.id});
            if self.remote_key_edited {
                params["api_key"] = self.credentials.value(0).unwrap_or_default().into();
            }
            return EditorAction::SaveRemotePreset(params);
        }
        let mut params = serde_json::json!({
            "name": self.profile.as_ref().map(|profile| profile.name.trim()).unwrap_or_default(),
            "protocol": self.protocol, "model": self.model.trim(),
            "endpoint": self.endpoint.trim(), "request_path": self.request_path.trim(),
        });
        if self.remote_key_edited {
            params["api_key"] = Value::String(self.credentials.value(0).unwrap_or_default().into());
        }
        if let Some(auth) = &self.remote_auth
            && auth.as_object().is_some_and(|fields| !fields.is_empty())
        {
            params["auth"] = auth.clone();
        }
        if self.remote_limits {
            match self.remote_limit_values() {
                Ok(limits) => params["limits"] = limits,
                Err(error) => {
                    self.error = Some(error);
                    return EditorAction::Continue;
                }
            }
        }
        if let Err(error) = self.append_remote_tuning(&mut params) {
            self.error = Some(error);
            return EditorAction::Continue;
        }
        if let Err(error) = self.append_remote_json(&mut params) {
            self.error = Some(error);
            return EditorAction::Continue;
        }
        EditorAction::SaveRemoteProfile(params)
    }

    fn remote_limit_values(&self) -> Result<Value, String> {
        let output = if self.override_is_clear(RowKind::OutputLimit) {
            None
        } else if self.output_choice == CHOICE_CUSTOM {
            parse_optional_u32(&self.output_limit, "Max Output Tokens")?
        } else {
            Some(OUTPUT_CHOICES[self.output_choice])
        };
        let context = if self.override_is_clear(RowKind::ContextWindow) {
            None
        } else if self.context_choice == CHOICE_CUSTOM {
            parse_optional_u32(&self.context_window, "Context Window")?
        } else {
            Some(CONTEXT_CHOICES[self.context_choice])
        };
        let budget = if self.budget_choice == CHOICE_CUSTOM {
            parse_optional_u64(&self.spend_budget, "Spend Budget")?
        } else {
            BUDGET_CHOICES[self.budget_choice]
        };
        if output == Some(0) || context.is_some_and(|value| value < 4096) {
            return Err("Output must be positive; context must be at least 4096".into());
        }
        Ok(serde_json::json!({
            "output_limit":output, "max_context_tokens":context, "run_token_budget":budget
        }))
    }

    pub(crate) fn clear_remote_key(&mut self) {
        self.clear_remote_auth();
        self.clear_remote_json();
        self.credentials.set_value(0, String::new());
        self.remote_key_edited = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remote_extra_body_is_explicit_and_masks_its_draft() {
        let route = serde_json::json!({"protocol":"open_ai_compatible", "model":"test",
            "endpoint":"https://api.deepseek.com/v1", "request_path":"/chat/completions",
            "extra_body_edit_supported":true,"extra_body":{"secret":"never-read"},
            "tuning":{"thinking_level":{"set":"max"}}});
        let mut editor = ModelEditor::remote_profile("body", &route).unwrap();
        editor.show_advanced = true;
        assert!(
            editor.visible_rows().contains(&RowKind::ExtraBody),
            "attach extra body missing"
        );
        assert!(editor.current_value(EditTarget::ExtraBody).is_empty());
        editor.commit_edit(EditTarget::Name, "body-new".into());
        editor.commit_edit(
            EditTarget::ExtraBody,
            "{\"reasoning_effort\":\"low\",\"metadata\":{\"secret\":true}}".into(),
        );
        let EditorAction::SaveRemoteProfile(params) = editor.save_action() else {
            panic!()
        };
        assert_eq!(params["extra_body"]["reasoning_effort"], "low");
        assert!(params["tuning"].get("thinking_level").is_none());
        assert!(!editor.visible_rows().contains(&RowKind::Thinking));
        editor.clear_remote_key();
        assert!(editor.current_value(EditTarget::ExtraBody).is_empty());
        let EditorAction::SaveRemoteProfile(params) = editor.save_action() else {
            panic!()
        };
        assert!(params.get("extra_body").is_none());
        assert!(
            params["tuning"].get("thinking_level").is_none(),
            "dispatch must not turn raw-body reset into a later Clear patch"
        );
        for invalid in ["", "[]", "secret-invalid"] {
            editor.commit_edit(EditTarget::ExtraBody, invalid.into());
            assert!(matches!(editor.save_action(), EditorAction::Continue));
            assert!(!editor.error.as_ref().unwrap().contains("secret-invalid"));
        }
        editor.commit_edit(EditTarget::ExtraBody, "{}".into());
        let EditorAction::SaveRemoteProfile(params) = editor.save_action() else {
            panic!()
        };
        assert_eq!(params["extra_body"], serde_json::json!({}));
    }

    #[test]
    fn remote_extra_headers_require_explicit_json_and_never_read_host_values() {
        let route = serde_json::json!({"protocol":"open_ai_compatible","model":"test",
            "endpoint":"https://example.invalid/v1","request_path":"/chat/completions",
            "extra_headers_edit_supported":true,"extra_headers":{"X-Secret":"never-read"}});
        let mut editor = ModelEditor::remote_profile("headers", &route).unwrap();
        editor.show_advanced = true;
        assert!(
            editor.visible_rows().contains(&RowKind::ExtraHeaders),
            "attach extra headers missing"
        );
        assert!(editor.current_value(EditTarget::ExtraHeaders).is_empty());
        let EditorAction::SaveRemoteProfile(params) = editor.save_action() else {
            panic!()
        };
        assert!(params.get("extra_headers").is_none());
        for input in ["", "[]", "broken-secret", "{\"X\":false}"] {
            editor.commit_edit(EditTarget::ExtraHeaders, input.into());
            assert!(matches!(editor.save_action(), EditorAction::Continue));
            assert!(!editor.error.as_ref().unwrap().contains("broken-secret"));
        }
        editor.commit_edit(
            EditTarget::ExtraHeaders,
            "{\"X-New\":\"replacement-secret\"}".into(),
        );
        let EditorAction::SaveRemoteProfile(params) = editor.save_action() else {
            panic!()
        };
        assert_eq!(
            params["extra_headers"],
            serde_json::json!({"X-New":"replacement-secret"})
        );
        assert!(
            !editor
                .row_label(RowKind::ExtraHeaders)
                .1
                .contains("replacement-secret")
        );
        editor.clear_remote_key();
        assert!(editor.current_value(EditTarget::ExtraHeaders).is_empty());
        let EditorAction::SaveRemoteProfile(params) = editor.save_action() else {
            panic!()
        };
        assert!(params.get("extra_headers").is_none());
        editor.commit_edit(EditTarget::ExtraHeaders, "{}".into());
        let EditorAction::SaveRemoteProfile(params) = editor.save_action() else {
            panic!()
        };
        assert_eq!(params["extra_headers"], serde_json::json!({}));
    }

    #[test]
    fn remote_auth_edits_are_write_only_and_explicit() {
        assert_masked_remote_field(RowKind::AuthPrefix);
        let route = serde_json::json!({
            "protocol":"open_ai_compatible", "model":"test", "endpoint":"https://example.invalid/v1",
            "request_path":"/chat/completions", "auth_edit_supported":true
        });
        let mut editor = ModelEditor::remote_profile("auth", &route).unwrap();
        editor.show_advanced = true;
        assert!(
            editor.visible_rows().contains(&RowKind::AuthPrefix),
            "attach auth editing missing"
        );
        assert!(editor.current_value(EditTarget::AuthPrefix).is_empty());
        let EditorAction::SaveRemoteProfile(params) = editor.save_action() else {
            panic!()
        };
        assert!(params.get("auth").is_none());
        editor.commit_edit(EditTarget::AuthPrefix, "Token secret ".into());
        assert!(!editor.auth_row_label(true).1.contains("secret"));
        let EditorAction::SaveRemoteProfile(params) = editor.save_action() else {
            panic!()
        };
        assert_eq!(
            params["auth"],
            serde_json::json!({"prefix":"Token secret "})
        );
        editor.commit_edit(EditTarget::AuthHeader, "".into());
        let EditorAction::SaveRemoteProfile(params) = editor.save_action() else {
            panic!()
        };
        assert_eq!(params["auth"]["header"], "");
        editor.clear_remote_key();
        assert!(editor.current_value(EditTarget::AuthPrefix).is_empty());
        let EditorAction::SaveRemoteProfile(params) = editor.save_action() else {
            panic!()
        };
        assert!(params.get("auth").is_none());
        let mut old_route = route;
        old_route
            .as_object_mut()
            .unwrap()
            .remove("auth_edit_supported");
        let mut old = ModelEditor::remote_profile("auth", &old_route).unwrap();
        old.show_advanced = true;
        assert!(!old.visible_rows().contains(&RowKind::AuthPrefix));
    }

    fn tuning_editor() -> ModelEditor {
        ModelEditor::remote_profile("tuning", &serde_json::json!({
            "protocol":"open_ai_compatible", "model":"test", "endpoint":"https://example.invalid/v1",
            "request_path":"/chat/completions",
            "tuning":{"temperature":{"set":0.5},"parallel_tool_calls":{"set":false},"thinking_level":{"set":"high"}}
        })).unwrap()
    }

    #[test]
    fn remote_tuning_uses_existing_controls_and_only_sends_changed_fields() {
        let mut editor = tuning_editor();
        assert!(
            editor.visible_rows().contains(&RowKind::Advanced),
            "attach must expose tuning controls"
        );
        editor.selected = editor
            .visible_rows()
            .iter()
            .position(|row| *row == RowKind::Advanced)
            .unwrap();
        editor.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        for row in [RowKind::Temperature, RowKind::Parallel, RowKind::Thinking] {
            assert!(editor.visible_rows().contains(&row));
        }
        let EditorAction::SaveRemoteProfile(params) = editor.save_action() else {
            panic!()
        };
        assert!(
            params.get("tuning").is_none(),
            "untouched fields must not be written"
        );
        editor.selected = editor
            .visible_rows()
            .iter()
            .position(|row| *row == RowKind::Parallel)
            .unwrap();
        editor.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        let EditorAction::SaveRemoteProfile(params) = editor.save_action() else {
            panic!()
        };
        assert_eq!(
            params["tuning"],
            serde_json::json!({"parallel_tool_calls":{"set":true}})
        );
        editor.handle_key(KeyEvent::new(KeyCode::Char('d'), KeyModifiers::CONTROL));
        editor.selected = editor
            .visible_rows()
            .iter()
            .position(|row| *row == RowKind::Thinking)
            .unwrap();
        editor.handle_key(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE));
        let EditorAction::SaveRemoteProfile(params) = editor.save_action() else {
            panic!()
        };
        assert_eq!(
            params["tuning"],
            serde_json::json!({"parallel_tool_calls":"clear","thinking_level":{"set":"max"}})
        );
        editor.commit_edit(EditTarget::Temperature, "NaN".into());
        assert!(matches!(editor.save_action(), EditorAction::Continue));
        editor.commit_edit(EditTarget::Temperature, "0.75".into());
        let EditorAction::SaveRemoteProfile(params) = editor.save_action() else {
            panic!()
        };
        assert_eq!(
            params["tuning"]["temperature"],
            serde_json::json!({"set":0.75})
        );
    }

    #[test]
    fn remote_tuning_carries_visible_values_when_saving_a_new_route() {
        for field in [
            EditTarget::Name,
            EditTarget::Model,
            EditTarget::Endpoint,
            EditTarget::RequestPath,
        ] {
            let mut editor = tuning_editor();
            editor.commit_edit(field, "changed".into());
            let EditorAction::SaveRemoteProfile(params) = editor.save_action() else {
                panic!()
            };
            assert_eq!(
                params["tuning"],
                serde_json::json!({
                    "temperature":{"set":0.5}, "parallel_tool_calls":{"set":false}, "thinking_level":{"set":"high"}
                })
            );
            assert!(params.get("api_key").is_none());
            assert!(params.get("extra_body").is_none());
        }
        let mut editor = tuning_editor();
        editor.selected = 4; // protocol row
        editor.handle_key(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE));
        let EditorAction::SaveRemoteProfile(params) = editor.save_action() else {
            panic!()
        };
        assert!(params["tuning"].is_object());
    }

    #[test]
    fn remote_profile_limits_are_editable_and_invalid_input_stays_local() {
        let mut editor = ModelEditor::remote_profile("limits", &serde_json::json!({
            "protocol":"open_ai_compatible", "model":"test", "endpoint":"https://example.invalid/v1",
            "request_path":"/chat/completions",
            "limits":{"output_limit":8192,"max_context_tokens":131072,"run_token_budget":0}
        })).unwrap();
        assert!(
            editor.visible_rows().contains(&RowKind::OutputLimit),
            "attach must expose numeric limits"
        );
        let EditorAction::SaveRemoteProfile(params) = editor.save_action() else {
            panic!()
        };
        assert_eq!(
            params["limits"],
            serde_json::json!({"output_limit":8192,"max_context_tokens":131072,"run_token_budget":0})
        );
        editor.selected = editor
            .visible_rows()
            .iter()
            .position(|row| *row == RowKind::OutputLimit)
            .unwrap();
        editor.handle_key(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE));
        let EditorAction::SaveRemoteProfile(params) = editor.save_action() else {
            panic!()
        };
        assert_ne!(params["limits"]["output_limit"], 8192);
        editor.handle_key(KeyEvent::new(KeyCode::Char('d'), KeyModifiers::CONTROL));
        let EditorAction::SaveRemoteProfile(params) = editor.save_action() else {
            panic!()
        };
        assert!(params["limits"]["output_limit"].is_null());
        editor.context_choice = CHOICE_CUSTOM;
        editor.commit_edit(EditTarget::ContextWindow, "bad".into());
        assert!(matches!(editor.save_action(), EditorAction::Continue));
        assert!(editor.error.is_some());
    }

    #[test]
    fn remote_preset_key_is_bound_to_id_and_only_sends_confirmed_input() {
        let preset = preset_by_id("deepseek-flash").unwrap();
        let mut editor = ModelEditor::remote_preset_key(preset);
        assert_eq!(
            editor.visible_rows(),
            vec![RowKind::ApiKey, RowKind::Save, RowKind::Cancel]
        );
        let EditorAction::SaveRemotePreset(params) = editor.remote_save_action() else {
            panic!()
        };
        assert_eq!(params, serde_json::json!({"id":preset.id}));
        editor.handle_paste("new-secret");
        assert!(matches!(
            editor.handle_key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL)),
            EditorAction::Continue
        ));
        editor.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        let EditorAction::SaveRemotePreset(params) = editor.remote_save_action() else {
            panic!()
        };
        assert_eq!(
            params,
            serde_json::json!({"id":preset.id,"api_key":"new-secret"})
        );
        editor.commit_edit(EditTarget::ApiKey, String::new());
        let EditorAction::SaveRemotePreset(params) = editor.remote_save_action() else {
            panic!()
        };
        assert_eq!(params["api_key"], "");
        editor.clear_remote_key();
        let EditorAction::SaveRemotePreset(params) = editor.remote_save_action() else {
            panic!()
        };
        assert!(params.get("api_key").is_none());
        let mut local = ModelPicker::new(&ModelConfig::default(), vec![]);
        local.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(matches!(
            local.handle_key(KeyEvent::new(KeyCode::Char('e'), KeyModifiers::NONE)),
            PickerAction::Continue
        ));
        assert!(matches!(
            local.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
            PickerAction::SelectPreset(_)
        ));
    }

    #[test]
    fn remote_profile_key_popup_is_masked_and_requires_confirmation() {
        assert_masked_remote_field(RowKind::ApiKey);
    }

    fn assert_masked_remote_field(kind: RowKind) {
        let mut editor = ModelEditor::remote_profile("new", &Value::Null).unwrap();
        editor.show_advanced = true;
        editor.selected = editor
            .visible_rows()
            .iter()
            .position(|row| *row == kind)
            .unwrap();
        editor.handle_paste(
            if matches!(kind, RowKind::ExtraHeaders | RowKind::ExtraBody) {
                "{\"X\":\"secret-visible-nowhere\"}"
            } else {
                "secret-visible-nowhere"
            },
        );
        assert!(matches!(
            editor.handle_key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL)),
            EditorAction::Continue
        ));
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(100, 30)).unwrap();
        editor.remote = false;
        terminal
            .draw(|frame| editor.draw(frame, frame.area()))
            .unwrap();
        let unmasked: String = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect();
        assert!(
            unmasked.contains("secret-visible-nowhere"),
            "render assertion must observe the popup content"
        );
        editor.remote = true;
        terminal
            .draw(|frame| editor.draw(frame, frame.area()))
            .unwrap();
        let rendered: String = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect();
        assert!(!rendered.contains("secret-visible-nowhere"));
        assert!(rendered.contains("*********************"));
        editor.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        let EditorAction::SaveRemoteProfile(params) = editor.save_action() else {
            panic!("remote save")
        };
        let saved = if kind == RowKind::ApiKey {
            &params["api_key"]
        } else if kind == RowKind::ExtraHeaders {
            &params["extra_headers"]["X"]
        } else if kind == RowKind::ExtraBody {
            &params["extra_body"]["X"]
        } else {
            &params["auth"]["prefix"]
        };
        assert_eq!(saved, "secret-visible-nowhere");
    }

    #[test]
    fn remote_extra_headers_popup_masks_draft() {
        assert_masked_remote_field(RowKind::ExtraHeaders);
    }

    #[test]
    fn remote_extra_body_popup_masks_draft() {
        assert_masked_remote_field(RowKind::ExtraBody);
    }

    #[test]
    fn remote_profile_never_serializes_unedited_key_or_hidden_settings() {
        assert!(
            ModelEditor::remote_profile(
                "secret-route",
                &serde_json::json!({"route_redacted":true})
            )
            .is_err()
        );
        let mut editor = ModelEditor::remote_profile("remote", &serde_json::json!({
            "protocol":"open_ai_compatible", "model":"test", "endpoint":"https://example.invalid/v1",
            "request_path":"/chat/completions", "credential_set":true,
            "extra_headers":{"authorization":"must-not-read"}
        })).unwrap();
        let EditorAction::SaveRemoteProfile(params) = editor.save_action() else {
            panic!("remote save")
        };
        assert!(params.get("api_key").is_none());
        assert!(params.get("extra_headers").is_none());
        assert!(
            params.get("limits").is_none(),
            "old host has no limits capability"
        );
        assert!(!editor.visible_rows().contains(&RowKind::ExtraHeaders));
        editor.commit_edit(EditTarget::ApiKey, "new-secret".into());
        let EditorAction::SaveRemoteProfile(params) = editor.save_action() else {
            panic!("remote save")
        };
        assert_eq!(params["api_key"], "new-secret");
        editor.commit_edit(EditTarget::ApiKey, String::new());
        let EditorAction::SaveRemoteProfile(params) = editor.save_action() else {
            panic!("remote save")
        };
        assert_eq!(params["api_key"], "");
        editor.clear_remote_key();
        let EditorAction::SaveRemoteProfile(params) = editor.save_action() else {
            panic!("remote save")
        };
        assert!(params.get("api_key").is_none());
    }
}
