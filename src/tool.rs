use crate::model::CancelToken;
use crate::plugin::{PluginId, PluginOwner};
use crate::project::Project;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::fmt;
use std::sync::{Arc, RwLock, Weak};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ToolEffect {
    Pure,
    Read,
    Write,
    Execute,
    Network,
    /// Read-only work performed by an untrusted external process. Unlike
    /// native [`Read`](Self::Read), this still crosses a permission boundary.
    ExternalRead,
    /// An external operation advertised as destructive (delete, overwrite,
    /// revoke, and similar irreversible effects).
    Destructive,
    /// Mutates CLAT-local session metadata only (e.g. the todo list): no
    /// project files, no processes, no network. Safe to auto-allow because
    /// the effect is confined to the current conversation's own state.
    SessionWrite,
}

impl fmt::Display for ToolEffect {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Pure => f.write_str("pure"),
            Self::Read => f.write_str("read-only"),
            Self::Write => f.write_str("writes files"),
            Self::Execute => f.write_str("runs commands"),
            Self::Network => f.write_str("network access"),
            Self::ExternalRead => f.write_str("external read access"),
            Self::Destructive => f.write_str("destructive external action"),
            Self::SessionWrite => f.write_str("updates this session's local state"),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ToolDefinition {
    pub name: String,
    pub description: String,
    pub input_schema: Value,
    pub effect: ToolEffect,
    pub strict: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub arguments: Value,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ToolResult {
    pub call_id: String,
    pub tool_name: String,
    pub output: Value,
    pub is_error: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ToolError {
    message: String,
}

impl ToolError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl fmt::Display for ToolError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for ToolError {}

pub trait Tool: Send + Sync {
    fn definition(&self) -> ToolDefinition;

    fn invoke(
        &self,
        arguments: &Value,
        project: &Project,
        cancel: &CancelToken,
    ) -> Result<Value, ToolError>;
}

/// Immutable, already-authorized tool invocation passed to core middleware.
/// Middleware cannot replace the tool or mutate arguments after permission
/// classification.
pub(crate) struct ToolInvocation<'a> {
    pub tool: &'a dyn Tool,
    pub arguments: &'a Value,
    pub project: &'a Project,
    pub cancel: &'a CancelToken,
}

pub(crate) trait ToolNext: Send + Sync {
    fn execute(&self, invocation: &ToolInvocation<'_>) -> Result<Value, ToolError>;
}

pub(crate) trait ToolMiddleware: Send + Sync {
    fn execute(
        &self,
        invocation: &ToolInvocation<'_>,
        next: &dyn ToolNext,
    ) -> Result<Value, ToolError>;
}

pub(crate) trait ToolObserver: Send + Sync {
    fn finished(&self, invocation: &ToolInvocation<'_>, result: &Result<Value, ToolError>);
}

/// Post-result seam: runs after the Run has constructed the final
/// [`ToolResult`] (success, tool error, or permission denial) and before the
/// item is persisted or `ToolFinished` is emitted. Unlike execute middleware
/// it sees `is_error` results too; unlike permission checks it runs strictly
/// after the decision, so it can never widen what a denied call leaked.
pub(crate) trait ToolResultTransformer: Send + Sync {
    fn transform_result(&self, result: &mut ToolResult);
}

#[derive(Default)]
struct ToolExecutionState {
    middleware: Vec<(u64, PluginId, Arc<dyn ToolMiddleware>)>,
    observers: Vec<(u64, PluginId, Arc<dyn ToolObserver>)>,
    result_transformers: Vec<(u64, PluginId, Arc<dyn ToolResultTransformer>)>,
    next_contribution: u64,
    frozen: bool,
}

/// Ordered around-execute middleware and post observers. Contributions are
/// structurally revocable by their plugin scope and frozen before a Run starts.
#[derive(Clone, Default)]
pub(crate) struct ToolExecutionPipeline {
    inner: Arc<RwLock<ToolExecutionState>>,
}

impl ToolExecutionPipeline {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    pub(crate) fn register_middleware(
        &self,
        owner: PluginOwner,
        middleware: Arc<dyn ToolMiddleware>,
    ) -> Result<ToolPipelineLease, ToolRegistryError> {
        let mut state = self
            .inner
            .write()
            .map_err(|_| ToolRegistryError::Poisoned)?;
        if state.frozen {
            return Err(ToolRegistryError::Frozen);
        }
        let owner = owner.id();
        let contribution = state.next_contribution;
        state.next_contribution = state.next_contribution.wrapping_add(1);
        state.middleware.push((contribution, owner, middleware));
        Ok(ToolPipelineLease {
            pipeline: Arc::downgrade(&self.inner),
            contribution,
            kind: PipelineContribution::Middleware,
        })
    }

    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "post observer registration is exercised by extension catalogs"
        )
    )]
    pub(crate) fn register_observer(
        &self,
        owner: PluginOwner,
        observer: Arc<dyn ToolObserver>,
    ) -> Result<ToolPipelineLease, ToolRegistryError> {
        let mut state = self
            .inner
            .write()
            .map_err(|_| ToolRegistryError::Poisoned)?;
        if state.frozen {
            return Err(ToolRegistryError::Frozen);
        }
        let owner = owner.id();
        let contribution = state.next_contribution;
        state.next_contribution = state.next_contribution.wrapping_add(1);
        state.observers.push((contribution, owner, observer));
        Ok(ToolPipelineLease {
            pipeline: Arc::downgrade(&self.inner),
            contribution,
            kind: PipelineContribution::Observer,
        })
    }

    pub(crate) fn freeze(&self) -> Result<(), ToolRegistryError> {
        self.inner
            .write()
            .map_err(|_| ToolRegistryError::Poisoned)?
            .frozen = true;
        Ok(())
    }

    /// Registers a post-result transformer. Contribution order is stable and
    /// revocable like every other pipeline contribution.
    pub(crate) fn register_result_transformer(
        &self,
        owner: PluginOwner,
        transformer: Arc<dyn ToolResultTransformer>,
    ) -> Result<ToolPipelineLease, ToolRegistryError> {
        let mut state = self
            .inner
            .write()
            .map_err(|_| ToolRegistryError::Poisoned)?;
        if state.frozen {
            return Err(ToolRegistryError::Frozen);
        }
        let owner = owner.id();
        let contribution = state.next_contribution;
        state.next_contribution = state.next_contribution.wrapping_add(1);
        state
            .result_transformers
            .push((contribution, owner, transformer));
        Ok(ToolPipelineLease {
            pipeline: Arc::downgrade(&self.inner),
            contribution,
            kind: PipelineContribution::ResultTransformer,
        })
    }

    /// Applies registered transformers in registration order to the final
    /// `ToolResult` before it is persisted or reported.
    pub(crate) fn transform_result(&self, result: &mut ToolResult) {
        let transformers = {
            let Ok(state) = self.inner.read() else {
                return;
            };
            state
                .result_transformers
                .iter()
                .map(|(_, _, item)| Arc::clone(item))
                .collect::<Vec<_>>()
        };
        for transformer in transformers {
            transformer.transform_result(result);
        }
    }

    pub(crate) fn execute(&self, invocation: &ToolInvocation<'_>) -> Result<Value, ToolError> {
        let (middleware, observers) = {
            let state = self
                .inner
                .read()
                .map_err(|_| ToolError::new("tool pipeline lock poisoned"))?;
            (
                state
                    .middleware
                    .iter()
                    .map(|(_, _, item)| Arc::clone(item))
                    .collect::<Vec<_>>(),
                state
                    .observers
                    .iter()
                    .map(|(_, _, item)| Arc::clone(item))
                    .collect::<Vec<_>>(),
            )
        };
        let terminal = TerminalToolExecutor;
        let chain = MiddlewareCursor {
            middleware: &middleware,
            index: 0,
            terminal: &terminal,
        };
        let result = chain.execute(invocation);
        for observer in observers {
            observer.finished(invocation, &result);
        }
        result
    }
}

struct TerminalToolExecutor;

impl ToolNext for TerminalToolExecutor {
    fn execute(&self, invocation: &ToolInvocation<'_>) -> Result<Value, ToolError> {
        invocation
            .tool
            .invoke(invocation.arguments, invocation.project, invocation.cancel)
    }
}

struct MiddlewareCursor<'a> {
    middleware: &'a [Arc<dyn ToolMiddleware>],
    index: usize,
    terminal: &'a dyn ToolNext,
}

