use super::*;

fn admitted_message() -> SessionEvent {
    SessionEvent::new(
        "user/message",
        0,
        1,
        payloads::admitted_user_message(
            "message-target",
            "target",
            &[],
            Some("delivery-target"),
            Some(&"a".repeat(64)),
        ),
    )
}

#[test]
fn admission_scan_retries_drift_without_publishing_partial_owner() {
    let event = admitted_message();
    let mut attempts = 0;
    let found = scan_committed_admission("delivery-target", |visitor| {
        attempts += 1;
        if attempts == 1 {
            visitor(&event).map_err(SessionError::Corruption)?;
            return Err(SessionError::Io("changed while streaming".into()));
        }
        Ok(())
    })
    .expect("a stable second read succeeds");
    assert_eq!(attempts, 2);
    assert!(found.is_none(), "a failed attempt cannot publish its owner");
}

#[test]
fn admission_scan_rebuilds_owner_after_drift() {
    let event = admitted_message();
    let mut attempts = 0;
    let found = scan_committed_admission("delivery-target", |visitor| {
        attempts += 1;
        visitor(&event).map_err(SessionError::Corruption)?;
        if attempts == 1 {
            Err(SessionError::Io("changed while streaming".into()))
        } else {
            Ok(())
        }
    })
    .expect("retry must not count the previous attempt as a duplicate");
    assert_eq!(attempts, 2);
    assert_eq!(found.unwrap().request_digest, Some("a".repeat(64)));
}

#[test]
fn admission_scan_bounds_io_and_rejects_corruption_without_retry() {
    let mut attempts = 0;
    let result = scan_committed_admission("delivery-target", |_| {
        attempts += 1;
        Err(SessionError::Io("persistent failure".into()))
    });
    assert!(matches!(result, Err(SessionError::Io(_))));
    assert_eq!(
        attempts, 3,
        "persistent drift must fail after bounded attempts"
    );
    let mut attempts = 0;
    let result = scan_committed_admission("delivery-target", |_| {
        attempts += 1;
        Err(SessionError::Corruption("invalid record".into()))
    });
    assert!(matches!(result, Err(SessionError::Corruption(_))));
    assert_eq!(attempts, 1, "corrupt logs must not be retried or hidden");
}

#[test]
fn admission_scan_rejects_two_owners_in_one_stable_log() {
    let event = admitted_message();
    let mut attempts = 0;
    let result = scan_committed_admission("delivery-target", |visitor| {
        attempts += 1;
        visitor(&event).map_err(SessionError::Corruption)?;
        visitor(&event).map_err(SessionError::Corruption)
    });
    assert!(matches!(result, Err(SessionError::Corruption(_))));
    assert_eq!(attempts, 1);
}
