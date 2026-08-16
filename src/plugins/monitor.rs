use super::services::{MONITOR_SERVICE, MONITOR_SERVICE_ID, MonitorService};
use crate::application::ApplicationEvent;
use crate::model::{ModelConfig, ProviderCredentials};
use crate::plugin::{
    DisposeError, Plugin, PluginContext, PluginDescriptor, PluginError, PluginId, ScopeKind,
    ServiceId,
};
use crate::providers::{fetch_deepseek_balance, fetch_glm_quota};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

const ID: PluginId = PluginId::new("builtin.provider_monitor");
const PROVIDES: &[ServiceId] = &[MONITOR_SERVICE_ID];
const DESCRIPTOR: PluginDescriptor = PluginDescriptor {
    id: ID,
    scope: ScopeKind::TrustedProject,
    provides: PROVIDES,
    requires: &[],
    optional: &[],
};
const REFRESH_INTERVAL: Duration = Duration::from_secs(5 * 60);

enum Command {
    Configure(Box<(ModelConfig, ProviderCredentials)>),
    Subscribe(Sender<ApplicationEvent>),
    Refresh,
    Stop,
}

pub(crate) struct MonitorPlugin;

impl Plugin for MonitorPlugin {
    fn descriptor(&self) -> &'static PluginDescriptor {
        &DESCRIPTOR
    }

    fn mount(&self, context: &mut PluginContext<'_>) -> Result<(), PluginError> {
        let service = context.acquire(CoreMonitor::start()?, |service| service.shutdown());
        let port: Arc<dyn MonitorService> = service;
        context
            .provide(MONITOR_SERVICE, port)
            .map_err(|error| PluginError::new(error.to_string()))
    }
}

struct CoreMonitor {
    commands: Sender<Command>,
    handle: Mutex<Option<JoinHandle<()>>>,
}

impl CoreMonitor {
    fn start() -> Result<Self, PluginError> {
        let (commands, receiver) = mpsc::channel();
        let handle = std::thread::Builder::new()
            .name("clat-provider-monitor".into())
            .spawn(move || monitor_loop(receiver))
            .map_err(|error| PluginError::new(format!("spawn provider monitor: {error}")))?;
        Ok(Self {
            commands,
            handle: Mutex::new(Some(handle)),
        })
    }

    fn shutdown(&self) -> Result<(), DisposeError> {
        let _ = self.commands.send(Command::Stop);
        if let Some(handle) = self
            .handle
            .lock()
            .map_err(|_| DisposeError::new("monitor join lock poisoned"))?
            .take()
        {
            handle
                .join()
                .map_err(|_| DisposeError::new("provider monitor thread panicked"))?;
        }
        Ok(())
    }
}

impl MonitorService for CoreMonitor {
    fn configure(&self, config: ModelConfig, credentials: ProviderCredentials) {
        let _ = self
            .commands
            .send(Command::Configure(Box::new((config, credentials))));
    }

    fn subscribe(&self, sender: Sender<ApplicationEvent>) {
        let _ = self.commands.send(Command::Subscribe(sender));
    }

    fn refresh(&self) {
        let _ = self.commands.send(Command::Refresh);
    }
}

fn monitor_loop(receiver: Receiver<Command>) {
    let mut state: Option<(ModelConfig, ProviderCredentials)> = None;
    let mut subscribers: Vec<Sender<ApplicationEvent>> = Vec::new();
    let mut next_sweep = Instant::now() + REFRESH_INTERVAL;
    loop {
        let wait = next_sweep.saturating_duration_since(Instant::now());
        let refresh = match receiver.recv_timeout(wait) {
            Ok(Command::Configure(values)) => {
                state = Some(*values);
                true
            }
            Ok(Command::Subscribe(sender)) => {
                subscribers.push(sender);
                false
            }
            Ok(Command::Refresh) => true,
            Ok(Command::Stop) | Err(mpsc::RecvTimeoutError::Disconnected) => break,
            Err(mpsc::RecvTimeoutError::Timeout) => true,
        };
        if !refresh {
            continue;
        }
        next_sweep = Instant::now() + REFRESH_INTERVAL;
        let value = state.as_ref().and_then(fetch_value);
        subscribers.retain(|sender| {
            sender
                .send(ApplicationEvent::MonitorUpdated(value.clone()))
                .is_ok()
        });
    }
}

fn fetch_value((config, credentials): &(ModelConfig, ProviderCredentials)) -> Option<String> {
    let endpoint = config.endpoint.to_lowercase();
    let api_key = credentials.value(0)?.trim();
    if api_key.is_empty() {
        return None;
    }
    if endpoint.contains("bigmodel.cn") || endpoint.contains("z.ai") {
        fetch_glm_quota(&config.endpoint, api_key)
    } else if endpoint.contains("deepseek.com") {
        fetch_deepseek_balance(&config.endpoint, api_key)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shutdown_is_idempotent_and_joins_the_monitor_thread() {
        let monitor = CoreMonitor::start().expect("start monitor");
        monitor.refresh();
        monitor.shutdown().expect("first shutdown");
        monitor.shutdown().expect("second shutdown");
        assert!(monitor.handle.lock().expect("handle").is_none());
    }
}
