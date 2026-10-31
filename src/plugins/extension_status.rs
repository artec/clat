//! Shared readiness and observations; never starts an external transport.
use super::services::{MCP_STATUS_SERVICE, MCP_STATUS_SERVICE_ID, McpServerStatus};
use crate::plugin::{Plugin, PluginContext, PluginDescriptor, PluginError, PluginId, ScopeKind};
#[cfg(test)]
mod tests;
pub(crate) struct ExtensionStatusPlugin;
const DESCRIPTOR: PluginDescriptor = PluginDescriptor {
    id: PluginId::new("builtin.extension_status"),
    scope: ScopeKind::TrustedProject,
    provides: &[MCP_STATUS_SERVICE_ID],
    requires: &[],
    optional: &[],
};
impl Plugin for ExtensionStatusPlugin {
    fn descriptor(&self) -> &'static PluginDescriptor {
        &DESCRIPTOR
    }
    fn mount(&self, context: &mut PluginContext<'_>) -> Result<(), PluginError> {
        let status = context.acquire(McpStatus::new(0), |status| {
            status.close();
            Ok(())
        });
        context
            .provide(MCP_STATUS_SERVICE, status)
            .map_err(|e| PluginError::new(e.to_string()))
    }
}
pub(crate) struct StartupBatch {
    status: std::sync::Arc<McpStatus>,
    name: &'static str,
}
impl Drop for StartupBatch {
    fn drop(&mut self) {
        self.status.finish_batch(self.name);
    }
}
type NoticeSink = std::sync::Arc<dyn Fn(usize) + Send + Sync>;

pub(crate) struct McpStatus {
    inner: std::sync::Mutex<McpStatusInner>,
    settled: std::sync::Condvar,
    /// A4-1（W1-21）：settle 时失败非空的**一次性**响亮通知出口
    ///（Application 层装入广播闭包；测试/无宿主为 None——静默降级）。
    notice_sink: std::sync::Mutex<Option<NoticeSink>>,
}

struct McpStatusInner {
    observation_revision: u64,
    capability_sources: std::collections::BTreeMap<(String, String), (&'static str, &'static str)>,
    configured: usize,
    connected: usize,
    /// 连接失败的 server 数（connecting 推导用；工具注册失败不算——
    /// server 本身已连上）。
    failed_servers: usize,
    failures: Vec<String>,
    servers: Vec<McpServerStatus>,
    settled: bool,
    closed: bool,
    batches: std::collections::BTreeSet<&'static str>,
    /// A4-1：响亮通知只发一次。
    noticed: bool,
}

/// `/mcp` 与 DTO 的只读快照（connecting 是推导值）。
#[derive(Debug)]
pub(crate) struct McpStatusSnapshot {
    pub configured: usize,
    pub connected: usize,
    pub connecting: usize,
    pub failures: Vec<String>,
    pub servers: Vec<McpServerStatus>,
}

impl McpStatus {
    pub(crate) fn record_capability_failed(
        &self,
        source: &str,
        name: &str,
        reason: &'static str,
        message: String,
    ) {
        self.record_capability_source(source, name, "blocked", reason);
        self.record_failed_server(message);
    }
    /// Typed, non-secret startup observations. No configuration/error text is copied.
    pub(crate) fn record_capability_source(
        &self,
        source: &str,
        name: &str,
        state: &'static str,
        reason: &'static str,
    ) {
        if let Ok(mut inner) = self.inner.lock()
            && !inner.closed
        {
            inner.observation_revision = inner.observation_revision.wrapping_add(1);
            inner
                .capability_sources
                .insert((source.into(), name.into()), (state, reason));
        }
    }
    pub(crate) fn capability_revision(&self) -> u64 {
        self.inner
            .lock()
            .map(|inner| inner.observation_revision)
            .unwrap_or(u64::MAX)
    }
    pub(crate) fn capability_sources(&self, source: &str) -> serde_json::Value {
        let Ok(inner) = self.inner.lock() else {
            return serde_json::json!({"state":"unknown"});
        };
        let items: Vec<_> = inner
            .capability_sources
            .iter()
            .filter(|((s, n), _)| source.is_empty() || source == s || source == n)
            .collect();
        serde_json::json!({"revision":inner.observation_revision,"configured":inner.configured,"connected_at_startup":inner.connected,"connecting":inner.configured.saturating_sub(inner.connected+inner.failed_servers),"diagnostics":inner.failures.len(),"items":items.iter().take(2).map(|((s,n),(state,reason))| serde_json::json!({
            "source":s,"name":crate::tool::capabilities::bounded_label(n),"state":state,"reason":reason,
            "end_to_end":"unknown","login":"unknown","next":"user: inspect /mcp, fix dependency/configuration or login; explicitly reload via plugin management"})).collect::<Vec<_>>(),"has_more":items.len()>2})
    }
    pub(crate) fn begin_batch(self: &std::sync::Arc<Self>, name: &'static str) -> StartupBatch {
        if let Ok(mut inner) = self.inner.lock()
            && !inner.closed
        {
            inner.batches.insert(name);
            inner.settled = false;
        }
        StartupBatch {
            status: self.clone(),
            name,
        }
    }
    fn finish_batch(&self, name: &'static str) {
        if let Ok(mut inner) = self.inner.lock()
            && !inner.closed
        {
            inner.batches.remove(name);
        }
        self.mark_settled();
        self.notify_settled_if_complete();
    }
    pub(crate) fn close(&self) {
        // Serialize delivery with revocation: once close returns no old notice can escape.
        let mut sink = self.notice_sink.lock().expect("extension notice");
        if let Ok(mut inner) = self.inner.lock() {
            inner.closed = true;
            inner.settled = true;
            inner.batches.clear();
            self.settled.notify_all();
        }
        *sink = None;
    }
    pub(crate) fn new(configured: usize) -> Self {
        Self {
            inner: std::sync::Mutex::new(McpStatusInner {
                capability_sources: Default::default(),
                observation_revision: 0,
                configured,
                connected: 0,
                failed_servers: 0,
                failures: Vec::new(),
                servers: Vec::new(),
                // 无配置即落定：不依赖 worker 的兜底 mark_settled，
                // `start_run` 的等待对空 MCP 是零成本且零窗口。
                settled: configured == 0,
                closed: false,
                batches: Default::default(),
                noticed: false,
            }),
            settled: std::sync::Condvar::new(),
            notice_sink: std::sync::Mutex::new(None),
        }
    }

