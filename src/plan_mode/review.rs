use super::*;
use std::sync::mpsc;
use std::time::Duration;
impl PlanModeService {
    pub(crate) fn request_review(
        &self,
        plan: &str,
        cancel: &CancelToken,
    ) -> Result<ApprovedPlan, String> {
        if !self.state().active {
            return Err("exit_plan_mode is available only while Plan Mode is active".into());
        }
        let latest = self
            .sessions
            .plan_documents()?
            .plans
            .into_iter()
            .rev()
            .find(|p| !p.approved);
        let doc = self.save_draft(
            latest.as_ref().map(|p| p.id.as_str()),
            latest.as_ref().map_or(0, |p| p.revision),
            plan,
        )?;
        let asker = self
            .asker
            .asker()
            .ok_or("plan review requires an interactive user asker")?;
        let (sender, receiver) = mpsc::channel();
        *self.review.lock().map_err(|_| "review lock")? = Some(service::Review {
            document: doc.clone(),
            sender,
            answered: false,
        });
        let local_cancel = CancelToken::new();
        let question = review_question(plan);
        let answer = std::thread::scope(|scope| {
            let token = local_cancel.clone();
            let review_id = doc.id.clone();
            let revision = doc.revision;
            scope.spawn(move || {
                let answer = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    asker.ask(question, &token)
                }))
                .unwrap_or(AskAnswer::Declined);
                let _ = self.respond_review(&review_id, revision, answer);
            });
            loop {
                if cancel.is_cancelled() {
                    local_cancel.cancel();
                    return AskAnswer::Declined;
                }
                match receiver.recv_timeout(Duration::from_millis(50)) {
                    Ok(answer) => {
                        local_cancel.cancel();
                        return answer;
                    }
                    Err(mpsc::RecvTimeoutError::Disconnected) => return AskAnswer::Declined,
                    Err(mpsc::RecvTimeoutError::Timeout) => {}
                }
            }
        });
        *self.review.lock().map_err(|_| "review lock")? = None;
        match answer {
            AskAnswer::Selected(label) if label.eq_ignore_ascii_case("approve") => {
                self.approve_document(&doc.id, doc.revision)
            }
            AskAnswer::Custom(feedback) => {
                self.cancel_review_handoff(&doc.id);
                Err(format!("plan was not approved; user feedback: {feedback}"))
            }
            _ => {
                self.cancel_review_handoff(&doc.id);
                Err("plan review was declined or cancelled".into())
            }
        }
    }
}

fn review_question(plan: &str) -> AskQuestion {
    AskQuestion {
        question: format!("Approve this plan and leave Plan Mode?\n\n{plan}"),
        options: vec![
            AskOption {
                label: "Approve".into(),
                description: Some("Save this plan and enable execution on the next run".into()),
            },
            AskOption {
                label: "Reject".into(),
                description: Some("Keep Plan Mode active and revise the plan".into()),
            },
        ],
        allow_custom: true,
    }
}
