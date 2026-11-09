use super::*;
use crate::session::{
    event::{TurnEndReason, payloads},
    persistence::PreparedSession,
};
use crate::session_query::tests::Fixture;
fn event(kind: &str, seq: u64, data: Value) -> SessionEvent {
    SessionEvent::new(kind, seq, 1000 + seq as i64, data)
}
fn request(f: &Fixture, session: &str) -> TrajectoryRequest {
    let mut reference = f.reference(session, 0);
    reference.seq = None;
    TrajectoryRequest {
        reference,
        trusted_projects: false,
        snapshot: None,
        cursor: None,
        anchor_seq: None,
        query: String::new(),
    }
}
fn append(f: &Fixture, p: PreparedSession, seq: u64, events: &[SessionEvent]) -> PreparedSession {
    f.backend.append_batch(p, seq, events).unwrap()
}
fn call(seq: u64, turn: u64) -> SessionEvent {
    event(
        "tool/call",
        seq,
        payloads::tool_call(
            turn,
            1,
            "reused",
            "read_file",
            &json!({"path":"needle.rs","api_key":"secret-value"}),
        ),
    )
}
fn result(seq: u64, turn: u64) -> SessionEvent {
    event(
        "tool/result",
        seq,
        payloads::tool_result(
            turn,
            1,
            "reused",
            payloads::tool_result_content(
                &json!({"text":"result needle","password":"secret-result"}),
            ),
            false,
        ),
    )
}
fn page(f: &Fixture, r: TrajectoryRequest) -> TrajectoryPage {
    f.service.trajectory(r, &CancelToken::new()).unwrap()
}
#[test]
fn trajectory_pairs_reused_ids_late_results_and_keeps_steering_in_recorded_turn() {
    let f = Fixture::new();
    let p = f.write("/tmp/query-one", "trace", "unowned");
    append(
        &f,
        p,
        1,
        &[
            event("turn/start", 1, payloads::turn_start(1)),
            call(2, 1),
            event("turn/start", 3, payloads::turn_start(2)),
            call(4, 2),
            result(5, 1),
            result(6, 2),
            event("user/message", 7, payloads::user_message("steering")).append(Vec::new()),
        ],
    );
    let r = page(&f, request(&f, "trace"));
    assert_eq!(
        r.records
            .iter()
            .filter(|r| r.kind == "tool")
            .map(|r| (r.id.as_str(), r.turn, r.source_seqs.clone()))
            .collect::<Vec<_>>(),
        vec![
            ("tool:2", Some(1), vec![2, 5]),
            ("tool:4", Some(2), vec![4, 6])
        ]
    );
    assert_eq!(r.records.iter().find(|r| r.seq == 7).unwrap().turn, Some(2));
    assert_eq!(r.records.iter().find(|r| r.seq == 0).unwrap().turn, None);
}
#[test]
fn trajectory_snapshot_survives_append_and_details_are_redacted_and_read_only() {
    let f = Fixture::new();
    let p = f.write("/tmp/query-one", "frozen", "hello");
    let p = append(
        &f,
        p,
        1,
        &[
            event("turn/start", 1, payloads::turn_start(1)),
            call(2, 1),
            result(3, 1),
        ],
    );
    let r = page(&f, request(&f, "frozen"));
    append(
        &f,
        p,
        4,
        &[event("user/message", 4, payloads::user_message("new tail")).append(Vec::new())],
    );
    let before = std::fs::read(f.log("frozen")).unwrap();
    let mut req = request(&f, "frozen");
    req.snapshot = Some(r.snapshot.clone());
    assert_eq!(page(&f, req).records.len(), r.records.len());
    let d = f
        .service
        .trajectory_detail(
            TrajectoryDetailRequest {
                reference: r_ref(&f, "frozen"),
                trusted_projects: false,
                snapshot: r.snapshot,
                id: "tool:2".into(),
            },
            &CancelToken::new(),
        )
        .unwrap();
    assert_eq!(d.record.source_seqs, vec![2, 3]);
    assert!(!text(&d.arguments.unwrap()).contains("secret-value"));
    assert!(!text(&d.output.unwrap()).contains("secret-result"));
    assert_eq!(d.raw.len(), 2);
    assert!(!d.raw.join("").contains("secret-"));
    assert!(f.service.reader.current().is_none());
    assert_eq!(std::fs::read(f.log("frozen")).unwrap(), before);
}
fn r_ref(f: &Fixture, id: &str) -> HistoryReference {
    request(f, id).reference
}
#[test]
fn trajectory_snapshot_detects_truncation_and_prefix_replacement() {
    let f = Fixture::new();
    let p = f.write("/tmp/query-one", "changed", "old");
    let original = std::fs::read(f.log("changed")).unwrap();
    let p = append(&f, p, 1, &[call(1, 1)]);
    let r = page(&f, request(&f, "changed"));
    let mut replacement = event("user/message", 2, payloads::user_message("replacement"));
    replacement.surface_op = Some(crate::session::event::SurfaceOp::Replace { start: 0, end: 0 });
    append(&f, p, 2, &[replacement]);
    let mut req = request(&f, "changed");
    req.snapshot = Some(r.snapshot.clone());
    assert_eq!(
        f.service
            .trajectory(req.clone(), &CancelToken::new())
            .unwrap_err()
            .code,
        "source_changed"
    );
    let fresh = page(&f, request(&f, "changed"));
    assert!(!fresh.records.iter().any(|row| row.seq == 0));
    assert!(fresh.records.iter().any(|row| row.preview == "replacement"));
    std::fs::write(f.log("changed"), original).unwrap();
    assert_eq!(
        f.service
            .trajectory(req, &CancelToken::new())
            .unwrap_err()
            .code,
        "source_changed"
    );
}
#[test]
fn trajectory_pages_anchor_exact_results_and_searches_tool_output() {
    let f = Fixture::new();
    let mut p = f.write("/tmp/query-one", "many", "start");
    for n in 1..=180 {
        p = append(&f, p, n, &[call(n, 1)]);
    }
    let first = page(&f, request(&f, "many"));
    assert!(first.records.len() <= PAGE);
    assert!(first.before_cursor.is_some());
    let mut req = request(&f, "many");
    req.cursor = first.before_cursor;
    let mut wrong_scope = req.clone();
    wrong_scope.trusted_projects = true;
    assert_eq!(
        f.service
            .trajectory(wrong_scope, &CancelToken::new())
            .unwrap_err()
            .code,
        "scope_changed"
    );
    let older = page(&f, req);
    assert!(older.records[0].seq < first.records[0].seq);
    let mut req = request(&f, "many");
    req.anchor_seq = Some(12);
    let anchored = page(&f, req);
    assert_eq!(anchored.selected_id.as_deref(), Some("tool:12"));
    assert!(anchored.records.iter().any(|r| r.seq == 12));
    let p = append(&f, p, 181, &[result(181, 1)]);
    drop(p);
    let mut req = request(&f, "many");
    req.query = "result needle".into();
    let found = page(&f, req);
    assert_eq!(found.matches, vec!["tool:180"]);
}
#[test]
fn trajectory_fences_cross_project_scope_and_cancellation() {
    let f = Fixture::new();
    f.write("/tmp/query-one", "one", "hello");
    f.write("/tmp/query-two", "two", "other");
    let mut req = request(&f, "two");
    req.reference.project = Some("/tmp/query-two".into());
    assert_eq!(
        f.service
            .trajectory(req.clone(), &CancelToken::new())
            .unwrap_err()
            .code,
        "unauthorized"
    );
    f.service
        .control
        .add_trust(std::path::Path::new("/tmp/query-two"))
        .unwrap();
    req.trusted_projects = true;
    assert!(!page(&f, req.clone()).records.is_empty());
    f.service
        .control
        .remove_trust(std::path::Path::new("/tmp/query-two"))
        .unwrap();
    assert_eq!(
        f.service
            .trajectory(req, &CancelToken::new())
            .unwrap_err()
            .code,
        "unauthorized"
    );
    let cancel = CancelToken::new();
    cancel.cancel();
    assert_eq!(
        f.service
            .trajectory(request(&f, "one"), &cancel)
            .unwrap_err()
            .code,
        "cancelled"
    );
}
#[test]
fn trajectory_only_real_terminal_marks_final_answer_and_missing_time_stays_unknown() {
    let mut fold = Projection::new("", None);
    fold.push(&event("turn/start", 0, payloads::turn_start(1)));
    // A normal settled assistant record is not itself a terminal signal.
    let data = json!({"turn":1,"step":1,"message":{"role":"assistant","source":{"provider":"test","model":"test"},"content":[{"type":"reasoning","text":"think"},{"type":"text","text":"answer"}]}});
    fold.push(&SessionEvent::new("assistant/message", 1, 0, data));
    assert!(fold.entries.iter().all(|e| !e.row.final_answer));
    assert!(fold.entries.iter().all(|e| e.row.started_at.is_none()));
    let wire = serde_json::to_value(&fold.entries[0].row).unwrap();
    assert!(wire.get("started_at").is_none());
    assert!(wire.get("ended_at").is_none());
    fold.push(&event(
        "turn/end",
        2,
        payloads::turn_end(1, &TurnEndReason::Completed),
    ));
    assert_eq!(
        fold.entries
            .iter()
            .filter(|e| e.row.final_answer)
            .map(|e| e.row.kind.as_str())
            .collect::<Vec<_>>(),
        vec!["message"]
    );
}

