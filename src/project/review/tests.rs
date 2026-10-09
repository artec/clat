use super::*;
use crate::project::Project;

fn fixture() -> (std::path::PathBuf, Project, Arc<FileReview>) {
    let root = std::env::temp_dir().join(format!("clat-file-review-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(root.join("workspace")).unwrap();
    std::fs::create_dir(root.join("storage")).unwrap();
    let project = Project::new(root.join("workspace"))
        .with_file_review(&root.join("storage"))
        .unwrap();
    let review = project.file_review().unwrap();
    review.begin("session", 1).unwrap();
    (root, project, review)
}

fn write(project: &Project, path: &str, text: &str) {
    project
        .writable_target(path, true, crate::permission::WriteScope::ProjectRoot)
        .unwrap()
        .atomic_write(text, None)
        .unwrap();
}

#[test]
fn dirty_baseline_captured_and_restored_with_persistent_three_way_facts() {
    let (root, project, review) = fixture();
    std::fs::write(project.root().join("a"), "user dirty before\n").unwrap();
    write(&project, "a", "agent edit\n");
    review.end();
    let reloaded = FileReview::open(&root.join("storage"), project.root()).unwrap();
    let ledger = reloaded.load("session", 1).unwrap().unwrap();
    assert_eq!(
        ledger.files[0].before.as_deref(),
        Some("user dirty before\n")
    );
    assert_eq!(ledger.files[0].after, "agent edit\n");
    assert_eq!(reloaded.preview(&project, &ledger)[0].1, "safe");
    let done = reloaded.apply(&project, ledger).unwrap();
    assert_eq!(done.files[0].status, "restored");
    assert_eq!(
        std::fs::read_to_string(project.root().join("a")).unwrap(),
        "user dirty before\n"
    );
    assert!(done.files[0].recovery_path.is_some());
    crate::test_support::cleanup_tree(&root);
}

#[test]
fn changed_after_preview_and_new_file_takeover_are_never_overwritten_or_deleted() {
    let (root, project, review) = fixture();
    std::fs::write(project.root().join("a"), "original").unwrap();
    write(&project, "a", "agent");
    write(&project, "new", "agent new");
    review.end();
    let ledger = review.load("session", 1).unwrap().unwrap();
    assert!(
        review
            .preview(&project, &ledger)
            .iter()
            .all(|(_, state)| state == "safe")
    );
    std::fs::write(project.root().join("a"), "later user").unwrap();
    std::fs::write(project.root().join("new"), "user took over").unwrap();
    assert!(
        review
            .preview(&project, &ledger)
            .iter()
            .all(|(_, state)| state.contains("conflict"))
    );
    let result = review.apply(&project, ledger).unwrap();
    assert!(result.files[0].note.contains("conflict"));
    assert_eq!(
        std::fs::read_to_string(project.root().join("a")).unwrap(),
        "later user"
    );
    assert_eq!(
        std::fs::read_to_string(project.root().join("new")).unwrap(),
        "user took over"
    );
    crate::test_support::cleanup_tree(&root);
}

#[test]
fn multi_file_failure_is_durable_and_retry_skips_already_restored_file() {
    let (root, project, review) = fixture();
    write(&project, "a", "agent a");
    write(&project, "b", "agent b");
    review.end();
    review
        .fail_restore_index
        .store(1, std::sync::atomic::Ordering::SeqCst);
    let partial = review
        .apply(&project, review.load("session", 1).unwrap().unwrap())
        .unwrap();
    assert_eq!(partial.files[0].status, "restored");
    assert_eq!(partial.files[1].status, "captured");
    assert!(partial.files[1].note.contains("injected"));
    assert!(!project.root().join("a").exists());
    assert!(project.root().join("b").exists());
    std::fs::write(project.root().join("a"), "new user file").unwrap();
    review
        .fail_restore_index
        .store(usize::MAX, std::sync::atomic::Ordering::SeqCst);
    let partial = review.load("session", 1).unwrap().unwrap();
    let done = review.apply(&project, partial).unwrap();
    assert!(done.files.iter().all(|f| f.status == "restored"));
    assert_eq!(
        std::fs::read_to_string(project.root().join("a")).unwrap(),
        "new user file"
    );
    assert!(!project.root().join("b").exists());
    crate::test_support::cleanup_tree(&root);
}

#[test]
fn intervening_writer_and_prepared_crash_record_fail_closed() {
    let (root, project, review) = fixture();
    write(&project, "a", "agent first");
    std::fs::write(project.root().join("a"), "user between").unwrap();
    write(&project, "a", "agent second");
    review.end();
    let mut ledger = review.load("session", 1).unwrap().unwrap();
    assert!(review.preview(&project, &ledger)[0].1.contains("mixed"));
    ledger.files[0].mixed = false;
    ledger.files[0].status = "prepared".into();
    assert!(review.preview(&project, &ledger)[0].1.contains("prepared"));
    crate::test_support::cleanup_tree(&root);
}

#[test]
fn prepared_commit_uncertainty_is_durable_and_never_auto_publishes_expected_bytes() {
    let (root, project, review) = fixture();
    review
        .prepare("uncertain", None, "intended only", None)
        .unwrap();
    review.end();
    let reopened = FileReview::open(&root.join("storage"), project.root()).unwrap();
    let ledger = reopened.load("session", 1).unwrap().unwrap();
    assert_eq!(ledger.files[0].status, "prepared");
    assert!(
        reopened.preview(&project, &ledger)[0]
            .1
            .contains("prepared")
    );
    reopened.apply(&project, ledger).unwrap();
    assert!(!project.root().join("uncertain").exists());
    crate::test_support::cleanup_tree(&root);
}

#[test]
fn prepared_followup_does_not_erase_last_confirmed_write() {
    let (root, project, review) = fixture();
    std::fs::write(project.root().join("a"), "original dirty baseline").unwrap();
    write(&project, "a", "confirmed native write");
    project
        .writable_target("a", true, crate::permission::WriteScope::ProjectRoot)
        .unwrap()
        .prepare_capture("intent only, no second publish")
        .unwrap();
    let prepared = review.load("session", 1).unwrap().unwrap();
    assert_eq!(
        prepared.files[0].confirmed_after(),
        Some("confirmed native write")
    );
    assert!(
        review.preview(&project, &prepared)[0]
            .1
            .contains("prepared")
    );
    // A retry may replace an unconfirmed intent, never the confirmed facts.
    project
        .writable_target("a", true, crate::permission::WriteScope::ProjectRoot)
        .unwrap()
        .prepare_capture("replacement intent, still no publish")
        .unwrap();
    review.end();
    let reopened = FileReview::open(&root.join("storage"), project.root()).unwrap();
    let ledger = reopened.load("session", 1).unwrap().unwrap();
    let serialized = serde_json::to_value(&ledger).unwrap();
    assert_eq!(
        serialized["files"][0]["previous_capture"]["after"],
        "confirmed native write"
    );
    assert_eq!(
        ledger.files[0].before.as_deref(),
        Some("original dirty baseline")
    );
    assert_eq!(
        ledger.files[0].after,
        "replacement intent, still no publish"
    );
    assert_eq!(
        ledger.files[0].confirmed_after(),
        Some("confirmed native write")
    );
    let capture = ledger.files[0].previous_capture.as_ref().unwrap();
    assert_eq!(capture.stamp.hash, digest(capture.after.as_bytes()));
    assert_eq!(ledger.files[0].status, "prepared");
    assert!(reopened.preview(&project, &ledger)[0].1.contains("mixed"));
    assert_eq!(
        std::fs::read_to_string(project.root().join("a")).unwrap(),
        "confirmed native write"
    );
    crate::test_support::cleanup_tree(&root);
}

#[test]
fn confirmed_followup_replaces_intent_but_retains_baseline_and_uncertainty() {
    let (root, project, review) = fixture();
    std::fs::write(project.root().join("a"), "dirty baseline").unwrap();
    write(&project, "a", "first confirmed");
    project
        .writable_target("a", true, crate::permission::WriteScope::ProjectRoot)
        .unwrap()
        .prepare_capture("unpublished intent")
        .unwrap();
    write(&project, "a", "latest confirmed");
    review.end();
    let reopened = FileReview::open(&root.join("storage"), project.root()).unwrap();
    let ledger = reopened.load("session", 1).unwrap().unwrap();
    let entry = &ledger.files[0];
    assert_eq!(entry.before.as_deref(), Some("dirty baseline"));
    assert_eq!(entry.confirmed_after(), Some("latest confirmed"));
    assert!(entry.previous_capture.is_none());
    assert_eq!(
        entry.stamp.as_ref().unwrap().hash,
        digest(b"latest confirmed")
    );
    assert_eq!(entry.status, "captured");
    assert!(
        entry.mixed,
        "uncertain intermediate writes must remain fail-closed"
    );
    assert!(reopened.preview(&project, &ledger)[0].1.contains("mixed"));
    reopened.apply(&project, ledger).unwrap();
    assert_eq!(
        std::fs::read_to_string(project.root().join("a")).unwrap(),
        "latest confirmed"
    );
    crate::test_support::cleanup_tree(&root);
}

#[cfg(unix)]
#[test]
fn replaced_file_or_parent_symlink_never_recovers_outside_project() {
    let (root, project, review) = fixture();
    write(&project, "dir/a", "agent");
    review.end();
    let ledger = review.load("session", 1).unwrap().unwrap();
    std::fs::write(root.join("outside"), "outside user").unwrap();
    std::fs::remove_file(project.root().join("dir/a")).unwrap();
    std::os::unix::fs::symlink(root.join("outside"), project.root().join("dir/a")).unwrap();
    assert_ne!(review.preview(&project, &ledger)[0].1, "safe");
    review.apply(&project, ledger.clone()).unwrap();
    std::fs::rename(project.root().join("dir"), project.root().join("old-dir")).unwrap();
    std::os::unix::fs::symlink(&root, project.root().join("dir")).unwrap();
    review
        .apply(&project, review.load("session", 1).unwrap().unwrap())
        .unwrap();
    assert_eq!(
        std::fs::read_to_string(root.join("outside")).unwrap(),
        "outside user"
    );
    assert!(!root.join("a").exists());
    crate::test_support::cleanup_tree(&root);
}

#[test]
fn racing_new_occupant_is_not_clobbered_and_detached_material_survives_restart() {
    let (root, project, review) = fixture();
    std::fs::write(project.root().join("a"), "dirty original").unwrap();
    write(&project, "a", "agent bytes");
    review.end();
    review
        .takeover_after_detach
        .store(true, std::sync::atomic::Ordering::SeqCst);
    let partial = review
        .apply(&project, review.load("session", 1).unwrap().unwrap())
        .unwrap();
    assert_eq!(
        std::fs::read_to_string(project.root().join("a")).unwrap(),
        "user takeover during recovery"
    );
    assert_eq!(partial.files[0].status, "restoring");
    assert!(!partial.files[0].note.is_empty());
    let held = project
        .root()
        .join(partial.files[0].recovery_path.as_ref().unwrap());
    assert_eq!(
        std::fs::read_to_string(held.join("after")).unwrap(),
        "agent bytes"
    );
    assert_eq!(
        std::fs::read_to_string(held.join("before")).unwrap(),
        "dirty original"
    );
    let reopened = FileReview::open(&root.join("storage"), project.root()).unwrap();
    let ledger = reopened.load("session", 1).unwrap().unwrap();
    assert_ne!(reopened.preview(&project, &ledger)[0].1, "safe");
    reopened.apply(&project, ledger).unwrap();
    assert_eq!(
        std::fs::read_to_string(project.root().join("a")).unwrap(),
        "user takeover during recovery"
    );
    crate::test_support::cleanup_tree(&root);
}

#[test]
fn scratch_writes_do_not_enter_file_review_and_read_only_stays_fenced() {
    let (root, project, review) = fixture();
    let roots = crate::sandbox::roots::WritableRoots::create(project.root()).unwrap();
    let path = roots.scratch().join("one-off.py");
    *project.roots.write().unwrap() = Some(roots.clone());
    let ro = crate::permission::mode_write_scope(crate::permission::PermissionMode::ReadOnly);
    assert!(project.writable_target(&path, true, ro).is_err());
    let pw = crate::permission::mode_write_scope(crate::permission::PermissionMode::ProjectWrite);
    project
        .writable_target(&path, true, pw)
        .unwrap()
        .atomic_write("print(1)", None)
        .unwrap();
    write(&project, "actual-edit", "tracked");
    review.end();
    let ledger = review.load("session", 1).unwrap().unwrap();
    assert_eq!(ledger.files.len(), 1);
    assert_eq!(ledger.files[0].path, "actual-edit");
    roots.close().unwrap();
    crate::test_support::cleanup_tree(&root);
}
