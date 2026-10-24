use super::*;
fn ms(iso: &str) -> i64 {
    iso.parse::<jiff::Timestamp>().unwrap().as_millisecond()
}
#[test]
fn scheduler_civil_dst_gaps_skip_and_folds_only_fire_the_earlier_instant() {
    let daily = TimeRule::Daily {
        hour: 2,
        minute: 30,
        timezone: "America/New_York".into(),
    };
    let before = ms("2026-03-07T07:30:00Z");
    assert_eq!(
        daily.after(before).unwrap(),
        Some(ms("2026-03-09T06:30:00Z"))
    );
    assert_eq!(
        daily.latest(ms("2026-03-08T23:59:00Z")).unwrap(),
        Some(before)
    );
    let fold = TimeRule::Daily {
        hour: 1,
        minute: 30,
        timezone: "America/New_York".into(),
    };
    let first = ms("2026-11-01T05:30:00Z");
    assert_eq!(
        fold.latest(ms("2026-11-01T06:30:00Z")).unwrap(),
        Some(first)
    );
    assert_eq!(fold.after(first).unwrap(), Some(ms("2026-11-02T06:30:00Z")));
    let weekly = TimeRule::Weekly {
        weekday: 7,
        hour: 2,
        minute: 30,
        timezone: "America/New_York".into(),
    };
    assert_eq!(
        weekly.after(ms("2026-03-01T07:30:00Z")).unwrap(),
        Some(ms("2026-03-15T06:30:00Z"))
    );
    let invalid = TimeRule::Daily {
        hour: 1,
        minute: 10,
        timezone: "not/a-zone".into(),
    };
    assert!(invalid.validate().is_err());
}
#[test]
fn scheduler_delayed_periodic_work_catches_only_latest_and_clock_rollback_never_replays() {
    let f = Fixture::new();
    let mut spec = f.spec();
    spec.trigger = Trigger::Time {
        rule: TimeRule::Interval {
            anchor_ms: 1000,
            every_ms: 1000,
        },
    };
    spec.budget.max_runs = 2;
    spec.budget.max_tokens *= 2;
    spec.budget.max_seconds *= 2;
    spec.budget.max_paid_web_calls *= 2;
    let r = f.service.create(f.scope.clone(), spec).unwrap();
    let p = f
        .service
        .review(&f.scope.project, &r.id, r.revision)
        .unwrap();
    let r = f
        .service
        .confirm(
            &f.scope.project,
            &r.id,
            r.revision,
            &p.nonce,
            &p.digest,
            1000,
        )
        .unwrap();
    let due = f.service.due_times(&f.scope.project, 10500).unwrap();
    assert_eq!(due.len(), 1);
    assert_eq!(due[0].1, 10000);
    assert!(
        f.service
            .reserve(&f.scope.project, &r.id, r.revision, 2000, 10500)
            .is_err()
    );
    let c = f
        .service
        .reserve(&f.scope.project, &r.id, r.revision, 10000, 10500)
        .unwrap();
    assert_eq!(c.rule.next_at, Some(11000));
    assert!(
        f.service
            .due_times(&f.scope.project, 9999)
            .unwrap()
            .is_empty()
    );
    let reopened = SchedulerService::new(f.control.clone());
    assert_eq!(
        reopened
            .pending_claim(&f.scope.project)
            .unwrap()
            .unwrap()
            .delivery
            .id,
        c.delivery.id
    );
    assert!(
        reopened
            .due_times(&f.scope.project, 10500)
            .unwrap()
            .is_empty()
    );
    assert_eq!(reopened.list(&f.scope.project).unwrap()[0].used.runs, 1);
    assert_eq!(reopened.deliveries(&f.scope.project).unwrap().len(), 1);
    // There is still budget and this is a different due occurrence. Only the
    // unsettled-delivery fence may reject it; budget/not-due cannot mask it.
    assert_eq!(
        reopened
            .reserve(&f.scope.project, &r.id, r.revision, 11000, 11500)
            .err()
            .unwrap()
            .code,
        "already_delivered"
    );
    assert_eq!(
        reopened.list(&f.scope.project).unwrap()[0].next_at,
        Some(11000)
    );
    complete_claim(&reopened, &f.scope.project, &c, 11500);
    let second = reopened
        .reserve(&f.scope.project, &r.id, r.revision, 11000, 11500)
        .unwrap();
    assert_ne!(second.delivery.id, c.delivery.id);
    complete_claim(&reopened, &f.scope.project, &second, 11600);
    assert_eq!(
        reopened.list(&f.scope.project).unwrap()[0].phase,
        RulePhase::Exhausted
    );
    assert!(
        reopened
            .due_times(&f.scope.project, 12500)
            .unwrap()
            .is_empty()
    );
}

fn complete_claim(service: &SchedulerService, project: &str, claim: &Claim, at: i64) {
    let receipt = crate::message::AdmissionReceipt::committed(
        format!("auto-{}", claim.delivery.id),
        "message".into(),
        vec![],
    );
    service
        .admitted(project, &claim.delivery.id, receipt)
        .unwrap();
    service
        .settle(
            project,
            &claim.delivery.id,
            DeliveryPhase::Completed,
            None,
            None,
            at,
        )
        .unwrap();
}