impl ToolNext for MiddlewareCursor<'_> {
    fn execute(&self, invocation: &ToolInvocation<'_>) -> Result<Value, ToolError> {
        let Some(current) = self.middleware.get(self.index) else {
            return self.terminal.execute(invocation);
        };
        let next = MiddlewareCursor {
            middleware: self.middleware,
            index: self.index + 1,
            terminal: self.terminal,
        };
        current.execute(invocation, &next)
    }
}

enum PipelineContribution {
    Middleware,
    #[cfg_attr(
        not(test),
        expect(dead_code, reason = "reserved for registered post observers")
    )]
    Observer,
    ResultTransformer,
}

pub(crate) struct ToolPipelineLease {
    pipeline: Weak<RwLock<ToolExecutionState>>,
    contribution: u64,
    kind: PipelineContribution,
}

impl ToolPipelineLease {
    pub(crate) fn revoke(self) -> Result<(), ToolRegistryError> {
        let Some(pipeline) = self.pipeline.upgrade() else {
            return Ok(());
        };
        let mut state = pipeline.write().map_err(|_| ToolRegistryError::Poisoned)?;
        match self.kind {
            PipelineContribution::Middleware => {
                state
                    .middleware
                    .retain(|(contribution, _, _)| *contribution != self.contribution);
            }
            PipelineContribution::Observer => {
                state
                    .observers
                    .retain(|(contribution, _, _)| *contribution != self.contribution);
            }
            PipelineContribution::ResultTransformer => {
                state
                    .result_transformers
                    .retain(|(contribution, _, _)| *contribution != self.contribution);
            }
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum ToolRegistryError {
    Duplicate {
        name: String,
        existing_owner: PluginId,
        attempted_owner: PluginId,
    },
    Frozen,
    Poisoned,
}

impl fmt::Display for ToolRegistryError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Duplicate {
                name,
                existing_owner,
                attempted_owner,
            } => write!(
                formatter,
                "tool `{name}` is already registered by `{existing_owner}`; `{attempted_owner}` cannot replace it"
            ),
            Self::Frozen => formatter.write_str("tool registry is frozen"),
            Self::Poisoned => formatter.write_str("tool registry lock poisoned"),
        }
    }
}

