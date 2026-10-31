use super::*;
use crate::session::event::SessionEvent;
use crate::session::projection::{CheckpointIdentity, ProjectionRegistry};
use crate::session::{key::ProjectKey, persistence::JsonlCompression};
struct Fixture {
    sessions: Arc<SessionService>,
    service: PlanModeService,
    root: std::path::PathBuf,
    id: crate::SessionId,
}
impl Fixture {
    fn new() -> Self {
        let base = std::env::temp_dir().join(format!("clat-plans-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&base).unwrap();
        let root = base.join("sessions");
        let sessions = Arc::new(SessionService::new(root.clone(), JsonlCompression::None).unwrap());
        let id = sessions
            .new_session(&ProjectKey::from_cwd("/plan-project"))
            .unwrap()
            .id;
        let service = PlanModeService::new(sessions.clone(), AskUserSlot::shared());
        service.set_durable(true).unwrap();
        Self {
            sessions,
            service,
            root,
            id,
        }
    }
    fn path(&self, doc: &documents::PlanDocument) -> std::path::PathBuf {
        crate::session::path_layout::session_dir(&self.root, Some("/plan-project"), &self.id)
            .join("plans")
            .join(format!("{}-r{}.md", doc.id, doc.revision))
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = self.sessions.quiesce_active();
        crate::test_support::cleanup_tree(self.root.parent().unwrap());
    }
}
#[test]
fn plan_document_is_saved_before_review_and_missing_markdown_is_rebuilt() {
    let f = Fixture::new();
    let doc = f.service.save_draft(None, 0, "# Ship\n\nA plan.").unwrap();
    assert_eq!(std::fs::read_to_string(f.path(&doc)).unwrap(), doc.text);
    assert!(f.service.state().approved.is_none());
    std::fs::remove_file(f.path(&doc)).unwrap();
    f.service.document(&doc.id, doc.revision).unwrap();
    assert_eq!(std::fs::read_to_string(f.path(&doc)).unwrap(), doc.text);
}
#[test]
fn plan_approval_is_exact_version_and_never_changes_approved_text() {
    let f = Fixture::new();
    let first = f.service.save_draft(None, 0, "# First plan").unwrap();
    let revised = f
        .service
        .save_draft(Some(&first.id), 1, "# Revised plan")
        .unwrap();
    assert!(f.service.approve_document(&first.id, 1).is_err());
    assert!(
        f.service
            .save_draft(Some(&first.id), 1, "stale editor")
            .is_err()
    );
    f.service.approve_document(&revised.id, 2).unwrap();
    f.service.set_durable(true).unwrap();
    f.service
        .save_draft(Some(&revised.id), 2, "# Third plan")
        .unwrap();
    assert_eq!(
        std::fs::read_to_string(f.path(&revised)).unwrap(),
        "# Revised plan"
    );
    assert_eq!(f.sessions.plan_documents().unwrap().plans.len(), 3);
}
#[test]
fn externally_modified_markdown_cannot_be_approved_or_overwritten() {
    let f = Fixture::new();
    let doc = f.service.save_draft(None, 0, "# Trusted plan").unwrap();
    std::fs::write(f.path(&doc), "unreviewed external edit").unwrap();
    assert!(f.service.approve_document(&doc.id, 1).is_err());
    assert_eq!(
        std::fs::read_to_string(f.path(&doc)).unwrap(),
        "unreviewed external edit"
    );
    assert!(f.service.state().approved.is_none());
    assert_eq!(f.service.documents().unwrap()[0].text, doc.text);
}
#[test]
fn plan_receipts_replay_exactly_and_reject_operation_id_reuse() {
    let f = Fixture::new();
    let value = serde_json::json!({"plan":"stable"});
    f.service
        .workflow
        .remember(&f.sessions, "op-1", "request-one", value.clone())
        .unwrap();
    assert_eq!(
        f.service
            .workflow
            .receipt(&f.sessions, "op-1", "request-one")
            .unwrap(),
        Some(value)
    );
    assert!(
        f.service
            .workflow
            .receipt(&f.sessions, "op-1", "different request")
            .is_err()
    );
}
#[test]
fn document_and_receipt_survive_cold_reopen_without_arming_execution() {
    let f = Fixture::new();
    let doc = f.service.save_draft(None, 0, "# Durable plan").unwrap();
    f.service.approve_document(&doc.id, 1).unwrap();
    f.service
        .workflow
        .remember(
            &f.sessions,
            "approval",
            "request",
            serde_json::json!({"approved":true}),
        )
        .unwrap();
    f.sessions.quiesce_active().unwrap();
    let reopened = SessionService::new(f.root.clone(), JsonlCompression::None).unwrap();
    reopened
        .resume(&crate::session::key::SessionKey {
            project: ProjectKey::from_cwd("/plan-project"),
            id: f.id.clone(),
        })
        .unwrap();
    let service = PlanModeService::new(Arc::new(reopened), AskUserSlot::shared());
    let restored = service.documents().unwrap();
    assert_eq!(restored.len(), 1);
    assert!(restored[0].approved);
    assert_eq!(restored[0].text, doc.text);
    assert_eq!(
        service
            .workflow
            .receipt(&service.sessions, "approval", "request")
            .unwrap(),
        Some(serde_json::json!({"approved":true}))
    );
    assert!(service.workflow.handoff.lock().unwrap().is_none());
    drop(service);
}
#[test]
fn plan_event_golden_has_catalog_admission_and_checkpoint_live_replay_parity() {
    let source = include_str!("../../tests/fixtures/workflow-plan-change.json");
    let event: SessionEvent = serde_json::from_str(source).unwrap();
    let spec = crate::session::catalog::event_spec(&event.event_type).unwrap();
    spec.validate(&event, 4).unwrap();
    let mut full = ProjectionRegistry::clat();
    full.fold_one(&event).unwrap();
    let checkpoint = full.checkpoint(
        CheckpointIdentity {
            created_at: 0,
            cwd: None,
        },
        1,
    );
    let mut restored = ProjectionRegistry::clat();
    restored.restore(&checkpoint, &[], 1).unwrap();
    assert_eq!(
        full.state_snapshot("plan-documents"),
        restored.state_snapshot("plan-documents")
    );
    let mut broken = checkpoint.clone();
    broken.rows.get_mut("plan-documents").unwrap().val["plans"][0]["text"] =
        serde_json::json!("cache is not an independent plan source");
    let mut rejected = ProjectionRegistry::clat();
    assert!(rejected.restore(&broken, &[], 1).is_err());
    let mut bad = event.clone();
    bad.ignorable = None;
    assert!(spec.validate(&bad, 4).is_err());
    bad = event.clone();
    bad.data["document"]["digest"] = serde_json::json!("forged");
    assert!(spec.validate(&bad, 4).is_err());
    let mut replay = Vec::new();
    crate::session::replay::ReplayAdapter::new().push(&event, &mut replay);
    assert!(replay.is_empty());
}
#[test]
fn plan_change_cannot_mutate_approved_revision_or_bind_another_session() {
    let f = Fixture::new();
    let doc = f.service.save_draft(None, 0, "# Immutable").unwrap();
    f.service.approve_document(&doc.id, 1).unwrap();
    let mut forged = f.sessions.plan_documents().unwrap().plans.pop().unwrap();
    forged.text = "different".into();
    forged.digest = plan_digest(&forged.text);
    let version = f.sessions.plan_documents().unwrap().version;
    assert!(
        f.sessions
            .commit_plan_change(
                documents::Change {
                    version: version + 1,
                    document: Some(forged),
                    operation: None
                },
                &[]
            )
            .is_err()
    );
    let mut foreign = doc.clone();
    foreign.session = "another-session".into();
    assert!(
        f.sessions
            .commit_plan_change(
                documents::Change {
                    version: version + 1,
                    document: Some(foreign),
                    operation: None
                },
                &[]
            )
            .is_err()
    );
}

#[test]
fn simultaneous_plan_editors_have_one_revision_winner() {
    let f = Fixture::new();
    let doc = f.service.save_draft(None, 0, "# Baseline").unwrap();
    let (left_start, left_ready) = std::sync::mpsc::channel();
    let (right_start, right_ready) = std::sync::mpsc::channel();
    let results = std::thread::scope(|scope| {
        let service = &f.service;
        let id = &doc.id;
        let left = scope.spawn(move || {
            left_ready
                .recv_timeout(std::time::Duration::from_secs(10))
                .unwrap();
            service.save_draft(Some(id), 1, "# Editor A")
        });
        let right = scope.spawn(move || {
            right_ready
                .recv_timeout(std::time::Duration::from_secs(10))
                .unwrap();
            service.save_draft(Some(id), 1, "# Editor B")
        });
        left_start.send(()).unwrap();
        right_start.send(()).unwrap();
        [left.join().unwrap(), right.join().unwrap()]
    });
    assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
    assert_eq!(f.service.documents().unwrap().len(), 2);
    assert_eq!(std::fs::read_to_string(f.path(&doc)).unwrap(), "# Baseline");
}
#[test]
#[cfg(unix)]
fn plan_projection_symlink_cannot_write_or_approve_outside_session() {
    use std::os::unix::fs::symlink;
    let f = Fixture::new();
    let doc = f.service.save_draft(None, 0, "# Owned document").unwrap();
    let outside = f.root.parent().unwrap().join("outside.md");
    std::fs::write(&outside, &doc.text).unwrap();
    std::fs::remove_file(f.path(&doc)).unwrap();
    symlink(&outside, f.path(&doc)).unwrap();
    assert!(f.service.approve_document(&doc.id, 1).is_err());
    assert_eq!(std::fs::read_to_string(&outside).unwrap(), doc.text);
    assert!(f.service.state().approved.is_none());
}
#[test]
fn plan_goal_binding_and_goal_creation_commit_together_or_not_at_all() {
    let f = Fixture::new();
    let doc = f.service.save_draft(None, 0, "# Atomic goal").unwrap();
    f.service.approve_document(&doc.id, 1).unwrap();
    let goal_id = format!("goal-{}", uuid::Uuid::new_v4());
    let failed: Result<(), String> = f.service.bind_goal_atomically(&doc.id, 1, &goal_id, |_| {
        Err("injected admission failure".into())
    });
    assert!(failed.is_err());
    assert!(f.service.document(&doc.id, 1).unwrap().goal_id.is_none());
    let goals = crate::goal::GoalService::new(f.sessions.clone(), f.root.clone());
    let goal = f
        .service
        .bind_goal_atomically(&doc.id, 1, &goal_id, |event| {
            goals.create_for_plan(&doc.title, &goal_id, &[event])
        })
        .unwrap();
    assert_eq!(goal.state.id, goal_id);
    assert_eq!(
        f.service.document(&doc.id, 1).unwrap().goal_id.as_deref(),
        Some(goal_id.as_str())
    );
    assert!(
        f.service
            .bind_goal_atomically(&doc.id, 1, &goal_id, |_| Ok(()))
            .is_err()
    );
}
#[test]
fn legacy_approved_plan_is_visible_without_rewriting_history_and_can_be_imported() {
    let f = Fixture::new();
    f.sessions
        .record_plan_mode(
            false,
            Some(crate::session::use_cases::ApprovedPlanWrite {
                text: "# Legacy plan".into(),
                digest: plan_digest("# Legacy plan"),
            }),
        )
        .unwrap();
    let before = f.sessions.plan_documents().unwrap().version;
    let doc = f.service.documents().unwrap().pop().unwrap();
    assert!(doc.approved);
    assert_eq!(doc.text, "# Legacy plan");
    assert_eq!(
        f.sessions.plan_documents().unwrap().version,
        before,
        "reading legacy approval must not add facts"
    );
    f.service.approve_document(&doc.id, 1).unwrap();
    assert!(f.sessions.plan_documents().unwrap().plans[0].approved);
}
#[test]
fn losing_review_response_cannot_schedule_execution_after_approve_only() {
    let f = Fixture::new();
    let doc = f
        .service
        .save_draft(None, 0, "# First answer wins")
        .unwrap();
    let (sender, receiver) = std::sync::mpsc::channel();
    *f.service.review.lock().unwrap() = Some(service::Review {
        document: doc.clone(),
        sender,
        answered: false,
    });
    assert!(f.service.approve_review(&doc.id, 1, false).unwrap());
    assert!(f.service.approve_review(&doc.id, 1, true).is_err());
    assert!(f.service.workflow.handoff.lock().unwrap().is_none());
    assert!(matches!(
        receiver
            .recv_timeout(std::time::Duration::from_secs(1))
            .unwrap(),
        AskAnswer::Selected(_)
    ));
}
#[test]
fn immutable_projection_publication_never_clobbers_a_concurrent_writer() {
    let f = Fixture::new();
    let dir = cap_std::fs::Dir::open_ambient_dir(&f.root, cap_std::ambient_authority()).unwrap();
    dir.write("raced.md", "external content").unwrap();
    assert!(
        crate::private_fs::write_text_new_atomic_in_dir(&dir, "raced.md", "derived content")
            .is_err()
    );
    assert_eq!(dir.read_to_string("raced.md").unwrap(), "external content");
    crate::private_fs::write_text_new_atomic_in_dir(&dir, "new.md", "complete revision").unwrap();
    assert_eq!(dir.read_to_string("new.md").unwrap(), "complete revision");
}
