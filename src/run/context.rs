//! Request-boundary maintenance; persistence and summarization stay outside the loop.
use crate::model::{ModelItem, ModelOptions, Usage};
use crate::tool::ToolDefinition;

pub(crate) trait RequestContextManager: Send {
    /// Update only after durable admission. Usage also survives a failed update.
    fn maintain(
        &mut self,
        items: &mut Vec<ModelItem>,
        usage: &mut Usage,
        instructions: Option<&str>,
        definitions: &[ToolDefinition],
        options: &ModelOptions,
    ) -> Result<(), String>;
}