    /// 全部 server 落定（成功/失败皆计）；无配置时 mount 后立即为真。
    pub(crate) fn is_settled(&self) -> bool {
        self.inner
            .lock()
            .map(|inner| inner.settled)
            .unwrap_or(false)
    }

    /// 有界等待落定。返回 false = 超时（调用方以现状冻结，INV-M3 的
    /// 例外通道）；锁中毒视为已落定（fail-open，不阻塞 run）。
    pub(crate) fn wait_until_settled(&self, timeout: std::time::Duration) -> bool {
        let Ok(mut inner) = self.inner.lock() else {
            return true;
        };
        let deadline = std::time::Instant::now() + timeout;
        while !inner.settled {
            let now = std::time::Instant::now();
            if now >= deadline {
                return false;
            }
            let (guard, result) = self
                .settled
                .wait_timeout(inner, deadline - now)
                .expect("mcp status lock");
            inner = guard;
            if result.timed_out() && !inner.settled {
                return false;
            }
        }
        true
    }

    pub(crate) fn record_connected(&self, server: McpServerStatus) {
        self.record_capability_source(
            if server.transport == "wasm" {
                "wasm"
            } else {
                "mcp"
            },
            &server.name,
            "unknown",
            "startup_connected_only",
        );
        if let Ok(mut inner) = self.inner.lock()
            && !inner.closed
        {
            inner.observation_revision = inner.observation_revision.wrapping_add(1);
            inner.connected += 1;
            inner.servers.push(server);
        }
        self.notify_settled_if_complete();
    }

    /// server 级失败（连接/握手/列工具失败）：计入 connecting 推导。
    pub(crate) fn record_failed_server(&self, message: String) {
        if let Ok(mut inner) = self.inner.lock()
            && !inner.closed
        {
            inner.observation_revision = inner.observation_revision.wrapping_add(1);
            inner.failed_servers += 1;
            inner.failures.push(message);
        }
        self.notify_settled_if_complete();
    }

    /// 非 server 级失败（如工具注册失败）：不影响 connecting 推导。
    pub(crate) fn record_failure(&self, message: String) {
        if let Ok(mut inner) = self.inner.lock()
            && !inner.closed
        {
            inner.observation_revision = inner.observation_revision.wrapping_add(1);
            inner.failures.push(message);
        }
    }

    pub(crate) fn mark_settled(&self) {
        if let Ok(mut inner) = self.inner.lock()
            && !inner.closed
        {
            if inner.batches.is_empty() {
                inner.settled = true;
                self.settled.notify_all();
            }
        }
    }

    /// 追加外部能力计数（wasm 插件与 MCP server 同一状态面板，
    /// INV-W6）。wasm 同步挂载期、登记任何插件状态前调用；只扩大
    /// 分母，随后的 record_connected/record_failed_server 照常把
    /// settled 推到完成。
    pub(crate) fn extend_configured(&self, extra: usize) {
        if extra == 0 {
            return;
        }
        if let Ok(mut inner) = self.inner.lock()
            && !inner.closed
        {
            inner.observation_revision = inner.observation_revision.wrapping_add(1);
            inner.configured += extra;
            inner.settled = false;
        }
    }

    fn notify_settled_if_complete(&self) {
        let sink = self.notice_sink.lock().expect("extension notice");
        let count = {
            let Ok(mut inner) = self.inner.lock() else {
                return;
            };
            if inner.closed
                || !inner.batches.is_empty()
                || inner.connected + inner.failed_servers < inner.configured
            {
                return;
            }
            inner.settled = true;
            self.settled.notify_all();
            if inner.noticed || inner.failures.is_empty() || sink.is_none() {
                return;
            }
            inner.noticed = true;
            inner.failures.len()
        };
        if let Some(sink) = sink.as_ref() {
            sink(count);
        }
    }

    pub(crate) fn set_notice_sink(&self, sink: NoticeSink) {
        {
            let mut slot = self.notice_sink.lock().expect("extension notice");
            if self.inner.lock().is_ok_and(|inner| !inner.closed) {
                *slot = Some(sink);
            }
        }
        // Synchronous WASM and fast MCP startup may finish before the host wires notices.
        self.notify_settled_if_complete();
    }

    pub(crate) fn snapshot(&self) -> McpStatusSnapshot {
        self.inner
            .lock()
            .map(|inner| McpStatusSnapshot {
                connecting: inner
                    .configured
                    .saturating_sub(inner.connected + inner.failed_servers),
                configured: inner.configured,
                connected: inner.connected,
                failures: inner.failures.clone(),
                servers: inner.servers.clone(),
            })
            .unwrap_or_else(|_| McpStatusSnapshot {
                configured: 0,
                connected: 0,
                connecting: 0,
                failures: Vec::new(),
                servers: Vec::new(),
            })
    }
}
