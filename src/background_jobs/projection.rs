use super::*;
use crate::session::{
    catalog::{self, JobPhase},
    event::SessionEvent,
    projection::{CheckpointRow, ProjectionUnit},
};
pub(crate) struct JobUnit {
    rows: BTreeMap<String, JobView>,
    seq: i64,
}
impl Default for JobUnit {
    fn default() -> Self {
        Self::fresh()
    }
}
impl JobUnit {
    pub(super) fn fresh() -> Self {
        Self {
            rows: BTreeMap::new(),
            seq: -1,
        }
    }
    pub(super) fn rows(&self) -> Vec<JobView> {
        self.rows.values().cloned().collect()
    }
    fn trim(&mut self) {
        let mut completed: Vec<_> = self
            .rows
            .values()
            .filter(|r| r.finished_at.is_some())
            .map(|r| (r.finished_at, r.id.clone()))
            .collect();
        completed.sort();
        let excess = completed.len().saturating_sub(64);
        for (_, id) in completed.into_iter().take(excess) {
            self.rows.remove(&id);
        }
    }
}
impl ProjectionUnit for JobUnit {
    fn key(&self) -> &'static str {
        "jobs"
    }
    fn state_version(&self) -> u64 {
        1
    }
    fn as_of(&self) -> i64 {
        self.seq
    }
    fn fold(&mut self, event: &SessionEvent) -> Result<(), String> {
        if let Some(phase) = catalog::event_spec(&event.event_type).and_then(|s| s.job_phase)
            && let Some(row) = record::view(event, phase)
        {
            if let Some(previous) = self.rows.get(&row.id) {
                if previous.project != row.project
                    || previous.session != row.session
                    || previous.started_at != row.started_at
                {
                    return Err("native job identity changed".into());
                }
                if phase == JobPhase::Registered {
                    return Err("duplicate native job registration".into());
                }
            }
            self.rows.insert(row.id.clone(), row);
            self.trim();
        }
        self.seq = event.seq as i64;
        Ok(())
    }
    fn snapshot(&self) -> Value {
        json!({"rows":self.rows})
    }
    fn restore(&mut self, row: &CheckpointRow) -> Result<(), String> {
        if row.ver != 1 {
            return Err("unsupported native job checkpoint".into());
        }
        let rows: BTreeMap<String, JobView> =
            serde_json::from_value(row.val["rows"].clone()).map_err(|e| e.to_string())?;
        if rows.len() > 4096 || rows.iter().any(|(id, v)| id != &v.id) {
            return Err("invalid native job checkpoint".into());
        }
        self.rows = rows;
        self.seq = row.seq;
        self.trim();
        Ok(())
    }
}
