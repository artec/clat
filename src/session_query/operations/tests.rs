use super::*;
use crate::session::{
    event::{SessionEvent, payloads},
    persistence::PreparedSession,
};
use crate::session_query::tests::Fixture;
fn event(kind: &str, seq: u64, data: Value) -> SessionEvent {
    SessionEvent::new(kind, seq, 1000 + seq as i64, data)
}
fn request(f: &Fixture, id: &str) -> OperationsRequest {
    OperationsRequest {
        reference: f.reference(id, 0),
        filter: OperationFilter::default(),
        cursor: None,
    }
}
fn append(
    f: &Fixture,
    prepared: PreparedSession,
    seq: u64,
    events: &[SessionEvent],
) -> PreparedSession {
    f.backend.append_batch(prepared, seq, events).unwrap()
}
fn call(seq: u64, turn: u64, id: &str, name: &str) -> SessionEvent {
    event(
        "tool/call",
        seq,
        payloads::tool_call(
            turn,
            0,
            id,
            name,
            &json!({"path":format!("file-{seq}"),"api_key":"private-value"}),
        ),
    )
}
fn result(seq: u64, turn: u64, id: &str, failed: bool) -> SessionEvent {
    event(
        "tool/result",
        seq,
        payloads::tool_result(
            turn,
            0,
            id,
            payloads::tool_result_content(&json!({"text":"result","password":"never-show"})),
            failed,
        ),
    )
}
#[test]
fn operations_pair_by_turn_and_call_sequence_not_reused_wire_id() {
    let f = Fixture::new();
    let handle = f.write("/tmp/query-one", "calls", "hello");
    append(
        &f,
        handle,
        1,
        &[
            event("turn/start", 1, payloads::turn_start(1)),
            call(2, 1, "same", "write_file"),
            result(3, 1, "same", false),
            call(4, 1, "unfinished", "run_command"),
            event("turn/start", 5, payloads::turn_start(2)),
            call(6, 2, "same", "mcp__remote"),
            result(7, 2, "same", true),
            result(8, 2, "orphan", false),
        ],
    );
    let rows = f
        .service
        .operations(request(&f, "calls"), &CancelToken::new())
        .unwrap()
        .rows;
    assert_eq!(rows.len(), 4);
    assert_eq!(
        rows.iter()
            .map(|r| (r.id.as_str(), r.turn, r.status.as_str()))
            .collect::<Vec<_>>(),
        vec![
            ("result:8", 2, "unknown"),
            ("call:6", 2, "failed"),
            ("call:4", 1, "unfinished"),
            ("call:2", 1, "succeeded")
        ]
    );
    let detail = f
        .service
        .operation_detail(
            OperationDetailRequest {
                reference: f.reference("calls", 0),
                id: "call:2".into(),
            },
            &CancelToken::new(),
        )
        .unwrap();
    assert!(detail.arguments.as_ref().unwrap().contains("file-2"));
    assert!(!detail.arguments.unwrap().contains("private-value"));
    assert!(!detail.result.unwrap().contains("never-show"));
}
#[test]
fn operations_cursor_freezes_prefix_and_accepts_later_append_without_duplicates() {
    let f = Fixture::new();
    let handle = f.write("/tmp/query-one", "pages", "hello");
    let mut events = vec![event("turn/start", 1, payloads::turn_start(1))];
    for seq in 2..=52 {
        events.push(call(seq, 1, "reused", "read_file"));
    }
    let handle = append(&f, handle, 1, &events);
    let first = f
        .service
        .operations(request(&f, "pages"), &CancelToken::new())
        .unwrap();
    assert_eq!(first.rows.len(), 40);
    append(
        &f,
        handle,
        53,
        &[
            result(53, 1, "reused", false),
            call(54, 1, "new", "run_command"),
        ],
    );
    let mut query = request(&f, "pages");
    query.cursor = first.next_cursor;
    let second = f
        .service
        .operations(query.clone(), &CancelToken::new())
        .unwrap();
    assert_eq!(second.rows.len(), 11);
    assert!(second.next_cursor.is_none());
    assert!(
        second
            .rows
            .iter()
            .all(|r| first.rows.iter().all(|old| old.id != r.id))
    );
    assert_eq!(second.boundary, Some(52));
    let latest = f
        .service
        .operations(request(&f, "pages"), &CancelToken::new())
        .unwrap();
    assert_eq!(latest.rows[0].id, "call:54");
    assert_eq!(latest.rows[1].status, "succeeded");
    query.reference.session = "other".into();
    assert!(f.service.operations(query, &CancelToken::new()).is_err());
}
#[test]
fn operations_cold_reader_filters_and_project_session_isolation() {
    let f = Fixture::new();
    let handle = f.write("/tmp/query-one", "one", "hello");
    append(
        &f,
        handle,
        1,
        &[
            event("turn/start", 1, payloads::turn_start(1)),
            call(2, 1, "id", "read_file"),
            result(3, 1, "id", false),
        ],
    );
    f.write("/tmp/query-one", "two", "no tools");
    let bytes_before = std::fs::read(f.log("one")).unwrap();
    let cold = SessionQueryService::new(
        f.service.index.clone(),
        f._sessions.query_reader(),
        f.service.control.clone(),
        f.service.project.clone(),
    );
    let mut query = request(&f, "one");
    query.filter.status = Some("succeeded".into());
    assert_eq!(
        cold.operations(query.clone(), &CancelToken::new())
            .unwrap()
            .rows
            .len(),
        1
    );
    query.filter.turn = Some(2);
    assert!(
        cold.operations(query, &CancelToken::new())
            .unwrap()
            .rows
            .is_empty()
    );
    assert!(
        cold.operations(request(&f, "two"), &CancelToken::new())
            .unwrap()
            .rows
            .is_empty()
    );
    let mut query = request(&f, "one");
    query.reference.project = Some("/tmp/other-project".into());
    assert!(cold.operations(query, &CancelToken::new()).is_err());
    assert!(
        cold.operation_detail(
            OperationDetailRequest {
                reference: f.reference("two", 0),
                id: "call:2".into()
            },
            &CancelToken::new()
        )
        .is_err()
    );
    assert_eq!(
        bytes_before,
        std::fs::read(f.log("one")).unwrap(),
        "querying never rewrites or appends session facts"
    );
    let cancelled = CancelToken::new();
    cancelled.cancel();
    assert!(cold.operations(request(&f, "one"), &cancelled).is_err());
}

