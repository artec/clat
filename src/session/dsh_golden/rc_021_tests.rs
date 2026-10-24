//! R20: native DSH 0.2.1-rc.2 bytes, not a synthetic copy of CLAT output.
use super::tests::{key_for, load_golden, mount_fixture_generation};
use crate::session::admission::{AdmissionError, admit_events_for_version};
use crate::session::event::{SessionEvent, payloads};
use crate::session::persistence::JsonlCompression;
use crate::session::projection::{CheckpointIdentity, ProjectionRegistry};
use crate::session::replay::ReplayAdapter;
use crate::session::run_journal::NewSessionEvent;
use crate::session::use_cases::SessionService;
use serde_json::{Value, json};

const REGISTERED: (&str, &str) = (
    "v4-registered-0.2.1.jsonl.zstd",
    "018f2a64-9d3f-7cde-8123-9a4f2b6c0f01",
);
const SETTLED: (&str, &str) = (
    "v4-settled-0.2.1.jsonl.zstd",
    "018f2a64-9d3f-7cde-8123-9a4f2b6c0f02",
);
const TYPES: [&str; 3] = ["working-directory/change", "job/registered", "job/settled"];

fn golden(sample: (&str, &str)) -> Vec<SessionEvent> {
    let (root, backend) = mount_fixture_generation(sample.0, sample.1, 4);
    let events = load_golden(&backend, sample.1);
    crate::test_support::cleanup_tree(&root);
    events
}

fn metadata() -> Vec<SessionEvent> {
    golden(SETTLED)
        .into_iter()
        .filter(|event| TYPES.contains(&event.event_type.as_str()))
        .collect()
}

#[test]
fn native_registered_session_loads_without_inventing_a_settlement() {
    let events = golden(REGISTERED);
    assert_eq!(events.len(), 8);
    let job = events
        .iter()
        .find(|event| event.event_type == TYPES[1])
        .unwrap();
    assert_eq!(
        job.data["toolCall"],
        json!({"callId":"nested-call","rootCallId":"root-call"})
    );
    assert!(!events.iter().any(|event| event.event_type == TYPES[2]));
    assert!(events.iter().any(|event| event.event_type == TYPES[0]));
}

#[test]
fn native_settled_session_loads_all_three_required_metadata_types() {
    let events = golden(SETTLED);
    assert_eq!(events.len(), 9);
    for kind in TYPES {
        let event = events
            .iter()
            .find(|event| event.event_type == kind)
            .unwrap();
        assert_eq!(event.ignorable, None, "native records are required");
        assert_eq!(
            event.surface_op, None,
            "metadata must not become model messages"
        );
    }
    let settlement = events
        .iter()
        .find(|event| event.event_type == TYPES[2])
        .unwrap();
    assert_eq!(settlement.data["status"], "completed");
    assert_eq!(settlement.data["cause"], "producer");
    assert_eq!(settlement.data["producerReported"], true);
    let plugin = events
        .iter()
        .find(|event| event.event_type == "plugin:r20/probe")
        .unwrap();
    assert_eq!(plugin.ignorable, Some(true));
    assert_eq!(plugin.data, json!({"arbitrary":[null,{"future":true}]}));
    let user = events
        .iter()
        .find(|event| event.event_type == "user/message")
        .unwrap();
    assert_eq!(user.data["source"]["principal"]["kind"], "share-viewer");
}

fn rejects_bad_fields(kind: &str, required: &[(&str, Value)], optional: &[(&str, Value)]) {
    let event = metadata()
        .into_iter()
        .find(|event| event.event_type == kind)
        .unwrap();
    for (field, bad) in required.iter().chain(optional) {
        let mut changed = event.clone();
        changed.data[*field] = bad.clone();
        assert!(
            matches!(admit_events_for_version(&[changed], 4),
            Err(AdmissionError::MalformedPayload { event_type, .. }) if event_type == kind),
            "{kind} must validate {field}"
        );
    }
    for (field, _) in required {
        let mut missing = event.clone();
        missing.data.as_object_mut().unwrap().remove(*field);
        assert!(
            matches!(admit_events_for_version(&[missing], 4),
            Err(AdmissionError::MalformedPayload { event_type, .. }) if event_type == kind),
            "{kind} must require {field}"
        );
    }
}

