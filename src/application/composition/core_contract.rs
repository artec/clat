//! Mandatory runtime contracts, independent of their current plugin implementations.
use crate::plugin::ServiceId;
use crate::plugin::selection::Requirement;
use crate::plugins::services::*;

// Each root's identity and human-readable responsibility have exactly one home.
const CONTRACTS: &[(ServiceId, &str)] = &[
    (SESSION_SERVICE_ID, "extension.core.sessions"),
    (CONFIG_SERVICE_ID, "extension.core.config"),
    (AGENT_SERVICE_ID, "extension.core.agent"),
    (PROCESS_SERVICE_ID, "extension.core.process"),
    (SANDBOX_SERVICE_ID, "extension.core.sandbox"),
    (PERMISSION_SERVICE_ID, "extension.core.permissions"),
    (TOOL_ACCESS_SERVICE_ID, "extension.core.tool_access"),
    (TOOL_PIPELINE_SERVICE_ID, "extension.core.tool_pipeline"),
    (TOOL_SERVICE_ID, "extension.core.tools"),
    (PROVIDER_SERVICE_ID, "extension.core.providers"),
    (PROMPT_SERVICE_ID, "extension.core.prompt"),
    (COMMAND_SERVICE_ID, "extension.core.commands"),
    (MCP_STATUS_SERVICE_ID, "extension.core.extension_status"),
];

pub(super) fn requirements() -> Vec<Requirement> {
    CONTRACTS
        .iter()
        .map(|(service, reason)| Requirement::new(*service, crate::i18n::UiMessage::new(reason)))
        .collect()
}
