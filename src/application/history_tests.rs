use super::*;
#[test]
fn session_share_request_identity_is_stable_and_distinguishes_sources() {
    let selection = SessionShareSelection {
        reference: HistoryReference {
            project: Some("/project".into()),
            session: "one".into(),
            seq: None,
        },
        recent_turns: Some(4),
        version: "revision".into(),
    };
    let request =
        || crate::message::PendingMessage::from_front_end("hello", Some("id".into()), Vec::new());
    let mut original = request();
    original.bind_session_share_identity(std::slice::from_ref(&selection));
    let digest = original.request_digest();
    original.content.blocks.push(crate::ContentBlock::Text {
        text: "frozen snapshot".into(),
    });
    assert_eq!(original.request_digest(), digest);
    let mut retry = request();
    retry.bind_session_share_identity(std::slice::from_ref(&selection));
    assert_eq!(retry.request_digest(), digest);
    let mut changed = selection;
    changed.reference.session = "two".into();
    let mut other = request();
    other.bind_session_share_identity(&[changed]);
    assert_ne!(other.request_digest(), digest);
}
#[test]
fn session_share_freeze_enforces_source_and_version() {
    let (storage, project) = roots("share-freeze-application");
    std::fs::create_dir_all(&project).unwrap();
    let mut app = mount(&Project::new(&project), &storage, TestBehavior::Success);
    configure_test_model(&app);
    if let Some(worker) = &mut app.title_worker {
        worker.shutdown().unwrap();
    }
    run(&mut app, "source content").unwrap();
    let mut reference = app
        .history_reader()
        .search(
            HistorySearch {
                query: "source content".into(),
                ..Default::default()
            },
            &crate::CancelToken::new(),
        )
        .unwrap()
        .hits[0]
        .reference
        .clone();
    reference.seq = None;
    let preview = app
        .history_reader()
        .share(&reference, None, &crate::CancelToken::new())
        .unwrap();
    let selection = SessionShareSelection {
        reference,
        recent_turns: None,
        version: preview.version,
    };
    assert!(
        app.freeze_session_shares(std::slice::from_ref(&selection), &crate::CancelToken::new())
            .is_err()
    );
    app.new_session().unwrap();
    let shares = app
        .freeze_session_shares(std::slice::from_ref(&selection), &crate::CancelToken::new())
        .unwrap();
    assert!(shares[0].text.contains("source content"));
    let message = crate::message::PendingMessage::from_front_end(
        "new request",
        Some("id".into()),
        Vec::new(),
    );
    let frozen = app
        .freeze_session_share_message(
            message,
            std::slice::from_ref(&selection),
            &crate::CancelToken::new(),
        )
        .unwrap();
    assert!(frozen.content.plain_text().contains("source content"));
    assert!(frozen.content.plain_text().starts_with("new request"));
    let mut stale = selection;
    stale.version.push('x');
    assert!(
        app.freeze_session_shares(&[stale], &crate::CancelToken::new())
            .is_err()
    );
    app.close().unwrap();
    crate::test_support::cleanup_tree(&storage);
    crate::test_support::cleanup_tree(&project);
}

