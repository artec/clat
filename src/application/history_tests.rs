use super::*;
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
