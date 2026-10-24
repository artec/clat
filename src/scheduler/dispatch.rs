//! Control transitions used by the Host pump, never by model tools.
use super::*;
impl SchedulerService {
    pub(crate) fn projects(&self) -> Result<Vec<String>, ScheduleError> {
        self.project_roots()
    }
    pub(crate) fn due_times(
        &self,
        project: &str,
        now: i64,
    ) -> Result<Vec<(Rule, i64)>, ScheduleError> {
        let mut due = vec![];
        for rule in self.list(project)? {
            if rule.phase != RulePhase::Enabled || !rule.can_reserve() {
                continue;
            }
            if let Trigger::Time { rule: time } = &rule.spec.trigger
                && rule.next_at.is_some_and(|next| next <= now)
                && let Some(at) = time.latest(now)?
            {
                due.push((rule, at));
            }
        }
        due.sort_by_key(|(_, at)| *at);
        Ok(due)
    }
    pub(crate) fn has_session_rules(
        &self,
        project: &str,
        session: &str,
    ) -> Result<bool, ScheduleError> {
        Ok(self
            .list(project)?
            .iter()
            .any(|r| r.scope.session == session))
    }
    pub(crate) fn has_unsettled(&self, project: &str) -> Result<bool, ScheduleError> {
        Ok(self.deliveries(project)?.iter().any(Delivery::unsettled))
    }
}
