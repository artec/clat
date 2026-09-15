use super::*;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant};

fn shell() -> (App, PathBuf) {
    let (storage, project) = crate::test_support::roots("native-shell");
    std::fs::create_dir_all(&storage).unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    // Protocol fixture, not a second runtime owner. Tests never touch the
    // user's storage, launch providers, or connect to an existing host.
    let description = json!({"ok":true,"value":{
        "storage_root":storage, "protocol_version":1,"wire_version":1,
        // SV 最小适配行（S6 例外，见 docs/todo/session-v3-execution.md）：
        // 握手判据 journal_version 随 SESSION_FORMAT_VERSION bump 到 3。
        "journal_version":3,"instance_id":"11111111-1111-1111-1111-111111111111"
    }})
    .to_string();
    let stop = Arc::new(AtomicBool::new(false));
    let wake = Arc::clone(&stop);
    thread::spawn(move || {
        thread::sleep(Duration::from_secs(3));
        wake.store(true, Ordering::Release);
        let _ = TcpStream::connect(("127.0.0.1", port));
    });
    let server_stop = Arc::clone(&stop);
    let _server = thread::spawn(move || {
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
        drop(reader);
        let handshake_completed = Instant::now();
        // Keep the fixture listener alive for subsequent native requests.
        // Returning an HTTP error is deterministic on every platform and
        // avoids making failure-path tests depend on how quickly a closed
        // local port reports ECONNREFUSED/WSAECONNREFUSED. Keep accept
        // blocking: on Windows an accepted socket may inherit a listener's
        // nonblocking mode, so this fixture never enters that path and also
        // explicitly restores blocking mode below.
        loop {
            let (mut stream, _) = match listener.accept() {
                Ok(connection) => connection,
                Err(error) => {
                    eprintln!("native test host accept failed: {error}");
                    break;
                }
            };
            eprintln!(
                "native test host accepted connection after {:?}",
                handshake_completed.elapsed()
            );
            if server_stop.load(Ordering::Acquire) {
                break;
            }
            if let Err(error) = stream.set_nonblocking(false) {
                eprintln!("native test host socket mode failed: {error}");
                continue;
            }
            if let Err(error) = reject_request(&mut stream) {
                eprintln!("native test host rejection failed: {error}");
            }
        }
    });
    let client = HostClient::connect(port, "test-token".into(), &storage).unwrap();
    let mut app = App::open_native(Project::new(&project), client).unwrap();
    app.focused = Some(true);
    (app, storage)
}

fn reject_request(stream: &mut TcpStream) -> std::io::Result<()> {
    stream.write_all(
        b"HTTP/1.1 500 Internal Server Error\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
    )
}

#[test]
fn suggestion_is_preview_until_explicit_accept_and_never_sends() {
    for attached in [false, true] {
        let (mut app, storage, request) = suggestion_shell(attached);
        suggestion_reply(&mut app, request, "next question");
        assert_eq!(
            app.input.text(),
            "",
            "provider replies must not edit the composer"
        );
        app.handle_ui_event(UiEvent::Terminal(Event::Key(KeyEvent::new(
            KeyCode::Char('y'),
            KeyModifiers::CONTROL,
        ))));
        assert_eq!(app.input.text(), "next question");
        assert!(!app.running, "accept is not send");
        assert!(
            app.native
                .as_ref()
                .is_none_or(|native| native.pending.is_none())
        );
        assert!(
            app.application.is_none(),
            "reply must not introduce a local writer"
        );
        app.handle_ui_event(UiEvent::Terminal(Event::Key(KeyEvent::from(
            KeyCode::Char('!'),
        ))));
        assert_eq!(app.input.text(), "next question!");
        drop(app);
        crate::test_support::cleanup_tree(&storage);
    }
}

#[test]
fn suggestion_in_flight_does_not_freeze_editing_or_append_a_stale_reply() {
    for attached in [false, true] {
        let (mut app, storage, request) = suggestion_shell(attached);
        app.handle_ui_event(UiEvent::Terminal(Event::Key(KeyEvent::from(
            KeyCode::Char('x'),
        ))));
        assert_eq!(app.input.text(), "x", "suggestion must not freeze editing");
        suggestion_reply(&mut app, request, "stale");
        assert_eq!(
            app.input.text(),
            "x",
            "a late suggestion cannot modify newer input"
        );
        assert!(app.suggestions.preview.is_none());
        assert!(!app.suggestions.pending());
        drop(app);
        crate::test_support::cleanup_tree(&storage);
    }
}

