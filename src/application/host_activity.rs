//! Identifiers for the desktop quit notice. Presentation stays in the desktop catalogs.
use super::TrustedProjectApplication;
use crate::background_jobs::JobView;
use serde_json::{Value, json};

pub(crate) struct HostActivity {
    kind: &'static str,
    name: String,
    running: bool,
}

impl HostActivity {
    fn running(kind: &'static str, name: impl Into<String>) -> Self {
        Self {
            kind,
            name: name.into(),
            running: true,
        }
    }

    fn held(kind: &'static str) -> Self {
        Self {
            kind,
            name: String::new(),
            running: false,
        }
    }

    pub(crate) fn json(&self) -> Value {
        json!({"kind": self.kind, "name": self.name, "running": self.running})
    }
}

impl TrustedProjectApplication {
    pub(crate) fn host_activities(&self) -> Vec<HostActivity> {
        let mut items = Vec::new();
        if let Some(service) = &self.development {
            for (kind, name) in service.quit_activities() {
                items.push(HostActivity::running(kind, name));
            }
        }
        items.extend(self.job_activities());
        if self.has_automatic_work() {
            items.push(HostActivity::running("schedule", ""));
        }
        if self.has_active_acp() {
            items.push(HostActivity::running("agent", ""));
        }
        if !self.next_turn_queue().is_empty() {
            items.push(HostActivity::held("queue"));
        }
        if !self.pending_inputs().items.is_empty() {
            items.push(HostActivity::held("input"));
        }
        items
    }

    fn job_activities(&self) -> Vec<HostActivity> {
        let Some(jobs) = &self.jobs else {
            return Vec::new();
        };
        let open: Vec<HostActivity> = jobs
            .list()
            .into_iter()
            .filter(job_open)
            .map(|view| HostActivity::running("job", view.label))
            .collect();
        if open.is_empty() && jobs.has_active() {
            vec![HostActivity::running("job", "")]
        } else {
            open
        }
    }
}

fn job_open(view: &JobView) -> bool {
    view.finished_at.is_none() || view.persistence_error.is_some() || view.cleanup_error.is_some()
}
