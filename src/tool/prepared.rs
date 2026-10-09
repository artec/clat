use super::*;

/// Opaque native authority. External tools cannot construct an enforcement proof.
pub struct PreparedTool {
    pub(crate) tool: Arc<dyn Tool>,
    pub(crate) workspace_enforced: bool,
    pub(crate) status: String,
}

pub(crate) fn prepare_tool(
    tool: Arc<dyn Tool>,
    arguments: &Value,
) -> (Arc<dyn Tool>, Option<PreparedTool>) {
    let prepared = tool.prepare_execution(arguments);
    let execution = prepared
        .as_ref()
        .map_or(tool, |prepared| Arc::clone(&prepared.tool));
    (execution, prepared)
}