impl std::error::Error for ToolRegistryError {}

struct ToolEntry {
    owner: PluginId,
    tool: Arc<dyn Tool>,
}

#[derive(Default)]
struct ToolRegistryState {
    by_name: HashMap<String, ToolEntry>,
    order: Vec<String>,
    frozen: bool,
}

#[derive(Clone, Default)]
pub(crate) struct ToolRegistry {
    inner: Arc<RwLock<ToolRegistryState>>,
}

impl ToolRegistry {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    pub(crate) fn register(
        &self,
        owner: PluginOwner,
        tool: Arc<dyn Tool>,
    ) -> Result<ToolLease, ToolRegistryError> {
        let owner = owner.id();
        let definition = tool.definition();
        let mut state = self
            .inner
            .write()
            .map_err(|_| ToolRegistryError::Poisoned)?;
        if state.frozen {
            return Err(ToolRegistryError::Frozen);
        }
        if let Some(existing) = state.by_name.get(&definition.name) {
            return Err(ToolRegistryError::Duplicate {
                name: definition.name,
                existing_owner: existing.owner,
                attempted_owner: owner,
            });
        }
        state.order.push(definition.name.clone());
        state
            .by_name
            .insert(definition.name.clone(), ToolEntry { owner, tool });
        Ok(ToolLease {
            registry: Arc::downgrade(&self.inner),
            name: definition.name,
            owner,
            active: true,
        })
    }

