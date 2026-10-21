//! Transport-owned annotation cleanup feeds the shared public-schema boundary.
//! Validation strings and property names are never rewritten.
use super::client::{McpServer, McpToolInfo, Session};
use crate::{ToolDefinition, ToolError};
pub(super) fn public_tool_info(server: &McpServer, mut info: McpToolInfo) -> McpToolInfo {
    info.description = redact_text(server, &info.description);
    info
}
pub(super) fn public_definition(
    server: &McpServer,
    definition: ToolDefinition,
) -> Result<ToolDefinition, ToolError> {
    crate::tool::capabilities::schema::definition(
        definition,
        &|text| redact_text(server, text),
        &|text| match &server.session {
            Session::Stdio(s) => s.private_schema_text(text),
            Session::Http(s) => s.private_schema_text(text),
        },
    )
}
fn redact_text(server: &McpServer, text: &str) -> String {
    match &server.session {
        Session::Stdio(s) => s.public_metadata(text),
        Session::Http(s) => s.public_metadata(text),
    }
}
