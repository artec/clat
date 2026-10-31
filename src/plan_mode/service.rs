use super::documents::{Change, PlanDocument};
use super::*;
use serde_json::json;

pub(crate) struct Review {
    pub document: PlanDocument,
    pub sender: std::sync::mpsc::Sender<AskAnswer>,
    pub answered: bool,
}
impl PlanModeService {
    pub(crate) fn documents(&self) -> Result<Vec<PlanDocument>, String> {
        crate::plan_mode::documents::historical(&self.sessions)
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
        let _lane = self.workflow.lane.lock().map_err(|_| "plan lane")?;
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
                crate::plan_mode::documents::legacy_document(
                    self.sessions.active_id(),
                    self.state(),
                )
                .filter(|doc| doc.id == id && doc.revision == revision)
            })
            .ok_or("plan revision not found")?;
        self.sessions.materialize_plan(&doc)?;
        Ok(doc)
    }
    pub(crate) fn approve_document(&self, id: &str, revision: u64) -> Result<ApprovedPlan, String> {
        let _lane = self.workflow.lane.lock().map_err(|_| "plan lane")?;
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
        let _lane = self.workflow.lane.lock().map_err(|_| "plan lane")?;
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
        let _lane = self.workflow.lane.lock().map_err(|_| "plan lane")?;
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
        let mut handoff = self.workflow.handoff.lock().map_err(|_| "handoff")?;
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
    pub(crate) fn cancel_review_handoff(&self, id: &str) {
        self.workflow.cancel_review_handoff(id);
    }
    pub(crate) fn cancel_handoff(&self) {
        self.workflow.cancel_handoff();
    }
}
