//! Client staging is lazy and bound to this Host instance and project route.
use super::*;
use std::sync::Arc;
impl HostClient {
    pub fn ensure_image_input_available(&self) -> Result<(), HostCallError> {
        #[cfg(any(test, feature = "test-support"))]
        if self
            .fixture_image_availability
            .as_ref()
            .is_some_and(|availability| availability.enabled)
        {
            return Ok(());
        }
        let info = self.call_with_receipt("workbench.info", &json!({}))?;
        if info["builtin_availability"]["builtin.draft_images"]["enabled"] == true {
            return Ok(());
        }
        let message = serde_json::from_value(
            info["builtin_availability"]["builtin.draft_images"]["reason"].clone(),
        )
        .unwrap_or_else(|_| {
            crate::plugins::inventory::unavailable(crate::plugin::PluginId::new(
                "builtin.draft_images",
            ))
        });
        Err(crate::i18n::PresentedText::from(message).into())
    }
    pub fn clipboard_drafts(&self) -> Result<Arc<crate::draft::DraftImageStore>, HostCallError> {
        self.ensure_image_input_available()?;
        let mut slot = self
            .clipboard
            .lock()
            .map_err(|_| "clipboard staging busy")?;
        Ok(slot
            .get_or_insert_with(|| Arc::new(crate::draft::DraftImageStore::new(&self.root)))
            .clone())
    }
    pub fn existing_clipboard_drafts(&self) -> Option<Arc<crate::draft::DraftImageStore>> {
        self.clipboard.lock().ok()?.clone()
    }
}
