//! Native macOS accessibility. Platform reads, human authorization and
//! execution authority meet here; no process, shell or input-event fallback.
use crate::CancelToken;
use crate::control_storage::ControlStorage;
use crate::i18n::UiMessage;
use crate::interaction::{AskUserSlot, UserAsker};
use crate::tool::{ToolAccessPolicy, ToolError};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::any::Any;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicU64, Ordering},
};
use std::time::{Duration, Instant};

mod access;
mod action;
mod api;
#[cfg(target_os = "macos")]
mod mac;
#[cfg(target_os = "macos")]
pub(crate) mod plugin;
#[cfg(test)]
mod tests;
mod tree;
mod wait;

use super::ui_state::{ApplicationKind, Element, Failure, TargetFence, UiState};
pub(crate) use access::Authority;
use access::Lease;
#[cfg(target_os = "macos")]
pub(super) use mac::{console_ready, trusted, window_count};
use tree::Snapshot;

#[derive(Clone, Debug, Eq, PartialEq)]
struct App {
    pid: i32,
    identity: String,
    process_identity: String,
    label: String,
    bundle: Option<String>,
    kind: ApplicationKind,
}

type Handle = Arc<dyn Any + Send + Sync>;
type Result<T> = std::result::Result<T, ToolError>;

#[derive(Clone, Copy, PartialEq, Eq)]
enum ValuePurpose {
    Read,
    RepeatGuard,
}

/// An observation uses one deadline across all native requests. Handles are
/// opaque, retained by the backend and never supplied by a caller.
trait Backend: Send + Sync {
    fn ready(&self) -> Result<()>;
    fn app(&self, pid: i32) -> Result<App>;
    fn root(&self, app: &App, call: &CancelToken) -> Result<Handle>;
    fn same(&self, left: &Handle, right: &Handle) -> bool;
    fn hash(&self, node: &Handle) -> u64;
    fn children(
        &self,
        node: &Handle,
        offset: usize,
        count: usize,
        windows: bool,
        call: &CancelToken,
    ) -> Result<Vec<Handle>>;
    fn menu_bar(&self, node: &Handle, call: &CancelToken) -> Result<Option<Handle>>;
    fn describe(&self, node: &Handle, app: &App, call: &CancelToken) -> Result<Node>;
    fn value(
        &self,
        node: &Node,
        purpose: ValuePurpose,
        call: &CancelToken,
    ) -> Result<Option<String>>;
    fn window(&self, node: &Node, app: &App, call: &CancelToken) -> Result<Option<Handle>>;
    fn dispatch(&self, node: &Node, action: &str, call: &CancelToken) -> Result<()>;
    fn target_fence(&self, standalone_terminal: bool) -> TargetFence;
}

#[derive(Clone)]
struct Node {
    handle: Handle,
    role: String,
    title: String,
    subrole: Option<String>,
    description: Option<String>,
    actions: Vec<String>,
    label_complete: bool,
    enabled: Option<bool>,
    focused: Option<bool>,
    selected: Option<bool>,
    expanded: Option<bool>,
}

impl Node {
    fn element(
        &self,
        app: &App,
        backend: &dyn Backend,
        window: &Handle,
        value: Option<String>,
    ) -> Element {
        Element {
            app: app.identity.clone(),
            pid: app.pid,
            process_identity: app.process_identity.clone(),
            window: backend.hash(window).to_string(),
            identity: backend.hash(&self.handle).to_string(),
            role: self.role.clone(),
            title: self.title.clone(),
            actions: self.actions.clone(),
            state: format!(
                "{:x}",
                Sha256::digest(
                    json!([
                        self.enabled,
                        self.focused,
                        self.selected,
                        self.expanded,
                        value
                    ])
                    .to_string()
                )
            ),
        }
    }
}

pub(crate) struct Service {
    backend: Arc<dyn Backend>,
    control: Arc<ControlStorage>,
    asker: Arc<AskUserSlot>,
    slot: Mutex<Option<Arc<Run>>>,
}

struct Run {
    session: String,
    source: CancelToken,
    lifetime: CancelToken,
    access: ToolAccessPolicy,
    standalone_terminal: bool,
    interactive: bool,
    snapshot_prefix: uuid::Uuid,
    next_snapshot: AtomicU64,
    data: Mutex<UiState<Snapshot>>,
}

impl Run {
    fn snapshot_id(&self) -> String {
        format!(
            "{}:{:016x}",
            self.snapshot_prefix,
            self.next_snapshot.fetch_add(1, Ordering::Relaxed)
        )
    }
}

fn display_text(text: &str) -> String {
    text.chars()
        .filter(|ch| {
            !ch.is_control() && !matches!(ch, '\u{2028}'..='\u{202e}' | '\u{2066}'..='\u{2069}')
        })
        .collect()
}

impl Service {
    fn new(
        control: Arc<ControlStorage>,
        asker: Arc<AskUserSlot>,
        backend: Arc<dyn Backend>,
    ) -> Self {
        Self {
            control,
            asker,
            backend,
            slot: Mutex::new(None),
        }
    }

