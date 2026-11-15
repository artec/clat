//! Restart records uncertainty; no command is ever replayed or adopted.
use super::*;
use crate::session::{
    catalog::JobPhase, projection::ProjectionUnit, use_cases::query::QueryReader,
};
impl JobRegistry {
    pub(crate) fn recover(&self) -> Result<(), JobError> {
        #[cfg(test)]
        crate::test_support::execution_observation::record("jobs.recover");
        let recent = project_jobs(
            &self.sessions.query_reader(),
            &self.project,
            &self.scope,
            |key, row| {
                if row.finished_at.is_none() {
                    row.state = "interrupted".into();
                    row.reference.last_observation = "interrupted".into();
                    row.finished_at = Some(now());
                    row.notification_id = Some(format!("job:{}:settled", row.id));
                    row.observation =
                        Some(json!({"execution_outcome":"unknown","reason":"host_restarted"}));
                    self.sessions
                        .commit_background(key, &[record::event(row, JobPhase::Settled)])
                        .map_err(failed)?;
                }
                Ok(())
            },
        )?;
        for row in recent {
            let _ = self.restore_entry(row);
        } // Retained output is a bounded, disposable cache.
        Ok(())
    }
}

pub(crate) fn read_unmounted_jobs(
    reader: &QueryReader,
    project: &ProjectKey,
    capacity: &JobCapacity,
) -> Result<Vec<JobView>, JobError> {
    project_jobs(reader, project, &capacity.scope(project), |_, row| {
        if row.finished_at.is_none() {
            // The old process cannot be observed without a live registry.
            row.state = "interrupted".into();
        }
        Ok(())
    })
}

fn project_jobs(
    reader: &QueryReader,
    project: &ProjectKey,
    scope: &str,
    mut observe: impl FnMut(&SessionKey, &mut JobView) -> Result<(), JobError>,
) -> Result<Vec<JobView>, JobError> {
    let snapshots = reader.list(project).map_err(failed)?;
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
            if row.project != project.header_cwd.as_deref().unwrap_or("")
                || row.session != key.id.as_str()
                || row.reference.scope != scope
            {
                continue;
            }
            observe(&key, &mut row)?;
            recent.push(row);
            recent.sort_by_key(|r| std::cmp::Reverse((r.finished_at, r.started_at)));
            recent.truncate(64);
        }
    }
    Ok(recent)
}
fn failed(error: impl ToString) -> JobError {
    JobError::new("persistence_failed").detail(error)
}

#[cfg(test)]
impl JobRegistry {
    pub(crate) fn seed_registered_for_test(&self, id: &SessionId) -> String {
        self.bind(id.clone());
        let entry = self.prepare_entry("never launched".into(), id).unwrap();
        self.persist(&entry, JobPhase::Registered).unwrap();
        self.unbind();
        entry.view().id.clone()
    }
}
