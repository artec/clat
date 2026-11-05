//! Project-owned tool policy publication and authority revision.
use super::ToolAccessPolicy;
use std::sync::{Arc, RwLock};

/// Project-owned slot read by permission policies and the PluginHost bridge.
/// Application writes one immutable snapshot at run start and resets it after
/// teardown. The slot is not a second registry and cannot add authority.
#[derive(Default)]
pub(crate) struct ToolAccessSlot {
    current: RwLock<ToolAccessPolicy>,
    revision: std::sync::atomic::AtomicU64,
}

impl ToolAccessSlot {
    pub(crate) fn shared() -> Arc<Self> {
        Arc::new(Self::default())
    }

    pub(crate) fn install(&self, policy: ToolAccessPolicy) {
        if let Ok(mut current) = self.current.write() {
            self.revision
                .fetch_add(1, std::sync::atomic::Ordering::AcqRel);
            *current = policy;
        }
    }

    pub(crate) fn clear(&self) {
        self.install(ToolAccessPolicy::all());
    }

    pub(crate) fn revision(&self) -> u64 {
        self.revision.load(std::sync::atomic::Ordering::Acquire)
    }

    pub(crate) fn snapshot(&self) -> ToolAccessPolicy {
        self.current
            .read()
            .map(|value| value.clone())
            .unwrap_or_default()
    }
}

/// Keep the always available segment stable when optional run tools appear.
pub(super) fn sort_model_definitions(definitions: &mut [super::ToolDefinition]) {
    let contextual = |name: &str| {
        super::is_task_tool(name)
            || matches!(
                name,
                "view_image"
                    | "delegate_readonly"
                    | "request_plan_mode"
                    | "exit_plan_mode"
                    | "propose_goal"
            )
    };
    definitions.sort_unstable_by(|a, b| {
        (contextual(&a.name), &a.name).cmp(&(contextual(&b.name), &b.name))
    });
}