fn suggestion_shell(attached: bool) -> (App, PathBuf, u64) {
    let (mut app, storage) = shell();
    if !attached {
        app.native = None;
    }
    app.session_id = Some(SessionId::new("hint-session"));
    let (request, _) = app.suggestions.begin(app.input.generation()).unwrap();
    (app, storage, request)
}

fn suggestion_reply(app: &mut App, request: u64, text: &str) {
    let event = if app.native.is_some() {
        UiEvent::Native(NativeEvent::PromptSuggestion(
            0,
            0,
            request,
            Ok(json!({"text":text,"session_id":"hint-session","selection_generation":0})),
        ))
    } else {
        UiEvent::Worker(WorkerMessage::PromptSuggestionFinished {
            request,
            outcome: Ok(crate::application::PromptSuggestion {
                session_id: SessionId::new("hint-session"),
                text: text.into(),
            }),
        })
    };
    app.handle_ui_event(event);
}

#[test]
fn suggestion_ignore_and_edit_back_to_same_text_preserve_composer() {
    for attached in [false, true] {
        let (mut app, storage, request) = suggestion_shell(attached);
        suggestion_reply(&mut app, request, "ignore me");
        app.handle_ui_event(UiEvent::Terminal(Event::Key(KeyEvent::from(KeyCode::Esc))));
        assert_eq!(app.input.text(), "");
        assert!(app.suggestions.preview.is_none());
        let (request, _) = app.suggestions.begin(app.input.generation()).unwrap();
        app.handle_ui_event(UiEvent::Terminal(Event::Paste("temporary".into())));
        for _ in 0..9 {
            app.handle_ui_event(UiEvent::Terminal(Event::Key(KeyEvent::from(
                KeyCode::Backspace,
            ))));
        }
        assert_eq!(app.input.text(), "");
        suggestion_reply(&mut app, request, "ABA-stale");
        assert!(
            app.suggestions.preview.is_none(),
            "text equality is not an input generation fence"
        );
        drop(app);
        crate::test_support::cleanup_tree(&storage);
    }
}

#[test]
fn suggestion_preview_is_rendered_without_changing_input() {
    use ratatui::{Terminal, backend::TestBackend};
    for attached in [false, true] {
        let (mut app, storage, request) = suggestion_shell(attached);
        suggestion_reply(&mut app, request, "visible preview");
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        terminal.draw(|frame| app.draw(frame)).unwrap();
        let screen: String = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect();
        assert!(screen.contains("visible preview"));
        assert!(screen.contains("Ctrl+Y use suggestion"));
        assert_eq!(app.input.text(), "");
        drop(app);
        crate::test_support::cleanup_tree(&storage);
    }
}

#[test]
fn native_suggestion_has_one_request_in_flight() {
    let (mut app, storage) = shell();
    app.native.as_mut().unwrap().online = true;
    let (sender, received) = ui_event_channel();
    app.event_sender = Some(sender);
    app.open_native_suggestion();
    app.open_native_suggestion();
    let first = received.recv_timeout(Duration::from_secs(5)).unwrap();
    assert!(matches!(
        first,
        UiEvent::Native(NativeEvent::PromptSuggestion(..))
    ));
    assert!(
        received.recv_timeout(Duration::from_millis(200)).is_err(),
        "a duplicate trigger must not launch another utility request"
    );
    drop(app);
    crate::test_support::cleanup_tree(&storage);
}

fn clipboard_png() -> Vec<u8> {
    let mut png = Vec::new();
    image::DynamicImage::ImageRgba8(image::RgbaImage::new(2, 2))
        .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
        .unwrap();
    png
}

#[test]
fn native_quit_detaches_even_offline_without_cancelling_host_work() {
    for command in ["/quit", " /exit "] {
        let (mut app, storage) = shell();
        app.running = true;
        app.native.as_mut().unwrap().pending = Some("in flight".into());
        app.input.insert_str("newer draft");
        app.submit_native(command.into());
        assert!(
            app.should_quit,
            "quit must detach an offline or busy client"
        );
        assert!(app.running, "detaching is not run cancellation");
        assert_eq!(app.input.text(), "newer draft");
        assert_eq!(
            app.native.as_ref().unwrap().pending.as_deref(),
            Some("in flight")
        );
        drop(app);
        crate::test_support::cleanup_tree(&storage);
    }
}

