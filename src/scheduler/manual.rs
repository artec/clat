//! Explicit run-once requests share reservation and durable delivery identity.
use super::*;
impl SchedulerService {
    pub(crate) fn run_now(
        &self,
        project: &str,
        id: &str,
        revision: u64,
        request: &str,
    ) -> Result<Delivery, ScheduleError> {
        if uuid::Uuid::parse_str(request).is_err() {
            return Err(ScheduleError::new("invalid_request"));
        }
        if !self
            .control
            .is_project_trusted(std::path::Path::new(project))
        {
            return Err(ScheduleError::new("untrusted"));
        }
        let hash = digest(&request);
        let occurrence = -1 - (u64::from_str_radix(&hash[..15], 16).unwrap() as i64);
        self.transaction(|doc| {
            let rule = store::find(doc, project, id, revision)?.clone();
            let delivery_id = digest(&(&rule.id, rule.spec_digest(), occurrence));
            if let Some(prior) = doc.deliveries.iter().find(|d| d.id == delivery_id) {
                return Ok(prior.clone());
            }
            if rule.phase != RulePhase::Enabled || !rule.authorization_valid() {
                return Err(ScheduleError::new("confirmation_required"));
            }
            if !matches!(rule.spec.trigger, Trigger::Time { .. }) {
                return Err(ScheduleError::new("invalid_rule"));
            }
            if !rule.can_reserve() {
                return Err(ScheduleError::new("budget_exhausted"));
            }
            store::reserve_delivery(doc, rule, revision, occurrence, now_ms())
                .map(|claim| claim.delivery)
        })
    }
}
