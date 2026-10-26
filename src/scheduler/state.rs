use super::*;
#[derive(Clone, Debug, Serialize, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct WorkScope {
    pub project: String,
    pub session: String,
}
#[derive(Clone, Debug, Serialize, Deserialize, Eq, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Trigger {
    Job { job_id: String },
    Time { rule: TimeRule },
}
#[derive(Clone, Debug, Serialize, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct WorkBudget {
    pub max_runs: u32,
    pub max_tokens: u64,
    pub max_seconds: u64,
    pub run_tokens: u64,
    pub run_seconds: u32,
    pub max_paid_web_calls: u32,
    pub run_paid_web_calls: u32,
}
impl WorkBudget {
    pub(crate) fn validate(&self) -> Result<(), ScheduleError> {
        if self.max_runs == 0
            || self.max_runs > 10000
            || self.run_tokens == 0
            || self.run_tokens > self.max_tokens
            || self.run_seconds == 0
            || self.run_seconds > 86400
            || u64::from(self.run_seconds) > self.max_seconds
            || self.run_paid_web_calls > self.max_paid_web_calls
            || self.max_tokens > 9_007_199_254_740_991
            || self.max_seconds > 9_007_199_254_740_991
        {
            return Err(ScheduleError::new("invalid_budget"));
        }
        Ok(())
    }
}
#[derive(Clone, Debug, Serialize, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ApprovedModel {
    pub profile: Option<String>,
    pub fingerprint: String,
    pub protocol: String,
    pub model: String,
    pub endpoint: String,
}
#[derive(Clone, Debug, Serialize, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct WorkSpec {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notification: Option<NotificationPolicy>,
    pub instruction: String,
    pub trigger: Trigger,
    pub budget: WorkBudget,
    pub model: ApprovedModel,
}
#[derive(Clone, Debug, Serialize, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum NotificationPolicy {
    Attention,
    Completion,
    Off,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Consumption {
    pub runs: u32,
    pub tokens: u64,
    pub seconds: u64,
    pub paid_web_calls: u32,
}
#[derive(Clone, Debug, Serialize, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum RulePhase {
    Draft,
    Enabled,
    Paused,
    Exhausted,
}
#[derive(Clone, Debug, Serialize, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Authorization {
    pub revision: u64,
    pub spec_digest: String,
    pub confirmed_at: i64,
}
#[derive(Clone, Debug, Serialize, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub(crate) struct Review {
    pub nonce: String,
    pub digest: String,
    pub revision: u64,
}
#[derive(Clone, Debug, Serialize, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Rule {
    pub id: String,
    pub revision: u64,
    pub scope: WorkScope,
    pub spec: WorkSpec,
    pub phase: RulePhase,
    pub authorization: Option<Authorization>,
    pub used: Consumption,
    pub blocked: Option<String>,
    pub next_at: Option<i64>,
    pub(crate) review: Option<Review>,
}
impl Rule {
    pub(crate) fn spec_digest(&self) -> String {
        digest(&(&self.id, &self.scope, &self.spec))
    }
    pub(crate) fn authorization_valid(&self) -> bool {
        self.authorization
            .as_ref()
            .is_some_and(|a| a.spec_digest == self.spec_digest())
    }
    pub(crate) fn can_reserve(&self) -> bool {
        let b = &self.spec.budget;
        self.used.runs < b.max_runs
            && self
                .used
                .tokens
                .checked_add(b.run_tokens)
                .is_some_and(|n| n <= b.max_tokens)
            && self
                .used
                .seconds
                .checked_add(u64::from(b.run_seconds))
                .is_some_and(|n| n <= b.max_seconds)
            && self
                .used
                .paid_web_calls
                .checked_add(b.run_paid_web_calls)
                .is_some_and(|n| n <= b.max_paid_web_calls)
    }
    pub(crate) fn validate(&self) -> Result<(), ScheduleError> {
        let model = &self.spec.model;
        if uuid::Uuid::parse_str(&self.id).is_err()
            || self.revision == 0
            || self.scope.project.is_empty()
            || !std::path::Path::new(&self.scope.project).is_absolute()
            || self.scope.session.is_empty()
            || self.scope.session.len() > 1024
            || self
                .spec
                .title
                .as_ref()
                .is_some_and(|title| title.len() > 256 || title.chars().any(char::is_control))
            || self.spec.instruction.trim().is_empty()
            || self.spec.instruction.len() > 16384
            || model.fingerprint.len() != 64
            || !model.fingerprint.bytes().all(|b| b.is_ascii_hexdigit())
            || model.model.is_empty()
            || model.endpoint.len() > 2048
            || model.profile.as_ref().is_some_and(|p| p.len() > 256)
        {
            return Err(ScheduleError::new("invalid_rule"));
        }
        if let Trigger::Job { job_id } = &self.spec.trigger
            && (uuid::Uuid::parse_str(job_id).is_err() || self.spec.budget.max_runs != 1)
        {
            return Err(ScheduleError::new("invalid_rule"));
        }
        if let Trigger::Time { rule } = &self.spec.trigger {
            rule.validate()?;
        }
        self.spec.budget.validate()?;
        if self.authorization.as_ref().is_some_and(|a| {
            a.revision > self.revision || a.revision == 0 || a.spec_digest != self.spec_digest()
        }) {
            return Err(ScheduleError::new("invalid_authorization"));
        }
        if self.phase == RulePhase::Enabled && !self.authorization_valid() {
            return Err(ScheduleError::new("invalid_authorization"));
        }
        Ok(())
    }
}
#[derive(Clone, Debug, Serialize, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum DeliveryPhase {
    Reserved,
    Admitted,
    Completed,
    Failed,
    Interrupted,
    Cancelled,
}
#[derive(Clone, Debug, Serialize, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Delivery {
    pub id: String,
    pub rule_id: String,
    pub rule_revision: u64,
    pub scope: WorkScope,
    pub scheduled_at: i64,
    pub started_at: i64,
    pub finished_at: Option<i64>,
    pub phase: DeliveryPhase,
    pub reservation: Consumption,
    pub actual: Option<Consumption>,
    pub receipt: Option<crate::message::AdmissionReceipt>,
    pub diagnostic: Option<String>,
}
impl Delivery {
    pub(crate) fn validate(&self) -> Result<(), ScheduleError> {
        let receipt_valid = self.receipt.as_ref().is_none_or(|r| {
            r.state == crate::message::AdmissionState::Committed
                && r.client_message_id == format!("auto-{}", self.id)
        });
        if self.id.len() != 64
            || !self.id.bytes().all(|b| b.is_ascii_hexdigit())
            || uuid::Uuid::parse_str(&self.rule_id).is_err()
            || self.rule_revision == 0
            || !std::path::Path::new(&self.scope.project).is_absolute()
            || self.scope.session.is_empty()
            || !receipt_valid
            || matches!(
                self.phase,
                DeliveryPhase::Admitted | DeliveryPhase::Completed
            ) && self.receipt.is_none()
            || self.unsettled() != self.finished_at.is_none()
            || self.reservation.runs != 1
            || self.reservation.tokens == 0
            || self.reservation.seconds == 0
        {
            return Err(ScheduleError::new("invalid_store"));
        }
        Ok(())
    }
    pub(crate) fn unsettled(&self) -> bool {
        matches!(
            self.phase,
            DeliveryPhase::Reserved | DeliveryPhase::Admitted
        )
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Document {
    pub unit: crate::control_storage::json_file::UnitTag,
    pub revision: u64,
    pub rules: Vec<Rule>,
    pub deliveries: Vec<Delivery>,
}
impl Default for Document {
    fn default() -> Self {
        Self {
            unit: crate::control_storage::json_file::UnitTag::new("clat.scheduler", 1),
            revision: 0,
            rules: vec![],
            deliveries: vec![],
        }
    }
}
impl Document {
    pub(crate) fn validate(&self) -> Result<(), ScheduleError> {
        if !self.unit.matches(("clat.scheduler", 1))
            || self.rules.len() > 64
            || self.deliveries.len() > 1024
        {
            return Err(ScheduleError::new("invalid_store"));
        }
        let mut ids = std::collections::HashSet::new();
        for rule in &self.rules {
            rule.validate()?;
            if !ids.insert(&rule.id) {
                return Err(ScheduleError::new("invalid_store"));
            }
        }
        let mut ids = std::collections::HashSet::new();
        for delivery in &self.deliveries {
            delivery.validate()?;
            if !ids.insert(&delivery.id) {
                return Err(ScheduleError::new("invalid_store"));
            }
        }
        Ok(())
    }
}
#[derive(Clone, Debug, Serialize)]
pub struct RuleReview {
    pub rule: Rule,
    pub nonce: String,
    pub digest: String,
}
#[derive(Clone)]
pub(crate) struct Claim {
    pub rule: Rule,
    pub delivery: Delivery,
}
