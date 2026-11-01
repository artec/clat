//! A durable reduction authorizes exactly one retry; summary failure cannot erase prior progress.
use super::*;
use crate::model::{ModelErrorKind, ModelItem, ModelOptions, Usage};
use crate::tool::ToolDefinition;
impl ActiveContext {
    pub(super) fn recover_capacity(
        &mut self,
        kind: ModelErrorKind,
        items: &mut Vec<ModelItem>,
        usage: &mut Usage,
        instructions: Option<&str>,
        definitions: &[ToolDefinition],
        options: &ModelOptions,
    ) -> Result<bool, String> {
        if self.cancel.is_cancelled() {
            return Ok(false);
        }
        self.boundary()?;
        let cost = |items: &[ModelItem]| {
            crate::model::estimate_request_tokens(instructions, items, definitions)
        };
        let before = cost(items);
        if kind == ModelErrorKind::ImageCapacity {
            let count = self.maintain_images(items, instructions, definitions, options, true)?;
            self.notify_maintenance(0, count);
            return Ok(count > 0);
        }
        let pruned = self.prune_results(items)?;
        self.notify_maintenance(pruned, 0);
        if cost(items) >= before && self.compactor.is_some() {
            // Active selection still protects user input and the newest complete tool exchange.
            let _ = self.reduce(items, usage, instructions, definitions, &cost, 0)?;
        }
        Ok(!self.cancel.is_cancelled() && cost(items) < before)
    }
}