#[test]
fn working_directory_payload_requires_a_string() {
    rejects_bad_fields(TYPES[0], &[("cwd", json!(false))], &[]);
}

fn registration_fields() -> Vec<(&'static str, Value)> {
    vec![
        ("jobId", json!(0)),
        ("owner", json!([])),
        ("kind", Value::Null),
        ("label", json!({})),
        ("startedAt", json!("yesterday")),
    ]
}

#[test]
fn registered_payload_validates_identity_time_and_optional_tool_call() {
    rejects_bad_fields(
        TYPES[1],
        &registration_fields(),
        &[
            ("toolCall", Value::Null),
            ("toolCall", json!({})),
            ("toolCall", json!({"callId":1,"rootCallId":"root"})),
            ("toolCall", json!({"callId":"call"})),
            ("toolCall", json!({"callId":"call","rootCallId":false})),
        ],
    );
}

#[test]
fn settled_payload_validates_terminal_facts_and_optional_detail() {
    let mut required = registration_fields();
    required.extend([
        ("finishedAt", json!(false)),
        ("status", json!("running")),
        ("cause", json!("timeout")),
        ("producerReported", json!(1)),
    ]);
    rejects_bad_fields(
        TYPES[2],
        &required,
        &[
            ("detail", Value::Null),
            ("toolCall", json!({"rootCallId":"root"})),
        ],
    );
}

#[test]
fn metadata_generation_windows_keep_old_required_types_closed_and_ignorable_opaque() {
    for event in metadata() {
        for version in [0, 2, 3] {
            assert_eq!(
                admit_events_for_version(std::slice::from_ref(&event), version),
                Err(AdmissionError::RequiredUnknown(event.event_type.clone()))
            );
            let mut opaque = event.clone();
            opaque.ignorable = Some(true);
            opaque.data = Value::Null;
            assert!(admit_events_for_version(&[opaque], version).is_ok());
        }
    }
}

#[test]
fn metadata_extensions_and_all_declared_terminal_values_remain_readable() {
    for mut event in metadata() {
        event.data["future"] = json!({"unrecognized":[1,2]});
        // TS numbers do not impose integer, monotonic-clock or positivity rules.
        if event.data.get("startedAt").is_some() {
            event.data["startedAt"] = json!(-0.5);
            event.data["toolCall"]["future"] = json!(true);
        }
        admit_events_for_version(std::slice::from_ref(&event), 4).unwrap();
        event.data.as_object_mut().unwrap().remove("toolCall");
        event.data.as_object_mut().unwrap().remove("detail");
        admit_events_for_version(std::slice::from_ref(&event), 4).unwrap();
        if event.event_type == TYPES[2] {
            for status in ["completed", "killed", "failed"] {
                for cause in ["producer", "kill", "teardown"] {
                    event.data["status"] = json!(status);
                    event.data["cause"] = json!(cause);
                    event.data["producerReported"] = json!(false);
                    admit_events_for_version(std::slice::from_ref(&event), 4).unwrap();
                }
            }
        }
    }
    let mut plugin = SessionEvent::new("plugin:r20/future", 0, 0, Value::Null);
    plugin.ignorable = Some(true);
    admit_events_for_version(std::slice::from_ref(&plugin), 4).unwrap();
    plugin.ignorable = None;
    assert!(matches!(
        admit_events_for_version(&[plugin], 4),
        Err(AdmissionError::RequiredUnknown(_))
    ));
}

