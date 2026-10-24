//! One atomic control document and a sticky, explicitly recoverable fault state.
use super::*;
use std::sync::Mutex;
#[derive(Default)]
struct Cache {
    seen: Option<Option<String>>,
    fault: Option<ScheduleError>,
}
pub(crate) struct SchedulerService {
    control: Arc<crate::control_storage::ControlStorage>,
    lane: Mutex<Cache>,
}
impl SchedulerService {
    pub(super) fn project_roots(&self) -> Result<Vec<String>, ScheduleError> {
        let mut cache = self
            .lane
            .lock()
            .map_err(|_| ScheduleError::new("unavailable"))?;
        let doc = self.read(&mut cache)?;
        let mut roots = std::collections::BTreeSet::new();
        for rule in doc.rules.iter().filter(|r| r.phase == RulePhase::Enabled) {
            roots.insert(rule.scope.project.clone());
        }
        for delivery in doc.deliveries.iter().filter(|d| d.unsettled()) {
            roots.insert(delivery.scope.project.clone());
        }
        Ok(roots.into_iter().collect())
    }
    pub(crate) fn block(
        &self,
        project: &str,
        id: &str,
        reason: Option<&str>,
    ) -> Result<(), ScheduleError> {
        let current = self
            .list(project)?
            .into_iter()
            .find(|r| r.id == id)
            .ok_or_else(|| ScheduleError::new("not_found"))?;
        if current.blocked.as_deref() == reason {
            return Ok(());
        }
        self.transaction(|doc| {
            let rule = find(doc, project, id, current.revision)?;
            rule.blocked = reason.map(str::to_owned);
            Ok(())
        })
    }
    pub(crate) fn pending_claim(&self, project: &str) -> Result<Option<Claim>, ScheduleError> {
        let rules = self.list(project)?;
        for delivery in self.deliveries(project)? {
            if delivery.phase != DeliveryPhase::Reserved {
                continue;
            }
            if let Some(rule) = rules.iter().find(|r| {
                r.id == delivery.rule_id
                    && r.phase == RulePhase::Enabled
                    && r.authorization_valid()
                    && digest(&(&r.id, r.spec_digest(), delivery.scheduled_at)) == delivery.id
            }) {
                return Ok(Some(Claim {
                    rule: rule.clone(),
                    delivery,
                }));
            }
        }
        Ok(None)
    }
    pub(crate) fn new(control: Arc<crate::control_storage::ControlStorage>) -> Self {
        Self {
            control,
            lane: Mutex::new(Cache::default()),
        }
    }
    fn read(&self, cache: &mut Cache) -> Result<Document, ScheduleError> {
        if let Some(error) = &cache.fault {
            return Err(error.clone());
        }
        let result = (|| {
            let raw = self
                .control
                .read_schedules()
                .map_err(|e| ScheduleError::new("store_failed").detail(e))?;
            let doc = match &raw {
                None => Document::default(),
                Some(raw) => serde_json::from_str::<Document>(raw)
                    .map_err(|e| ScheduleError::new("invalid_store").detail(e))?,
            };
            doc.validate()?;
            let signature = raw.as_ref().map(|_| digest(&doc));
            if cache.seen.as_ref().is_some_and(|seen| seen != &signature) {
                return Err(ScheduleError::new("external_change"));
            }
            cache.seen = Some(signature);
            Ok(doc)
        })();
        if let Err(error) = &result {
            cache.fault = Some(error.clone());
        }
        result
    }
    fn transaction<T>(
        &self,
        operation: impl FnOnce(&mut Document) -> Result<T, ScheduleError>,
    ) -> Result<T, ScheduleError> {
        let mut cache = self
            .lane
            .lock()
            .map_err(|_| ScheduleError::new("unavailable"))?;
        let mut doc = self.read(&mut cache)?;
        let value = operation(&mut doc)?;
        doc.revision = next(doc.revision)?;
        doc.validate()?;
        let text = serde_json::to_string_pretty(&doc)
            .map_err(|e| ScheduleError::new("store_failed").detail(e))?;
        if text.len() > 2 * 1024 * 1024 {
            return Err(ScheduleError::new("capacity"));
        }
        if let Err(error) = self.control.save_schedules(&text) {
            let error = ScheduleError::new("store_failed").detail(error);
            cache.fault = Some(error.clone());
            return Err(error);
        }
        cache.seen = Some(Some(digest(&doc)));
        Ok(value)
    }
    pub(crate) fn reload(&self) -> Result<(), ScheduleError> {
        let mut cache = self
            .lane
            .lock()
            .map_err(|_| ScheduleError::new("unavailable"))?;
        *cache = Cache::default();
        self.read(&mut cache).map(|_| ())
    }
    pub(crate) fn list(&self, project: &str) -> Result<Vec<Rule>, ScheduleError> {
        let mut cache = self
            .lane
            .lock()
            .map_err(|_| ScheduleError::new("unavailable"))?;
        Ok(self
            .read(&mut cache)?
            .rules
            .into_iter()
            .filter(|r| r.scope.project == project)
            .collect())
    }
    pub(crate) fn create(&self, scope: WorkScope, spec: WorkSpec) -> Result<Rule, ScheduleError> {
        if !self
            .control
            .is_project_trusted(std::path::Path::new(&scope.project))
        {
            return Err(ScheduleError::new("untrusted"));
        }
        self.transaction(|doc| {
            if doc.rules.len() >= 64 {
                return Err(ScheduleError::new("capacity"));
            }
            let rule = Rule {
                id: uuid::Uuid::new_v4().to_string(),
                revision: 1,
                scope,
                spec,
                phase: RulePhase::Draft,
                authorization: None,
                used: Consumption::default(),
                blocked: None,
                next_at: None,
                review: None,
            };
            rule.validate()?;
            doc.rules.push(rule.clone());
            Ok(rule)
        })
    }
    pub(crate) fn edit(
        &self,
        project: &str,
        id: &str,
        revision: u64,
        spec: WorkSpec,
    ) -> Result<Rule, ScheduleError> {
        self.transaction(|doc| {
            if doc
                .deliveries
                .iter()
                .any(|d| d.rule_id == id && d.unsettled())
            {
                return Err(ScheduleError::new("busy"));
            }
            let rule = find(doc, project, id, revision)?;
            rule.spec = spec;
            rule.revision = next(rule.revision)?;
            rule.authorization = None;
            rule.review = None;
            rule.phase = RulePhase::Draft;
            rule.blocked = None;
            rule.next_at = None;
            rule.validate()?;
            Ok(rule.clone())
        })
    }
    pub(crate) fn review(
        &self,
        project: &str,
        id: &str,
        revision: u64,
    ) -> Result<RuleReview, ScheduleError> {
        self.transaction(|doc| {
            let rule = find(doc, project, id, revision)?;
            if rule.phase == RulePhase::Enabled {
                return Err(ScheduleError::new("already_enabled"));
            }
            let nonce = uuid::Uuid::new_v4().to_string();
            let confirmation = digest(&(&nonce, rule.revision, rule.spec_digest()));
            rule.review = Some(Review {
                nonce: nonce.clone(),
                digest: confirmation.clone(),
                revision: rule.revision,
            });
            Ok(RuleReview {
                rule: rule.clone(),
                nonce,
                digest: confirmation,
            })
        })
    }
    pub(crate) fn confirm(
        &self,
        project: &str,
        id: &str,
        revision: u64,
        nonce: &str,
        confirmation: &str,
        confirmed_at: i64,
    ) -> Result<Rule, ScheduleError> {
        self.transaction(|doc| {
            let rule = find(doc, project, id, revision)?;
            let reviewed = rule.review.as_ref().is_some_and(|r| {
                r.nonce == nonce
                    && r.digest == confirmation
                    && r.revision == rule.revision
                    && r.digest == digest(&(&r.nonce, rule.revision, rule.spec_digest()))
            });
            if !reviewed {
                return Err(ScheduleError::new("confirmation_required"));
            }
            if !rule.can_reserve() {
                return Err(ScheduleError::new("budget_exhausted"));
            }
            rule.revision = next(rule.revision)?;
            rule.authorization = Some(Authorization {
                revision: rule.revision,
                spec_digest: rule.spec_digest(),
                confirmed_at,
            });
            rule.review = None;
            rule.phase = RulePhase::Enabled;
            rule.blocked = None;
            if let Trigger::Time { rule: time } = &rule.spec.trigger {
                rule.next_at = time.first(confirmed_at)?;
            }
            Ok(rule.clone())
        })
    }
    pub(crate) fn pause(
        &self,
        project: &str,
        id: &str,
        revision: u64,
    ) -> Result<Rule, ScheduleError> {
        self.transaction(|doc| {
            let rule = find(doc, project, id, revision)?;
            rule.revision = next(rule.revision)?;
            rule.phase = RulePhase::Paused;
            rule.review = None;
            Ok(rule.clone())
        })
    }
    pub(crate) fn resume(
        &self,
        project: &str,
        id: &str,
        revision: u64,
    ) -> Result<Rule, ScheduleError> {
        self.transaction(|doc| {
            let delivered = doc.deliveries.iter().any(|d| d.rule_id == id);
            let rule = find(doc, project, id, revision)?;
            if !rule.authorization_valid() {
                return Err(ScheduleError::new("confirmation_required"));
            }
            if !rule.can_reserve() {
                return Err(ScheduleError::new("budget_exhausted"));
            }
            if delivered
                && (matches!(rule.spec.trigger, Trigger::Job { .. }) || rule.next_at.is_none())
            {
                return Err(ScheduleError::new("already_delivered"));
            }
            rule.revision = next(rule.revision)?;
            rule.phase = RulePhase::Enabled;
            rule.blocked = None;
            Ok(rule.clone())
        })
    }
    pub(crate) fn delete(
        &self,
        project: &str,
        id: &str,
        revision: u64,
    ) -> Result<(), ScheduleError> {
        self.transaction(|doc| {
            find(doc, project, id, revision)?;
            doc.rules.retain(|r| r.id != id);
            Ok(())
        })
    }
    pub(crate) fn deliveries(&self, project: &str) -> Result<Vec<Delivery>, ScheduleError> {
        let mut cache = self
            .lane
            .lock()
            .map_err(|_| ScheduleError::new("unavailable"))?;
        Ok(self
            .read(&mut cache)?
            .deliveries
            .into_iter()
            .filter(|d| d.scope.project == project)
            .collect())
    }
    pub(crate) fn reserve(
        &self,
        project: &str,
        id: &str,
        revision: u64,
        scheduled_at: i64,
        now: i64,
    ) -> Result<Claim, ScheduleError> {
        if !self
            .control
            .is_project_trusted(std::path::Path::new(project))
        {
            return Err(ScheduleError::new("untrusted"));
        }
        self.transaction(|doc| {
            let rule = find(doc, project, id, revision)?;
            if rule.phase != RulePhase::Enabled || !rule.authorization_valid() {
                return Err(ScheduleError::new("confirmation_required"));
            }
            if !rule.can_reserve() {
                return Err(ScheduleError::new("budget_exhausted"));
            }
            if let Trigger::Time { rule: time } = &rule.spec.trigger {
                if rule.next_at.is_none_or(|next| next > now)
                    || time.latest(now)? != Some(scheduled_at)
                    || rule.next_at.is_some_and(|next| scheduled_at < next)
                {
                    return Err(ScheduleError::new("not_due"));
                }
                rule.next_at = time.after(now)?;
            }
            let delivery_id = digest(&(&rule.id, rule.spec_digest(), scheduled_at));
            let b = &rule.spec.budget;
            let reservation = Consumption {
                runs: 1,
                tokens: b.run_tokens,
                seconds: u64::from(b.run_seconds),
                paid_web_calls: b.run_paid_web_calls,
            };
            rule.used.runs += 1;
            rule.used.tokens += reservation.tokens;
            rule.used.seconds += reservation.seconds;
            rule.used.paid_web_calls += reservation.paid_web_calls;
            let rule = rule.clone();
            if doc
                .deliveries
                .iter()
                .any(|d| d.id == delivery_id || d.rule_id == id && d.unsettled())
            {
                return Err(ScheduleError::new("already_delivered"));
            }
            trim(doc)?;
            let delivery = Delivery {
                id: delivery_id,
                rule_id: id.into(),
                rule_revision: revision,
                scope: rule.scope.clone(),
                scheduled_at,
                started_at: now,
                finished_at: None,
                phase: DeliveryPhase::Reserved,
                reservation,
                actual: None,
                receipt: None,
                diagnostic: None,
            };
            doc.deliveries.push(delivery.clone());
            Ok(Claim { rule, delivery })
        })
    }
    pub(crate) fn admitted(
        &self,
        project: &str,
        id: &str,
        receipt: crate::message::AdmissionReceipt,
    ) -> Result<(), ScheduleError> {
        self.transaction(|doc| {
            let delivery = delivery_mut(doc, project, id)?;
            if !delivery.unsettled()
                || receipt.state != crate::message::AdmissionState::Committed
                || receipt.client_message_id != format!("auto-{}", delivery.id)
            {
                return Err(ScheduleError::new("invalid_receipt"));
            }
            delivery.receipt = Some(receipt);
            delivery.phase = DeliveryPhase::Admitted;
            Ok(())
        })
    }
    pub(crate) fn settle(
        &self,
        project: &str,
        id: &str,
        phase: DeliveryPhase,
        actual: Option<Consumption>,
        diagnostic: Option<String>,
        now: i64,
    ) -> Result<Delivery, ScheduleError> {
        self.transaction(|doc| {
            if matches!(phase, DeliveryPhase::Reserved | DeliveryPhase::Admitted) {
                return Err(ScheduleError::new("invalid_delivery"));
            }
            let delivery = delivery_mut(doc, project, id)?;
            if !delivery.unsettled() {
                return Ok(delivery.clone());
            }
            delivery.phase = phase;
            delivery.finished_at = Some(now);
            delivery.diagnostic = diagnostic.map(|s| {
                crate::redact::redact_secrets(&s)
                    .chars()
                    .take(512)
                    .collect()
            });
            delivery.actual = actual.clone();
            let delivery = delivery.clone();
            if let Some(rule) = doc.rules.iter_mut().find(|r| r.id == delivery.rule_id) {
                if let Some(actual) = actual {
                    reconcile(rule, &delivery.reservation, &actual);
                }
                if !rule.can_reserve() {
                    rule.phase = RulePhase::Exhausted;
                }
            }
            Ok(delivery)
        })
    }
}
fn next(revision: u64) -> Result<u64, ScheduleError> {
    revision
        .checked_add(1)
        .ok_or_else(|| ScheduleError::new("invalid_store"))
}
fn find<'a>(
    doc: &'a mut Document,
    project: &str,
    id: &str,
    revision: u64,
) -> Result<&'a mut Rule, ScheduleError> {
    let rule = doc
        .rules
        .iter_mut()
        .find(|r| r.id == id && r.scope.project == project)
        .ok_or_else(|| ScheduleError::new("not_found"))?;
    if rule.revision != revision {
        return Err(ScheduleError::new("stale_revision"));
    }
    Ok(rule)
}
fn delivery_mut<'a>(
    doc: &'a mut Document,
    project: &str,
    id: &str,
) -> Result<&'a mut Delivery, ScheduleError> {
    doc.deliveries
        .iter_mut()
        .find(|d| d.id == id && d.scope.project == project)
        .ok_or_else(|| ScheduleError::new("not_found"))
}
fn trim(doc: &mut Document) -> Result<(), ScheduleError> {
    if doc.deliveries.len() < 1024 {
        return Ok(());
    }
    let oldest = doc
        .deliveries
        .iter()
        .enumerate()
        .filter(|(_, d)| !d.unsettled())
        .min_by_key(|(_, d)| d.finished_at)
        .map(|(i, _)| i)
        .ok_or_else(|| ScheduleError::new("capacity"))?;
    doc.deliveries.remove(oldest);
    Ok(())
}
fn reconcile(rule: &mut Rule, reserved: &Consumption, actual: &Consumption) {
    rule.used.runs = rule
        .used
        .runs
        .saturating_sub(reserved.runs)
        .saturating_add(actual.runs);
    rule.used.tokens = rule
        .used
        .tokens
        .saturating_sub(reserved.tokens)
        .saturating_add(actual.tokens);
    rule.used.seconds = rule
        .used
        .seconds
        .saturating_sub(reserved.seconds)
        .saturating_add(actual.seconds);
    rule.used.paid_web_calls = rule
        .used
        .paid_web_calls
        .saturating_sub(reserved.paid_web_calls)
        .saturating_add(actual.paid_web_calls);
}
