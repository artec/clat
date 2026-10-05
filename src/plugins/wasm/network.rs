//! Version-two network components; the legacy linker stays closed.
use super::*;
use crate::plugin_host::network::{
    NetworkAction, NetworkApproval, NetworkError, NetworkLease, NetworkRequest,
};
use clat_wasm_net::{
    dns_authority::Run,
    runtime::{self, Authority, Failure, Invocation, Pending, Runtime},
};
mod mounting;
pub(super) use mounting::try_mount;
impl From<NetworkError> for Failure {
    fn from(value: NetworkError) -> Self {
        match value {
            NetworkError::Unavailable => Self::Unavailable,
            NetworkError::InvalidRequest => Self::InvalidRequest,
            NetworkError::Cancelled => Self::Cancelled,
            NetworkError::Deadline => Self::Deadline,
            NetworkError::Denied => Self::Denied,
            NetworkError::Busy => Self::Busy,
        }
    }
}
impl Pending for NetworkApproval {
    fn get(&self) -> Option<Result<(), Failure>> {
        self.get().map(|r| r.map_err(Into::into))
    }
}
struct LeaseAuthority {
    lease: NetworkLease,
    alive: Arc<AtomicBool>,
}
impl Authority for LeaseAuthority {
    fn check(&self) -> Result<(), Failure> {
        if !self.alive.load(AtomicOrdering::Acquire) {
            return Err(Failure::Cancelled);
        }
        self.lease.check().map_err(Into::into)
    }
    fn start(
        &self,
        origin: &clat_wasm_net::dns_authority::Origin,
        method: Option<&str>,
        deadline: Instant,
    ) -> Result<Box<dyn Pending>, Failure> {
        self.check()?;
        let request = NetworkRequest::new(
            if method.is_some() {
                NetworkAction::Http
            } else {
                NetworkAction::Resolve
            },
            origin.scheme(),
            origin.host(),
            origin.port(),
            method,
        )?;
        self.lease
            .restrict_deadline(deadline)?
            .start_approval(request)
            .map(|p| Box::new(p) as Box<dyn Pending>)
            .map_err(Into::into)
    }
}
struct Discovery;
impl Authority for Discovery {
    fn check(&self) -> Result<(), Failure> {
        Ok(())
    }
    fn start(
        &self,
        _: &clat_wasm_net::dns_authority::Origin,
        _: Option<&str>,
        _: Instant,
    ) -> Result<Box<dyn Pending>, Failure> {
        Err(Failure::Denied)
    }
}
struct Instance {
    runtime: Mutex<Runtime>,
    bridge: Arc<PluginHostBridge>,
    name: String,
    declaration: Vec<u8>,
    narrowing: Option<Vec<u8>>,
    config: Option<String>,
    run: Mutex<Option<(u64, Run)>>,
    alive: Arc<AtomicBool>,
}
impl Instance {
    fn invoke(
        &self,
        name: &str,
        arguments: &Value,
        cancel: &CancelToken,
    ) -> Result<Value, ToolError> {
        let deadline = Instant::now()
            + cancel
                .remaining()
                .unwrap_or(Duration::from_secs(120))
                .min(Duration::from_secs(120));
        let lease = self
            .bridge
            .network_lease(PluginSource::Wasm(self.name.clone()), deadline)
            .map_err(|_| ToolError::new("network plugin has no active authorized run"))?;
        let epoch = lease.run_identity();
        let mut state = self
            .run
            .lock()
            .map_err(|_| ToolError::new("network budget unavailable"))?;
        let run = active_run_budget(&mut state, epoch, deadline)?;
        let observed = cancel.clone();
        let invocation = Invocation::new(
            run,
            &self.declaration,
            self.narrowing.as_deref(),
            Arc::new(LeaseAuthority {
                lease,
                alive: self.alive.clone(),
            }),
            clat_wasm_net::CancelToken::from_check(move || observed.is_cancelled()),
            deadline,
            self.config.clone(),
        )
        .map_err(ToolError::new)?;
        #[cfg(all(test, feature = "test-support"))]
        let invocation = if std::env::var_os("CLAT_PLG4_NUMERIC_FIXTURE").is_some() {
            invocation.numeric_fixture()
        } else {
            invocation
        };
        let arguments = serde_json::to_string(arguments)
            .map_err(|_| ToolError::new("invalid plugin arguments"))?;
        let text = self
            .runtime
            .lock()
            .map_err(|_| ToolError::new("network component unavailable"))?
            .call(invocation, name, &arguments)
            .map_err(ToolError::new)?;
        Ok(serde_json::from_str(&text).unwrap_or(Value::String(text)))
    }
}
fn active_run_budget(
    state: &mut Option<(u64, Run)>,
    epoch: u64,
    deadline: Instant,
) -> Result<&Run, ToolError> {
    if state
        .as_ref()
        .is_none_or(|(previous, _)| *previous != epoch)
    {
        *state = Some((
            epoch,
            Run::for_active_run(deadline)
                .map_err(|_| ToolError::new("network run deadline exceeded"))?,
        ));
    }
    let run = &state.as_ref().expect("initialized run budget").1;
    run.admit_tool_window(deadline)
        .map_err(|_| ToolError::new("network run unavailable"))?;
    Ok(run)
}
struct NetworkTool {
    remote_name: String,
    definition: ToolDefinition,
    instance: Arc<Instance>,
}
impl Tool for NetworkTool {
    fn definition(&self) -> ToolDefinition {
        self.definition.clone()
    }
    fn invoke(&self, args: &Value, _: &Project, cancel: &CancelToken) -> Result<Value, ToolError> {
        self.instance.invoke(&self.remote_name, args, cancel)
    }
}
impl WitToolDef for runtime::Definition {
    fn wit_name(&self) -> String {
        self.name.clone()
    }
    fn wit_description(&self) -> String {
        self.description.clone()
    }
    fn wit_input_schema(&self) -> String {
        self.input_schema.clone()
    }
    fn wit_effect(&self) -> ToolEffect {
        match self.effect {
            runtime::Effect::Pure => ToolEffect::Pure,
            runtime::Effect::Read => ToolEffect::Read,
            runtime::Effect::ExternalRead => ToolEffect::ExternalRead,
            runtime::Effect::SessionWrite => ToolEffect::SessionWrite,
            runtime::Effect::Write => ToolEffect::Write,
            runtime::Effect::Execute => ToolEffect::Execute,
            runtime::Effect::Network => ToolEffect::Network,
            runtime::Effect::Destructive => ToolEffect::Destructive,
        }
    }
}

#[cfg(test)]
mod tests;
