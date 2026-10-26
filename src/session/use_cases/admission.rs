//! Durable admission owner lookup, independent of the selected session.
use super::*;

impl SessionService {
    /// Resolve a durable client admission across every materialized session
    /// in one project without changing the active selection. Frontend
    /// recovery uses this when its own mapping write can lag the session
    /// journal: zero matches means no admission committed, one identifies
    /// its owner, and multiple matches are corruption.
    pub(crate) fn find_committed_admission_session(
        &self,
        project: &ProjectKey,
        client_message_id: &str,
    ) -> Result<Option<(SessionId, crate::message::CommittedAdmission)>, SessionError> {
        #[cfg(test)]
        if self
            .fail_next_admission_owner_scan
            .swap(false, Ordering::AcqRel)
        {
            return Err(SessionError::Io(
                "intentional admission-owner scan failure".into(),
            ));
        }
        let mut found = None;
        for summary in self.list_sessions(project)? {
            let key = SessionKey {
                project: project.clone(),
                id: summary.id,
            };
            let session_admission = scan_committed_admission(client_message_id, |visitor| {
                self.backend.visit_from(&key, 0, visitor).map(|_| ())
            })?;
            let Some(admission) = session_admission else {
                continue;
            };
            if found.is_some() {
                return Err(SessionError::Corruption(
                    "client delivery is committed in multiple project sessions".into(),
                ));
            }
            found = Some((key.id, admission));
        }
        Ok(found)
    }
}

fn scan_committed_admission(
    client_message_id: &str,
    mut read: impl FnMut(
        &mut dyn FnMut(&SessionEvent) -> Result<(), String>,
    ) -> Result<(), SessionError>,
) -> Result<Option<crate::message::CommittedAdmission>, SessionError> {
    // A title or a write-behind batch can commit during stat→stream→stat.
    // Keep each attempt private: a rejected scan is neither an owner nor an
    // extra occurrence of the same delivery. Persistent I/O still fails closed.
    let mut last = None;
    for _ in 0..3 {
        let mut admission = None;
        let result = read(&mut |event| {
            let Some(committed) = committed_admission_from_event(event, client_message_id) else {
                return Ok(());
            };
            if admission.replace(committed).is_some() {
                return Err("client delivery is committed more than once in one session".into());
            }
            Ok(())
        });
        match result {
            Ok(()) => return Ok(admission),
            Err(SessionError::Io(message)) => last = Some(SessionError::Io(message)),
            Err(error) => return Err(error),
        }
    }
    Err(last.expect("at least one attempt ran"))
}

#[cfg(test)]
mod tests;
