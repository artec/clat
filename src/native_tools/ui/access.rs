//! Consent is stored by app identity. Every retained handle owns a revocable
//! lease. The authority lock orders authorization/dispatch against revocation.
use super::*;
use crate::control_storage::ui_consents::Decision;
use crate::interaction::{AskAnswer, AskOption, AskQuestion, AskQuestionPresentation};
use std::sync::Weak;

#[derive(Default)]
pub(crate) struct Authority {
    leases: Mutex<Vec<Weak<Lease>>>,
    prompt: Mutex<()>,
}

pub(super) struct Lease {
    app: String,
    session: String,
    pub(super) cancel: CancelToken,
}

impl Lease {
    fn check(&self) -> Result<()> {
        if self.cancel.is_cancelled() {
            Err(error("APP_ACCESS_REVOKED"))
        } else {
            Ok(())
        }
    }
}

impl Authority {
    pub(super) fn lease(&self, app: &App, run: &Run) -> Result<Arc<Lease>> {
        let mut leases = self.leases.lock().map_err(|_| error("SETTINGS_FAILED"))?;
        active(&run.lifetime)?;
        leases.retain(|lease| lease.strong_count() > 0);
        let lease = Arc::new(Lease {
            app: app.identity.clone(),
            session: run.session.clone(),
            cancel: run.lifetime.child(),
        });
        leases.push(Arc::downgrade(&lease));
        Ok(lease)
    }

    pub(super) fn revoke(
        &self,
        control: &ControlStorage,
        app: &str,
        session: Option<&str>,
    ) -> Result<()> {
        let mut leases = self.leases.lock().map_err(|_| error("SETTINGS_FAILED"))?;
        control
            .revoke_ui_consent(app, session)
            .map_err(|_| error("SETTINGS_FAILED"))?;
        leases.retain(|lease| {
            let Some(lease) = lease.upgrade() else {
                return false;
            };
            if lease.app == app && session.is_none_or(|session| lease.session == session) {
                lease.cancel.cancel();
                return false;
            }
            true
        });
        Ok(())
    }

    fn decision(control: &ControlStorage, lease: &Lease) -> Result<Option<Decision>> {
        lease.check()?;
        control
            .ui_consent(&lease.app, &lease.session)
            .map_err(|_| error("SETTINGS_FAILED"))
    }

    /// The final check and the one bounded native dispatch are ordered against
    /// revoke. Slow observation and both user prompts run outside this lock.
    pub(super) fn authorized<T>(
        &self,
        control: &ControlStorage,
        lease: &Lease,
        operation: impl FnOnce() -> Result<T>,
    ) -> Result<T> {
        let _guard = self.leases.lock().map_err(|_| error("SETTINGS_FAILED"))?;
        match Self::decision(control, lease)? {
            Some(Decision::Allow) => operation(),
            _ => Err(error("APP_ACCESS_REVOKED")),
        }
    }
}

impl Service {
    pub(super) fn consent(&self, run: &Run, app: &App, lease: &Lease) -> Result<()> {
        if !run.interactive {
            return Err(error("APP_APPROVAL_UNAVAILABLE"));
        }
        let asker = self
            .asker
            .asker()
            .ok_or_else(|| error("APP_APPROVAL_UNAVAILABLE"))?;
        let authority = &self.control.ui_authority;
        match Authority::decision(&self.control, lease)? {
            Some(Decision::Allow) => return Ok(()),
            Some(Decision::Deny) => return Err(error("APP_DENIED")),
            None => {}
        }
        let _prompt = authority
            .prompt
            .try_lock()
            .map_err(|_| error("APP_APPROVAL_PENDING"))?;
        match Authority::decision(&self.control, lease)? {
            Some(Decision::Allow) => return Ok(()),
            Some(Decision::Deny) => return Err(error("APP_DENIED")),
            None => {}
        }
        let answer = asker.ask(question(app), &lease.cancel);
        let _guard = authority
            .leases
            .lock()
            .map_err(|_| error("SETTINGS_FAILED"))?;
        lease.check()?;
        let (scope, decision) = match answer {
            AskAnswer::Selected(label) if label == "session" => {
                (Some(lease.session.as_str()), Decision::Allow)
            }
            AskAnswer::Selected(label) if label == "always" => (None, Decision::Allow),
            AskAnswer::Selected(label) if label == "deny" => {
                (Some(lease.session.as_str()), Decision::Deny)
            }
            _ => return Err(error("APP_DENIED")),
        };
        self.control
            .set_ui_consent_named(&lease.app, scope, decision, Some(&app.label))
            .map_err(|_| error("SETTINGS_FAILED"))?;
        match decision {
            Decision::Allow => Ok(()),
            Decision::Deny => Err(error("APP_DENIED")),
        }
    }
}

fn question(app: &App) -> AskQuestion {
    let question = UiMessage::new("cua.app_approval").arg("app", app.label.clone());
    let options = [
        ("session", "cua.allow_session"),
        ("always", "cua.allow_always"),
        ("deny", "cua.deny"),
    ];
    AskQuestion {
        question: question.english(),
        options: options
            .iter()
            .map(|(label, _)| AskOption {
                label: (*label).into(),
                description: None,
            })
            .collect(),
        allow_custom: false,
        presentation: Some(AskQuestionPresentation {
            question,
            options: options.iter().map(|(_, key)| UiMessage::new(key)).collect(),
        }),
    }
}
