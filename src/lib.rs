#[cfg(test)]
mod agent_eval;
pub mod application;
mod apply_patch;
mod command;
mod control_storage;
pub mod demo;
mod draft;
mod dsh;
pub mod event;
pub mod exec;
pub mod goal;
pub(crate) mod im;
mod interaction;
mod language_intelligence;
mod mcp;
pub mod media;
pub mod memory;
pub mod message;
pub mod model;
mod native_tools;
mod permission;
mod plan_mode;
mod plugin;
pub mod plugin_cli;
mod plugin_host;
mod plugins;
pub mod presets;
mod private_fs;
mod process;
pub mod project;
mod project_instructions;
mod providers;
mod redact;
pub mod run;
mod sandbox;
mod search;
pub mod serve;
mod skills;
pub mod subagent;
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
pub mod upgrade;
mod view_image;
mod wire;

pub use application::{
    ApplicationError, ApplicationEvent, ApplicationRunDone, ApplicationRunFailure,
    ApplicationRunRequest, ApplicationRunResult, BootstrapApplication, CompactHandle,
    CompactReport, CompactionStatus, ContextEstimateSnapshot, ContextSkillDiagnostic,
    McpServerInfoDto, McpStatusDto, ProjectAuthorization, ProjectSnapshot, RecalledSteering,
    RenameOutcome, RunHandle, SessionSnapshot, SteerOutcome, TrustedProjectApplication,
    WorkbenchModelSnapshot, WorkbenchProjectSnapshot, WorkbenchSessionSnapshot, WorkbenchSnapshot,
};
pub use command::{CommandError, CommandInfo, CommandOutcome};
pub use control_storage::ModelProfileSummary;
pub use event::{EventSink, ModelOutcome, RunEvent};
pub use interaction::{AskAnswer, AskOption, AskQuestion, UserAsker};
pub use message::{
    AdmissionReceipt, AdmissionState, AttachmentDescriptor, ClientMessageId, CommittedAdmission,
    ContentBlock, DraftScope, DraftTarget, MessageContent, PendingMessage, ToolResultContent,
};
pub use model::{
    CancelToken, ContentPart, FinishReason, ImageRequestPolicy, Modality, Model, ModelCapabilities,
    ModelConfig, ModelError, ModelErrorKind, ModelEvent, ModelEventSink, ModelFactory, ModelItem,
    ModelOptions, ModelOverrides, ModelProtocol, ModelRequest, ModelResponse, ModelVendor,
    Override, ProviderCredentials, ProviderDescriptor, ProviderFieldDescriptor, ProviderFieldKind,
    ProviderState, RetryHint, ThinkingLevel, Usage, apply_thinking_level, effective_thinking_level,
    endpoint_vendor, next_thinking_level, thinking_levels,
};
pub use permission::{
    AllowAll, InteractivePermissionPolicy, ModePolicy, PermissionApprover, PermissionDecision,
    PermissionMode, PermissionPolicy, PermissionRequest, SafeByDefault, WriteScope,
    escalation_targets, mode_allows, mode_decision, mode_guidance, mode_write_scope,
};
pub use presets::{MODEL_PRESETS, ModelPreset, preset_by_id};
pub use project::Project;
pub(crate) use run::Run;
pub use run::{RunError, RunOutput};
pub(crate) use session::id::SessionId;
pub(crate) use session::use_cases::SessionSummary;
pub use tool::{Tool, ToolCall, ToolDefinition, ToolEffect, ToolError, ToolResult};
pub(crate) use tool::{ToolExecutionPipeline, ToolRegistry};
