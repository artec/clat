use super::*;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;

fn shell() -> (App, PathBuf) {
    let (storage, project) = crate::test_support::roots("native-shell");
    std::fs::create_dir_all(&storage).unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    // Protocol fixture, not a second runtime owner. Tests never touch the
    // user's storage, launch providers, or connect to an existing host.
    let description = json!({"ok":true,"value":{
        "storage_root":storage, "protocol_version":1,"wire_version":1,
        "journal_version":2,"instance_id":"11111111-1111-1111-1111-111111111111"
    }})
    .to_string();
    let server = thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut reader = BufReader::new(stream);
        loop {
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            if line == "\r\n" {
                break;
            }
        }
        reader.read_exact(&mut [0; 2]).unwrap();
        write!(
            reader.get_mut(),
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            description.len(),
            description
        )
        .unwrap();
    });
    let client = HostClient::connect(port, "test-token".into(), &storage).unwrap();
    server.join().unwrap();
    let mut app = App::open_native(Project::new(&project), client).unwrap();
    app.focused = Some(true);
    (app, storage)
}

fn clipboard_png() -> Vec<u8> {
    let mut png = Vec::new();
    image::DynamicImage::ImageRgba8(image::RgbaImage::new(2, 2))
        .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
        .unwrap();
    png
}

fn question_notice(id: &str) -> Value {
    json!({"kind":"question_requested","payload":{"rpc_id":id,"question":{
        "question":id,"options":[{"label":"stable","description":null}],"allow_custom":true
    }}})
}

#[test]
fn native_question_queue_deduplicates_and_resolution_never_closes_another_question() {
    let (mut app, storage) = shell();
    let (tx, rx) = mpsc::sync_channel(16);
    app.event_sender = Some(tx);
    for id in ["first", "first", "second"] {
        app.native_control("notice", question_notice(id));
    }
    assert_eq!(
        app.pending_ask_user.as_ref().unwrap().question.question,
        "first"
    );
    let resolve = |id| json!({"kind":"question_resolved","payload":{"rpc_id":id}});
    app.native_control("notice", resolve("first"));
    assert_eq!(
        app.pending_ask_user.as_ref().unwrap().question.question,
        "second"
    );
    app.native_control("notice", resolve("first"));
    app.native_question_reply(
        0,
        "first",
        crate::interaction::AskAnswer::Declined,
        Err("late".into()),
    );
    assert_eq!(
        app.pending_ask_user.as_ref().unwrap().question.question,
        "second"
    );
    app.handle_native_event(NativeEvent::Offline(0, "disconnected".into()));
    assert!(app.pending_ask_user.is_none());
    assert!(
        rx.try_recv().is_err(),
        "closing local dialogs must not send decline replies"
    );
    drop(app);
    crate::test_support::cleanup_tree(&storage);
}

#[test]
fn native_question_failed_reply_keeps_answer_without_automatic_resend() {
    let (mut app, storage) = shell();
    let (tx, rx) = mpsc::sync_channel(16);
    app.event_sender = Some(tx);
    app.native_control("notice", question_notice("first"));
    app.pending_ask_user.as_mut().unwrap().custom = Some("my answer".into());
    let dialog = app.pending_ask_user.take().unwrap();
    dialog
        .answer_tx
        .send(crate::interaction::AskAnswer::Custom(
            dialog.custom.unwrap(),
        ))
        .unwrap();
    assert!(app.pending_ask_user.is_none());
    let UiEvent::Native(event) = rx.recv_timeout(Duration::from_secs(2)).unwrap() else {
        panic!()
    };
    assert!(
        matches!(&event, NativeEvent::QuestionReply(0, id, crate::interaction::AskAnswer::Custom(text), Err(_)) if id == "first" && text == "my answer")
    );
    app.handle_native_event(event);
    assert_eq!(
        app.pending_ask_user.as_ref().unwrap().custom.as_deref(),
        Some("my answer")
    );
    app.native_question_reply(
        1,
        "first",
        crate::interaction::AskAnswer::Declined,
        Ok(json!({})),
    );
    assert!(app.pending_ask_user.is_some());
    assert!(rx.try_recv().is_err());
    drop(app);
    crate::test_support::cleanup_tree(&storage);
}