    pub(crate) fn begin(
        &self,
        session: String,
        cancel: &CancelToken,
        access: ToolAccessPolicy,
        asker: Option<&dyn UserAsker>,
    ) {
        self.end();
        *self.slot.lock().expect("UI run slot") = Some(Arc::new(Run {
            session,
            source: cancel.clone(),
            lifetime: cancel.child(),
            access,
            standalone_terminal: asker.is_some_and(|asker| asker.is_standalone_terminal()),
            interactive: asker.is_some(),
            snapshot_prefix: uuid::Uuid::new_v4(),
            next_snapshot: AtomicU64::new(0),
            data: Mutex::new(UiState::default()),
        }));
    }

    pub(crate) fn end(&self) {
        if let Some(run) = self.slot.lock().expect("UI run slot").take() {
            run.lifetime.cancel();
        }
    }

    fn run(&self, cancel: &CancelToken) -> Result<Arc<Run>> {
        self.slot
            .lock()
            .map_err(|_| error("NATIVE_ERROR"))?
            .as_ref()
            .filter(|run| run.source.same_source(cancel) && !run.lifetime.is_cancelled())
            .cloned()
            .ok_or_else(|| error("RUN_UNAVAILABLE"))
    }

    fn app(&self, pid: i32) -> Result<App> {
        self.backend.ready()?;
        self.backend.app(pid)
    }

    fn recheck_app(&self, app: &App) -> Result<App> {
        let current = self.app(app.pid)?;
        if current.identity != app.identity || current.process_identity != app.process_identity {
            return Err(error("STALE_APP"));
        }
        Ok(current)
    }
}

pub(super) fn deadline(cancel: &CancelToken, duration: Duration) -> CancelToken {
    cancel.child_with_deadline(Instant::now() + duration)
}

pub(super) fn active(cancel: &CancelToken) -> Result<()> {
    if cancel.is_cancelled() {
        return Err(error(
            if cancel.remaining().is_some_and(|left| left.is_zero()) {
                "TIMEOUT"
            } else {
                "CANCELLED"
            },
        ));
    }
    Ok(())
}

pub(super) fn error(code: &'static str) -> ToolError {
    let key = match code {
        "APP_ACCESS_REVOKED" => "cua.error_revoked",
        "APP_APPROVAL_UNAVAILABLE" => "cua.error_no_approver",
        "APP_APPROVAL_PENDING" => "cua.error_approval_pending",
        "APP_DENIED" => "cua.error_app_denied",
        "ACTION_NOT_APPROVED" => "cua.error_action_not_approved",
        "CANCELLED" => "cua.error_cancelled",
        "DISPATCH_UNKNOWN" => "cua.error_dispatch_unknown",
        "ELEMENT_DISABLED" => "cua.error_disabled",
        "FOREGROUND_REQUIRED" => "cua.error_foreground",
        "INVALID_ARGUMENTS" => "cua.error_arguments",
        "LIMIT" => "cua.error_action_limit",
        "NATIVE_BUSY" => "cua.error_native_busy",
        "PERMISSION_DENIED" => "cua.error_tcc",
        "PLAN_MODE" => "cua.error_plan_mode",
        "PROTECTED_TARGET" => "cua.error_protected_target",
        "RUN_UNAVAILABLE" => "cua.error_run_unavailable",
        "SESSION_LOCKED" => "cua.error_session_locked",
        "SETTINGS_FAILED" => "cua.error_settings",
        "STALE_APP" => "cua.error_stale_app",
        "STALE_SNAPSHOT" => "cua.error_stale_snapshot",
        "TIMEOUT" => "cua.error_timeout",
        "TREE_LIMIT" => "cua.error_tree_limit",
        "UNCHANGED" => "cua.error_unchanged",
        "UNSUPPORTED_ACTION" => "cua.error_unsupported_action",
        _ => "cua.error_native",
    };
    let presentation = UiMessage::new(key);
    let mut recovery = json!({"code":code,"presentation":presentation});
    if code == "DISPATCH_UNKNOWN" {
        recovery["dispatched"] = Value::Null;
        recovery["verified"] = Value::Bool(false);
        recovery["retry_safe"] = Value::Bool(false);
    }
    ToolError::new(presentation.english()).with_recovery(recovery)
}

fn safety_error(failure: Failure) -> ToolError {
    error(match failure {
        Failure::StaleApp => "STALE_APP",
        Failure::StaleSnapshot => "STALE_SNAPSHOT",
        Failure::UnsupportedAction => "UNSUPPORTED_ACTION",
        Failure::Unchanged => "UNCHANGED",
        Failure::Limit => "LIMIT",
        Failure::ProtectedTarget => "PROTECTED_TARGET",
    })
}

pub(super) fn output_error(error: &ToolError) -> Value {
    let mut output = error.recovery().cloned().unwrap_or_else(|| json!({}));
    output["error"] = json!(error.to_string());
    output
}

#[cfg(target_os = "macos")]
pub(crate) fn denied_output(tool: &str, plan: bool) -> Option<Value> {
    let code = match tool {
        "ui_act" if plan => "PLAN_MODE",
        "ui_act" => "ACTION_NOT_APPROVED",
        "ui_tree" | "ui_wait" if plan => "PLAN_MODE",
        _ => return None,
    };
    Some(output_error(&error(code)))
}