#[test]
fn native_compaction_displays_host_receipt_and_completion_without_touching_draft() {
    let (mut app, storage) = shell();
    app.input.insert_str("newer draft");
    app.native.as_mut().unwrap().pending = Some("/compact".into());
    app.handle_native_event(NativeEvent::Submitted(Ok(json!({"status":"started"}))));
    assert!(app.native.as_ref().unwrap().pending.is_none());
    assert_eq!(app.status, "started");
    assert_eq!(app.input.text(), "newer draft");
    app.native_control(
        "notice",
        json!({"kind":"compaction","payload":{
            "status":"finished","note":"compaction failed: cancelled","succeeded":false
        }}),
    );
    assert_eq!(app.input.text(), "newer draft");
    assert_eq!(app.status, "compaction failed: cancelled");
    drop(app);
    crate::test_support::cleanup_tree(&storage);
}

#[test]
fn native_permission_dialog_requires_host_state_and_full_access_confirmation() {
    use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
    let (mut app, storage) = shell();
    app.submit_native("/perm".into());
    assert!(
        app.permission_picker.is_none(),
        "offline must not invent a mode"
    );
    app.native.as_mut().unwrap().online = true;
    app.submit_native("/permission".into());
    assert!(
        app.permission_picker.is_none(),
        "unknown host mode must not default"
    );
    app.native_snapshot(json!({"permission":{"mode":"danger-full-access"}}));
    app.submit_native("/perm".into());
    assert!(
        app.permission_picker.is_some(),
        "host permission picker missing"
    );
    app.handle_ui_event(UiEvent::Terminal(Event::Key(KeyEvent::new(
        KeyCode::Enter,
        KeyModifiers::NONE,
    ))));
    assert!(
        app.permission_picker.is_some(),
        "cached full access still requires explicit confirmation"
    );
    assert!(!app.native.as_ref().unwrap().permissions.pending);
    app.handle_ui_event(UiEvent::Terminal(Event::Key(KeyEvent::new(
        KeyCode::Esc,
        KeyModifiers::NONE,
    ))));
    app.handle_ui_event(UiEvent::Terminal(Event::Key(KeyEvent::new(
        KeyCode::Esc,
        KeyModifiers::NONE,
    ))));
    assert!(app.permission_picker.is_none());
    assert!(app.application.is_none());
    drop(app);
    crate::test_support::cleanup_tree(&storage);
}

#[test]
fn native_permission_target_and_replies_are_fenced_without_optimistic_state() {
    let (mut app, storage) = shell();
    app.native.as_mut().unwrap().online = true;
    app.native_snapshot(json!({"permission":{"mode":"read-only"}}));
    app.submit_native("/perm".into());
    let (tx, _rx) = mpsc::sync_channel(8);
    app.event_sender = Some(tx);
    app.native.as_mut().unwrap().selection += 1;
    app.apply_native_permission(PermissionMode::FullAccess);
    assert_eq!(app.status, "permission target changed; reopen /perm");
    assert!(!app.native.as_ref().unwrap().permissions.pending);
    app.event_sender = None;
    app.native.as_mut().unwrap().permissions.pending = true;
    for (epoch, selection) in [(1, 1), (0, 0)] {
        app.handle_native_event(NativeEvent::PermissionChanged(
            epoch,
            selection,
            Err("stale".into()),
        ));
        assert!(app.native.as_ref().unwrap().permissions.pending);
        assert_ne!(app.status, "stale");
    }
    app.handle_native_event(NativeEvent::PermissionChanged(
        0,
        1,
        Err("save failed".into()),
    ));
    assert!(!app.native.as_ref().unwrap().permissions.pending);
    assert_eq!(app.status, "save failed");
    assert_eq!(app.current_permission_mode(), PermissionMode::ReadOnly);
    drop(app);
    crate::test_support::cleanup_tree(&storage);
}

