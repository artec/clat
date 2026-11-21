//! Image-capacity recovery is independent of the configured context window.
//! A provider's context overflow is surfaced as-is, never used to infer tiers.
use super::*;
use crate::model::{ModelErrorKind, ModelItem, ModelOptions, Usage};
use crate::tool::ToolDefinition;

impl ActiveContext {
    pub(super) fn recover_capacity(
        &mut self,
        kind: ModelErrorKind,
        items: &mut [ModelItem],
        _usage: &mut Usage,
        instructions: Option<&str>,
        definitions: &[ToolDefinition],
        options: &ModelOptions,
    ) -> Result<bool, String> {
        if self.cancel.is_cancelled() || kind != ModelErrorKind::ImageCapacity {
            return Ok(false);
        }
        self.boundary()?;
        let count = self.maintain_images(items, instructions, definitions, options, true)?;
        self.notify_maintenance(0, count);
        Ok(count > 0)
    }
}
