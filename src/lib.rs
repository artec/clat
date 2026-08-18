pub mod application;
pub(crate) mod control_storage;
pub mod demo;
pub mod event;
pub mod exec;
mod mcp;
mod mcp_client;
pub mod model;
mod native_tools;
mod permission;
mod plugin;
mod plugins;
pub mod presets;
pub mod project;
mod providers;
pub mod run;
// DSH-compatible session stack (docs/todo/dsh-session-persistence.md):
// session facts live exclusively in these logs since the stage-5 cutover.
// Format-documentation items (catalog constants, layout helpers) exceed the
// current production surface; they stay as the compatibility reference.
#[allow(dead_code)]
pub(crate) mod session;
#[cfg(test)]
pub(crate) mod test_support;
mod tool;
pub mod tui;
mod tui_input;
mod tui_markdown;
mod tui_model;
mod tui_sessions;
mod tui_worker;
pub mod upgrade;

pub use application::{
    ApplicationError, ApplicationEvent, ApplicationRunDone, ApplicationRunFailure,
    ApplicationRunRequest, ApplicationRunResult, BootstrapApplication, CompactHandle,
    CompactReport, CompactionStatus, McpServerInfoDto, McpStatusDto, ProjectAuthorization,
    ProjectSnapshot, RunHandle, SessionSnapshot, TrustedProjectApplication,
};
pub use control_storage::ModelProfileSummary;
pub use event::{EventSink, ModelOutcome, RunEvent};
pub use model::{
    CancelToken, ContentPart, FinishReason, Model, ModelConfig, ModelError, ModelErrorKind,
    ModelEvent, ModelEventSink, ModelFactory, ModelItem, ModelOptions, ModelProtocol, ModelRequest,
    ModelResponse, ModelVendor, ProviderCredentials, ProviderDescriptor, ProviderFieldDescriptor,
    ProviderFieldKind, ProviderState, RetryHint, ThinkingLevel, Usage, apply_thinking_level,
    effective_thinking_level, endpoint_vendor, next_thinking_level, thinking_levels,
};
pub use permission::{
    AllowAll, InteractivePermissionPolicy, PermissionApprover, PermissionDecision,
    PermissionPolicy, PermissionRequest, SafeByDefault,
};
pub use presets::{MODEL_PRESETS, ModelPreset, preset_by_id};
pub use project::Project;
pub(crate) use run::Run;
pub use run::{RunError, RunOutput};
pub(crate) use session::id::SessionId;
pub(crate) use session::use_cases::{SessionSummary, TranscriptLine};
pub use tool::{Tool, ToolCall, ToolDefinition, ToolEffect, ToolError, ToolResult};
pub(crate) use tool::{ToolExecutionPipeline, ToolRegistry};
