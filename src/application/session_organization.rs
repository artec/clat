use super::*;
use crate::control_storage::workspace::SessionFlags;

impl TrustedProjectApplication {
    pub fn set_session_organization(
        &mut self,
        id: &str,
        pinned: Option<bool>,
        archived: Option<bool>,
        confirmed: bool,
    ) -> Result<(), ApplicationError> {
        self.reject_session_switch_while_busy()?;
        if archived == Some(true) && !confirmed {
            return Err(ApplicationError::new(
                "explicit archive confirmation required",
            ));
        }
        if !self.list_sessions()?.iter().any(|s| s.id.as_str() == id) {
            return Err(ApplicationError::new(
                "session does not exist in this project",
            ));
        }
        self.ensure_registered()?;
        let workspace = self
            .workspace_id
            .as_ref()
            .ok_or_else(|| ApplicationError::new("workspace unavailable"))?;
        let mut flags = self
            .control
            .session_flags(workspace)
            .get(id)
            .cloned()
            .unwrap_or_default();
        if let Some(value) = pinned {
            flags.pinned = value;
        }
        if let Some(value) = archived {
            flags.archived = value;
        }
        self.control
            .set_session_flags(workspace, id, flags)
            .map_err(|e| ApplicationError::new(e.to_string()))
    }

    pub(super) fn organize_summaries(&self, summaries: &mut [crate::SessionSummary]) {
        let flags = self
            .workspace_id
            .as_ref()
            .map(|id| self.control.session_flags(id))
            .unwrap_or_default();
        for summary in summaries.iter_mut() {
            let SessionFlags { pinned, archived } =
                flags.get(summary.id.as_str()).cloned().unwrap_or_default();
            summary.pinned = pinned;
            summary.archived = archived;
        }
        summaries.sort_by_key(|s| (!s.pinned, std::cmp::Reverse(s.last_activity_ms)));
    }
}
