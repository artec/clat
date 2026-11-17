//! Owner-authenticated, transient notification projections; never journal facts.
use super::HostClient;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct HostNotification {
    pub id: String,
    pub kind: String,
    pub workspace_id: String,
    pub project_root: std::path::PathBuf,
    pub session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completion: Option<NotificationCompletion>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct NotificationCompletion {
    pub outcome: NotificationOutcome,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub elapsed_ms: Option<u64>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NotificationOutcome {
    Succeeded,
    Failed,
    Stopped,
    Interrupted,
    #[serde(other)]
    Unknown,
}

impl HostClient {
    pub fn notification_events(&self) -> Result<super::HostEvents, String> {
        let mut root = self.clone();
        root.prefix.clear();
        super::transport::request(&root, "GET", "/api/notifications", &[])
            .map(super::HostEvents::new)
    }

    pub fn focus_notification(&self, notification: &HostNotification) -> Result<Self, String> {
        let client = self.open_project(&notification.project_root, false)?;
        if let Some(id) = &notification.session_id {
            client.focus_session(id)?;
        }
        Ok(client)
    }
}
