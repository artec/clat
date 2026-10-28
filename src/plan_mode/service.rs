use super::documents::{Change, Operation, PlanDocument};
use super::*;
use serde_json::{Value, json};

#[derive(Clone)]
pub(crate) enum Handoff {
    Goal {
        session: String,
        plan: Option<(String, u64)>,
    },
    Plan {
        session: String,
        plan: Option<(String, u64)>,
    },
}
pub(crate) struct Review {
    pub document: PlanDocument,
    pub sender: std::sync::mpsc::Sender<AskAnswer>,
    pub answered: bool,
}
impl PlanModeService {
    pub(crate) fn documents(&self) -> Result<Vec<PlanDocument>, String> {
        let mut plans = self.sessions.plan_documents()?.plans;
        if plans.is_empty()
            && let Some(doc) = self.legacy_document()
        {
            plans.push(doc);
        }
        Ok(plans)
    }
    fn legacy_document(&self) -> Option<PlanDocument> {
        let approved = self.state().approved?;
        let digest = &approved.digest;
        let id = format!(
            "plan-{}-{}-{}-{}-{}",
            &digest[..8],
            &digest[8..12],
            &digest[12..16],
            &digest[16..20],
            &digest[20..32]
        );
        let title = approved
            .text
            .lines()
            .map(|line| line.trim().trim_start_matches('#').trim())
            .find(|line| !line.is_empty())?
            .chars()
            .take(120)
            .collect();
        Some(PlanDocument {
            id,
            session: self.sessions.active_id()?.to_string(),
            revision: 1,
            title,
            text: approved.text,
            digest: approved.digest,
            approved: true,
            goal_id: None,
        })
    }
    pub(crate) fn save_draft(
        &self,
        id: Option<&str>,
        revision: u64,
        text: &str,
    ) -> Result<PlanDocument, String> {
        validate_plan_text(text)?;
        if !self.state().active {
            return Err("plan mode is not active".into());
        }
        let _lane = self.document_lane.lock().map_err(|_| "plan lane")?;
        let state = self.sessions.plan_documents()?;
        let session = self
            .sessions
            .active_id()
            .ok_or("plan requires a session")?
            .to_string();
        let latest = id.and_then(|id| state.plans.iter().rev().find(|p| p.id == id));
        if latest.map_or(0, |p| p.revision) != revision {
            return Err("plan changed; inspect again".into());
        }
        if let Some(doc) = latest.filter(|p| !p.approved && p.text == text) {
            self.sessions.materialize_plan(doc)?;
            return Ok(doc.clone());
        }
        let title = text
            .lines()
            .map(|s| s.trim().trim_start_matches('#').trim())
            .find(|s| !s.is_empty())
            .unwrap_or("Plan")
            .chars()
            .take(120)
            .collect();
        let doc = PlanDocument {
            id: id
                .map(str::to_owned)
                .unwrap_or_else(|| format!("plan-{}", uuid::Uuid::new_v4())),
            session,
            revision: revision + 1,
            title,
            text: text.to_owned(),
            digest: plan_digest(text),
            approved: false,
            goal_id: None,
        };
        self.sessions.commit_plan_change(
            Change {
                version: state.version + 1,
                document: Some(doc.clone()),
                operation: None,
            },
            &[],
        )?;
        self.sessions.materialize_plan(&doc)?;
        Ok(doc)
    }
    pub(crate) fn document(&self, id: &str, revision: u64) -> Result<PlanDocument, String> {
        let doc = self
            .sessions
            .plan_documents()?
            .plans
            .into_iter()
            .find(|p| p.id == id && p.revision == revision)
            .or_else(|| {
                self.legacy_document()
                    .filter(|doc| doc.id == id && doc.revision == revision)
            })
            .ok_or("plan revision not found")?;
        self.sessions.materialize_plan(&doc)?;
        Ok(doc)
    }
    pub(crate) fn approve_document(&self, id: &str, revision: u64) -> Result<ApprovedPlan, String> {
        let _lane = self.document_lane.lock().map_err(|_| "plan lane")?;
        let mut state = self.sessions.plan_documents()?;
        let mut doc = self.document(id, revision)?;
        if !state.plans.iter().any(|p| p.id == id) {
            let mut draft = doc.clone();
            draft.approved = false;
            self.sessions.commit_plan_change(
                Change {
                    version: state.version + 1,
                    document: Some(draft),
                    operation: None,
                },
                &[],
            )?;
            state = self.sessions.plan_documents()?;
        }
        if state
            .plans
            .iter()
            .any(|p| p.id == id && p.revision > revision)
        {
            return Err("newer plan revision requires review".into());
        }
        doc.approved = true;
        let approved = crate::session::run_journal::NewSessionEvent::new(
            "plan/mode",
            json!({"active":false,"approved":{"text":doc.text,"digest":doc.digest}}),
        );
        if !state
            .plans
            .iter()
            .any(|p| p.id == id && p.revision == revision && p.approved)
        {
            self.sessions.commit_plan_change(
                Change {
                    version: state.version + 1,
                    document: Some(doc.clone()),
                    operation: None,
                },
                &[approved],
            )?;
        } else if self.state().active
            || self
                .state()
                .approved
                .as_ref()
                .is_none_or(|p| p.digest != doc.digest)
        {
            self.sessions
                .record_plan_mode(
                    false,
                    Some(crate::session::use_cases::ApprovedPlanWrite {
                        text: doc.text.clone(),
                        digest: doc.digest.clone(),
                    }),
                )
                .map_err(|e| e.to_string())?;
        }
        let event_seq = self
            .state()
            .approved
            .ok_or("approval was not committed")?
            .event_seq;
        Ok(ApprovedPlan {
            text: doc.text,
            digest: doc.digest,
            event_seq,
        })
    }
    pub(crate) fn bind_goal(&self, id: &str, revision: u64, goal: &str) -> Result<(), String> {
        let _lane = self.document_lane.lock().map_err(|_| "plan lane")?;
        let state = self.sessions.plan_documents()?;
        let mut doc = self.document(id, revision)?;
        if !doc.approved {
            return Err("plan is not approved".into());
        }
        if let Some(existing) = &doc.goal_id {
            return if existing == goal {
                Ok(())
            } else {
                Err("plan already belongs to a different goal".into())
            };
        }
        doc.goal_id = Some(goal.into());
        self.sessions.commit_plan_change(
            Change {
                version: state.version + 1,
                document: Some(doc),
                operation: None,
            },
            &[],
        )
    }
    pub(crate) fn bind_goal_atomically<T>(
        &self,
        id: &str,
        revision: u64,
        goal: &str,
        publish: impl FnOnce(crate::session::run_journal::NewSessionEvent) -> Result<T, String>,
    ) -> Result<T, String> {
        let _lane = self.document_lane.lock().map_err(|_| "plan lane")?;
        let state = self.sessions.plan_documents()?;
        let mut doc = self.document(id, revision)?;
        if !doc.approved || doc.goal_id.is_some() {
            return Err("plan is not eligible for a new goal".into());
        }
        doc.goal_id = Some(goal.into());
        let change = Change {
            version: state.version + 1,
            document: Some(doc),
            operation: None,
        };
        let data = serde_json::to_value(change).map_err(|e| e.to_string())?;
        let mut validated = state;
        super::documents::fold(&mut validated, &data)?;
        publish(
            crate::session::run_journal::NewSessionEvent::new(super::documents::EVENT, data)
                .log_only(),
        )
    }
    pub(crate) fn receipt(&self, id: &str, digest: &str) -> Result<Option<Value>, String> {
        match self.sessions.plan_documents()?.operations.get(id) {
            Some(op) if op.digest == digest => Ok(Some(op.result.clone())),
            Some(_) => Err("workflow operation id conflict".into()),
            None => Ok(None),
        }
    }
    pub(crate) fn remember(&self, id: &str, digest: &str, result: Value) -> Result<(), String> {
        let _lane = self.document_lane.lock().map_err(|_| "plan lane")?;
        let state = self.sessions.plan_documents()?;
        if state.operations.contains_key(id) {
            return Ok(());
        }
        self.sessions.commit_plan_change(
            Change {
                version: state.version + 1,
                document: None,
                operation: Some((
                    id.into(),
                    Operation {
                        digest: digest.into(),
                        result,
                    },
                )),
            },
            &[],
        )
    }
    pub(crate) fn review_document(&self) -> Option<PlanDocument> {
        self.review
            .lock()
            .ok()?
            .as_ref()
            .map(|r| r.document.clone())
    }
    pub(crate) fn respond_review(
        &self,
        id: &str,
        revision: u64,
        answer: AskAnswer,
    ) -> Result<bool, String> {
        self.answer_review(id, revision, answer, false)
    }
    pub(crate) fn approve_review(
        &self,
        id: &str,
        revision: u64,
        execute: bool,
    ) -> Result<bool, String> {
        self.answer_review(id, revision, AskAnswer::Selected("Approve".into()), execute)
    }
    fn answer_review(
        &self,
        id: &str,
        revision: u64,
        answer: AskAnswer,
        execute: bool,
    ) -> Result<bool, String> {
        let mut slot = self.review.lock().map_err(|_| "plan review")?;
        let Some(review) = slot.as_mut() else {
            return Ok(false);
        };
        if review.answered {
            return Err("plan review already resolved".into());
        }
        if review.document.id != id || review.document.revision != revision {
            return Err("plan review changed".into());
        }
        let mut handoff = self.handoff.lock().map_err(|_| "handoff")?;
        if execute {
            if handoff.is_some() {
                return Err("a workflow handoff is already pending".into());
            }
            *handoff = Some(Handoff::Goal {
                session: review.document.session.clone(),
                plan: Some((id.into(), revision)),
            });
        }
        if review.sender.send(answer).is_err() {
            if execute {
                *handoff = None;
            }
            return Err("plan review already resolved".into());
        }
        review.answered = true;
        Ok(true)
    }
    pub(crate) fn generation(&self) -> String {
        self.authority.lock().expect("workflow authority").clone()
    }
    pub(crate) fn revoke_handoff(&self) {
        self.cancel_handoff();
        *self.authority.lock().expect("workflow authority") = uuid::Uuid::new_v4().to_string();
    }
    pub(crate) fn cancel_review_handoff(&self, id: &str) {
        *self.authority.lock().expect("workflow authority") = uuid::Uuid::new_v4().to_string();
        let mut handoff = self.handoff.lock().expect("handoff");
        if matches!(handoff.as_ref(),Some(Handoff::Goal{plan:Some((plan,_)),..}) if plan==id) {
            *handoff = None;
        }
    }
    pub(crate) fn cancel_handoff(&self) {
        *self.handoff.lock().expect("workflow handoff") = None;
    }
}