#[test]
fn native_preset_key_entry_opens_without_selecting_or_reading_credentials() {
    use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    let (mut app, storage) = shell();
    app.native.as_mut().unwrap().online = true;
    let choices = crate::host::HostModelChoices {
        settings: serde_json::from_value(json!({
            "current": {"protocol":"open_ai_compatible", "model":"", "endpoint":"",
                "request_path":"/chat/completions", "credential_set":true,
                "advanced_settings_present":false},
            "active_profile":null, "presets":[], "profiles":[]
        }))
        .unwrap(),
        profiles: vec![],
    };
    app.native_models(0, 0, Ok(choices));
    let picker = app.picker.as_mut().unwrap();
    picker.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    let action = picker.handle_key(KeyEvent::new(KeyCode::Char('e'), KeyModifiers::NONE));
    app.apply_picker_action(action);
    assert!(
        app.editor.is_some(),
        "attach preset e must open a key editor"
    );
    assert!(!app.native.as_ref().unwrap().models_pending);
    assert!(app.application.is_none());
    drop(app);
    crate::test_support::cleanup_tree(&storage);
}

#[test]
fn native_profile_editor_opens_without_a_local_application() {
    let (mut app, storage) = shell();
    app.native.as_mut().unwrap().online = true;
    app.apply_picker_action(crate::tui::model_editor::PickerAction::OpenProfileEditor {
        edit: None,
    });
    assert!(
        app.editor.is_some(),
        "attach must open a new profile editor"
    );
    assert!(app.application.is_none());
    drop(app);
    crate::test_support::cleanup_tree(&storage);
}

#[test]
fn native_profile_save_is_single_flight_and_keeps_newer_editor_input() {
    model_editor_save_lifetime(false);
}

#[test]
fn native_preset_key_save_is_single_flight_and_keeps_newer_editor_input() {
    model_editor_save_lifetime(true);
}

fn model_editor_save_lifetime(preset: bool) {
    use crate::tui::model_editor::EditorAction;
    let (mut app, storage) = shell();
    app.native.as_mut().unwrap().online = true;
    if preset {
        app.open_native_preset_key(crate::preset_by_id("deepseek-flash").unwrap());
    } else {
        app.open_native_profile(None);
    }
    let request = app.native.as_ref().unwrap().editor_request;
    let (tx, rx) = mpsc::sync_channel(4);
    app.event_sender = Some(tx);
    let action = || {
        if preset {
            EditorAction::SaveRemotePreset(json!({"id":"deepseek-flash", "api_key":"secret"}))
        } else {
            EditorAction::SaveRemoteProfile(json!({"name":"new", "api_key":"secret"}))
        }
    };
    app.apply_editor_action(action());
    app.apply_editor_action(action());
    app.apply_picker_action(crate::tui::model_editor::PickerAction::SelectPreset(
        crate::preset_by_id("deepseek-flash").unwrap(),
    ));
    assert!(!app.native.as_ref().unwrap().models_pending);
    let UiEvent::Native(event) = rx.recv_timeout(Duration::from_secs(2)).expect("host save") else {
        panic!("native reply")
    };
    assert!(matches!(&event, NativeEvent::ProfileSaved(_, _, Err(_))));
    assert!(rx.try_recv().is_err());
    app.handle_native_event(event);
    assert!(app.editor.is_some(), "failed save retains non-secret form");
    app.editor.as_mut().unwrap().handle_paste("newer draft");
    app.native_profile_saved(0, request, Ok(json!({"saved":true})));
    assert!(
        app.editor.is_some(),
        "late success cannot close newer input"
    );
    app.native.as_mut().unwrap().epoch = 2;
    app.native.as_mut().unwrap().editor_pending = true;
    app.native_profile_saved(0, request, Ok(json!({})));
    assert!(app.native.as_ref().unwrap().editor_pending);
    drop(app);
    crate::test_support::cleanup_tree(&storage);
}

#[test]
fn native_profile_load_cannot_replace_newer_editor() {
    let (mut app, storage) = shell();
    app.native.as_mut().unwrap().online = true;
    app.open_native_profile(None);
    let request = app.native.as_ref().unwrap().editor_request;
    app.open_native_profile(None);
    app.native_profile_loaded(
        0,
        request,
        "old",
        Ok(json!({
            "protocol":"open_ai_compatible", "model":"old", "endpoint":"https://example.invalid",
            "request_path":"/chat/completions"
        })),
    );
    let action = app
        .editor
        .as_mut()
        .unwrap()
        .handle_key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL));
    let crate::tui::model_editor::EditorAction::SaveRemoteProfile(params) = action else {
        panic!("remote form")
    };
    assert_eq!(params["name"], "");
    drop(app);
    crate::test_support::cleanup_tree(&storage);
}