#[test]
fn metadata_incremental_replay_and_every_checkpoint_cut_equal_full_fold() {
    for sample in [REGISTERED, SETTLED] {
        let events = golden(sample);
        let mut full = ProjectionRegistry::clat();
        full.fold_all(&events).unwrap();
        let identity = CheckpointIdentity {
            created_at: 42,
            cwd: Some("/project".into()),
        };
        let expected = full.checkpoint(identity.clone(), 1).rows;
        for cut in 0..=events.len() {
            let mut prefix = ProjectionRegistry::clat();
            for event in &events[..cut] {
                prefix.fold_one(event).unwrap();
            }
            let record = prefix.checkpoint(identity.clone(), 1);
            let mut restored = ProjectionRegistry::clat();
            restored
                .restore(&record, &events[cut..], cut as u64)
                .unwrap();
            assert_eq!(
                restored.checkpoint(identity.clone(), 1).rows,
                expected,
                "cut={cut}"
            );
        }
        let mut adapter = ReplayAdapter::new();
        let mut live = Vec::new();
        for event in &events {
            adapter.push(event, &mut live);
        }
        assert_eq!(live, ReplayAdapter::fold(&events));
        let visible: Vec<_> = events
            .iter()
            .filter(|event| {
                !TYPES.contains(&event.event_type.as_str())
                    && !event.event_type.starts_with("plugin:")
            })
            .cloned()
            .collect();
        assert_eq!(
            live,
            ReplayAdapter::fold(&visible),
            "metadata emits no replay item"
        );
        assert_eq!(full.surface_nodes().unwrap().len(), 1);
        assert!(full.state_snapshot("workingDirectory").is_none());
        assert_eq!(
            full.state_snapshot("jobs"),
            Some(json!({"rows":{}})),
            "the native Job projection exists, but must never adopt foreign DSH jobs"
        );
    }
}

#[test]
fn metadata_sessions_resume_append_close_and_reopen_without_rewriting_native_bytes() {
    for sample in [REGISTERED, SETTLED] {
        let (root, backend) = mount_fixture_generation(sample.0, sample.1, 4);
        let key = key_for(sample.1);
        let log = crate::session::path_layout::log_path(
            &root,
            Some("/Users/deng/Documents/GitHub/clat"),
            &key.id,
            JsonlCompression::Zstd,
        );
        let bytes = std::fs::read(&log).unwrap();
        let initial = load_golden(&backend, sample.1);
        let service = SessionService::new(root.clone(), JsonlCompression::Zstd).unwrap();
        service.resume(&key).expect("native V4 resume");
        assert!(!service.is_read_only());
        let journal = service.journal().unwrap();
        journal
            .append_atomic(&[
                NewSessionEvent::new("turn/start", payloads::turn_start(2)),
                NewSessionEvent::new("user/message", payloads::user_message("continue"))
                    .append(Vec::new()),
                NewSessionEvent::new(
                    "turn/end",
                    payloads::turn_end(2, &crate::session::event::TurnEndReason::Completed),
                ),
            ])
            .unwrap();
        journal.flush().unwrap();
        service.quiesce_active().unwrap();
        let appended = std::fs::read(&log).unwrap();
        assert!(appended.starts_with(&bytes) && appended.len() > bytes.len());
        let loaded = load_golden(&backend, sample.1);
        assert_eq!(&loaded[..initial.len()], initial.as_slice());
        assert_eq!(
            loaded[initial.len()..]
                .iter()
                .map(|event| event.event_type.as_str())
                .collect::<Vec<_>>(),
            ["session/end-seed", "turn/start", "user/message", "turn/end"],
            "resume retains the established seed marker before the new turn"
        );
        assert!(
            loaded[initial.len()..]
                .iter()
                .all(|event| !TYPES.contains(&event.event_type.as_str()))
        );
        let view = service.resume(&key).expect("reopen after checkpoint");
        assert_eq!(
            view.header.cwd.as_deref(),
            Some("/Users/deng/Documents/GitHub/clat"),
            "recorded effective cwd does not rewrite the immutable project header"
        );
        assert_eq!(view.replay, ReplayAdapter::fold(&loaded));
        service.quiesce_active().unwrap();
        crate::test_support::cleanup_tree(&root);
    }
}
