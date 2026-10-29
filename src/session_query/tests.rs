use super::*;
use crate::session::{
    event::{SessionEvent, payloads},
    header::SessionHeader,
    id::SessionId,
    persistence::{JsonlBackend, JsonlCompression, PreparedSession},
    use_cases::SessionService,
};
use std::path::PathBuf;
pub(super) struct Fixture {
    root: PathBuf,
    pub(super) service: SessionQueryService,
    pub(super) backend: JsonlBackend,
    pub(super) _sessions: Arc<SessionService>,
}
impl Fixture {
    pub(super) fn new() -> Self {
        let root = std::env::temp_dir().join(format!("clat-history-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        crate::control_storage::sentinel::initialize(&root).unwrap();
        let control = Arc::new(crate::control_storage::ControlStorage::open_ready(&root).unwrap());
        control
            .add_trust(std::path::Path::new("/tmp/query-one"))
            .unwrap();
        let sessions =
            Arc::new(SessionService::new(root.join("sessions"), JsonlCompression::Zstd).unwrap());
        let service = SessionQueryService::new(
            Arc::new(QueryIndex::new(root.clone())),
            sessions.query_reader(),
            control,
            ProjectKey::from_cwd("/tmp/query-one"),
        );
        let backend = JsonlBackend::new(root.join("sessions"), JsonlCompression::Zstd, true);
        Self {
            root,
            service,
            backend,
            _sessions: sessions,
        }
    }
    pub(super) fn write(&self, project: &str, id: &str, text: &str) -> PreparedSession {
        let key = SessionKey {
            project: ProjectKey::from_cwd(project),
            id: SessionId::new(id),
        };
        let header = SessionHeader::new(key.id.clone(), key.project.header_cwd.clone(), 42000);
        let prepared = self.backend.create(key, header).unwrap();
        self.backend
            .append_batch(prepared, 0, &[user(0, text)])
            .unwrap()
    }
    fn query(&self, text: &str) -> HistoryPage {
        self.service
            .search(
                HistorySearch {
                    query: text.into(),
                    ..Default::default()
                },
                &CancelToken::new(),
                false,
            )
            .unwrap()
    }
    pub(super) fn log(&self, id: &str) -> PathBuf {
        self.root
            .join("sessions")
            .join(crate::session::path_layout::project_key("/tmp/query-one"))
            .join(id)
            .join("session.v4.jsonl.zstd")
    }
    pub(super) fn reference(&self, id: &str, seq: u64) -> HistoryReference {
        HistoryReference {
            project: Some("/tmp/query-one".into()),
            session: id.into(),
            seq: Some(seq),
        }
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = self._sessions.quiesce_active();
        self.service.index.reset_connection();
        crate::test_support::cleanup_tree(&self.root);
    }
}
fn user(seq: u64, text: &str) -> SessionEvent {
    SessionEvent::new(
        "user/message",
        seq,
        43000 + seq as i64,
        payloads::user_message(text),
    )
    .append(Vec::new())
}
#[test]
fn history_query_three_languages_short_code_literals_and_source_unchanged() {
    let f = Fixture::new();
    f.write(
        "/tmp/query-one",
        "one",
        "English needle 简体中文 繁體中文 中文 aa::bb OR \"quote\" <img src=x>",
    );
    let before = std::fs::read(f.log("one")).unwrap();
    for query in [
        "needle",
        "简体中文",
        "繁體中文",
        "中文",
        "文",
        "aa::bb",
        "OR",
        "\"quote\"",
    ] {
        let page = f.query(query);
        assert_eq!(page.index_state, "ready");
        assert_eq!(page.hits.len(), 1, "{query}");
        assert_eq!(page.hits[0].reference.session, "one");
    }
    assert_eq!(std::fs::read(f.log("one")).unwrap(), before);
    assert!(f.service.reader.current().is_none());
}
#[test]
fn history_query_incremental_external_append_truncate_delete_and_rebuild() {
    let f = Fixture::new();
    let prepared = f.write("/tmp/query-one", "one", "original");
    let original = std::fs::read(f.log("one")).unwrap();
    assert_eq!(f.query("original").hits.len(), 1);
    drop(
        f.backend
            .append_batch(prepared, 1, &[user(1, "appended-new")])
            .unwrap(),
    );
    assert_eq!(f.query("appended-new").hits.len(), 1);
    std::fs::write(f.log("one"), &original).unwrap();
    assert!(f.query("appended-new").hits.is_empty());
    assert_eq!(f.query("original").hits.len(), 1);
    let cache = f.root.join("history-index/index.sqlite");
    f.service.index.reset_connection();
    for suffix in ["-wal", "-shm"] {
        let _ = std::fs::remove_file(format!("{}{suffix}", cache.display()));
    }
    std::fs::write(&cache, b"invalid derived database").unwrap();
    assert_eq!(f.query("original").hits.len(), 1);
    std::fs::remove_file(f.log("one")).unwrap();
    assert!(f.query("original").hits.is_empty());
}
#[test]
fn history_query_project_fence_explicit_trusted_scope_and_read_fence() {
    let f = Fixture::new();
    f.write("/tmp/query-one", "same", "private-one");
    f.write("/tmp/query-two", "same", "private-two");
    assert!(f.query("private-two").hits.is_empty());
    let all = HistorySearch {
        query: "private-two".into(),
        trusted_projects: true,
        ..Default::default()
    };
    assert!(
        f.service
            .search(all.clone(), &CancelToken::new(), false)
            .unwrap()
            .hits
            .is_empty()
    );
    f.service
        .control
        .add_trust(std::path::Path::new("/tmp/query-two"))
        .unwrap();
    assert_eq!(
        f.service
            .search(all.clone(), &CancelToken::new(), false)
            .unwrap()
            .hits
            .len(),
        1
    );
    assert_eq!(
        f.service
            .search(all, &CancelToken::new(), true)
            .unwrap_err()
            .code,
        "unauthorized"
    );
    let request = HistoryReadRequest {
        reference: HistoryReference {
            project: Some("/tmp/query-two".into()),
            ..f.reference("same", 0)
        },
        trusted_projects: false,
        offset: 0,
        version: None,
    };
    assert_eq!(
        f.service
            .read(request.clone(), &CancelToken::new(), false)
            .unwrap_err()
            .code,
        "unauthorized"
    );
    let read = f
        .service
        .read(
            HistoryReadRequest {
                trusted_projects: true,
                ..request
            },
            &CancelToken::new(),
            false,
        )
        .unwrap();
    assert!(read.json.contains("private-two"));
    assert!(!read.json.contains("private-one"));
}
#[test]
fn history_query_inspection_paginates_redacts_and_rejects_stale_continuation() {
    let f = Fixture::new();
    let text = format!("long {} api_key=abcdefghijklmnop", "中文".repeat(4000));
    f.write("/tmp/query-one", "one", &text);
    let request = HistoryReadRequest {
        reference: f.reference("one", 0),
        trusted_projects: false,
        offset: 0,
        version: None,
    };
    let first = f
        .service
        .read(request.clone(), &CancelToken::new(), false)
        .unwrap();
    assert!(!first.json.contains("abcdefghijklmnop"));
    let offset = first.next_offset.unwrap();
    let mut all = first.json;
    let mut next = Some(offset);
    while let Some(offset) = next {
        let page = f
            .service
            .read(
                HistoryReadRequest {
                    offset,
                    version: Some(first.version.clone()),
                    ..request.clone()
                },
                &CancelToken::new(),
                false,
            )
            .unwrap();
        all.push_str(&page.json);
        next = page.next_offset;
    }
    assert!(all.contains("[REDACTED]"));
    assert!(!all.contains("abcdefghijklmnop"));
    assert!(serde_json::from_str::<Value>(&all).is_ok());
    assert_eq!(
        f.service
            .read(
                HistoryReadRequest {
                    offset,
                    version: Some("bad".into()),
                    ..request
                },
                &CancelToken::new(),
                false
            )
            .unwrap_err()
            .code,
        "source_changed"
    );
}
#[test]
fn history_query_empty_corrupt_source_cancel_and_cursor_boundaries() {
    let f = Fixture::new();
    assert!(f.query("nothing").hits.is_empty());
    let cancel = CancelToken::new();
    cancel.cancel();
    assert_eq!(
        f.service
            .search(
                HistorySearch {
                    query: "hello".into(),
                    ..Default::default()
                },
                &cancel,
                false
            )
            .unwrap_err()
            .code,
        "cancelled"
    );
    for n in 0..25 {
        f.write("/tmp/query-one", &format!("session-{n}"), "common needle");
    }
    let first = f.query("common");
    assert_eq!(first.hits.len(), 20);
    let token = first.next_cursor.unwrap();
    let second = f
        .service
        .search(
            HistorySearch {
                query: "common".into(),
                cursor: Some(token.clone()),
                ..Default::default()
            },
            &CancelToken::new(),
            false,
        )
        .unwrap();
    assert_eq!(second.hits.len(), 5);
    f.write("/tmp/query-one", "new", "common");
    assert_eq!(
        f.service
            .search(
                HistorySearch {
                    query: "common".into(),
                    cursor: Some(token),
                    ..Default::default()
                },
                &CancelToken::new(),
                false
            )
            .unwrap_err()
            .code,
        "cursor_expired"
    );
    let before = std::fs::read(f.log("new")).unwrap();
    std::fs::write(f.log("new"), b"broken").unwrap();
    assert_eq!(
        f.service
            .search(
                HistorySearch {
                    query: "common".into(),
                    ..Default::default()
                },
                &CancelToken::new(),
                false
            )
            .unwrap_err()
            .code,
        "source_unavailable"
    );
    assert_eq!(std::fs::read(f.log("new")).unwrap(), b"broken");
    assert_ne!(before, b"broken");
}
#[test]
fn history_query_replacement_hides_old_body_and_fallback_title() {
    let f = Fixture::new();
    let prepared = f.write("/tmp/query-one", "one", "erased-secret-phrase");
    assert_eq!(f.query("erased-secret-phrase").hits.len(), 1);
    let mut replacement = user(1, "replacement-visible");
    replacement.surface_op = Some(crate::session::event::SurfaceOp::Replace { start: 0, end: 0 });
    drop(f.backend.append_batch(prepared, 1, &[replacement]).unwrap());
    assert!(f.query("erased-secret-phrase").hits.is_empty());
    assert_eq!(f.query("replacement-visible").hits.len(), 1);
    let request = HistoryReadRequest {
        reference: f.reference("one", 0),
        trusted_projects: false,
        offset: 0,
        version: None,
    };
    assert_eq!(
        f.service
            .read(request, &CancelToken::new(), false)
            .unwrap_err()
            .code,
        "not_found"
    );
}
#[test]
fn history_query_agent_current_session_cutoff_and_no_authority_in_arguments() {
    use crate::Tool;
    let f = Fixture::new();
    let prepared = f.write("/tmp/query-one", "one", "earlier-visible");
    let events = [
        SessionEvent::new("step/start", 1, 44000, json!({"turn":1,"step":1})),
        user(2, "current-step-hidden"),
    ];
    drop(f.backend.append_batch(prepared, 1, &events).unwrap());
    f._sessions
        .resume(&SessionKey {
            project: f.service.project.clone(),
            id: SessionId::new("one"),
        })
        .unwrap();
    let search = tools::HistoryTool {
        service: Arc::new(SessionQueryService::new(
            f.service.index.clone(),
            f._sessions.query_reader(),
            f.service.control.clone(),
            f.service.project.clone(),
        )),
        operation: tools::Operation::Events,
    };
    let project = crate::Project::new("/tmp/query-one");
    let found = search
        .invoke(
            &json!({"session_id":"one","query":"earlier-visible"}),
            &project,
            &CancelToken::new(),
        )
        .unwrap();
    assert_eq!(found["hits"].as_array().unwrap().len(), 1);
    let hidden = search
        .invoke(
            &json!({"session_id":"one","query":"current-step-hidden"}),
            &project,
            &CancelToken::new(),
        )
        .unwrap();
    assert!(hidden["hits"].as_array().unwrap().is_empty());
    assert!(
        search
            .invoke(
                &json!({"session_id":"one","query":"earlier","project":"/tmp/query-two"}),
                &project,
                &CancelToken::new()
            )
            .is_err()
    );
    let read = tools::HistoryTool {
        service: search.service,
        operation: tools::Operation::Read,
    };
    assert!(
        read.invoke(
            &json!({"session_id":"one","event_seq":2}),
            &project,
            &CancelToken::new()
        )
        .is_err()
    );
}
#[test]
fn history_query_compaction_keeps_existing_body_and_ignores_private_domains() {
    let f = Fixture::new();
    let prepared = f.write("/tmp/query-one", "one", "before-compaction-needle");
    let mut summary = SessionEvent::new(
        "compaction/summary",
        1,
        44000,
        json!({"compactionId":"c1","shadowedRange":{"start":0,"end":0},"summary":"private-summary-not-indexed"}),
    );
    summary.surface_op = Some(crate::session::event::SurfaceOp::Replace { start: 0, end: 0 });
    let mut carrier = SessionEvent::new(
        "user/message",
        2,
        44001,
        crate::session::event::payloads::compaction_user_message("internal-checkpoint-not-indexed"),
    );
    carrier.surface_op = Some(crate::session::event::SurfaceOp::Replace { start: 0, end: 0 });
    carrier.source_event_seqs = Some(vec![0]);
    drop(
        f.backend
            .append_batch(prepared, 1, &[summary, carrier])
            .unwrap(),
    );
    assert_eq!(f.query("before-compaction-needle").hits.len(), 1);
    assert!(f.query("private-summary-not-indexed").hits.is_empty());
    assert!(f.query("internal-checkpoint-not-indexed").hits.is_empty());
    // An old derived index may have hidden the original user row under the
    // synthetic replacement. Opening its previous projection version must rebuild it.
    f.service
        .index
        .run(&CancelToken::new(), |connection, _, _| {
            connection
                .execute_batch("UPDATE documents SET visible=0 WHERE seq=0; PRAGMA user_version=0;")
                .map_err(index::sql_error)?;
            Ok(())
        })
        .unwrap();
    f.service.index.reset_connection();
    assert_eq!(f.query("before-compaction-needle").hits.len(), 1);
    assert!(f.query("internal-checkpoint-not-indexed").hits.is_empty());
}
#[cfg(unix)]
#[test]
fn history_query_cache_symlinks_fail_closed_without_touching_target() {
    let f = Fixture::new();
    let outside = f.root.join("outside");
    std::fs::write(&outside, "private target").unwrap();
    std::fs::create_dir(f.root.join("history-index")).unwrap();
    std::os::unix::fs::symlink(&outside, f.root.join("history-index/index.sqlite")).unwrap();
    assert_eq!(
        f.service
            .search(
                HistorySearch {
                    query: "x".into(),
                    ..Default::default()
                },
                &CancelToken::new(),
                false
            )
            .unwrap_err()
            .code,
        "index_unavailable"
    );
    assert_eq!(std::fs::read_to_string(outside).unwrap(), "private target");
}
#[test]
fn history_query_model_results_keep_references_under_general_pruning_limit() {
    let reference = HistoryReference {
        project: Some("p".repeat(4096)),
        session: "s".repeat(1024),
        seq: Some(1),
    };
    let value = json!({"hits":(0..8).map(|_|json!({"reference":reference,"excerpt":"x".repeat(200)})).collect::<Vec<_>>(),"truncated":false});
    let result = tools::bounded_results(value);
    assert!(
        serde_json::to_string(&result).unwrap().chars().count() < crate::plugins::THRESHOLD_CHARS
    );
    assert_eq!(result["hits"][0]["reference"], json!(reference));
    assert_eq!(result["truncated"], true);
}

#[test]
fn navigation_conversation_read_is_public_scoped_and_does_not_select() {
    let fixture = Fixture::new();
    let id = "navigation-reader";
    fixture.write("/tmp/query-one", id, "Visible original message");
    let before = fixture.service.reader.current();
    let request = ConversationRequest {
        reference: fixture.reference(id, 0),
        trusted_projects: false,
        after_seq: None,
        version: None,
        attachments_only: false,
    };
    let page = fixture
        .service
        .conversation(request.clone(), &CancelToken::new())
        .unwrap();
    assert_eq!(page.messages[0].text, "Visible original message");
    assert_eq!(page.messages[0].reference.seq, Some(0));
    assert_eq!(fixture.service.reader.current(), before);
    let mut wrong = request.clone();
    wrong.reference.project = Some("/tmp/not-trusted".into());
    assert!(
        fixture
            .service
            .conversation(wrong, &CancelToken::new())
            .is_err()
    );
    let mut stale = request;
    stale.version = Some("stale".into());
    assert!(
        fixture
            .service
            .conversation(stale, &CancelToken::new())
            .is_err()
    );
}