#[test]
fn native_readonly_commands_open_a_local_dialog() {
    let (mut app, storage) = shell();
    for command in [
        "/help", "/skill", "/mem", "/goal", "/sub", "/mcp", "/context",
    ] {
        app.submit_native(command.into());
        assert!(
            app.info_dialog.is_some(),
            "attach info dialog missing: {command}"
        );
        assert!(app.native.as_ref().unwrap().pending.is_none());
        app.info_dialog = None;
    }
    drop(app);
    crate::test_support::cleanup_tree(&storage);
}

#[test]
fn native_readonly_dialog_replies_are_fenced_and_scroll_locally() {
    let (mut app, storage) = shell();
    app.input.insert_str("keep my draft");
    app.submit_native(" /skills ".into());
    let request = app.native.as_ref().unwrap().info_request;
    let text = |app: &App| match app.content_view.as_ref().unwrap() {
        ContentView::Remote { title, text } => (*title, text.clone()),
        _ => panic!("remote content"),
    };
    let initial = text(&app);
    for (epoch, selection, request) in [(1, 0, request), (0, 1, request), (0, 0, request + 1)] {
        app.handle_native_event(NativeEvent::Info(
            epoch,
            selection,
            request,
            Ok(json!({"message":"stale"})),
        ));
        assert_eq!(
            text(&app),
            initial,
            "each fence independently rejects late content"
        );
    }
    app.handle_native_event(NativeEvent::Info(
        0,
        0,
        request,
        Ok(json!({"message":"host skill catalog"})),
    ));
    assert_eq!(text(&app), ("/skill", "host skill catalog".into()));
    app.info_scroll_max = 40;
    app.info_page = 5;
    app.handle_ui_event(UiEvent::Terminal(Event::Key(KeyEvent::new(
        KeyCode::PageDown,
        KeyModifiers::NONE,
    ))));
    assert_eq!(app.info_dialog.as_ref().unwrap().offset, 5);
    app.handle_ui_event(UiEvent::Terminal(Event::Key(KeyEvent::new(
        KeyCode::Esc,
        KeyModifiers::NONE,
    ))));
    app.handle_native_event(NativeEvent::Info(
        0,
        0,
        request,
        Ok(json!({"message":"closed"})),
    ));
    assert!(app.info_dialog.is_none());
    assert_eq!(text(&app).1, "host skill catalog");
    app.submit_native("/help".into());
    app.handle_native_event(NativeEvent::Info(
        0,
        0,
        request,
        Ok(json!({"message":"old skills"})),
    ));
    assert_eq!(text(&app).0, "/help");
    assert!(!text(&app).1.contains("old skills"));
    let request = app.native.as_ref().unwrap().info_request;
    app.handle_native_event(NativeEvent::Info(
        0,
        0,
        request,
        Err("host rejected".into()),
    ));
    assert!(text(&app).1.contains("host rejected"));
    app.start_native();
    assert!(app.info_dialog.is_none());
    assert!(app.content_view.is_none());
    assert_eq!(app.input.text(), "keep my draft");
    assert!(!app.open_native_info("/goal run"));
    assert!(!app.open_native_info("/skill invoke"));
    drop(app);
    crate::test_support::cleanup_tree(&storage);
}