#[test]
fn operations_late_result_cannot_settle_a_new_turn_with_the_same_wire_id() {
    let f = Fixture::new();
    let handle = f.write("/tmp/query-one", "late", "hello");
    append(
        &f,
        handle,
        1,
        &[
            event("turn/start", 1, payloads::turn_start(1)),
            call(2, 1, "id", "write_file"),
            event("turn/start", 3, payloads::turn_start(2)),
            call(4, 2, "id", "run_command"),
            result(5, 1, "id", true),
        ],
    );
    let rows = f
        .service
        .operations(request(&f, "late"), &CancelToken::new())
        .unwrap()
        .rows;
    assert_eq!(rows.len(), 2);
    assert_eq!(
        (&*rows[0].id, &*rows[0].status, rows[0].result_seq),
        ("call:4", "unfinished", None)
    );
    assert_eq!(
        (&*rows[1].id, &*rows[1].status, rows[1].result_seq),
        ("call:2", "failed", Some(5))
    );
}

#[test]
fn operations_cursor_rejects_changed_boundaries_and_filters() {
    let f = Fixture::new();
    let handle = f.write("/tmp/query-one", "cursor", "hello");
    let mut events = vec![event("turn/start", 1, payloads::turn_start(1))];
    for seq in 2..=44 {
        events.push(call(seq, 1, "id", "read_file"));
    }
    append(&f, handle, 1, &events);
    let first = f
        .service
        .operations(request(&f, "cursor"), &CancelToken::new())
        .unwrap();
    let mut query = request(&f, "cursor");
    query.cursor = first.next_cursor;
    let mut changed = query.clone();
    changed.filter.turn = Some(2);
    assert!(f.service.operations(changed, &CancelToken::new()).is_err());
    let mut cursor: Value = serde_json::from_str(query.cursor.as_ref().unwrap()).unwrap();
    cursor["digest"] = json!("different-prefix");
    query.cursor = Some(cursor.to_string());
    assert_eq!(
        f.service
            .operations(query, &CancelToken::new())
            .unwrap_err()
            .code,
        "source_changed"
    );
}