    pub(crate) fn freeze(&self) -> Result<(), ToolRegistryError> {
        self.inner
            .write()
            .map_err(|_| ToolRegistryError::Poisoned)?
            .frozen = true;
        Ok(())
    }

    pub(crate) fn definitions(&self) -> Vec<ToolDefinition> {
        let Ok(state) = self.inner.read() else {
            return Vec::new();
        };
        state
            .order
            .iter()
            .filter_map(|name| state.by_name.get(name))
            .map(|entry| entry.tool.definition())
            .collect()
    }

    pub(crate) fn get(&self, name: &str) -> Option<Arc<dyn Tool>> {
        self.inner
            .read()
            .ok()?
            .by_name
            .get(name)
            .map(|entry| Arc::clone(&entry.tool))
    }

    #[cfg(test)]
    pub(crate) fn is_empty(&self) -> bool {
        self.inner
            .read()
            .map(|state| state.by_name.is_empty())
            .unwrap_or(true)
    }
}

pub(crate) struct ToolLease {
    registry: Weak<RwLock<ToolRegistryState>>,
    name: String,
    owner: PluginId,
    active: bool,
}

impl ToolLease {
    pub(crate) fn revoke(mut self) -> Result<(), ToolRegistryError> {
        self.revoke_inner()
    }

    fn revoke_inner(&mut self) -> Result<(), ToolRegistryError> {
        if !self.active {
            return Ok(());
        }
        self.active = false;
        let Some(registry) = self.registry.upgrade() else {
            return Ok(());
        };
        let mut state = registry.write().map_err(|_| ToolRegistryError::Poisoned)?;
        if state
            .by_name
            .get(&self.name)
            .is_some_and(|entry| entry.owner == self.owner)
        {
            state.by_name.remove(&self.name);
            state.order.retain(|name| name != &self.name);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    struct NamedTool(&'static str);

    impl Tool for NamedTool {
        fn definition(&self) -> ToolDefinition {
            ToolDefinition {
                name: self.0.into(),
                description: String::new(),
                input_schema: json!({}),
                effect: ToolEffect::Pure,
                strict: true,
            }
        }

        fn invoke(
            &self,
            _arguments: &Value,
            _project: &Project,
            _cancel: &CancelToken,
        ) -> Result<Value, ToolError> {
            Ok(Value::Null)
        }
    }

    #[test]
    fn registry_rejects_duplicates_preserves_order_and_revokes_by_lease() {
        let registry = ToolRegistry::new();
        let first_owner = PluginId::new("first");
        let second_owner = PluginId::new("second");
        let first = registry
            .register(PluginOwner::for_test(first_owner), Arc::new(NamedTool("z")))
            .expect("first");
        let second = registry
            .register(PluginOwner::for_test(first_owner), Arc::new(NamedTool("a")))
            .expect("second");
        assert_eq!(
            registry
                .definitions()
                .into_iter()
                .map(|definition| definition.name)
                .collect::<Vec<_>>(),
            ["z", "a"]
        );
        assert!(matches!(
            registry.register(
                PluginOwner::for_test(second_owner),
                Arc::new(NamedTool("z"))
            ),
            Err(ToolRegistryError::Duplicate { .. })
        ));
        first.revoke().expect("revoke");
        assert!(registry.get("z").is_none());
        assert!(registry.get("a").is_some());
        second.revoke().expect("revoke");
        assert!(registry.is_empty());
    }

    #[test]
    fn frozen_registry_rejects_new_contributions_but_leases_still_revoke() {
        let registry = ToolRegistry::new();
        let lease = registry
            .register(
                PluginOwner::for_test(PluginId::new("owner")),
                Arc::new(NamedTool("tool")),
            )
            .unwrap();
        registry.freeze().unwrap();
        assert!(matches!(
            registry.register(
                PluginOwner::for_test(PluginId::new("later")),
                Arc::new(NamedTool("later"))
            ),
            Err(ToolRegistryError::Frozen)
        ));
        lease.revoke().unwrap();
        assert!(registry.is_empty());
    }
}
