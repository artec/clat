use super::*;

fn queued(queue: &InputQueue, id: &str) {
    queue
        .enqueue(
            id.to_owned(),
            PendingMessage::from_front_end(id, Some(id.to_owned()), Vec::new()),
            InputDraft::default(),
        )
        .unwrap();
}

#[test]
fn pending_input_claim_is_retained_until_commit_and_fifo_is_preserved() {
    let queue = InputQueue::default();
    queued(&queue, "first");
    queued(&queue, "second");
    assert_eq!(
        queue.claim(false).unwrap().client_message_id.as_deref(),
        Some("first")
    );
    assert_eq!(queue.snapshot().items.len(), 2);
    assert!(
        queue.claim(false).is_none(),
        "a claim cannot be delivered twice"
    );
    assert!(queue.remove("first", 1).is_err());
    queue
        .committed(
            "first",
            &PendingMessage::from_front_end("first", Some("first".into()), Vec::new())
                .request_digest(),
        )
        .unwrap();
    assert_eq!(
        queue.claim(false).unwrap().client_message_id.as_deref(),
        Some("second")
    );
}

#[test]
fn pending_input_interrupt_is_single_and_keeps_the_remaining_order() {
    let queue = InputQueue::default();
    for id in ["first", "urgent", "last"] {
        queued(&queue, id);
    }
    queue.interrupt("urgent", 1).unwrap();
    assert!(queue.interrupt("last", 1).is_err());
    assert!(
        queue.claim(false).is_none(),
        "old worker must not receive another message"
    );
    assert_eq!(
        queue.claim(true).unwrap().client_message_id.as_deref(),
        Some("urgent")
    );
    queue
        .committed(
            "urgent",
            &PendingMessage::from_front_end("urgent", Some("urgent".into()), Vec::new())
                .request_digest(),
        )
        .unwrap();
    assert_eq!(
        queue
            .snapshot()
            .items
            .iter()
            .map(|e| e.id.as_str())
            .collect::<Vec<_>>(),
        ["first", "last"]
    );
}

#[test]
fn pending_input_stop_cancels_handoff_and_failure_keeps_uncommitted_content() {
    let queue = InputQueue::default();
    queued(&queue, "urgent");
    queue.interrupt("urgent", 1).unwrap();
    queue.pause();
    queue.settled(InputSettlement::Failed);
    assert!(queue.claim(true).is_none());
    assert!(!queue.snapshot().interrupting);
    queued(&queue, "resume");
    queue.begin_run();
    assert_eq!(
        queue.claim(false).unwrap().client_message_id.as_deref(),
        Some("urgent")
    );
    queue.settled(InputSettlement::Failed);
    assert_eq!(queue.snapshot().items[0].phase, "queued");
    assert!(queue.snapshot().paused);
}

#[test]
fn pending_input_edit_and_recall_are_versioned_atomic_and_preserve_drafts() {
    let queue = InputQueue::default();
    queued(&queue, "text");
    queue
        .replace(
            "text",
            1,
            PendingMessage::from_front_end("changed", Some("text".into()), Vec::new()),
            InputDraft::default(),
            || Ok(()),
        )
        .unwrap();
    assert!(queue.remove("text", 1).is_err());
    assert!(
        queue
            .recall("text", 2, |_| Err("capacity exceeded".into()))
            .is_err()
    );
    assert_eq!(queue.snapshot().items.len(), 1);
    let view = queue.recall("text", 2, |_| Ok(())).unwrap();
    assert_eq!(view.text, "changed");
    assert!(queue.is_empty());
    queue.remember(
        "operation".into(),
        "args".into(),
        serde_json::json!({"item":view}),
    );
    assert!(queue.receipt("operation", "args").unwrap().is_some());
    assert!(queue.receipt("operation", "other args").is_err());
}

#[test]
fn pending_input_idle_submission_starts_before_paused_messages_without_reordering_them() {
    let queue = InputQueue::default();
    queued(&queue, "old-first");
    queued(&queue, "old-second");
    queue.pause();
    queued(&queue, "new-draft");
    queue.prefer_initial("new-draft".into());
    queued(&queue, "later-draft");
    queue.prefer_initial("later-draft".into());
    assert!(queue.claim(false).is_none());
    let message = queue.claim(true).unwrap();
    assert_eq!(message.client_message_id.as_deref(), Some("new-draft"));
    queue
        .committed("new-draft", &message.request_digest())
        .unwrap();
    assert_eq!(
        queue.claim(false).unwrap().client_message_id.as_deref(),
        Some("old-first")
    );
    assert_eq!(
        queue
            .snapshot()
            .items
            .iter()
            .map(|entry| entry.id.as_str())
            .collect::<Vec<_>>(),
        ["old-first", "old-second", "later-draft"]
    );
}

#[test]
fn pending_input_late_receipt_cannot_remove_unclaimed_or_different_content() {
    let queue = InputQueue::default();
    queued(&queue, "human");
    let message = queue.existing("human").unwrap();
    assert!(
        queue
            .committed("human", &message.request_digest())
            .is_none()
    );
    assert!(!queue.accepts_initial(&message));
    queue.claim(false).unwrap();
    assert!(queue.accepts_initial(&message));
    assert!(queue.committed("human", "different digest").is_none());
    queue.committed("human", &message.request_digest()).unwrap();
    assert!(
        queue
            .committed("human", &message.request_digest())
            .is_none()
    );
}

#[test]
fn pending_input_resubmission_during_cleanup_resumes_but_later_failure_pauses() {
    let queue = InputQueue::default();
    queued(&queue, "old");
    queue.terminal();
    queued(&queue, "resume");
    queue.settled(InputSettlement::Failed);
    assert!(!queue.snapshot().paused);
    queue.begin_run();
    queue.agent_started();
    queue.settled(InputSettlement::Failed);
    assert!(queue.snapshot().paused);
}

#[test]
fn pending_input_failed_handoff_restores_selected_item_and_pauses() {
    let queue = InputQueue::default();
    queued(&queue, "first");
    queued(&queue, "urgent");
    queue.interrupt("urgent", 1).unwrap();
    queue.settled(InputSettlement::Failed);
    let snapshot = queue.snapshot();
    assert!(
        snapshot.paused,
        "failed handoff must wait for explicit retry"
    );
    assert!(!snapshot.interrupting);
    assert_eq!(snapshot.items[1].phase, "queued");
    assert_eq!(
        snapshot
            .items
            .iter()
            .map(|item| item.id.as_str())
            .collect::<Vec<_>>(),
        ["first", "urgent"]
    );
}
