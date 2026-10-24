use super::*;
pub use crate::web_tools::{FetchProvider, WebSettingsEdit, WebSettingsView};
impl TrustedProjectApplication {
    pub fn web_settings(&self) -> Result<WebSettingsView, ApplicationError> {
        self.web
            .view()
            .map_err(|e| ApplicationError::presented(e.presentation()))
    }
    pub fn save_web_settings(
        &self,
        edit: WebSettingsEdit,
    ) -> Result<WebSettingsView, ApplicationError> {
        self.web
            .save(edit)
            .map_err(|e| ApplicationError::presented(e.presentation()))?;
        self.web_settings()
    }
}
