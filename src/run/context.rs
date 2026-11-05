//! Request-boundary maintenance; persistence and summarization stay outside the loop.
use crate::model::{ModelItem, ModelOptions, Usage};
use crate::tool::ToolDefinition;

pub(crate) trait RequestContextManager: Send {
    fn observe(
        &mut self,
        _instructions: Option<&str>,
        _items: &[ModelItem],
        _tools: &[ToolDefinition],
        _usage: &Usage,
        _tail_tokens: u64,
    ) {
    }
    fn failed_capacity_attempt(
        &mut self,
        _kind: crate::model::ModelErrorKind,
    ) -> Result<(), String> {
        Ok(())
    }
    /// Return true only after durable maintenance changed this request.
    fn recover(
        &mut self,
        _kind: crate::model::ModelErrorKind,
        _items: &mut Vec<ModelItem>,
        _usage: &mut Usage,
        _instructions: Option<&str>,
        _definitions: &[ToolDefinition],
        _options: &ModelOptions,
    ) -> Result<bool, String> {
        Ok(false)
    }
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