#[test]
fn trajectory_compaction_carriers_and_repeated_ids_do_not_duplicate_disclosure() {
    let f = Fixture::new();
    let p = f.write("/tmp/query-one", "compact", "original");
    let summary = payloads::compaction_summary(
        "same",
        "summary",
        (0, 0),
        &[0],
        100,
        "provider",
        "model",
        512,
        json!({}),
    );
    let mut carrier = event(
        "user/message",
        2,
        payloads::compaction_user_message("summary"),
    );
    carrier.surface_op = Some(crate::session::event::SurfaceOp::Replace { start: 0, end: 0 });
    carrier.source_event_seqs = Some(vec![0]);
    append(
        &f,
        p,
        1,
        &[
            event("compaction/summary", 1, summary.clone()).log_only(),
            carrier,
            event("compaction/summary", 3, summary).log_only(),
            event(
                "compaction/end",
                4,
                payloads::compaction_end("failed", 1, Some("failed")),
            )
            .log_only(),
        ],
    );
    let mut req = request(&f, "compact");
    req.anchor_seq = Some(3);
    let result = page(&f, req);
    assert_eq!(result.selected_id.as_deref(), Some("compaction:1"));
    assert_eq!(result.records.len(), 2);
    assert_eq!(result.records[0].preview, "original");
    let detail = f
        .service
        .trajectory_detail(
            TrajectoryDetailRequest {
                reference: r_ref(&f, "compact"),
                trusted_projects: false,
                snapshot: result.snapshot,
                id: "compaction:1".into(),
            },
            &CancelToken::new(),
        )
        .unwrap();
    assert_eq!(
        detail.disclosure,
        Some(json!({"id":"same","range":[0,0],"records":1,"estimated_tokens":100}))
    );
    let mut legacy = Projection::new("", Some("compaction:0".into()));
    legacy.push(&event(
        "compaction/summary",
        0,
        json!({"summary":[{"type":"text","text":"legacy"}]}),
    ));
    assert_eq!(
        legacy.entries[0].detail.as_ref().unwrap().disclosure,
        Some(json!({}))
    );
}