#[test]
fn native_mcp_refresh_keys_replace_only_the_current_read_request() {
    let (mut app, storage) = shell();
    app.input.insert_str("draft survives refresh");
    app.submit_native("/mcp".into());
    assert!(app.native_info_refreshable());
    for key in ['r', 'R'] {
        let old = app.native.as_ref().unwrap().info_request;
        app.info_dialog.as_mut().unwrap().offset = 7;
        app.handle_ui_event(UiEvent::Terminal(Event::Key(KeyEvent::new(
            KeyCode::Char(key),
            KeyModifiers::NONE,
        ))));
        let current = app.native.as_ref().unwrap().info_request;
        assert_eq!(current, old + 1);
        assert_eq!(app.info_dialog.as_ref().unwrap().offset, 0);
        app.handle_native_event(NativeEvent::Info(
            0,
            0,
            old,
            Ok(json!({"message":"stale MCP"})),
        ));
        let Some(ContentView::Remote { text, .. }) = &app.content_view else {
            panic!("remote view");
        };
        assert!(!text.contains("stale MCP"));
        app.handle_native_event(NativeEvent::Info(
            0,
            0,
            current,
            Ok(json!({"message":"mcp: 1/2 connected · 1 connecting"})),
        ));
        let Some(ContentView::Remote { text, .. }) = &app.content_view else {
            panic!("remote view");
        };
        assert!(text.contains("1/2 connected"));
    }
    let backend = ratatui::backend::TestBackend::new(100, 30);
    let mut terminal = ratatui::Terminal::new(backend).unwrap();
    terminal
        .draw(|frame| app.draw_content_dialog(frame))
        .unwrap();
    let rendered = terminal
        .backend()
        .buffer()
        .content
        .iter()
        .map(|cell| cell.symbol())
        .collect::<String>();
    assert!(rendered.contains("r refresh"));
    assert!(rendered.contains("1/2 connected"));
    app.submit_native("/help".into());
    let request = app.native.as_ref().unwrap().info_request;
    app.handle_ui_event(UiEvent::Terminal(Event::Key(KeyEvent::new(
        KeyCode::Char('r'),
        KeyModifiers::NONE,
    ))));
    assert_eq!(app.native.as_ref().unwrap().info_request, request);
    assert!(!app.native_info_refreshable());
    assert!(!app.open_native_info("/mcp reconnect"));
    assert_eq!(app.input.text(), "draft survives refresh");
    assert!(app.application.is_none());
    drop(app);
    crate::test_support::cleanup_tree(&storage);
}

#[test]
fn native_context_malformed_reply_is_visible_without_losing_the_dialog() {
    let (mut app, storage) = shell();
    app.submit_native("/context".into());
    let request = app.native.as_ref().unwrap().info_request;
    app.handle_native_event(NativeEvent::Info(
        0,
        0,
        request,
        Ok(json!({"kind":"context", "context":{}})),
    ));
    let Some(ContentView::Remote { title, text }) = &app.content_view else {
        panic!("context dialog");
    };
    assert_eq!(*title, "/context");
    assert_eq!(text, "Host returned an invalid context estimate");
    assert!(app.info_dialog.is_some());
    assert!(!app.native_info_refreshable());
    drop(app);
    crate::test_support::cleanup_tree(&storage);
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
    let UiEvent::Native(event) = rx.recv_timeout(Duration::from_secs(5)).unwrap() else {
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
            "active_profile":null, "presets":[], "profiles":[],
            "utility":{"naming_enabled":true,"suggestions_enabled":false,"profile":null}
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
    let UiEvent::Native(event) = rx.recv_timeout(Duration::from_secs(5)).expect("host save") else {
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
        .recv_timeout(Duration::from_secs(5))
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

/// 缺陷修复判别腿（2026-09-15，负责人实机）：attach 壳的宿主投影**从不
/// 携带 endpoint**（路由脱敏纪律），快照报了模型名即代表宿主已配置。
/// 旧判定直接用本地 `is_configured()`（要求本地 endpoint 非空）→ 默认
/// 拓扑下标题恒 "not configured — /model"、prompt 提交恒被拒、/model
/// 选完也不变——"进去就没选中模型，选择也无效"。pre-fix 红 = 谓词缺席
/// （编译红）+ 实机 PTY 取证（标题原文与提交拒绝文案）。
#[test]
fn native_snapshot_reported_model_satisfies_the_submit_gate_without_endpoint() {
    let (mut app, storage) = shell();
    app.native_snapshot(json!({
        "model": {"protocol":"open_ai_compatible","model":"deepseek-flash",
                  "preset":"deepseek-flash","thinking_level":null},
        "session": {},
        "permission": {"mode":"workspace-write"}
    }));
    assert_eq!(app.config.model, "deepseek-flash");
    assert_eq!(
        app.config.preset.as_deref(),
        Some("deepseek-flash"),
        "the preset rides the snapshot so the title resolves its display name \
         instead of the 'protocol · model' fallback"
    );
    assert!(
        app.config.endpoint.is_empty(),
        "the host projection never carries the endpoint"
    );
    assert!(
        !app.config.is_configured(),
        "the local predicate must stay false; the gate must not depend on it"
    );
    assert!(
        app.model_ready(),
        "a host-reported model satisfies the submit gate in attach mode"
    );
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
        .recv_timeout(Duration::from_secs(5))
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
