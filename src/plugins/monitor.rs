//! Provider balance/quota monitor plugin: bounded background refresh +
//! `ApplicationEvent` broadcasts.

use super::services::{MONITOR_SERVICE, MONITOR_SERVICE_ID, MonitorService};
use crate::application::ApplicationEvent;
use crate::model::{ModelConfig, ProviderCredentials};
use crate::plugin::{
    DisposeError, Plugin, PluginContext, PluginDescriptor, PluginError, PluginId, ScopeKind,
    ServiceId,
};
use crate::providers::{ProviderAllowance, fetch_provider_allowance};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

mod goal_allowance;

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
    Configure(Box<MonitorRoute>),
    RefreshAccount(Box<MonitorRoute>),
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
    latest: MonitorSnapshot,
    commands: Sender<Command>,
    handle: Mutex<Option<JoinHandle<()>>>,
}

type MonitorRoute = (ModelConfig, ProviderCredentials);
type MonitorEntry = (MonitorRoute, ProviderAllowance);
type MonitorSnapshot = Arc<Mutex<MonitorCache>>;

#[derive(Default)]
struct MonitorCache {
    current: Option<MonitorEntry>,
    // Keep the most recent utility account separate from the foreground account.
    utility: Option<MonitorEntry>,
}

impl MonitorCache {
    fn allowance(
        &self,
        config: &ModelConfig,
        credentials: &ProviderCredentials,
    ) -> Option<&ProviderAllowance> {
        self.current
            .iter()
            .chain(&self.utility)
            .find(|((old, keys), _)| old.endpoint == config.endpoint && keys == credentials)
            .map(|(_, value)| value)
    }
}

impl CoreMonitor {
    fn start() -> Result<Self, PluginError> {
        Self::start_with(fetch_value)
    }

    fn start_with(fetch: fn(&MonitorRoute) -> ProviderAllowance) -> Result<Self, PluginError> {
        let (commands, receiver) = mpsc::channel();
        let latest = Arc::new(Mutex::new(MonitorCache::default()));
        let worker_latest = Arc::clone(&latest);
        let handle = std::thread::Builder::new()
            .name("clat-provider-monitor".into())
            .spawn(move || monitor_loop(receiver, worker_latest, fetch))
            .map_err(|error| PluginError::new(format!("spawn provider monitor: {error}")))?;
        Ok(Self {
            latest,
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
            // 进程退出语义（2026-08-19，对照 DSH 的 disposed 中止）：
            // Stop 排在在途额度拉取之后才被看到，而那次 HTTP 以
            // MONITOR_HTTP_TIMEOUT（15s）为界——无界 join 会把退出拖
            // 到拉取结束（"exit 有时很慢"的根因之一）。至多等 2s 后
            // 放弃，线程随进程退出回收。
            crate::application::join_with_grace(handle, Duration::from_secs(2), "provider monitor")
                .map_err(DisposeError::new)?;
        }
        Ok(())
    }
}

impl MonitorService for CoreMonitor {
    fn goal_allowance(
        &self,
        config: &ModelConfig,
        credentials: &ProviderCredentials,
        force: bool,
    ) -> ProviderAllowance {
        goal_allowance::read(config, credentials, force)
    }

    fn allowance(
        &self,
        config: &ModelConfig,
        credentials: &ProviderCredentials,
    ) -> ProviderAllowance {
        self.latest
            .lock()
            .expect("monitor snapshot")
            .allowance(config, credentials)
            .cloned()
            .unwrap_or_else(|| ProviderAllowance::state("loading"))
    }