#[test]
fn session_share_queue_edit_keeps_frozen_content_and_retry_identity() {
    let (storage, project) = roots("share-queue-edit");
    std::fs::create_dir_all(&project).unwrap();
    let mut app = mount(&Project::new(&project), &storage, TestBehavior::Success);
    configure_test_model(&app);
    if let Some(worker) = &mut app.title_worker {
        worker.shutdown().unwrap();
    }
    run(&mut app, "original shared content").unwrap();
    let mut reference = app
        .history_reader()
        .search(
            HistorySearch {
                query: "original shared content".into(),
                ..Default::default()
            },
            &crate::CancelToken::new(),
        )
        .unwrap()
        .hits[0]
        .reference
        .clone();
    reference.seq = None;
    let preview = app
        .history_reader()
        .share(&reference, None, &crate::CancelToken::new())
        .unwrap();
    let selection = SessionShareSelection {
        reference,
        recent_turns: None,
        version: preview.version,
    };
    app.new_session().unwrap();
    let shares = app
        .freeze_session_shares(std::slice::from_ref(&selection), &crate::CancelToken::new())
        .unwrap();
    let draft = InputDraft {
        session_shares: vec![selection],
        frozen_shares: vec![shares[0].text.clone().into()],
        ..Default::default()
    };
    let mut message = crate::message::PendingMessage::from_front_end(
        "before edit",
        Some("queued".into()),
        Vec::new(),
    );
    message.bind_session_share_identity(&draft.session_shares);
    let mut incomplete = draft.clone();
    incomplete.frozen_shares.clear();
    assert!(
        app.enqueue_input("queued".into(), message.clone(), incomplete)
            .is_err()
    );
    app.enqueue_input("queued".into(), message, draft).unwrap();
    app.update_pending_input(
        "queued",
        1,
        PendingInputEdit {
            text: "after edit".into(),
            keep_uploads: None,
            attachments: Vec::new(),
            draft_scope_id: None,
        },
        0,
        0,
    )
    .unwrap();
    let view = app.pending_inputs();
    assert_eq!(view.items[0].text, "after edit");
    assert!(
        !serde_json::to_string(&view)
            .unwrap()
            .contains("original shared content")
    );
    let message = app.input_queue().claim(true).unwrap();
    assert!(message.content.plain_text().contains("after edit"));
    assert!(
        message
            .content
            .plain_text()
            .contains("original shared content")
    );
    assert!(app.input_queue().accepts_initial(&message));
    app.close().unwrap();
    crate::test_support::cleanup_tree(&storage);
    crate::test_support::cleanup_tree(&project);
}
#[test]
fn history_query_real_application_commands_leave_session_facts_and_selection_unchanged() {
    let (storage, project) = roots("history-application");
    std::fs::create_dir_all(&project).unwrap();
    let mut app = mount(&Project::new(&project), &storage, TestBehavior::Success);
    configure_test_model(&app);
    if let Some(worker) = &mut app.title_worker {
        worker.shutdown().unwrap();
    }
    run(&mut app, "历史消息 needle-for-history").unwrap();
    let current = app.current_session_id();
    let files = snapshot_files(&storage.join("sessions"));
    let reader = app.history_reader();
    let result = reader
        .search(
            crate::application::HistorySearch {
                query: "needle-for-history".into(),
                ..Default::default()
            },
            &crate::CancelToken::new(),
        )
        .unwrap();
    assert_eq!(result.hits.len(), 1);
    assert_eq!(
        result.hits[0].reference.session,
        current.as_ref().unwrap().as_str()
    );
    let page = reader
        .events(
            crate::application::HistoryEventsRequest {
                reference: result.hits[0].reference.clone(),
                trusted_projects: false,
                after_seq: None,
                event_type: Some("user/message".into()),
            },
            &crate::CancelToken::new(),
        )
        .unwrap();
    assert_eq!(page.events.len(), 1);
    let read = reader
        .read(
            crate::application::HistoryReadRequest {
                reference: page.events[0].reference.clone(),
                trusted_projects: false,
                offset: 0,
                version: None,
            },
            &crate::CancelToken::new(),
        )
        .unwrap();
    assert!(read.json.contains("needle-for-history"));
    assert!(matches!(
        app.dispatch_command("/history search needle-for-history")
            .unwrap(),
        crate::CommandOutcome::QueryHistory(_)
    ));
    assert_eq!(app.current_session_id(), current);
    assert_eq!(snapshot_files(&storage.join("sessions")), files);
    for name in [
        "session_search",
        "session_event_search",
        "session_event_read",
    ] {
        assert!(app.tools.get(name).is_some(), "{name}");
    }
    drop(reader);
    app.close().unwrap();
    crate::test_support::cleanup_tree(&storage);
    crate::test_support::cleanup_tree(&project);
}
fn snapshot_files(
    root: &std::path::Path,
) -> std::collections::BTreeMap<std::path::PathBuf, Vec<u8>> {
    fn visit(
        root: &std::path::Path,
        out: &mut std::collections::BTreeMap<std::path::PathBuf, Vec<u8>>,
    ) {
        for entry in std::fs::read_dir(root).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                visit(&path, out)
            } else {
                out.insert(path.clone(), std::fs::read(path).unwrap());
            }
        }
    }
    let mut out = Default::default();
    visit(root, &mut out);
    out
}