#[test]
fn native_model_selection_uses_host_and_retains_picker_on_failure() {
    let (mut app, storage) = shell();
    app.native.as_mut().unwrap().online = true;
    app.picker = Some(crate::tui::model_editor::ModelPicker::new(
        &app.config,
        vec![],
    ));
    let (tx, rx) = mpsc::sync_channel(4);
    app.event_sender = Some(tx);
    app.apply_picker_action(crate::tui::model_editor::PickerAction::SwitchProfile(
        "selected".into(),
    ));
    let UiEvent::Native(event) = rx
        .recv_timeout(Duration::from_secs(2))
        .expect("host request must be sent without a local Application")
    else {
        panic!("native response");
    };
    assert!(matches!(&event, NativeEvent::ModelChanged(_, _, Err(_))));
    app.handle_native_event(event);
    assert!(app.picker.is_some(), "transport failure keeps the picker");
    assert!(!app.native.as_ref().unwrap().models_pending);
    assert!(app.application.is_none());
    drop(app);
    crate::test_support::cleanup_tree(&storage);
}

#[test]
fn native_model_thinking_never_claims_a_local_only_configuration_change() {
    let (mut app, storage) = shell();
    crate::presets::MODEL_PRESETS[0].apply(&mut app.config);
    let before = serde_json::to_value(&app.config).unwrap();
    app.cycle_thinking_level();
    assert_eq!(serde_json::to_value(&app.config).unwrap(), before);
    drop(app);
    crate::test_support::cleanup_tree(&storage);
}

#[test]
fn native_thinking_sends_once_and_ignores_previous_connection_reply() {
    let (mut app, storage) = shell();
    app.native.as_mut().unwrap().online = true;
    let before = serde_json::to_value(&app.config).unwrap();
    let (tx, rx) = mpsc::sync_channel(4);
    app.event_sender = Some(tx);
    app.cycle_thinking_level();
    app.cycle_thinking_level();
    let UiEvent::Native(event) = rx
        .recv_timeout(Duration::from_secs(2))
        .expect("Shift+Tab must send a host request")
    else {
        panic!("native reply")
    };
    assert!(
        rx.try_recv().is_err(),
        "pending key repeat must not send again"
    );
    assert_eq!(serde_json::to_value(&app.config).unwrap(), before);
    app.handle_native_event(event);
    assert!(
        app.status.contains("check host"),
        "failure cannot claim success"
    );
    assert!(!app.native.as_ref().unwrap().thinking_pending);
    app.native.as_mut().unwrap().epoch = 1;
    app.native.as_mut().unwrap().thinking_pending = true;
    app.handle_native_event(NativeEvent::ThinkingChanged(0, Ok(json!({}))));
    assert!(app.native.as_ref().unwrap().thinking_pending);
    app.native_snapshot(json!({"model":{"model":"remote", "thinking_level":"low"}}));
    assert_eq!(app.display_thinking_level(), Some("Low"));
    drop(app);
    crate::test_support::cleanup_tree(&storage);
}

#[test]
fn native_model_results_cannot_close_newer_or_reconnected_picker() {
    let (mut app, storage) = shell();
    app.picker = Some(crate::tui::model_editor::ModelPicker::new(
        &app.config,
        vec![],
    ));
    app.native.as_mut().unwrap().models_request = 2;
    app.native.as_mut().unwrap().models_pending = true;
    app.handle_native_event(NativeEvent::ModelChanged(0, 1, Ok(json!({}))));
    assert!(app.picker.is_some());
    assert!(!app.native.as_ref().unwrap().models_pending);
    app.native.as_mut().unwrap().models_pending = true;
    app.handle_native_event(NativeEvent::ModelChanged(9, 2, Ok(json!({}))));
    assert!(app.picker.is_some());
    assert!(app.native.as_ref().unwrap().models_pending);
    app.handle_native_event(NativeEvent::ModelChanged(0, 2, Ok(json!({}))));
    assert!(app.picker.is_none());
    drop(app);
    crate::test_support::cleanup_tree(&storage);
}