    fn snapshot(&self, config: &ModelConfig, credentials: &ProviderCredentials) -> Option<String> {
        self.latest
            .lock()
            .expect("monitor snapshot")
            .allowance(config, credentials)
            .and_then(ProviderAllowance::legacy)
    }

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
    fn refresh_account(&self, config: &ModelConfig, credentials: &ProviderCredentials) {
        let _ = self.commands.send(Command::RefreshAccount(Box::new((
            config.clone(),
            credentials.clone(),
        ))));
    }
}

fn monitor_loop(
    receiver: Receiver<Command>,
    latest: MonitorSnapshot,
    fetch: fn(&MonitorRoute) -> ProviderAllowance,
) {
    let mut state: Option<MonitorRoute> = None;
    let mut subscribers: Vec<Sender<ApplicationEvent>> = Vec::new();
    let mut next_sweep = Instant::now() + REFRESH_INTERVAL;
    loop {
        let wait = next_sweep.saturating_duration_since(Instant::now());
        let refresh = match receiver.recv_timeout(wait) {
            Ok(Command::Configure(values)) => {
                let refresh = configure_triggers_fetch(state.as_ref(), &values);
                state = Some(*values);
                refresh
            }
            Ok(Command::Subscribe(sender)) => {
                subscribers.push(sender);
                false
            }
            Ok(Command::RefreshAccount(route)) => {
                if !configure_triggers_fetch(state.as_ref(), &route) {
                    true
                } else {
                    let value = fetch(&route);
                    latest.lock().expect("monitor snapshot").utility = Some((*route, value));
                    false
                }
            }
            Ok(Command::Refresh) => true,
            Ok(Command::Stop) | Err(mpsc::RecvTimeoutError::Disconnected) => break,
            Err(mpsc::RecvTimeoutError::Timeout) => true,
        };
        if !refresh {
            continue;
        }
        next_sweep = Instant::now() + REFRESH_INTERVAL;
        let value = state.as_ref().map(fetch).unwrap_or_default();
        latest.lock().expect("monitor snapshot").current =
            state.as_ref().map(|route| (route.clone(), value.clone()));
        subscribers.retain(|sender| {
            sender
                .send(ApplicationEvent::MonitorUpdated(value.legacy()))
                .is_ok()
        });
    }
}

/// Configure 是否触发立即查询：仅端点或凭据变化才查。额度查询只由
/// 端点与 API key 决定（见 `fetch_value`），模型名、思考档位等其余
/// 配置变化不值得多打一次厂商接口——Shift+Tab 切档与编辑器重复
/// 保存都汇入同一条 Configure 路径，连按不应连打额度 API。
fn configure_triggers_fetch(previous: Option<&MonitorRoute>, incoming: &MonitorRoute) -> bool {
    match previous {
        None => true,
        Some((config, credentials)) => {
            config.endpoint != incoming.0.endpoint || *credentials != incoming.1
        }
    }
}

fn fetch_value((config, credentials): &MonitorRoute) -> ProviderAllowance {
    let Some(key) = credentials.value(0).filter(|key| !key.trim().is_empty()) else {
        return ProviderAllowance::state("unavailable");
    };
    fetch_provider_allowance(config, key.trim())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cached_monitor_snapshot_is_read_only_and_bound_to_endpoint_and_credentials() {
        let monitor = CoreMonitor::start().unwrap();
        let mut config = ModelConfig::default();
        let mut credentials = ProviderCredentials::for_protocol(config.protocol);
        credentials.set_value(0, "first-key".into());
        monitor.latest.lock().unwrap().current = Some((
            (config.clone(), credentials.clone()),
            ProviderAllowance {
                status: "available".into(),
                updated_at: Some(1),
                auth: None,
                items: vec![crate::providers::AllowanceItem {
                    kind: "quota".into(),
                    value: "87".into(),
                    unit: Some("%".into()),
                    ..Default::default()
                }],
            },
        ));
        assert_eq!(
            monitor.snapshot(&config, &credentials).as_deref(),
            Some("87%")
        );
        assert_eq!(
            monitor.allowance(&config, &credentials).items[0].value,
            "87"
        );
        config.model = "another model on the same account".into();
        assert_eq!(
            monitor.snapshot(&config, &credentials).as_deref(),
            Some("87%")
        );
        let mut other_keys = credentials.clone();
        other_keys.set_value(0, "second-key".into());
        assert!(monitor.snapshot(&config, &other_keys).is_none());
        assert!(monitor.allowance(&config, &other_keys).items.is_empty());
        config.endpoint = "https://other.invalid".into();
        assert!(monitor.snapshot(&config, &credentials).is_none());
        assert!(monitor.allowance(&config, &credentials).items.is_empty());
        monitor.shutdown().unwrap();
    }

    #[test]
    fn refresh_account_preserves_foreground_allowance_and_uses_utility_credentials() {
        let monitor = CoreMonitor::start_with(|(_, credentials)| ProviderAllowance {
            status: "available".into(),
            items: vec![crate::providers::AllowanceItem {
                value: credentials.value(0).unwrap_or_default().into(),
                ..Default::default()
            }],
            ..Default::default()
        })
        .unwrap();
        let config = ModelConfig::default();
        let mut selected = ProviderCredentials::for_protocol(config.protocol);
        selected.set_value(0, "41".into());
        let mut utility = selected.clone();
        utility.set_value(0, "72".into());
        let (sender, updates) = mpsc::channel();
        monitor.subscribe(sender);
        monitor.configure(config.clone(), selected.clone());
        assert!(matches!(
            updates.recv_timeout(Duration::from_secs(3)).unwrap(),
            ApplicationEvent::MonitorUpdated(Some(value)) if value == "41"
        ));
        monitor.refresh_account(&config, &utility);
        // Both refreshes share the command queue. Only the foreground update is published.
        monitor.refresh_account(&config, &selected);
        assert!(matches!(
            updates.recv_timeout(Duration::from_secs(3)).unwrap(),
            ApplicationEvent::MonitorUpdated(Some(value)) if value == "41"
        ));
        assert_eq!(monitor.snapshot(&config, &utility).as_deref(), Some("72"));
        assert_eq!(monitor.snapshot(&config, &selected).as_deref(), Some("41"));
        monitor.shutdown().unwrap();
    }

    #[test]
    fn shutdown_is_idempotent_and_joins_the_monitor_thread() {
        let monitor = CoreMonitor::start().expect("start monitor");
        monitor.refresh();
        monitor.shutdown().expect("first shutdown");
        monitor.shutdown().expect("second shutdown");
        assert!(monitor.handle.lock().expect("handle").is_none());
    }

    /// F3（对抗式审查）：Configure 仅在端点或凭据变化时触发立即查询。
    /// Shift+Tab 切档、编辑器重复保存（同端点同 key）不得连打额度
    /// API；首次配置、换端点、换 key 必须立即查询。
    #[test]
    fn configure_triggers_fetch_only_when_endpoint_or_credentials_change() {
        fn state(endpoint: &str, key: &str) -> (ModelConfig, ProviderCredentials) {
            let config = ModelConfig {
                endpoint: endpoint.into(),
                ..ModelConfig::default()
            };
            let mut credentials = ProviderCredentials::for_protocol(config.protocol);
            credentials.set_value(0, key.into());
            (config, credentials)
        }

        // 首次配置：立即查询。
        let glm = state("https://open.bigmodel.cn/api/coding/paas/v4", "k1");
        assert!(configure_triggers_fetch(None, &glm));

        // 完全相同：不查（Shift+Tab 连按 / 重复保存）。
        let same = state("https://open.bigmodel.cn/api/coding/paas/v4", "k1");
        assert!(!configure_triggers_fetch(Some(&glm), &same));

        // 模型名或思考档位变化、端点与 key 不变：不查。
        let mut other_model = glm.clone();
        other_model.0.model = "glm-other".into();
        other_model.0.thinking_level = Some(crate::model::ThinkingLevel::Max);
        assert!(!configure_triggers_fetch(Some(&glm), &other_model));

        // 换 key：查（余额归属变化）。
        let rekeyed = state("https://open.bigmodel.cn/api/coding/paas/v4", "k2");
        assert!(configure_triggers_fetch(Some(&glm), &rekeyed));

        // 换端点：查。
        let deepseek = state("https://api.deepseek.com", "k1");
        assert!(configure_triggers_fetch(Some(&glm), &deepseek));
    }
}
