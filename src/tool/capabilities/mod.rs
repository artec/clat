//! Read-only, transient capability views derived from registry ownership.
mod interface;
mod metadata;
mod query;
pub(crate) mod schema;
#[cfg(test)]
mod tests;
mod view;
use super::*;
pub(crate) use interface::{DiscoveryPlugin, command_spec, is_discovery};
pub use metadata::ToolCapability;
pub(crate) use view::CapabilityView;

pub(super) type IdleView = Option<(String, Arc<CapabilityView>)>;

impl ToolRegistry {
    pub(crate) fn user_capability_view(&self, context: &str) -> Arc<CapabilityView> {
        let revision = self.inner.read().expect("registry").revision;
        let mut cached = self.idle_discovery.write().expect("idle capability slot");
        if let Some((key, view)) = cached.as_ref()
            && key == context
            && view.revision == revision
        {
            return Arc::clone(view);
        }
        let view = self.capability_view(None);
        *cached = Some((context.into(), Arc::clone(&view)));
        view
    }
    /// The same capture supplies both model schemas and discovery visibility.
    pub(crate) fn capability_view(&self, access: Option<&ToolAccessPolicy>) -> Arc<CapabilityView> {
        let state = self.inner.read().expect("tool registry");
        Arc::new(CapabilityView::capture(&state, access))
    }
    pub(crate) fn install_capabilities(&self, view: Arc<CapabilityView>) {
        *self.discovery.write().expect("capability slot") = Some(view);
    }
    pub(crate) fn clear_capabilities(&self) {
        *self.idle_discovery.write().expect("idle capability slot") = None;
        *self.discovery.write().expect("capability slot") = None;
    }
    pub(crate) fn query_capabilities(
        &self,
        request: &Value,
        cancel: &CancelToken,
    ) -> Result<Value, ToolError> {
        let view = self
            .discovery
            .read()
            .map_err(|_| ToolError::new("capability slot unavailable"))?
            .clone()
            .ok_or_else(|| {
                ToolError::new("no active capability snapshot; requery in the current Run")
            })?;
        let result = self.query_capability_view(&view, request, cancel)?;
        if !self
            .discovery
            .read()
            .map_err(|_| ToolError::new("capability slot unavailable"))?
            .as_ref()
            .is_some_and(|current| Arc::ptr_eq(current, &view))
        {
            return Err(ToolError::new("capability Run ended or changed; requery"));
        }
        Ok(result)
    }
    pub(crate) fn query_capability_view(
        &self,
        view: &CapabilityView,
        request: &Value,
        cancel: &CancelToken,
    ) -> Result<Value, ToolError> {
        let revision = self
            .inner
            .read()
            .map_err(|_| ToolError::new("tool registry unavailable"))?
            .revision;
        if revision != view.revision {
            return Err(ToolError::new(
                "capability view expired; query a new snapshot",
            ));
        }
        let value = view.query(request, cancel)?;
        if self
            .inner
            .read()
            .map_err(|_| ToolError::new("tool registry unavailable"))?
            .revision
            != revision
        {
            return Err(ToolError::new(
                "capability view expired during query; requery",
            ));
        }
        Ok(value)
    }
}

pub(crate) fn bounded_label(value: &str) -> String {
    query::clip(value, 128)
}