#[test]
fn native_clipboard_paste_stages_without_mounting_a_session_writer() {
    let (mut app, storage) = shell();
    let png = clipboard_png();
    app.clipboard_paste_reader = Arc::new(move |store, _mode| {
        let store = store.ok_or("native clipboard staging is unavailable")?;
        store
            .stage_png(&png)
            .map(super::super::attachments::PreparedClipboardPaste::Image)
    });
    let (tx, rx) = mpsc::sync_channel(4);
    app.event_sender = Some(tx);
    app.start_smart_clipboard_paste();
    let UiEvent::Worker(message) = rx.recv_timeout(Duration::from_secs(5)).unwrap() else {
        panic!("clipboard result");
    };
    app.handle_worker_message(message);
    assert_eq!(
        app.attachments.len(),
        1,
        "attach must accept clipboard images"
    );
    let path = app.attachments.paths()[0].clone();
    assert!(path.exists());
    let selected = storage.join("user-selected.png");
    std::fs::copy(&path, &selected).unwrap();
    app.attachments.add_unchecked_for_test(selected.clone());
    assert!(app.application.is_none() && app.bootstrap.is_none());
    app.clear_attachment_draft();
    assert!(
        !path.exists(),
        "clearing the draft releases its owned temporary source"
    );
    assert!(selected.exists(), "user-selected sources are never deleted");
    drop(app);
    crate::test_support::cleanup_tree(&storage);
}

#[test]
fn native_clipboard_clients_have_independent_source_ownership() {
    let (app, storage) = shell();
    let client = app.native.as_ref().unwrap().client.clone();
    let store = client.clipboard_drafts();
    let sibling = crate::draft::DraftImageStore::new(&storage);
    let own = store.stage_png(&clipboard_png()).unwrap();
    let other = sibling.stage_png(&clipboard_png()).unwrap();
    assert_ne!(own.parent(), other.parent());
    assert!(!store.release_clipboard_path(&other));
    drop(store);
    drop(app);
    assert!(
        own.exists(),
        "a HostClient worker clone retains its sources"
    );
    drop(client);
    assert!(!own.exists());
    assert!(
        other.exists(),
        "dropping one client cannot erase another draft"
    );
    drop(sibling);
    crate::test_support::cleanup_tree(&storage);
}

#[test]
fn native_image_submission_restores_only_uncommitted_images_before_new_draft() {
    for outcome in ["rollback", "success", "committed-error"] {
        let committed = outcome != "rollback";
        let (mut app, storage) = shell();
        let store = app.native_clipboard_drafts().unwrap();
        let old_image = store.stage_png(&clipboard_png()).unwrap();
        let new_image = store.stage_png(&clipboard_png()).unwrap();
        app.native.as_mut().unwrap().pending = Some("old".into());
        app.native
            .as_mut()
            .unwrap()
            .pending_images
            .add_unchecked_for_test(old_image.clone());
        app.attachments.add_unchecked_for_test(new_image.clone());
        app.input.insert_str("new text");
        let result = if outcome == "committed-error" {
            Err(HostCallError {
                message: "post-commit failure".into(),
                code: None,
                receipt: Some(
                    serde_json::from_value(json!({
                        "client_message_id":"image-request", "state":"committed",
                        "committed_message_id":"image-message", "retryable":false
                    }))
                    .unwrap(),
                ),
            })
        } else if committed {
            Ok(json!({}))
        } else {
            Err("upload failed".into())
        };
        app.handle_native_event(NativeEvent::Submitted(result));
        let expected: Vec<PathBuf> = if committed {
            vec![new_image.clone()]
        } else {
            vec![old_image.clone(), new_image.clone()]
        };
        assert_eq!(app.attachments.paths(), expected);
        assert_eq!(old_image.exists(), !committed);
        assert!(
            new_image.exists(),
            "completion must not release a newer draft"
        );
        assert_eq!(
            app.input.text(),
            if committed {
                "new text"
            } else {
                "old\nnew text"
            }
        );
        drop(app);
        assert!(new_image.exists(), "a worker clone still owns the store");
        drop(store);
        assert!(!new_image.exists(), "last owner releases temporary sources");
        crate::test_support::cleanup_tree(&storage);
    }
}

#[test]
fn native_rename_dialog_retains_failure_and_rejects_changed_selection() {
    let (mut app, storage) = shell();
    app.native.as_mut().unwrap().online = true;
    app.native.as_mut().unwrap().selection = 3;
    app.session_id = Some(SessionId::new("original"));
    app.session_title = Some("original title".into());
    app.open_native_rename();
    assert!(app.rename_dialog.is_some());
    app.native_renamed(0, 3, Err("rejected".into()));
    assert!(
        app.rename_dialog.is_some(),
        "failed rename retains editable title"
    );
    app.native.as_mut().unwrap().selection = 4;
    let (tx, _rx) = mpsc::sync_channel(4);
    app.event_sender = Some(tx);
    app.commit_native_rename("do not rename another session".into());
    assert!(app.rename_dialog.is_none());
    assert!(!app.native.as_ref().unwrap().rename_pending);
    assert_eq!(app.session_title.as_deref(), Some("original title"));
    drop(app);
    crate::test_support::cleanup_tree(&storage);
}

