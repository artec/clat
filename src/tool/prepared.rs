use super::*;

/// Opaque native authority. External tools cannot construct an enforcement proof.
pub struct PreparedTool {
    pub(crate) tool: Arc<dyn Tool>,
    pub(crate) workspace_enforced: bool,
    pub(crate) status: String,
    pub(crate) approval: Option<PreparedApproval>,
}

pub(crate) struct PreparedApproval {
    pub(crate) arguments: Value,
    pub(crate) reason: String,
    pub(crate) required: bool,
    pub(crate) cancel: CancelToken,
    pub(crate) permit: Arc<std::sync::atomic::AtomicBool>,
}

pub(crate) fn prepare_tool(
    tool: Arc<dyn Tool>,
    arguments: &Value,
    cancel: &CancelToken,
) -> Result<(Arc<dyn Tool>, Option<PreparedTool>), ToolError> {
    let prepared = tool.prepare_for_call(arguments, cancel)?;
    let execution = prepared
        .as_ref()
        .map_or(tool, |prepared| Arc::clone(&prepared.tool));
    Ok((execution, prepared))
}
