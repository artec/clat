//! Provider balance/quota monitor plugin: bounded background refresh +
//! `ApplicationEvent` broadcasts.

use super::services::{MONITOR_SERVICE, MONITOR_SERVICE_ID, MonitorService};
use crate::application::ApplicationEvent;
use crate::model::{ModelConfig, ProviderCredentials};
use crate::plugin::{
    DisposeError, Plugin, PluginContext, PluginDescriptor, PluginError, PluginId, ScopeKind,
    ServiceId,
};
use crate::providers::{fetch_deepseek_balance, fetch_glm_quota, fetch_kimi_quota};
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
    latest: MonitorSnapshot,
    commands: Sender<Command>,
    handle: Mutex<Option<JoinHandle<()>>>,
}

type MonitorSnapshot = Arc<Mutex<Option<(ModelConfig, ProviderCredentials, Option<String>)>>>;

impl CoreMonitor {
    fn start() -> Result<Self, PluginError> {
        let (commands, receiver) = mpsc::channel();
        let latest = Arc::new(Mutex::new(None));
        let worker_latest = Arc::clone(&latest);
        let handle = std::thread::Builder::new()
            .name("clat-provider-monitor".into())
            .spawn(move || monitor_loop(receiver, worker_latest))
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
    fn snapshot(&self, config: &ModelConfig, credentials: &ProviderCredentials) -> Option<String> {
        self.latest
            .lock()
            .expect("monitor snapshot")
            .as_ref()
            .filter(|(previous, keys, _)| {
                previous.endpoint == config.endpoint && keys == credentials
            })
            .and_then(|(_, _, value)| value.clone())
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
}

fn monitor_loop(receiver: Receiver<Command>, latest: MonitorSnapshot) {
    let mut state: Option<(ModelConfig, ProviderCredentials)> = None;
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
            Ok(Command::Refresh) => true,
            Ok(Command::Stop) | Err(mpsc::RecvTimeoutError::Disconnected) => break,
            Err(mpsc::RecvTimeoutError::Timeout) => true,
        };
        if !refresh {
            continue;
        }
        next_sweep = Instant::now() + REFRESH_INTERVAL;
        let value = state.as_ref().and_then(fetch_value);
        *latest.lock().expect("monitor snapshot") = state
            .as_ref()
            .map(|(config, credentials)| (config.clone(), credentials.clone(), value.clone()));
        subscribers.retain(|sender| {
            sender
                .send(ApplicationEvent::MonitorUpdated(value.clone()))
                .is_ok()
        });
    }
}

/// Configure 是否触发立即查询：仅端点或凭据变化才查。额度查询只由
/// 端点与 API key 决定（见 `fetch_value`），模型名、思考档位等其余
/// 配置变化不值得多打一次厂商接口——Shift+Tab 切档与编辑器重复
/// 保存都汇入同一条 Configure 路径，连按不应连打额度 API。
fn configure_triggers_fetch(
    previous: Option<&(ModelConfig, ProviderCredentials)>,
    incoming: &(ModelConfig, ProviderCredentials),
) -> bool {
    match previous {
        None => true,
        Some((config, credentials)) => {
            config.endpoint != incoming.0.endpoint || *credentials != incoming.1
        }
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
    } else if endpoint.contains("kimi.com/coding") {
        // 额度接口与模型接口同源 UA 白名单：带上预设注入的 UA（用户
        // 在 Extra Headers 覆盖过则以用户为准）。Qwen Token Plan 无
        // 公开余额 API（官方只提供控制台"我的订阅"页），不在此分支。
        let user_agent = config
            .extra_headers
            .get("User-Agent")
            .and_then(serde_json::Value::as_str);
        fetch_kimi_quota(&config.endpoint, api_key, user_agent)
    } else {
        None
    }
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
        *monitor.latest.lock().unwrap() =
            Some((config.clone(), credentials.clone(), Some("87%".into())));
        assert_eq!(
            monitor.snapshot(&config, &credentials).as_deref(),
            Some("87%")
        );
        config.model = "another model on the same account".into();
        assert_eq!(
            monitor.snapshot(&config, &credentials).as_deref(),
            Some("87%")
        );
        let mut other_keys = credentials.clone();
        other_keys.set_value(0, "second-key".into());
        assert!(monitor.snapshot(&config, &other_keys).is_none());
        config.endpoint = "https://other.invalid".into();
        assert!(monitor.snapshot(&config, &credentials).is_none());
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
