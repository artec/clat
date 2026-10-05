//! Trusted embedding interface. The guest never supplies policy or authority.
use crate::http_authority::permission::Gate;
pub use crate::http_authority::permission::{Authority, Failure, Pending};
use crate::network_resources::component::{
    HostState, invocation::Lane, plugin_interface::ToolsConsumer,
};
use crate::{
    CancelToken,
    capabilities::Policy,
    dns_authority::{Run, SystemDns},
};
use std::{sync::Arc, time::Instant};

pub struct Runtime(Lane);
pub struct Invocation(HostState);
pub struct Definition {
    pub name: String,
    pub description: String,
    pub input_schema: String,
    pub effect: Effect,
}
pub enum Effect {
    Pure,
    Read,
    ExternalRead,
    SessionWrite,
    Write,
    Execute,
    Network,
    Destructive,
}
impl Invocation {
    pub fn new(
        run: &Run,
        declaration: &[u8],
        narrowing: Option<&[u8]>,
        authority: Arc<dyn Authority>,
        cancel: CancelToken,
        deadline: Instant,
        config: Option<String>,
    ) -> Result<Self, String> {
        let policy = Policy::parse(declaration, narrowing).map_err(|_| "invalid network policy")?;
        let dns = SystemDns::shared().map_err(|_| "DNS service unavailable")?;
        let mut host = HostState::from_policy(
            run,
            Arc::new(Gate::new(authority)),
            cancel,
            deadline,
            dns,
            policy,
        )
        .map_err(|_| "network invocation unavailable")?;
        host.config = config;
        Ok(Self(host))
    }
}
impl Runtime {
    pub fn new(bytes: &[u8]) -> Result<Self, String> {
        if !bytes.starts_with(b"\0asm") {
            return Err("expected a binary WASM component".into());
        }
        Lane::new(bytes)
            .map(Self)
            .map_err(|_| "invalid network component".into())
    }
    /// Reuse authenticated code while keeping each invocation's Store fresh.
    pub fn new_cached(bytes: &[u8], storage: &std::path::Path) -> Result<Self, String> {
        if !bytes.starts_with(b"\0asm") {
            return Err("expected a binary WASM component".into());
        }
        Lane::new_cached(bytes, Some(storage))
            .map(Self)
            .map_err(|_| "invalid network component".into())
    }
    pub fn list(&mut self, invocation: Invocation) -> Result<Vec<Definition>, String> {
        self.0
            .invoke(invocation.0, |store, component, linker| {
                let plugin = ToolsConsumer::instantiate(&mut *store, component, linker)?;
                let tools = plugin.clat_plugin_tools().call_list_tools(&mut *store)?;
                Ok(tools
                    .into_iter()
                    .map(|tool| Definition {
                        name: tool.name,
                        description: tool.description,
                        input_schema: tool.input_schema,
                        effect: effect(tool.effect),
                    })
                    .collect())
            })
            .map_err(|_| "network component discovery failed".into())
    }
    pub fn call(
        &mut self,
        invocation: Invocation,
        name: &str,
        arguments: &str,
    ) -> Result<String, String> {
        self.0
            .invoke(invocation.0, |store, component, linker| {
                let plugin = ToolsConsumer::instantiate(&mut *store, component, linker)?;
                plugin
                    .clat_plugin_tools()
                    .call_call(&mut *store, name, arguments)
            })
            .map_err(|_| "network component execution failed".to_owned())?
            // Guest error messages can contain arbitrary configuration secrets.
            .map_err(|_| "network plugin tool failed".to_owned())
    }
}
fn effect(
    value: crate::network_resources::component::plugin_interface::exports::clat::plugin::tools::Effect,
) -> Effect {
    use crate::network_resources::component::plugin_interface::exports::clat::plugin::tools::Effect as W;
    match value {
        W::Pure => Effect::Pure,
        W::ExternalRead => Effect::ExternalRead,
        W::SessionWrite => Effect::SessionWrite,
        W::Read => Effect::Read,
        W::Write => Effect::Write,
        W::Execute => Effect::Execute,
        W::Network => Effect::Network,
        W::Destructive => Effect::Destructive,
    }
}
pub fn validate_policy(declaration: &[u8], narrowing: Option<&[u8]>) -> Result<(), String> {
    Policy::parse(declaration, narrowing)
        .map(|_| ())
        .map_err(|_| "invalid network capability declaration".into())
}

#[cfg(feature = "test-support")]
impl Invocation {
    /// Numeric test route, unavailable in ordinary production builds. DNS still
    /// crosses the actual capability fence and injected permission authority.
    pub fn numeric_fixture(mut self) -> Self {
        self.0.dns =
            SystemDns::fake(|_| Ok(vec!["8.8.8.8".parse().expect("fixed public fixture IP")]));
        self.0.numeric_fixture = true;
        self
    }
}