#[test]
fn trajectory_recent_selection_uses_latest_turn_even_without_a_final_response() {
    let f = Fixture::new();
    let p = f.write("/tmp/query-one", "recent", "original");
    let assistant = payloads::assistant_message(
        1,
        1,
        vec![json!({"type":"text","text":"answer"})],
        "test",
        "test",
        None,
    );
    append(
        &f,
        p,
        1,
        &[
            event("turn/start", 1, payloads::turn_start(1)),
            event("assistant/message", 2, assistant),
            event(
                "turn/end",
                3,
                payloads::turn_end(1, &TurnEndReason::Completed),
            ),
            event("turn/start", 4, payloads::turn_start(2)),
            event("user/message", 5, payloads::user_message("new question")).append(vec![]),
        ],
    );
    let result = page(&f, request(&f, "recent"));
    assert_eq!(result.selected_id.as_deref(), Some("user:5"));
    let answer = result
        .overview
        .iter()
        .find(|span| span.id == "message:2")
        .unwrap();
    assert!(answer.time.is_some());
    assert!(answer.started_at.is_none());
    assert!(answer.ended_at.is_some());
    assert_eq!(
        result
            .turns
            .iter()
            .find(|turn| turn.turn == Some(2))
            .unwrap()
            .question,
        "new question"
    );
}

#[test]
fn trajectory_pairs_late_results_with_their_recorded_step_when_call_ids_repeat() {
    let mut projection = Projection::new("", None);
    let mut second_call = call(2, 1);
    second_call.data["step"] = json!(2);
    let mut second_result = result(4, 1);
    second_result.data["step"] = json!(2);
    for event in [call(1, 1), second_call, result(3, 1), second_result] {
        projection.push(&event);
    }
    assert_eq!(projection.entries.len(), 2);
    assert_eq!(projection.entries[0].row.source_seqs, vec![1, 3]);
    assert_eq!(projection.entries[1].row.source_seqs, vec![2, 4]);
}
