//! Resolve before consent and per-action approval, then independently
//! re-observe before issuing one native action.
use super::api::UiTool;
use super::tree::{Observed, get_observation, same_window};
use super::*;
use crate::Project;
use crate::tool::{PreparedApproval, PreparedTool, Tool, ToolDefinition};
use serde::Deserialize;
use std::sync::atomic::{AtomicBool, Ordering};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ActionRequest {
    snapshot_id: String,
    element_index: usize,
    action: String,
}

struct PreparedAction {
    source: UiTool,
    run: Arc<Run>,
    request: ActionRequest,
    app: App,
    lease: Arc<Lease>,
    observed: Observed,
    permit: Arc<AtomicBool>,
    used: AtomicBool,
}

impl UiTool {
    pub(super) fn prepare_action(
        &self,
        run: Arc<Run>,
        request: ActionRequest,
    ) -> Result<PreparedTool> {
        let (app, lease, observed) =
            get_observation(&run, &request.snapshot_id, request.element_index)?;
        let mut prepared = PreparedAction {
            source: self.clone(),
            run,
            request,
            app,
            lease,
            observed,
            permit: Arc::new(AtomicBool::new(false)),
            used: AtomicBool::new(false),
        };
        let (node, _) = prepared.revalidate()?;
        if !prepared.run.interactive {
            return Err(error("APP_APPROVAL_UNAVAILABLE"));
        }
        prepared
            .source
            .service
            .consent(&prepared.run, &prepared.app, &prepared.lease)?;
        let high_risk = (node.title.is_empty()
            && node.description.as_deref().is_none_or(str::is_empty))
            || super::super::ui_state::needs_approval(&node.title)
            || node
                .description
                .as_deref()
                .is_some_and(super::super::ui_state::needs_approval);
        let title = display_text(if node.title.is_empty() {
            node.description.as_deref().unwrap_or(&node.role)
        } else {
            &node.title
        });
        let reason = UiMessage::new(if high_risk {
            "cua.action_high_risk_reason"
        } else {
            "cua.action_reason"
        })
        .english();
        let approval = PreparedApproval {
            arguments: json!({
                "snapshot_id": prepared.request.snapshot_id, "element_index": prepared.request.element_index,
                "action": prepared.request.action,
                "target":{"app":prepared.app.label,"pid":prepared.app.pid,"bundle_id":prepared.app.bundle,
                    "role":node.role,"title":title,"description":node.description.as_deref().map(display_text),"high_risk":high_risk}
            }),
            reason,
            required: high_risk,
            cancel: prepared.lease.cancel.clone(),
            permit: prepared.permit.clone(),
        };
        // A single immutable prepared target is passed to both permission and execution.
        prepared.observed.node = node;
        Ok(PreparedTool {
            tool: Arc::new(prepared),
            workspace_enforced: false,
            status: String::new(),
            approval: Some(approval),
        })
    }
}

impl PreparedAction {
    fn revalidate(&self) -> Result<(Node, Handle)> {
        let service = &self.source.service;
        let app = service.recheck_app(&self.app)?;
        let fence = service.backend.target_fence(self.run.standalone_terminal);
        fence
            .check_app(app.pid, &app.identity, app.kind)
            .map_err(safety_error)?;
        let Some(window) = self.observed.window.as_ref() else {
            return Err(error("UNSUPPORTED_ACTION"));
        };
        let original =
            self.observed
                .node
                .element(&self.app, service.backend.as_ref(), window, None);
        let call = deadline(&self.lease.cancel, Duration::from_secs(2));
        active(&call)?;
        let node = service
            .backend
            .describe(&self.observed.node.handle, &app, &call)?;
        let current_window = service.backend.window(&node, &app, &call)?;
        if !service
            .backend
            .same(&node.handle, &self.observed.node.handle)
            || !same_window(
                service.backend.as_ref(),
                &self.observed.window,
                &current_window,
            )
            || node.subrole != self.observed.node.subrole
            || node.description != self.observed.node.description
        {
            return Err(error("STALE_SNAPSHOT"));
        }
        let window = current_window.ok_or_else(|| error("STALE_SNAPSHOT"))?;
        let current = node.element(&app, service.backend.as_ref(), &window, None);
        super::super::ui_state::check_element(&original, &current, &self.request.action)
            .map_err(safety_error)?;
        if !node.label_complete || !self.observed.node.label_complete {
            return Err(error("STALE_SNAPSHOT"));
        }
        if node.enabled == Some(false) {
            return Err(error("ELEMENT_DISABLED"));
        }
        Ok((node, window))
    }

    fn execute(&self, cancel: &CancelToken) -> Result<Value> {
        if !self.permit.load(Ordering::Acquire) {
            return Err(error("ACTION_NOT_APPROVED"));
        }
        if self.used.swap(true, Ordering::AcqRel) {
            return Err(error("STALE_SNAPSHOT"));
        }
        let service = &self.source.service;
        service.run(cancel)?;
        service
            .control
            .ui_authority
            .authorized(&service.control, &self.lease, || Ok(()))?;
        let call = deadline(&self.lease.cancel, Duration::from_secs(2));
        let value = if self.observed.node.secure() {
            None
        } else {
            service.backend.value(&self.observed.node, &call)?
        };
        let (node, window) = self.revalidate()?;
        let current = node.element(&self.app, service.backend.as_ref(), &window, value);
        let mut state = self.run.data.try_lock().map_err(|_| error("NATIVE_BUSY"))?;
        let snapshot = state.get(&self.request.snapshot_id).map_err(safety_error)?;
        if !Arc::ptr_eq(&snapshot.lease, &self.lease) {
            return Err(error("STALE_SNAPSHOT"));
        }
        service
            .control
            .ui_authority
            .authorized(&service.control, &self.lease, || {
                active(&call)?;
                service.recheck_app(&self.app)?;
                state.record_dispatch(&current).map_err(safety_error)?;
                service.backend.dispatch(&node, &self.request.action, &call)
            })?;
        Ok(
            json!({"dispatched":true,"verified":false,"target":{"pid":self.app.pid,"app_id":self.app.identity}}),
        )
    }
}

impl Tool for PreparedAction {
    fn definition(&self) -> ToolDefinition {
        self.source.definition()
    }

    fn invoke(&self, _: &Value, _: &Project, cancel: &CancelToken) -> Result<Value> {
        self.execute(cancel)
    }

    fn error_output(&self, error: &ToolError) -> Value {
        output_error(error)
    }
}
