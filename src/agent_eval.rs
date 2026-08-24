//! Deterministic Agent Scenario Harness (Roadmap R0-A).
//!
//! Scenarios are byte-exact disk fixtures, but execution uses the same
//! Application, plugin catalog, permission pipeline, RunEvent stream, and
//! session journal as the product. The only fake is the model provider: it
//! supplies a declared sequence of responses and rejects request drift.

use crate::model::{FinishReason, ModelEvent, ModelRequest, ModelResponse, Usage};
use crate::test_support::{
    SharedEvents, TestBehavior, TestModelScript, TestProviderPlugin, cleanup_tree,
    configure_test_model, roots,
};
use crate::{
    ApplicationRunRequest, BootstrapApplication, PermissionDecision, PermissionRequest, Project,
    RunEvent, ToolCall,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, mpsc};

const SCENARIO_SCHEMA_VERSION: u32 = 1;
const REPORT_SCHEMA_VERSION: u32 = 1;
const MAX_SCENARIO_BYTES: u64 = 64 * 1024;
const MAX_SCENARIO_STEPS: usize = 64;
const MAX_FIXTURE_FILES: usize = 256;
const MAX_FIXTURE_ENTRIES: usize = 1024;
const MAX_FIXTURE_BYTES: usize = 1024 * 1024;
const MAX_FIXTURE_DEPTH: usize = 32;

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ScenarioDefinition {
    schema_version: u32,
    id: String,
    prompt: String,
    model_steps: Vec<ScenarioStep>,
    expected: ExpectedDefinition,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ScenarioStep {
    #[serde(default)]
    expect: StepExpectation,
    response: StepResponse,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct StepExpectation {
    available_tools_include: Vec<String>,
    available_tools_exclude: Vec<String>,
    required_tool_results: Vec<String>,
    instructions_include: Vec<String>,
    instructions_exclude: Vec<String>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct StepResponse {
    #[serde(default)]
    text: String,
    #[serde(default)]
    tool_calls: Vec<ToolCall>,
    finish_reason: ScenarioFinishReason,
    #[serde(default)]
    usage: Option<ScenarioUsage>,
}

#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
enum ScenarioFinishReason {
    Completed,
    ToolCalls,
    Refusal,
    Incomplete,
}

impl ScenarioFinishReason {
    fn into_model(self) -> FinishReason {
        match self {
            Self::Completed => FinishReason::Completed,
            Self::ToolCalls => FinishReason::ToolCalls,
            Self::Refusal => FinishReason::Refusal,
            Self::Incomplete => FinishReason::Incomplete,
        }
    }
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct ScenarioUsage {
    input_tokens: u64,
    output_tokens: u64,
    cached_input_tokens: Option<u64>,
    reasoning_tokens: Option<u64>,
}

impl From<ScenarioUsage> for Usage {
    fn from(value: ScenarioUsage) -> Self {
        Self {
            input_tokens: value.input_tokens,
            output_tokens: value.output_tokens,
            cached_input_tokens: value.cached_input_tokens,
            reasoning_tokens: value.reasoning_tokens,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct ExpectedDefinition {
    outcome: OutcomeStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    category: Option<FailureCategory>,
    #[serde(skip_serializing_if = "Option::is_none")]
    message_contains: Option<String>,
    event_kinds: Vec<String>,
    durable_event_kinds: Vec<String>,
    #[serde(default)]
    permission_checks: Vec<PermissionReport>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
enum OutcomeStatus {
    Success,
    Failure,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
enum FailureCategory {
    ToolUnavailable,
    ModelContract,
    Permission,
    Tool,
    Persistence,
    Cleanup,
    RunFailure,
}

#[derive(Clone, Debug, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
enum ObservedOutcome {
    Success {
        output: String,
        turns: usize,
    },
    Failure {
        category: FailureCategory,
        message: String,
        turns: usize,
    },
}

impl ObservedOutcome {
    fn status(&self) -> OutcomeStatus {
        match self {
            Self::Success { .. } => OutcomeStatus::Success,
            Self::Failure { .. } => OutcomeStatus::Failure,
        }
    }

    fn category(&self) -> Option<FailureCategory> {
        match self {
            Self::Success { .. } => None,
            Self::Failure { category, .. } => Some(*category),
        }
    }

    fn message(&self) -> Option<&str> {
        match self {
            Self::Success { .. } => None,
            Self::Failure { message, .. } => Some(message),
        }
    }
}

#[derive(Clone, Debug, Serialize)]
struct ModelStepReport {
    expected: usize,
    observed: usize,
    violations: Vec<String>,
}

#[derive(Clone, Debug, Serialize)]
struct FileReport {
    path: String,
    bytes: usize,
    sha256: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct PermissionReport {
    tool: String,
    decision: PermissionDecisionKind,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
enum PermissionDecisionKind {
    Allow,
    Ask,
    Deny,
    Unavailable,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
enum GateStatus {
    Matched,
    Mismatch,
}

#[derive(Clone, Debug, Serialize)]
struct ScenarioReport {
    schema_version: u32,
    scenario_id: String,
    gate: GateStatus,
    expected: ExpectedDefinition,
    observed: ObservedOutcome,
    model_steps: ModelStepReport,
    event_kinds: Vec<String>,
    durable_event_kinds: Vec<String>,
    permission_checks: Vec<PermissionReport>,
    files: Vec<FileReport>,
    files_match: bool,
    application_close: CloseStatus,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
enum CloseStatus {
    Clean,
    Failed(String),
}

#[derive(Default)]
struct ScriptState {
    cursor: usize,
    violations: Vec<String>,
}

struct ScenarioScript {
    steps: Vec<ScenarioStep>,
    state: Mutex<ScriptState>,
}

impl ScenarioScript {
    fn new(steps: Vec<ScenarioStep>) -> Self {
        Self {
            steps,
            state: Mutex::new(ScriptState::default()),
        }
    }

    fn report(&self) -> ModelStepReport {
        let state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut violations = state.violations.clone();
        if state.cursor < self.steps.len() {
            violations.push(format!(
                "run ended after {} of {} declared model steps",
                state.cursor,
                self.steps.len()
            ));
        }
        ModelStepReport {
            expected: self.steps.len(),
            observed: state.cursor,
            violations,
        }
    }

    fn reject(state: &mut ScriptState, message: String) -> crate::ModelError {
        state.violations.push(message.clone());
        crate::ModelError::request(format!("scenario model contract: {message}"))
    }
}

impl TestModelScript for ScenarioScript {
    fn stream(
        &self,
        request: ModelRequest<'_>,
        events: &mut dyn crate::model::ModelEventSink,
    ) -> Result<ModelResponse, crate::ModelError> {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let index = state.cursor;
        state.cursor = state.cursor.saturating_add(1);
        let Some(step) = self.steps.get(index) else {
            return Err(Self::reject(
                &mut state,
                format!("unexpected model request {} after script end", index + 1),
            ));
        };

        let available = request
            .tools
            .iter()
            .map(|tool| tool.name.as_str())
            .collect::<BTreeSet<_>>();
        for required in &step.expect.available_tools_include {
            if !available.contains(required.as_str()) {
                return Err(Self::reject(
                    &mut state,
                    format!("request {} is missing tool `{required}`", index + 1),
                ));
            }
        }
        for forbidden in &step.expect.available_tools_exclude {
            if available.contains(forbidden.as_str()) {
                return Err(Self::reject(
                    &mut state,
                    format!(
                        "request {} unexpectedly exposes tool `{forbidden}`",
                        index + 1
                    ),
                ));
            }
        }
        let tool_results = request
            .items
            .iter()
            .filter_map(|item| match item {
                crate::ModelItem::ToolResult(result) => Some(result.tool_name.as_str()),
                _ => None,
            })
            .collect::<BTreeSet<_>>();
        for required in &step.expect.required_tool_results {
            if !tool_results.contains(required.as_str()) {
                return Err(Self::reject(
                    &mut state,
                    format!("request {} is missing tool result `{required}`", index + 1),
                ));
            }
        }
        let instructions = request.instructions.unwrap_or_default();
        for required in &step.expect.instructions_include {
            if !instructions.contains(required) {
                return Err(Self::reject(
                    &mut state,
                    format!(
                        "request {} instructions are missing `{required}`",
                        index + 1
                    ),
                ));
            }
        }
        for forbidden in &step.expect.instructions_exclude {
            if instructions.contains(forbidden) {
                return Err(Self::reject(
                    &mut state,
                    format!(
                        "request {} instructions unexpectedly contain `{forbidden}`",
                        index + 1
                    ),
                ));
            }
        }

        let response = &step.response;
        if !response.text.is_empty() {
            events.emit(ModelEvent::TextDelta {
                delta: response.text.clone(),
            });
        }
        let usage = response.usage.clone().map(Usage::from);
        if let Some(usage) = &usage {
            events.emit(ModelEvent::Usage(usage.clone()));
        }
        Ok(ModelResponse {
            text: response.text.clone(),
            tool_calls: response.tool_calls.clone(),
            finish_reason: response.finish_reason.into_model(),
            usage,
            provider_response_id: None,
            provider_state: Vec::new(),
            reasoning: None,
        })
    }
}

struct TempTree(PathBuf);

impl Drop for TempTree {
    fn drop(&mut self) {
        cleanup_tree(&self.0);
    }
}

fn run_fixture(fixture: &Path) -> Result<ScenarioReport, String> {
    let definition = load_definition(fixture)?;
    validate_definition(&definition, fixture)?;
    let (storage_root, project_root) = roots(&format!("agent-eval-{}", definition.id));
    let base = storage_root
        .parent()
        .ok_or_else(|| "scenario temporary root has no parent".to_owned())?
        .to_path_buf();
    let _temp = TempTree(base);
    std::fs::create_dir_all(&project_root).map_err(|error| error.to_string())?;
    copy_fixture_tree(&fixture.join("input"), &project_root)?;

    let script = Arc::new(ScenarioScript::new(definition.model_steps.clone()));
    let bootstrap = BootstrapApplication::open(Project::new(&project_root), storage_root.clone())
        .map_err(|error| error.to_string())?;
    let mut application = bootstrap
        .authorize_and_mount_with_provider(Arc::new(TestProviderPlugin {
            behavior: TestBehavior::Scripted(script.clone()),
        }))
        .map_err(|error| error.to_string())?;
    configure_test_model(&application);

    let live_events = Arc::new(Mutex::new(Vec::new()));
    // Mount 之后不再使用 `?` 越过显式 close：即便 start/join/recv
    // 本身坏掉，Harness 也必须先走 Application 的生产 teardown。
    let run_attempt = (|| -> Result<crate::ApplicationRunResult, String> {
        let (completion, receiver) = mpsc::channel();
        let handle = application
            .start_run(ApplicationRunRequest {
                prompt: definition.prompt.clone(),
                attachments: Vec::new(),
                approver: Arc::new(AllowAllApprover),
                asker: None,
                events: Box::new(SharedEvents(Arc::clone(&live_events))),
                completion,
            })
            .map_err(|error| error.to_string())?;
        handle.join().map_err(|error| error.to_string())?;
        receiver.recv().map_err(|error| error.to_string())
    })();
    let close = match application.close() {
        Ok(()) => CloseStatus::Clean,
        Err(error) => CloseStatus::Failed(error.to_string()),
    };
    let outcome = match run_attempt {
        Ok(outcome) => outcome,
        Err(error) => {
            return Err(match close {
                CloseStatus::Clean => format!("scenario run infrastructure failed: {error}"),
                CloseStatus::Failed(close) => format!(
                    "scenario run infrastructure failed: {error}; application close failed: {close}"
                ),
            });
        }
    };

    let observed = match outcome {
        Ok(done) => ObservedOutcome::Success {
            output: done.output,
            turns: done.turns,
        },
        Err(failure) => ObservedOutcome::Failure {
            category: classify_failure(&failure.error),
            message: failure.error,
            turns: failure.turns,
        },
    };
    let events = live_events
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone();
    let event_kinds = events
        .iter()
        .map(event_kind)
        .map(str::to_owned)
        .collect::<Vec<_>>();
    let permission_checks = events
        .iter()
        .filter_map(|event| match event {
            RunEvent::PermissionChecked { tool, decision } => Some(PermissionReport {
                tool: tool.clone(),
                decision: permission_decision(decision),
            }),
            _ => None,
        })
        .collect::<Vec<_>>();
    let durable_event_kinds = load_durable_event_kinds(&storage_root)?;
    let actual_files = collect_files(&project_root)?;
    let expected_files = collect_files(&fixture.join("expected"))?;
    let files_match = actual_files == expected_files;
    let files = actual_files
        .iter()
        .map(|(path, bytes)| FileReport {
            path: path.clone(),
            bytes: bytes.len(),
            sha256: hex_digest(bytes),
        })
        .collect::<Vec<_>>();
    let model_steps = script.report();
    let gate = evaluate_gate(
        &definition.expected,
        GateEvidence {
            observed: &observed,
            model_steps: &model_steps,
            event_kinds: &event_kinds,
            durable_event_kinds: &durable_event_kinds,
            permission_checks: &permission_checks,
            files_match,
            close: &close,
        },
    );

    Ok(ScenarioReport {
        schema_version: REPORT_SCHEMA_VERSION,
        scenario_id: definition.id,
        gate,
        expected: definition.expected,
        observed,
        model_steps,
        event_kinds,
        durable_event_kinds,
        permission_checks,
        files,
        files_match,
        application_close: close,
    })
}

struct AllowAllApprover;

impl crate::PermissionApprover for AllowAllApprover {
    fn decide(
        &self,
        _request: PermissionRequest,
        _cancel: &crate::CancelToken,
    ) -> PermissionDecision {
        PermissionDecision::Allow
    }
}

fn load_definition(fixture: &Path) -> Result<ScenarioDefinition, String> {
    let path = fixture.join("scenario.json");
    let metadata = std::fs::symlink_metadata(&path).map_err(|error| error.to_string())?;
    if !metadata.file_type().is_file() || metadata.file_type().is_symlink() {
        return Err("scenario.json must be a regular non-symlink file".into());
    }
    if metadata.len() > MAX_SCENARIO_BYTES {
        return Err(format!("scenario.json exceeds {MAX_SCENARIO_BYTES} bytes"));
    }
    let bytes = std::fs::read(&path).map_err(|error| error.to_string())?;
    serde_json::from_slice(&bytes).map_err(|error| format!("scenario.json: {error}"))
}

fn validate_definition(definition: &ScenarioDefinition, fixture: &Path) -> Result<(), String> {
    if definition.schema_version != SCENARIO_SCHEMA_VERSION {
        return Err(format!(
            "unsupported scenario schema version {}",
            definition.schema_version
        ));
    }
    let directory_name = fixture
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| "scenario fixture directory is not UTF-8".to_owned())?;
    if definition.id != directory_name {
        return Err(format!(
            "scenario id `{}` does not match fixture directory `{directory_name}`",
            definition.id
        ));
    }
    if definition.id.is_empty()
        || !definition
            .id
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
    {
        return Err("scenario id must be non-empty lowercase kebab-case".into());
    }
    if definition.prompt.trim().is_empty() {
        return Err("scenario prompt must not be empty".into());
    }
    if definition.model_steps.is_empty() || definition.model_steps.len() > MAX_SCENARIO_STEPS {
        return Err(format!(
            "scenario must contain 1..={MAX_SCENARIO_STEPS} model steps"
        ));
    }
    match definition.expected.outcome {
        OutcomeStatus::Success => {
            if definition.expected.category.is_some()
                || definition.expected.message_contains.is_some()
            {
                return Err("successful expectation cannot declare failure details".into());
            }
        }
        OutcomeStatus::Failure => {
            if definition.expected.category.is_none()
                || definition
                    .expected
                    .message_contains
                    .as_deref()
                    .unwrap_or("")
                    .is_empty()
            {
                return Err("failure expectation requires category and message_contains".into());
            }
        }
    }
    if definition.expected.event_kinds.is_empty() {
        return Err("expected event_kinds must not be empty".into());
    }
    if definition.expected.durable_event_kinds.is_empty() {
        return Err("expected durable_event_kinds must not be empty".into());
    }
    for (index, step) in definition.model_steps.iter().enumerate() {
        let include = step
            .expect
            .available_tools_include
            .iter()
            .collect::<BTreeSet<_>>();
        if step
            .expect
            .available_tools_exclude
            .iter()
            .any(|tool| include.contains(tool))
        {
            return Err(format!(
                "model step {} includes and excludes the same tool",
                index + 1
            ));
        }
        let instruction_include = step
            .expect
            .instructions_include
            .iter()
            .collect::<BTreeSet<_>>();
        if step
            .expect
            .instructions_exclude
            .iter()
            .any(|text| instruction_include.contains(text))
        {
            return Err(format!(
                "model step {} includes and excludes the same instruction text",
                index + 1
            ));
        }
        let has_tool_calls = !step.response.tool_calls.is_empty();
        if matches!(step.response.finish_reason, ScenarioFinishReason::ToolCalls) != has_tool_calls
        {
            return Err(format!(
                "model step {} finish_reason/tool_calls disagree",
                index + 1
            ));
        }
        let mut call_ids = BTreeSet::new();
        for call in &step.response.tool_calls {
            if call.id.trim().is_empty() || call.name.trim().is_empty() {
                return Err(format!(
                    "model step {} has an empty tool call id or name",
                    index + 1
                ));
            }
            if !call_ids.insert(&call.id) {
                return Err(format!(
                    "model step {} repeats tool call id `{}`",
                    index + 1,
                    call.id
                ));
            }
        }
    }
    Ok(())
}

struct GateEvidence<'a> {
    observed: &'a ObservedOutcome,
    model_steps: &'a ModelStepReport,
    event_kinds: &'a [String],
    durable_event_kinds: &'a [String],
    permission_checks: &'a [PermissionReport],
    files_match: bool,
    close: &'a CloseStatus,
}

fn evaluate_gate(expected: &ExpectedDefinition, evidence: GateEvidence<'_>) -> GateStatus {
    let outcome_matches = expected.outcome == evidence.observed.status()
        && expected.category == evidence.observed.category()
        && expected.message_contains.as_deref().is_none_or(|needle| {
            evidence
                .observed
                .message()
                .is_some_and(|message| message.contains(needle))
        });
    let matched = outcome_matches
        && evidence.model_steps.expected == evidence.model_steps.observed
        && evidence.model_steps.violations.is_empty()
        && expected.event_kinds == evidence.event_kinds
        && expected.durable_event_kinds == evidence.durable_event_kinds
        && expected.permission_checks == evidence.permission_checks
        && evidence.files_match
        && evidence.close == &CloseStatus::Clean;
    if matched {
        GateStatus::Matched
    } else {
        GateStatus::Mismatch
    }
}

fn classify_failure(message: &str) -> FailureCategory {
    if message.starts_with("unknown tool `") {
        FailureCategory::ToolUnavailable
    } else if message.contains("scenario model contract:") {
        FailureCategory::ModelContract
    } else if message.contains("permission") {
        FailureCategory::Permission
    } else if message.contains("session journal") || message.contains("persist") {
        FailureCategory::Persistence
    } else if message.contains("cleanup") || message.contains("close") {
        FailureCategory::Cleanup
    } else if message.contains("tool") {
        FailureCategory::Tool
    } else {
        FailureCategory::RunFailure
    }
}

fn event_kind(event: &RunEvent) -> &'static str {
    match event {
        RunEvent::RunStarted { .. } => "run_started",
        RunEvent::ModelRequested { .. } => "model_requested",
        RunEvent::ModelStream { .. } => "model_stream",
        RunEvent::ModelResponded { .. } => "model_responded",
        RunEvent::ToolRequested { .. } => "tool_requested",
        RunEvent::PermissionChecked { .. } => "permission_checked",
        RunEvent::PermissionDenied { .. } => "permission_denied",
        RunEvent::ToolStarted { .. } => "tool_started",
        RunEvent::ToolFinished { .. } => "tool_finished",
        RunEvent::SteeringApplied { .. } => "steering_applied",
        RunEvent::RunCompleted { .. } => "run_completed",
        RunEvent::RunCancelled { .. } => "run_cancelled",
        RunEvent::RunFailed { .. } => "run_failed",
    }
}

fn permission_decision(decision: &PermissionDecision) -> PermissionDecisionKind {
    match decision {
        PermissionDecision::Allow => PermissionDecisionKind::Allow,
        PermissionDecision::Ask { .. } => PermissionDecisionKind::Ask,
        PermissionDecision::Deny { .. } => PermissionDecisionKind::Deny,
        PermissionDecision::Unavailable { .. } => PermissionDecisionKind::Unavailable,
    }
}

fn load_durable_event_kinds(storage_root: &Path) -> Result<Vec<String>, String> {
    let backend = crate::session::persistence::JsonlBackend::new(
        storage_root.join("sessions"),
        crate::session::persistence::JsonlCompression::Zstd,
        false,
    );
    let headers = backend.list_headers().map_err(|error| error.to_string())?;
    if headers.len() != 1 {
        return Err(format!(
            "scenario expected exactly one durable session, found {}",
            headers.len()
        ));
    }
    let header = &headers[0];
    let cwd = header
        .cwd
        .clone()
        .ok_or_else(|| "scenario session header has no cwd".to_owned())?;
    let key = crate::session::key::SessionKey {
        project: crate::session::key::ProjectKey::from_cwd(&cwd),
        id: header.id.clone(),
    };
    let loaded = backend
        .load(&key, false)
        .map_err(|error| error.to_string())?;
    Ok(loaded
        .events
        .into_iter()
        .map(|event| event.event_type)
        .collect())
}

fn copy_fixture_tree(source: &Path, destination: &Path) -> Result<(), String> {
    let files = collect_files(source)?;
    for (relative, bytes) in files {
        let target = destination.join(relative_path(&relative)?);
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
        }
        std::fs::write(target, bytes).map_err(|error| error.to_string())?;
    }
    Ok(())
}

fn collect_files(root: &Path) -> Result<BTreeMap<String, Vec<u8>>, String> {
    let mut files = BTreeMap::new();
    let mut total_bytes = 0usize;
    let mut entries_seen = 0usize;
    collect_files_at(
        root,
        root,
        0,
        &mut files,
        &mut total_bytes,
        &mut entries_seen,
    )?;
    Ok(files)
}

fn collect_files_at(
    root: &Path,
    current: &Path,
    depth: usize,
    files: &mut BTreeMap<String, Vec<u8>>,
    total_bytes: &mut usize,
    entries_seen: &mut usize,
) -> Result<(), String> {
    if depth > MAX_FIXTURE_DEPTH {
        return Err(format!(
            "scenario fixture depth exceeds {MAX_FIXTURE_DEPTH}"
        ));
    }
    let metadata = std::fs::symlink_metadata(current).map_err(|error| error.to_string())?;
    if metadata.file_type().is_symlink() {
        return Err(format!(
            "scenario fixture/project contains symlink `{}`",
            current.display()
        ));
    }
    if metadata.is_file() {
        if files.len() >= MAX_FIXTURE_FILES {
            return Err(format!("scenario file count exceeds {MAX_FIXTURE_FILES}"));
        }
        let remaining = MAX_FIXTURE_BYTES.saturating_sub(*total_bytes);
        let mut bytes = Vec::new();
        std::fs::File::open(current)
            .and_then(|file| {
                use std::io::Read as _;
                file.take((remaining + 1) as u64).read_to_end(&mut bytes)
            })
            .map_err(|error| error.to_string())?;
        if bytes.len() > remaining {
            return Err(format!("scenario fixture bytes exceed {MAX_FIXTURE_BYTES}"));
        }
        *total_bytes += bytes.len();
        let relative = current
            .strip_prefix(root)
            .map_err(|error| error.to_string())?;
        files.insert(display_relative(relative)?, bytes);
        return Ok(());
    }
    if !metadata.is_dir() {
        return Err(format!(
            "scenario fixture contains special file `{}`",
            current.display()
        ));
    }
    let mut entries = Vec::new();
    for entry in std::fs::read_dir(current).map_err(|error| error.to_string())? {
        *entries_seen = entries_seen.saturating_add(1);
        if *entries_seen > MAX_FIXTURE_ENTRIES {
            return Err(format!(
                "scenario fixture entries exceed {MAX_FIXTURE_ENTRIES}"
            ));
        }
        entries.push(entry.map_err(|error| error.to_string())?);
    }
    entries.sort_by_key(std::fs::DirEntry::file_name);
    for entry in entries {
        collect_files_at(
            root,
            &entry.path(),
            depth + 1,
            files,
            total_bytes,
            entries_seen,
        )?;
    }
    Ok(())
}

fn display_relative(path: &Path) -> Result<String, String> {
    let mut parts = Vec::new();
    for component in path.components() {
        match component {
            std::path::Component::Normal(part) => parts.push(
                part.to_str()
                    .ok_or_else(|| "scenario path is not UTF-8".to_owned())?,
            ),
            _ => return Err("scenario path is not a normal relative path".into()),
        }
    }
    Ok(parts.join("/"))
}

fn relative_path(display: &str) -> Result<PathBuf, String> {
    if display.is_empty() {
        return Err("scenario relative path is empty".into());
    }
    let mut path = PathBuf::new();
    for part in display.split('/') {
        if part.is_empty() || part == "." || part == ".." {
            return Err(format!("invalid scenario relative path `{display}`"));
        }
        path.push(part);
    }
    Ok(path)
}

fn hex_digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn report_json(report: &ScenarioReport) -> Result<String, String> {
    serde_json::to_string_pretty(report)
        .map(|mut text| {
            text.push('\n');
            text
        })
        .map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(name: &str) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/agent-scenarios")
            .join(name)
    }

    #[test]
    fn every_registered_scenario_matches_its_golden_report() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/agent-scenarios");
        let mut fixtures = std::fs::read_dir(&root)
            .expect("scenario catalog")
            .collect::<Result<Vec<_>, _>>()
            .expect("scenario entries");
        fixtures.sort_by_key(std::fs::DirEntry::file_name);
        let mut ran = Vec::new();
        for entry in fixtures {
            let file_type = entry.file_type().expect("scenario entry type");
            if !file_type.is_dir() {
                continue;
            }
            let fixture = entry.path();
            let report = run_fixture(&fixture).expect("catalog scenario runs");
            assert_eq!(report.gate, GateStatus::Matched, "report: {report:#?}");
            let actual = report_json(&report).expect("serialize report");
            let expected = std::fs::read_to_string(fixture.join("expected-report.json"))
                .expect("expected report");
            assert_eq!(
                actual,
                expected,
                "scenario `{}` report drift needs explicit review",
                fixture.display()
            );
            ran.push(report.scenario_id);
        }
        assert!(
            !ran.is_empty(),
            "the scenario catalog must contain at least one gate"
        );
    }

    #[test]
    fn patch_multi_hunk_pre_fix_baseline_is_stable() {
        let fixture = fixture("patch-multi-hunk-before-plugin");
        let report = run_fixture(&fixture).expect("scenario runs through Application");
        assert_eq!(report.gate, GateStatus::Matched, "report: {report:#?}");
        let actual = report_json(&report).expect("serialize report");
        let expected =
            std::fs::read_to_string(fixture.join("expected-report.json")).expect("expected report");
        assert_eq!(
            actual, expected,
            "report protocol drift needs explicit review"
        );
        assert!(
            !actual.contains(std::env::temp_dir().to_string_lossy().as_ref()),
            "stable report must not contain the random temporary root"
        );
    }

    #[test]
    fn scenario_schema_rejects_unknown_version_and_empty_steps() {
        let fixture = fixture("patch-multi-hunk-before-plugin");
        let unconsumed =
            ScenarioScript::new(load_definition(&fixture).expect("fixture").model_steps).report();
        assert!(
            unconsumed.violations[0].contains("run ended after 0 of 1"),
            "too few model requests must be a named contract violation"
        );
        let mut definition = load_definition(&fixture).expect("fixture");
        definition.schema_version += 1;
        assert!(
            validate_definition(&definition, &fixture)
                .unwrap_err()
                .contains("unsupported scenario schema")
        );
        definition.schema_version = SCENARIO_SCHEMA_VERSION;
        definition.model_steps.clear();
        assert!(
            validate_definition(&definition, &fixture)
                .unwrap_err()
                .contains("model steps")
        );
        let mut definition = load_definition(&fixture).expect("fixture");
        definition.model_steps[0].response.finish_reason = ScenarioFinishReason::Completed;
        assert!(
            validate_definition(&definition, &fixture)
                .unwrap_err()
                .contains("finish_reason/tool_calls disagree")
        );
    }

    #[test]
    fn model_script_reports_catalog_drift_and_extra_requests() {
        let fixture = fixture("patch-multi-hunk-before-plugin");
        let definition = load_definition(&fixture).expect("fixture");
        let script = ScenarioScript::new(definition.model_steps);
        let tools = vec![crate::ToolDefinition {
            name: "apply_patch".into(),
            description: String::new(),
            input_schema: serde_json::json!({}),
            effect: crate::ToolEffect::Write,
            strict: true,
        }];
        let items = vec![crate::ModelItem::user_text("probe")];
        let options = crate::model::ModelOptions::default();
        let cancel = crate::CancelToken::new();
        let request = ModelRequest {
            instructions: None,
            items: &items,
            tools: &tools,
            options: &options,
            cancel: &cancel,
        };
        let mut events = Vec::new();
        let first = script
            .stream(request, &mut events)
            .expect_err("catalog drift");
        assert!(first.to_string().contains("unexpectedly exposes tool"));
        let second = script
            .stream(request, &mut events)
            .expect_err("extra request");
        assert!(second.to_string().contains("after script end"));
        let report = script.report();
        assert_eq!(report.observed, 2);
        assert_eq!(report.violations.len(), 2);
    }

    #[test]
    fn gate_distinguishes_expected_failure_from_baseline_drift() {
        let expected = ExpectedDefinition {
            outcome: OutcomeStatus::Failure,
            category: Some(FailureCategory::ToolUnavailable),
            message_contains: Some("unknown tool".into()),
            event_kinds: vec!["run_failed".into()],
            durable_event_kinds: vec!["turn/end".into()],
            permission_checks: Vec::new(),
        };
        let observed = ObservedOutcome::Failure {
            category: FailureCategory::ToolUnavailable,
            message: "unknown tool `apply_patch`".into(),
            turns: 1,
        };
        let steps = ModelStepReport {
            expected: 1,
            observed: 1,
            violations: Vec::new(),
        };
        assert_eq!(
            evaluate_gate(
                &expected,
                GateEvidence {
                    observed: &observed,
                    model_steps: &steps,
                    event_kinds: &["run_failed".into()],
                    durable_event_kinds: &["turn/end".into()],
                    permission_checks: &[],
                    files_match: true,
                    close: &CloseStatus::Clean,
                },
            ),
            GateStatus::Matched
        );
        assert_eq!(
            evaluate_gate(
                &expected,
                GateEvidence {
                    observed: &observed,
                    model_steps: &steps,
                    event_kinds: &["run_completed".into()],
                    durable_event_kinds: &["turn/end".into()],
                    permission_checks: &[],
                    files_match: true,
                    close: &CloseStatus::Clean,
                },
            ),
            GateStatus::Mismatch,
            "an event drift is a gate failure even when the registered disease remains"
        );
    }
}
