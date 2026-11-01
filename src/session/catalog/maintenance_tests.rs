use crate::session::{
    adapter,
    event::{SessionEvent, payloads},
    surface::Surface,
};
use serde_json::json;

#[test]
fn phase8_offload_survives_replay_without_destroying_attachment_evidence() {
    let original = SessionEvent::new("user/message", 0, 0, json!({"role":"user","content":[{"type":"text","text":"evidence"},{"type":"image","path":"/not-read.png","mediaType":"image/png"}]})).append(vec![]);
    let offload = SessionEvent::new(
        "image/offload",
        1,
        1,
        json!({"targets":[{"seq":0,"imageIndexes":[0]}]}),
    );
    let events = vec![original.clone(), offload];
    let surface = Surface::fold(&events).unwrap();
    let items = adapter::surface_to_model_items(&events, &surface).unwrap();
    assert_eq!(
        items
            .iter()
            .flat_map(crate::model::model_item_image_parts)
            .count(),
        0
    );
    assert_eq!(events[0], original);
    let duplicate = SessionEvent::new(
        "image/offload",
        2,
        2,
        json!({"targets":[{"seq":0,"imageIndexes":[0]}]}),
    );
    assert!(Surface::fold(&[events, vec![duplicate]].concat()).is_err());
}
#[test]
fn phase8_offload_payload_rejects_duplicate_targets_and_invalid_indexes() {
    for targets in [
        json!([]),
        json!([{"seq":0,"imageIndexes":[0,0]}]),
        json!([{"seq":0,"imageIndexes":[]}]),
    ] {
        let event = SessionEvent::new("image/offload", 0, 0, json!({"targets":targets}));
        assert!(
            super::event_spec("image/offload")
                .unwrap()
                .validate(&event, 4)
                .is_err()
        );
    }
    let _ = payloads::user_message("keep vocabulary test imports explicit");
}

#[test]
fn phase8_prune_replacement_is_not_a_second_tool_execution_in_history() {
    let data = payloads::tool_result(
        1,
        1,
        "call",
        payloads::tool_result_content(&json!({"raw":"original evidence"})),
        false,
    );
    let original = SessionEvent::new("tool/result", 0, 0, data.clone()).append(vec![]);
    let mut next = data;
    next["message"]["content"] = json!([{"type":"text","text":"short"}]);
    let mut replacement = SessionEvent::new("tool/result", 1, 1, next);
    replacement.surface_op = Some(crate::session::event::SurfaceOp::Replace { start: 0, end: 0 });
    replacement.source_event_seqs = Some(vec![0]);
    let mut projections = crate::session::projection::ProjectionRegistry::clat();
    projections
        .fold_all(&[original.clone(), replacement.clone()])
        .unwrap();
    assert_eq!(
        projections.state_snapshot("transcript").unwrap()["entries"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(projections.state_snapshot("stats").unwrap()["messages"], 1);
    let replay = crate::session::replay::ReplayAdapter::fold(&[original, replacement]);
    assert_eq!(
        replay
            .iter()
            .filter(|event| matches!(
                event,
                crate::session::replay::ReplayEvent::ToolFinished { .. }
            ))
            .count(),
        1
    );
}

#[test]
fn phase8_offload_checkpoint_tail_and_complete_replay_are_identical() {
    use crate::session::projection::{CheckpointIdentity, ProjectionRegistry};
    let user=SessionEvent::new("user/message",0,0,json!({"role":"user","content":[{"type":"image","path":"/not-read","mediaType":"image/png"},{"type":"image","path":"/also-not-read","mediaType":"image/png"}]})).append(vec![]);
    let first = SessionEvent::new(
        "image/offload",
        1,
        1,
        json!({"targets":[{"seq":0,"imageIndexes":[0]}]}),
    );
    let tail = SessionEvent::new(
        "image/offload",
        2,
        2,
        json!({"targets":[{"seq":0,"imageIndexes":[1]}]}),
    );
    let identity = CheckpointIdentity {
        created_at: 0,
        cwd: None,
    };
    let mut prefix = ProjectionRegistry::clat();
    prefix.fold_all(&[user.clone(), first.clone()]).unwrap();
    let checkpoint = prefix.checkpoint(identity.clone(), 1);
    let mut restored = ProjectionRegistry::clat();
    restored
        .restore(&checkpoint, std::slice::from_ref(&tail), 2)
        .unwrap();
    let mut complete = ProjectionRegistry::clat();
    complete.fold_all(&[user, first, tail]).unwrap();
    assert_eq!(
        restored.checkpoint(identity.clone(), 2).rows,
        complete.checkpoint(identity, 2).rows
    );
    assert_eq!(
        restored
            .surface_nodes()
            .unwrap()
            .iter()
            .flat_map(|(_, item)| crate::model::model_item_image_parts(item))
            .count(),
        0
    );
}
#[test]
fn phase8_offload_invalid_second_target_cannot_publish_the_first() {
    let user = SessionEvent::new(
        "user/message",
        0,
        0,
        json!({"content":[{"type":"image","path":"/not-read","mediaType":"image/png"}]}),
    )
    .append(vec![]);
    let mut surface = Surface::fold(std::slice::from_ref(&user)).unwrap();
    let original = surface.clone();
    let invalid = SessionEvent::new(
        "image/offload",
        1,
        1,
        json!({"targets":[{"seq":0,"imageIndexes":[0]},{"seq":5,"imageIndexes":[0]}]}),
    );
    assert!(surface.apply_public(&invalid, &[user]).is_err());
    assert_eq!(surface, original);
}
