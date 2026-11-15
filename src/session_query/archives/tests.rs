use super::*;
use crate::session::{id::SessionId, portable::Incoming};

#[test]
fn history_archive_is_read_only_project_scoped_and_round_trips_original_bytes() {
    let fixture = super::super::tests::Fixture::new();
    drop(fixture.write("/tmp/query-one", "source", "portable history"));
    let log = fixture.log("source");
    let before = std::fs::read(&log).unwrap();
    let cancel = CancelToken::new();
    let archive = fixture
        .service
        .export_archive(
            ArchiveRequest {
                reference: fixture.reference("source", 0),
                trusted_projects: false,
                imported: false,
            },
            &cancel,
        )
        .unwrap();
    let incoming = Incoming::parse(&archive.bytes, || Ok(())).unwrap();
    assert_eq!(incoming.logs[0].header.id.as_str(), "source");
    assert_eq!(incoming.logs[0].events.len(), 1);
    let rows = fixture
        .service
        .import_archive(&archive.bytes, &cancel)
        .unwrap();
    let reference = rows[0].reference.clone();
    assert_ne!(reference.session, "source");
    fixture
        .service
        .import_archive(&archive.bytes, &cancel)
        .unwrap();
    let rows = fixture
        .service
        .list_archives(ArchiveListRequest::default(), &cancel)
        .unwrap();
    assert_eq!(rows.len(), 1);
    let read = fixture
        .service
        .trajectory_from(
            TrajectoryRequest {
                reference: reference.clone(),
                trusted_projects: false,
                snapshot: None,
                cursor: None,
                anchor_seq: None,
                query: "portable history".into(),
            },
            &cancel,
            true,
        )
        .unwrap();
    assert_eq!(read.matches.len(), 1);
    let error = fixture
        .service
        .trajectory(
            TrajectoryRequest {
                reference: reference.clone(),
                trusted_projects: false,
                snapshot: None,
                cursor: None,
                anchor_seq: None,
                query: String::new(),
            },
            &cancel,
        )
        .unwrap_err();
    assert_eq!(error.code, "source_unavailable");
    let redownload = fixture
        .service
        .export_archive(
            ArchiveRequest {
                reference: reference.clone(),
                trusted_projects: false,
                imported: true,
            },
            &cancel,
        )
        .unwrap();
    assert_eq!(redownload.bytes, archive.bytes);
    assert_eq!(std::fs::read(log).unwrap(), before);
    let foreign = HistoryReference {
        project: Some("/tmp/untrusted-query".into()),
        ..reference
    };
    assert_eq!(
        fixture
            .service
            .export_archive(
                ArchiveRequest {
                    reference: foreign,
                    trusted_projects: true,
                    imported: true,
                },
                &cancel
            )
            .err()
            .unwrap()
            .code,
        "unauthorized"
    );
}

#[test]
fn history_archive_rejects_untrusted_paths_corrupted_objects_and_torn_logs() {
    let fixture = super::super::tests::Fixture::new();
    let header = SessionHeader::new(
        SessionId::new("bad-input"),
        Some("/source/project".into()),
        4,
    );
    let cancel = CancelToken::new();
    let bad_digest = format!("media/sha256:{}.png", "a".repeat(64));
    for name in [
        "../session.v4.jsonl",
        "files/xx/xxxx/hi",
        "media/sha256:invalid.png",
        &bad_digest,
    ] {
        let mut archive = Archive::default();
        archive
            .insert(LOG_NAME.into(), portable::log_bytes(&header, &[]))
            .unwrap();
        archive.insert(name.into(), b"unexpected".to_vec()).unwrap();
        assert!(
            fixture
                .service
                .import_archive(&archive.zip().unwrap(), &cancel)
                .is_err()
        );
    }
    let mut archive = Archive::default();
    let mut log = portable::log_bytes(&header, &[]);
    log.extend(b"{\"seq\":0");
    archive.insert(LOG_NAME.into(), log).unwrap();
    assert!(
        fixture
            .service
            .import_archive(&archive.zip().unwrap(), &cancel)
            .is_err()
    );
    assert!(
        fixture
            .service
            .list_archives(ArchiveListRequest::default(), &cancel)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn history_archive_preserves_denial_as_a_tool_that_never_started() {
    use crate::session::event::payloads;
    let mut events = vec![
        SessionEvent::new(
            "assistant/message",
            0,
            1,
            payloads::assistant_message(1, 1, Vec::new(), "test", "test", None),
        )
        .append(Vec::new()),
        SessionEvent::new(
            "approval/asked",
            1,
            2,
            json!({"callId":"call-1","toolName":"write_file"}),
        )
        .log_only(),
        SessionEvent::new(
            "tool/result",
            2,
            3,
            payloads::tool_result(
                1,
                1,
                "call-1",
                json!([{"type":"text","text":"The user denied the write."}]),
                true,
            ),
        )
        .append(Vec::new()),
    ];
    let source_id = events[2].data["message"]["id"].clone();
    portable::normalize(&mut events).unwrap();
    assert_eq!(events.len(), 3);
    let advertised = &events[0].data["message"]["content"][0];
    assert_eq!(advertised["id"], "call-1");
    assert_eq!(advertised["name"], "write_file");
    assert_eq!(advertised["plugin:clat:argumentsUnknown"], true);
    let result = &events[2].data;
    assert_eq!(result["error"]["code"], "TOOL_NOT_STARTED");
    assert_eq!(result["message"]["id"], "forked-tool-result-call-1-2");
    assert_eq!(result["message"]["plugin:clat:originalId"], source_id);
    assert_eq!(
        result["message"]["content"][0]["text"],
        "The user denied the write."
    );
}