#[test]
fn native_session_picker_ignores_an_old_selection_response() {
    let (mut app, storage) = shell();
    app.native.as_mut().unwrap().epoch = 2;
    app.native.as_mut().unwrap().selection = 3;
    app.handle_native_event(NativeEvent::Sessions(2, 2, Ok(Vec::new())));
    assert!(app.session_picker.is_none());
    app.handle_native_event(NativeEvent::Sessions(1, 3, Ok(Vec::new())));
    assert!(app.session_picker.is_none());
    app.handle_native_event(NativeEvent::Sessions(2, 3, Ok(Vec::new())));
    assert!(app.session_picker.is_some());
    assert!(app.application.is_none());
    drop(app);
    crate::test_support::cleanup_tree(&storage);
}

#[test]
fn native_shell_never_mounts_a_writer_and_ignores_old_stream_snapshots() {
    let (mut app, storage) = shell();
    assert!(app.application.is_none() && app.bootstrap.is_none());
    assert!(!app.trust_prompt);
    let native = app.native.as_mut().unwrap();
    native.epoch = 4;
    native.snapshot_request = 2;
    for (epoch, request) in [(3, 2), (4, 1)] {
        app.handle_native_event(NativeEvent::Snapshot(
            epoch,
            request,
            Ok(json!({"session":{"id":"stale","title":"stale"}})),
        ));
        assert!(app.session_id.is_none());
    }
    app.handle_native_event(NativeEvent::Snapshot(
        4,
        2,
        Ok(json!({"session":{"id":"current","title":"current"}})),
    ));
    assert_eq!(app.session_id.as_ref().unwrap().as_str(), "current");
    app.handle_native_event(NativeEvent::Offline(3, "old disconnect".into()));
    assert!(!app.status.contains("old disconnect"));
    drop(app);
    crate::test_support::cleanup_tree(&storage);
}

#[test]
fn native_failed_submission_retains_both_drafts_and_offline_submit_never_sends() {
    let (mut app, storage) = shell();
    app.input.insert_str("new draft");
    app.native.as_mut().unwrap().pending = Some("earlier draft".into());
    app.handle_native_event(NativeEvent::Submitted(Err("uncertain result".into())));
    assert_eq!(app.input.text(), "earlier draft\nnew draft");
    app.input.clear();
    app.submit_native("keep offline".into());
    assert_eq!(app.input.text(), "keep offline");
    assert!(app.native.as_ref().unwrap().pending.is_none());
    drop(app);
    crate::test_support::cleanup_tree(&storage);
}

#[test]
fn committed_submission_error_never_restores_a_duplicate_draft() {
    let (mut app, storage) = shell();
    app.input.insert_str("new draft");
    app.native.as_mut().unwrap().pending = Some("already committed".into());
    let receipt = serde_json::from_value(json!({
        "client_message_id":"request-1", "state":"committed",
        "committed_message_id":"message-1", "retryable":false,
        "failure_phase":"run-start"
    }))
    .unwrap();
    app.handle_native_event(NativeEvent::Submitted(Err(HostCallError {
        message: "worker failed after commit".into(),
        code: Some("internal".into()),
        receipt: Some(receipt),
    })));
    assert_eq!(
        app.input.text(),
        "new draft",
        "committed input must not become a new request"
    );
    assert!(app.native.as_ref().unwrap().pending.is_none());
    drop(app);
    crate::test_support::cleanup_tree(&storage);
}

#[test]
fn native_resolved_approval_dismisses_only_the_matching_dialog() {
    let (mut app, storage) = shell();
    let (tx, _rx) = mpsc::sync_channel(16);
    app.event_sender = Some(tx);
    app.native_control(
        "approval.requested",
        json!({"rpc_id":"approval-a", "request":{
            "tool":"write_file","reason":"write", "call_id":"call-a", "effect":"write",
            "arguments":{"path":"example.txt"}
        }}),
    );
    assert!(app.pending_permission.is_some());
    app.native_control(
        "notice",
        json!({"kind":"approval_resolved","payload":{"rpc_id":"older"}}),
    );
    assert!(app.pending_permission.is_some());
    app.native_control(
        "notice",
        json!({"kind":"approval_resolved","payload":{"rpc_id":"approval-a"}}),
    );
    assert!(app.pending_permission.is_none());
    assert!(app.native.as_ref().unwrap().approval_id.is_none());
    drop(app);
    crate::test_support::cleanup_tree(&storage);
}
