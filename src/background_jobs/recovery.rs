//! Restart records uncertainty; no command is ever replayed or adopted.
use super::*;
use crate::session::{catalog::JobPhase, projection::ProjectionUnit};
impl JobRegistry {
    pub(crate) fn recover(&self) -> Result<(), JobError> {
        let reader = self.sessions.query_reader();
        let snapshots = reader.list(&self.project).map_err(failed)?;
        let mut recent = Vec::new();
        for (key, header, _) in snapshots {
            if header.version != crate::session::compat::SESSION_FORMAT_VERSION
                || header.origin.is_some()
                || header.parent_session.is_some()
            {
                continue;
            }
            let mut unit = projection::JobUnit::default();
            reader.visit(&key, &mut |e| unit.fold(e)).map_err(failed)?;
            for mut row in unit.rows() {
                // Seeded/copied facts retain their original identity; they never
                // grant this session ownership of another session's process.
                if row.project != self.project.header_cwd.as_deref().unwrap_or("")
                    || row.session != key.id.as_str()
                    || row.reference.scope != self.scope
                {
                    continue;
                }
                if row.finished_at.is_none() {
                    row.state = "interrupted".into();
                    row.reference.last_observation = "interrupted".into();
                    row.finished_at = Some(now());
                    row.notification_id = Some(format!("job:{}:settled", row.id));
                    row.observation =
                        Some(json!({"execution_outcome":"unknown","reason":"host_restarted"}));
                    self.sessions
                        .commit_background(&key, &[record::event(&row, JobPhase::Settled)])
                        .map_err(failed)?;
                }
                recent.push(row);
                recent.sort_by_key(|r| std::cmp::Reverse((r.finished_at, r.started_at)));
                recent.truncate(64);
            }
        }
        for row in recent {
            let _ = self.restore_entry(row);
        } // Retained output is a bounded, disposable cache.
        Ok(())
    }
}
fn failed(error: impl ToString) -> JobError {
    JobError::new("persistence_failed").detail(error)
}
