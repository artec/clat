use crate::model::{ModelConfig, ProviderCredentials, ProviderDescriptor};
use crate::permission::PermissionMode;
use crate::plugins::services::McpStatus;
use crate::session::id::SessionId;
use crate::session::replay::ReplayEvent;
use crate::session::use_cases::TranscriptLine;
use std::path::PathBuf;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ContextEstimateSnapshot {
    pub estimator: String,
    pub unit: String,
    pub base_prompt_estimate: u64,
    pub project_instructions_estimate: u64,
    pub plan_policy_estimate: u64,
    pub skill_catalog_estimate: u64,
    /// Bytes of the explicitly invoked skill layer (`/skill <name>`), zero
    /// when no skill is armed for the next request (SC-2).
    pub invoked_skill_estimate: u64,
    pub goal_policy_estimate: u64,
    /// Actual memory bytes injected for the inspected request (zero when
    /// `/context` has no future prompt to search with).
    pub memory_estimate: u64,
    pub memory_budget_bytes: u64,
    pub tool_schemas_estimate: u64,
    pub history_estimate: u64,
    /// Images in the effective model history, including typed tool results,
    /// in provider projection order.
    pub image_count: u64,
    pub image_original_count: u64,
    pub image_offloaded_count: u64,
    /// Sum of currently reachable normalized attachment bytes.
    pub image_bytes: u64,
    /// Visual-token component from the same estimator used by request
    /// preflight and compaction. It is included in `history_estimate`.
    pub image_token_estimate: u64,
    pub image_token_safety_factor: u64,
    pub output_reserve_estimate: u64,
    pub input_estimate: u64,
    pub total_estimate: u64,
    pub tool_names: Vec<String>,
    pub skill_names: Vec<String>,
    pub skill_diagnostics: Vec<ContextSkillDiagnostic>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ContextSkillDiagnostic {
    pub source: String,
    pub name: Option<String>,
    pub kind: String,
    pub message: String,
}

/// `/skill` 列表投影（SC-2）：三层 catalog 的 display-ready 条目加发现
/// 诊断。正文与 digest 不进 DTO——加载仍走 run 冻结 catalog 的 `skill`
/// 工具/调用解析，列表只回答"有哪些、来自哪层、什么约束"。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SkillsOverviewDto {
    pub entries: Vec<SkillEntryDto>,
    pub diagnostics: Vec<ContextSkillDiagnostic>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SkillEntryDto {
    pub name: String,
    /// "bundled" | "user" | "project"
    pub source: String,
    pub requires_execution: bool,
    pub description: String,
}

/// 轻量、前端中立的工作台读模型。
///
/// 与 [`ProjectSnapshot`] 的边界刻意不同：这里不读 transcript/replay，
/// 不携带 credentials，也不触发 monitor 配置。PWA、未来桌面端和 IDE
/// 可用它绘制应用壳；会话正文仍只从 journal replay / RunEvent 获得。
#[derive(Clone, Debug, PartialEq)]
pub struct WorkbenchSnapshot {
    pub project: WorkbenchProjectSnapshot,
    pub session: WorkbenchSessionSnapshot,
    pub model: WorkbenchModelSnapshot,
    pub permission_mode: PermissionMode,
    pub plan_mode_active: bool,
    pub goal_armed: bool,
    pub mcp: McpStatusDto,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorkbenchProjectSnapshot {
    /// Trusted Project 的规范根路径；仅供本机前端展示。
    pub root: PathBuf,
    pub name: String,
    /// 惰性 workspace 注册前为 None。
    pub workspace_id: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorkbenchSessionSnapshot {
    pub id: Option<SessionId>,
    pub title: Option<String>,
    pub committed_seq: Option<u64>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct WorkbenchModelSnapshot {
    pub protocol: crate::model::ModelProtocol,
    pub model: String,
    pub preset: Option<String>,
    pub active_profile: Option<String>,
    pub thinking_level: Option<crate::model::ThinkingLevel>,
    pub max_context_tokens: Option<u32>,
    /// W2b frontend-neutral tri-state source. Credentials and unrestricted
    /// provider JSON remain excluded from this lightweight snapshot.
    pub overrides: crate::model::ModelOverrides,
    /// 已解析缺省值后的单次 run token 花费护栏；0 表示关闭。
    pub run_token_budget: u64,
    /// 当前冻结配置是否接受图片输入。唯一事实源是
    /// `ModelCapabilities::accepts_image_input()`；前端不得重判。
    pub image_input: bool,
}

#[derive(Clone, Debug)]
pub struct ProjectSnapshot {
    pub session_id: Option<SessionId>,
    /// 会话右标题（effective：显式标题事件，否则首条用户消息派生的
    /// fallback；全新空会话为 None）。快照携带 + `TitleUpdated` 事件
    /// 增量更新，两路同源（title 投影）。
    pub session_title: Option<String>,
    pub transcript: Vec<TranscriptLine>,
    /// Structured replay of the active session's journal (transcript
    /// rebuild input for frontends; `transcript` above is the legacy
    /// text-flattened view until the TUI migrates).
    pub replay: Vec<ReplayEvent>,
    pub input_history: Vec<String>,
    /// Journal-derived session usage aggregate (status-bar Cache ratio).
    pub session_usage: crate::model::Usage,
    /// Journal-derived per-route usage buckets (INV-C1：Cache 段按当前
    /// 模型路由取桶，键由内部 `model_route_key` 规则生成）。
    pub usage_routes: std::collections::BTreeMap<String, crate::model::Usage>,
    /// Journal-derived most recent request usage (status-bar Context).
    pub last_request_usage: Option<crate::model::Usage>,
    pub config: ModelConfig,
    pub credentials: ProviderCredentials,
    pub provider_descriptors: Vec<ProviderDescriptor>,
    pub mcp: McpStatusDto,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct McpStatusDto {
    pub configured: usize,
    pub connected: usize,
    /// 仍在后台连接中的 server 数（`configured − connected − failed`；
    /// 启动落定后为 0——docs/todo/mcp-async-startup.md INV-M4）。
    pub connecting: usize,
    pub failures: Vec<String>,
    pub servers: Vec<McpServerInfoDto>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct McpServerInfoDto {
    pub name: String,
    pub server_version: String,
    pub protocol_version: String,
    /// 该服务器注册进 Tool Registry 的工具数（/mcp 视图用）。
    pub tools: usize,
    /// 传输类型 `"stdio"` / `"http"`（/mcp 视图用）。
    pub transport: String,
}

impl From<&McpStatus> for McpStatusDto {
    fn from(status: &McpStatus) -> Self {
        let snapshot = status.snapshot();
        Self {
            configured: snapshot.configured,
            connected: snapshot.connected,
            connecting: snapshot.connecting,
            failures: snapshot.failures,
            servers: snapshot
                .servers
                .iter()
                .map(|server| McpServerInfoDto {
                    name: server.name.clone(),
                    server_version: server.server_version.clone(),
                    protocol_version: server.protocol_version.clone(),
                    tools: server.tools,
                    transport: server.transport.clone(),
                })
                .collect(),
        }
    }
}

#[derive(Clone, Debug)]
pub struct SessionSnapshot {
    pub id: SessionId,
    /// 见 `ProjectSnapshot::session_title`（同源：title 投影）。
    pub session_title: Option<String>,
    pub transcript: Vec<TranscriptLine>,
    /// Structured replay of this session's journal (see `ProjectSnapshot::
    /// replay`).
    pub replay: Vec<ReplayEvent>,
    /// Journal-derived session stats of the target session (see
    /// `ProjectSnapshot::session_usage`).
    pub session_usage: crate::model::Usage,
    pub usage_routes: std::collections::BTreeMap<String, crate::model::Usage>,
    pub last_request_usage: Option<crate::model::Usage>,
    pub input_history: Vec<String>,
}

/// 工作区枚举行（MP-1 §5：多项目地基 API，v1 无 UI 消费方——
/// `workspaces()` / `active_workspace()`）。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorkspaceInfo {
    pub id: String,
    /// realpath 规范形（注册时盖章）。
    pub path: String,
    pub title: String,
    pub session_ids: Vec<String>,
    /// 该工作区自己的当前会话（None = Fresh）。
    pub active_session_id: Option<String>,
}
