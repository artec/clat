use super::*;
#[path = "time_tests.rs"]
mod time_tests;
pub(super) struct Fixture {
    root: std::path::PathBuf,
    control: Arc<crate::control_storage::ControlStorage>,
    service: Arc<SchedulerService>,
    scope: WorkScope,
}
impl Fixture {
    fn new() -> Self {
        let root =
            std::env::temp_dir().join(format!("clat-scheduler-test-{}", uuid::Uuid::new_v4()));
        let project = root.join("project");
        std::fs::create_dir_all(&project).unwrap();
        crate::control_storage::sentinel::initialize(&root).unwrap();
        let control = Arc::new(crate::control_storage::ControlStorage::open_ready(&root).unwrap());
        let project = project.canonicalize().unwrap();
        control.add_trust(&project).unwrap();
        let service = Arc::new(SchedulerService::new(control.clone()));
        Self {
            root,
            control,
            service,
            scope: WorkScope {
                project: project.to_string_lossy().into_owned(),
                session: uuid::Uuid::new_v4().to_string(),
            },
        }
    }
    fn spec(&self) -> WorkSpec {
        WorkSpec {
            instruction: "Inspect the finished job and report its actual result".into(),
            trigger: Trigger::Job {
                job_id: uuid::Uuid::new_v4().to_string(),
            },
            model: ApprovedModel {
                profile: None,
                fingerprint: "a".repeat(64),
                protocol: "test".into(),
                model: "test-model".into(),
                endpoint: "https://example.test".into(),
            },
            budget: WorkBudget {
                max_runs: 1,
                max_tokens: 10000,
                max_seconds: 30,
                run_tokens: 10000,
                run_seconds: 30,
                max_paid_web_calls: 2,
                run_paid_web_calls: 2,
            },
        }
    }
    fn rule(&self) -> Rule {
        self.service
            .create(self.scope.clone(), self.spec())
            .unwrap()
    }
    fn enabled(&self) -> Rule {
        let r = self.rule();
        let p = self
            .service
            .review(&self.scope.project, &r.id, r.revision)
            .unwrap();
        self.service
            .confirm(
                &self.scope.project,
                &r.id,
                r.revision,
                &p.nonce,
                &p.digest,
                100,
            )
            .unwrap()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        crate::test_support::cleanup_tree(&self.root);
    }
}
#[test]
fn scheduler_authorization_requires_frozen_review_and_edit_revokes_it() {
    let f = Fixture::new();
    let r = f.rule();
    assert_eq!(
        f.service
            .reserve(&f.scope.project, &r.id, r.revision, 100, 100)
            .err()
            .unwrap()
            .code,
        "confirmation_required"
    );
    let p = f
        .service
        .review(&f.scope.project, &r.id, r.revision)
        .unwrap();
    let mut spec = r.spec.clone();
    spec.instruction = "A broader changed instruction".into();
    let changed = f
        .service
        .edit(&f.scope.project, &r.id, r.revision, spec)
        .unwrap();
    assert_eq!(changed.phase, RulePhase::Draft);
    assert!(changed.authorization.is_none());
    assert!(
        f.service
            .confirm(
                &f.scope.project,
                &r.id,
                changed.revision,
                &p.nonce,
                &p.digest,
                101
            )
            .is_err()
    );
    assert!(
        f.service
            .resume(&f.scope.project, &r.id, changed.revision)
            .is_err()
    );
    let p = f
        .service
        .review(&f.scope.project, &r.id, changed.revision)
        .unwrap();
    let enabled = f
        .service
        .confirm(
            &f.scope.project,
            &r.id,
            changed.revision,
            &p.nonce,
            &p.digest,
            102,
        )
        .unwrap();
    assert!(enabled.authorization_valid());
    assert!(
        f.service
            .confirm(
                &f.scope.project,
                &r.id,
                enabled.revision,
                &p.nonce,
                &p.digest,
                103
            )
            .is_err()
    );
    assert!(f.service.deliveries(&f.scope.project).unwrap().is_empty());
}
#[test]
fn scheduler_reservation_is_scoped_bounded_durable_and_not_replayed() {
    let f = Fixture::new();
    let r = f.enabled();
    assert!(
        f.service
            .reserve("/another-project", &r.id, r.revision, 200, 205)
            .is_err()
    );
    let claim = f
        .service
        .reserve(&f.scope.project, &r.id, r.revision, 200, 205)
        .unwrap();
    assert_eq!(claim.rule.scope, f.scope);
    assert_eq!(claim.delivery.reservation.tokens, 10000);
    assert!(
        f.service
            .reserve(&f.scope.project, &r.id, r.revision, 200, 205)
            .is_err()
    );
    let reopened = SchedulerService::new(f.control.clone());
    assert_eq!(reopened.deliveries(&f.scope.project).unwrap().len(), 1);
    assert!(
        reopened
            .reserve(&f.scope.project, &r.id, r.revision, 200, 206)
            .is_err()
    );
    let receipt = crate::message::AdmissionReceipt::committed(
        format!("auto-{}", claim.delivery.id),
        "message-id".into(),
        vec![],
    );
    reopened
        .admitted(&f.scope.project, &claim.delivery.id, receipt)
        .unwrap();
    let actual = Consumption {
        runs: 1,
        tokens: 500,
        seconds: 2,
        paid_web_calls: 1,
    };
    let done = reopened
        .settle(
            &f.scope.project,
            &claim.delivery.id,
            DeliveryPhase::Completed,
            Some(actual.clone()),
            None,
            210,
        )
        .unwrap();
    assert_eq!(done.actual, Some(actual));
    assert_eq!(
        reopened.list(&f.scope.project).unwrap()[0].phase,
        RulePhase::Exhausted
    );
    assert_eq!(reopened.list(&f.scope.project).unwrap()[0].used.tokens, 500);
    assert_eq!(reopened.deliveries(&f.scope.project).unwrap().len(), 1);
}
#[test]
fn scheduler_corrupt_external_or_linked_control_state_never_defaults_or_overwrites() {
    let f = Fixture::new();
    let r = f.rule();
    let path = f.root.join("schedules.json");
    let valid = std::fs::read(&path).unwrap();
    std::fs::write(&path, b"{broken").unwrap();
    assert!(f.service.list(&f.scope.project).is_err());
    assert!(
        f.service
            .pause(&f.scope.project, &r.id, r.revision)
            .is_err()
    );
    assert_eq!(std::fs::read(&path).unwrap(), b"{broken");
    std::fs::write(&path, &valid).unwrap();
    assert!(f.service.list(&f.scope.project).is_err());
    f.service.reload().unwrap();
    assert_eq!(f.service.list(&f.scope.project).unwrap().len(), 1);
    let mut d: serde_json::Value = serde_json::from_slice(&valid).unwrap();
    d["revision"] = serde_json::json!(999);
    std::fs::write(&path, d.to_string()).unwrap();
    assert_eq!(
        f.service.list(&f.scope.project).err().unwrap().code,
        "external_change"
    );
    #[cfg(unix)]
    {
        std::fs::remove_file(&path).unwrap();
        let target = f.root.join("foreign");
        std::fs::write(&target, &valid).unwrap();
        std::os::unix::fs::symlink(&target, &path).unwrap();
        assert!(f.service.reload().is_err());
        assert_eq!(std::fs::read(&target).unwrap(), valid);
    }
}
#[test]
fn scheduler_edit_racing_confirmation_cannot_enable_changed_work() {
    let f = Fixture::new();
    let r = f.rule();
    let p = f
        .service
        .review(&f.scope.project, &r.id, r.revision)
        .unwrap();
    let one = f.service.clone();
    let scope = f.scope.clone();
    let old = r.clone();
    let (go, receive) = std::sync::mpsc::sync_channel(1);
    let thread = std::thread::spawn(move || {
        receive
            .recv_timeout(std::time::Duration::from_secs(2))
            .unwrap();
        one.confirm(
            &scope.project,
            &old.id,
            old.revision,
            &p.nonce,
            &p.digest,
            100,
        )
    });
    let mut spec = r.spec.clone();
    spec.instruction = "Changed after the human review".into();
    go.send(()).unwrap();
    let changed = f.service.edit(&f.scope.project, &r.id, r.revision, spec);
    let confirmed = thread.join().unwrap();
    let final_rule = f.service.list(&f.scope.project).unwrap().remove(0);
    if changed.is_ok() {
        assert_eq!(final_rule.phase, RulePhase::Draft);
        assert!(final_rule.authorization.is_none());
    } else {
        assert!(confirmed.is_ok());
        assert_eq!(final_rule.spec.instruction, r.spec.instruction);
    }
    assert!(f.service.deliveries(&f.scope.project).unwrap().is_empty());
}
