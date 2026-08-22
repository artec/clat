//! MCP adapter plugin: mounts configured MCP servers (stdio/HTTP) after
//! trust and contributes their tools through revocable leases.

use super::services::{
    MCP_STATUS_SERVICE, MCP_STATUS_SERVICE_ID, McpServerStatus, McpStatus, TOOL_SERVICE,
    TOOL_SERVICE_ID,
};
use crate::mcp::client::{
    McpServer, McpServerRequestHandler, McpTool, load_mcp_config, merge_vendor_pack,
};
use crate::plugin::{
    Plugin, PluginContext, PluginDescriptor, PluginError, PluginId, PluginOwner, ScopeKind,
    ServiceId,
};
use crate::plugin_host::{McpHostHandler, PluginHostBridge};
use crate::{Tool, ToolRegistry};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

const ID: PluginId = PluginId::new("builtin.mcp_adapter");
const PROVIDES: &[ServiceId] = &[MCP_STATUS_SERVICE_ID];
const REQUIRES: &[ServiceId] = &[TOOL_SERVICE_ID];
const DESCRIPTOR: PluginDescriptor = PluginDescriptor {
    id: ID,
    scope: ScopeKind::TrustedProject,
    provides: PROVIDES,
    requires: REQUIRES,
    optional: &[],
};

pub(crate) struct McpAdapterPlugin {
    storage_root: PathBuf,
    /// 厂商专属 MCP 包（如 GLM Coding Plan 的四件套），由 application
    /// 在挂载期按激活厂商与凭据算好传入；密钥只在内存，用户
    /// `mcp.json` 同名条目优先（见 `merge_vendor_pack`）。
    vendor_pack: Vec<(String, crate::mcp::client::McpServerConfig)>,
    /// 宿主桥（sampling/elicitation 的传输无关实现）：connect 时按
    /// server 包成 McpHostHandler 注入——服务端请求在 dispatcher 线程
    /// 经桥过权限门/记账/问答（docs/todo/mcp-sampling-elicitation.md）。
    host: Arc<PluginHostBridge>,
}

impl McpAdapterPlugin {
    pub(crate) fn new(
        storage_root: PathBuf,
        vendor_pack: Vec<(String, crate::mcp::client::McpServerConfig)>,
        host: Arc<PluginHostBridge>,
    ) -> Self {
        Self {
            storage_root,
            vendor_pack,
            host,
        }
    }
}

/// worker 与 close 协作的共享状态：取消位 + 清理闭包列表。worker 在
/// server 间检查取消；清理按 push 序执行（**每 server 先工具 lease 后
/// 进程关闭**——线性 drain 等价旧 LIFO teardown 语义：工具先撤、进程
/// 后关）。
struct McpStartupState {
    cancelled: AtomicBool,
    cleanups: Mutex<Vec<Box<dyn FnOnce() + Send>>>,
}

impl McpStartupState {
    fn cancel(&self) {
        self.cancelled.store(true, Ordering::Release);
    }

    fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Acquire)
    }

    fn push_cleanup(&self, cleanup: Box<dyn FnOnce() + Send>) {
        if let Ok(mut cleanups) = self.cleanups.lock() {
            cleanups.push(cleanup);
        }
    }

    fn run_cleanups(&self) {
        if let Ok(mut cleanups) = self.cleanups.lock() {
            for cleanup in cleanups.drain(..) {
                cleanup();
            }
        }
    }
}

impl Plugin for McpAdapterPlugin {
    fn descriptor(&self) -> &'static PluginDescriptor {
        &DESCRIPTOR
    }

    fn mount(&self, context: &mut PluginContext<'_>) -> Result<(), PluginError> {
        let registry = context
            .require(TOOL_SERVICE)
            .map_err(|error| PluginError::new(error.to_string()))?;
        // 本地配置读取保留 fail-fast（无网络/子进程 I/O，INV-M1）；
        // 连接与握手全部挪进后台 worker——mount 即返回，ready 不再被
        // MCP 阻塞（docs/todo/mcp-async-startup.md）。
        let mut config = load_mcp_config(&self.storage_root).map_err(PluginError::new)?;
        merge_vendor_pack(&mut config, &self.vendor_pack);
        let status = Arc::new(McpStatus::new(config.len()));
        let state = Arc::new(McpStartupState {
            cancelled: AtomicBool::new(false),
            cleanups: Mutex::new(Vec::new()),
        });
        let owner = context.owner();
        let worker = {
            let status = Arc::clone(&status);
            let registry = Arc::clone(&registry);
            let state = Arc::clone(&state);
            let host = Arc::clone(&self.host);
            let storage_root = self.storage_root.clone();
            std::thread::Builder::new()
                .name("clat-mcp-startup".into())
                .spawn(move || {
                    run_startup(config, &storage_root, registry, owner, status, state, host)
                })
                .map_err(|error| PluginError::new(format!("spawn mcp startup worker: {error}")))?
        };
        context
            .provide(MCP_STATUS_SERVICE, status)
            .map_err(|error| PluginError::new(error.to_string()))?;
        // 单一 defer：cancel → 有界 join → 依序执行 worker 登记的清理。
        // 卡住的握手不拖关闭（对齐 monitor/title 的 EXIT_JOIN_GRACE 纪
        // 律，INV-M5）；被放弃的 worker 由进程退出回收。
        let join_state = Arc::clone(&state);
        context.defer(move || {
            join_state.cancel();
            let _ = crate::application::join_with_grace(
                worker,
                crate::application::EXIT_JOIN_GRACE,
                "mcp startup worker",
            );
            join_state.run_cleanups();
            Ok(())
        });
        Ok(())
    }
}

/// 后台启动 server：**并行**连接（W1-16/A2——此前串行逐 server，第一
/// 个坏 server 的握手/list 全额超时会顺延所有后续 server 的工具注册）。
/// 每 server 一线程做 connect（spawn/HTTP + initialize 握手）→
/// `list_tools`（各超时有界：握手 10s / discover 3s / list 总帽 30s），
/// 完成即经 channel 交回主 worker 按到达序注册工具与上报状态；失败
/// （含 stderr 尾部）记入状态，不影响其余 server。全部落定（或取消）
/// 后 mark_settled——`start_run` 的有界等待以此为准（INV-M2/M3）。
fn run_startup(
    config: BTreeMap<String, crate::mcp::client::McpServerConfig>,
    storage_root: &std::path::Path,
    registry: Arc<ToolRegistry>,
    owner: PluginOwner,
    status: Arc<McpStatus>,
    state: Arc<McpStartupState>,
    host: Arc<PluginHostBridge>,
) {
    struct ServerOutcome {
        name: String,
        transport: &'static str,
        result: Result<
            (
                crate::mcp::client::McpServer,
                Vec<crate::mcp::client::McpToolInfo>,
            ),
            String,
        >,
    }
    let (tx, rx) = std::sync::mpsc::channel::<ServerOutcome>();
    let mut pending = 0usize;
    for (name, server_config) in &config {
        let valid = server_config.is_http() || !server_config.command.trim().is_empty();
        if !valid {
            status.record_failed_server(format!("mcp `{name}`: empty command and no url"));
            continue;
        }
        pending += 1;
        // 每 server 一个 McpHostHandler：服务端请求带 server 名过桥。
        let server_requests: Option<Arc<dyn McpServerRequestHandler>> =
            Some(Arc::new(McpHostHandler::new(Arc::clone(&host), name)));
        let tx = tx.clone();
        let thread_name = name.clone();
        let name = name.clone();
        let server_config = server_config.clone();
        let storage_root = storage_root.to_path_buf();
        let spawned = std::thread::Builder::new()
            .name(format!("clat-mcp-connect-{thread_name}"))
            .spawn(move || {
                let outcome =
                    match McpServer::connect(&name, &server_config, &storage_root, server_requests)
                    {
                        Ok(server) => match server.list_tools() {
                            Ok(infos) => Ok((server, infos)),
                            Err(error) => {
                                // 失败消息带上服务器 stderr 尾部——npx/npm 的
                                // 报错就在这里；连上但列具失败的连接显式关闭。
                                let tail = crate::mcp::client::format_stderr_tail_public(
                                    &server.stderr_tail(),
                                );
                                let _ = server.shutdown();
                                Err(format!("mcp `{name}`: {error}{tail}"))
                            }
                        },
                        Err(error) => Err(format!("mcp `{name}`: {error}")),
                    };
                let transport = if server_config.is_http() {
                    "http"
                } else {
                    "stdio"
                };
                // 接收端已消失（worker 取消退出）：自关闭，不外泄进程。
                let _ = tx.send(ServerOutcome {
                    name,
                    transport,
                    result: outcome,
                });
            });
        if spawned.is_err() {
            pending -= 1;
            status
                .record_failed_server(format!("mcp `{thread_name}`: spawn connect thread failed"));
        }
    }
    drop(tx);
    while pending > 0 {
        let outcome = match rx.recv_timeout(std::time::Duration::from_millis(100)) {
            Ok(outcome) => outcome,
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                if state.is_cancelled() {
                    break;
                }
                continue;
            }
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
        };
        pending -= 1;
        if state.is_cancelled() {
            // 已连上但来不及注册的：显式关闭，不依赖 Drop 兜底。
            if let Ok((server, _)) = outcome.result {
                let _ = server.shutdown();
            }
            continue;
        }
        let ServerOutcome {
            name,
            transport,
            result,
        } = outcome;
        let (server, infos) = match result {
            Ok(pair) => pair,
            Err(message) => {
                status.record_failed_server(message);
                continue;
            }
        };
        let server = Arc::new(server);
        let mut tools = 0usize;
        for info in infos {
            let remote_name = info.name.clone();
            // N-6（审计，W1-19 的 MCP 半边）：description 同 4096 字符闸
            //（帧帽 4MiB 兜底的是内存，不是模型可见注入面）。
            let mut info = info;
            if info.description.chars().count() > crate::tool::MAX_TOOL_DESCRIPTION_CHARS {
                let kept: String = info
                    .description
                    .chars()
                    .take(crate::tool::MAX_TOOL_DESCRIPTION_CHARS)
                    .collect();
                status.record_failure(format!(
                    "mcp `{name}` tool `{remote_name}`: description exceeded \
                     {} chars and was truncated",
                    crate::tool::MAX_TOOL_DESCRIPTION_CHARS
                ));
                info.description = format!("{kept}… [truncated by host]");
            }
            let tool: Arc<dyn Tool> = Arc::new(McpTool::new(&server, info));
            match registry.register(owner, tool) {
                Ok(lease) => {
                    // 清理序（每 server）：先工具 lease、后进程关闭。
                    state.push_cleanup(Box::new(move || {
                        let _ = lease.revoke();
                    }));
                    tools += 1;
                }
                Err(error) => {
                    // server 已连上：连接级失败之外的登记（不参与
                    // connecting 推导）。
                    status.record_failure(format!("mcp `{name}` tool `{remote_name}`: {error}"));
                }
            }
        }
        let shutdown_server = Arc::clone(&server);
        state.push_cleanup(Box::new(move || {
            // 工具 lease 已先行 revoke（McpTool 释放 Arc）；仍被放弃的
            // 连接线程持有 Arc 时放弃显式关闭（其自身超时有界、发送失
            // 败即自关闭，进程退出兜底），不算失败。
            if let Ok(server) = Arc::try_unwrap(shutdown_server) {
                let _ = server.shutdown();
            }
        }));
        status.record_connected(McpServerStatus {
            name: name.clone(),
            server_version: server.server_version().to_owned(),
            protocol_version: server.negotiated_version().to_owned(),
            tools,
            transport: transport.to_owned(),
        });
    }
    // 兜底：取消/异常路径下也置 settled（等待者不被挂起）。
    status.mark_settled();
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugin::{Plugin, PluginManager};
    use crate::plugins::ToolRegistryPlugin;
    use std::fs;
    use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

    /// INV-M1（空配置）：mount 即 settled、connecting 0——无 server 时
    /// 启动等待是零成本的；close 干净回收。
    #[test]
    fn mount_with_no_servers_settles_immediately() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let root = std::env::temp_dir().join(format!("clat-mcp-empty-{unique}"));
        fs::create_dir_all(&root).expect("root");

        let catalog: Vec<Arc<dyn Plugin>> = vec![
            Arc::new(ToolRegistryPlugin),
            Arc::new(McpAdapterPlugin::new(
                root.clone(),
                Vec::new(),
                crate::plugin_host::PluginHostBridge::shared(),
            )),
        ];
        let mut manager = PluginManager::root(ScopeKind::TrustedProject);
        manager.mount_all(catalog).expect("mount plugins");
        let status = manager.require(MCP_STATUS_SERVICE).expect("status service");
        assert!(status.is_settled(), "no servers ⇒ settled at mount");
        assert!(status.wait_until_settled(Duration::from_millis(50)));
        let snapshot = status.snapshot();
        assert_eq!(snapshot.configured, 0);
        assert_eq!(snapshot.connecting, 0);
        manager.close().expect("close project scope");
        fs::remove_dir_all(root).expect("remove root");
    }

    #[test]
    #[ignore = "spawns a python3 subprocess; run explicitly with --ignored"]
    fn project_scope_close_revokes_tools_and_reaps_mcp_process() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let root = std::env::temp_dir().join(format!("clat-mcp-plugin-{unique}"));
        fs::create_dir_all(&root).expect("root");
        let marker = root.join("server-closed");
        // 门控文件：discover 前阻塞等待它出现——mount 返回时 server 必然
        // 仍在 connecting（INV-M1 断言无竞态）。
        let gate = root.join("startup-gate");
        let script = r#"
import json, sys, os, time
def send(obj):
    sys.stdout.write(json.dumps(obj) + "\n")
    sys.stdout.flush()
for line in sys.stdin:
    msg = json.loads(line)
    if "id" not in msg:
        continue
    method = msg.get("method", "")
    if method == "server/discover":
        while not os.path.exists(sys.argv[2]):
            time.sleep(0.01)
        send({"jsonrpc": "2.0", "id": msg["id"], "result": {
            "resultType": "complete",
            "supportedVersions": ["2026-07-28"],
            "capabilities": {"tools": {}},
            "_meta": {"io.modelcontextprotocol/serverInfo": {
                "name": "plugin-test", "version": "1.0"}}}})
    elif method == "tools/list":
        send({"jsonrpc": "2.0", "id": msg["id"], "result": {
            "resultType": "complete", "tools": [{
                "name": "echo", "description": "test tool",
                "inputSchema": {"type": "object"}}]}})
    else:
        send({"jsonrpc": "2.0", "id": msg["id"],
              "error": {"code": -32601, "message": "unsupported"}})
with open(sys.argv[1], "w", encoding="utf-8") as output:
    output.write("closed")
"#;
        let config = serde_json::json!({
            "fixture": {
                "command": "python3",
                "args": ["-u", "-c", script, marker.to_string_lossy(), gate.to_string_lossy()]
            }
        });
        fs::write(
            root.join("mcp.json"),
            serde_json::to_vec(&config).expect("serialize config"),
        )
        .expect("config");

        let catalog: Vec<Arc<dyn Plugin>> = vec![
            Arc::new(ToolRegistryPlugin),
            Arc::new(McpAdapterPlugin::new(
                root.clone(),
                Vec::new(),
                crate::plugin_host::PluginHostBridge::shared(),
            )),
        ];
        let mut manager = PluginManager::root(ScopeKind::TrustedProject);
        // INV-M1：mount 在握手完成前即返回（connecting 态、无工具）。
        let mounted = Instant::now();
        manager.mount_all(catalog).expect("mount plugins");
        assert!(
            mounted.elapsed() < Duration::from_secs(2),
            "mount must not wait for the MCP handshake"
        );
        let registry = manager.require(TOOL_SERVICE).expect("tool registry");
        let status = manager.require(MCP_STATUS_SERVICE).expect("status service");
        assert!(!status.is_settled(), "startup worker still connecting");
        assert_eq!(status.snapshot().connecting, 1);

        // INV-M2/M3：等待落定后工具已注册（run 在此之后才会冻结）。
        fs::write(&gate, b"go").expect("open the startup gate");
        assert!(
            status.wait_until_settled(Duration::from_secs(30)),
            "startup settles"
        );
        let snapshot = status.snapshot();
        assert_eq!(snapshot.connected, 1);
        assert_eq!(snapshot.connecting, 0);
        assert_eq!(
            registry
                .definitions()
                .into_iter()
                .map(|definition| definition.name)
                .collect::<Vec<_>>(),
            ["mcp_fixture_echo"]
        );

        manager.close().expect("close project scope");
        assert!(registry.is_empty(), "MCP tool lease was not revoked");
        assert!(
            marker.exists(),
            "MCP subprocess did not observe stdin close"
        );
        fs::remove_dir_all(root).expect("remove root");
    }

    /// W1-16/A2：server **并行**连接——慢 server（discover 被门控阻塞）
    /// 不得顺延快 server 的工具注册。pre-fix 红：串行循环里排第二的
    /// fast 要等 slow 的门放行（超时断言失败）。
    #[test]
    #[ignore = "spawns python3 subprocesses; run explicitly with --ignored"]
    fn parallel_startup_does_not_delay_fast_servers_behind_slow_ones() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let root = std::env::temp_dir().join(format!("clat-mcp-parallel-{unique}"));
        fs::create_dir_all(&root).expect("root");
        let gate = root.join("startup-gate");
        let gate_arg = gate.to_string_lossy().to_string();
        let slow_script = r#"
import json, sys, os, time
def send(obj):
    sys.stdout.write(json.dumps(obj) + "\n")
    sys.stdout.flush()
for line in sys.stdin:
    msg = json.loads(line)
    if "id" not in msg:
        continue
    method = msg.get("method", "")
    if method in ("server/discover", "initialize"):
        while not os.path.exists(sys.argv[1]):
            time.sleep(0.01)
        if method == "server/discover":
            send({"jsonrpc": "2.0", "id": msg["id"], "result": {
                "resultType": "complete",
                "supportedVersions": ["2026-07-28"],
                "capabilities": {"tools": {}},
                "_meta": {"io.modelcontextprotocol/serverInfo": {
                    "name": "slow", "version": "1.0"}}}})
        else:
            send({"jsonrpc": "2.0", "id": msg["id"], "result": {
                "protocolVersion": "2025-06-18",
                "capabilities": {"tools": {}},
                "serverInfo": {"name": "slow", "version": "1.0"}}})
    elif method == "tools/list":
        send({"jsonrpc": "2.0", "id": msg["id"], "result": {
            "resultType": "complete", "tools": []}})
    else:
        send({"jsonrpc": "2.0", "id": msg["id"],
              "error": {"code": -32601, "message": "unsupported"}})
"#;
        let fast_script = r#"
import json, sys
def send(obj):
    sys.stdout.write(json.dumps(obj) + "\n")
    sys.stdout.flush()
for line in sys.stdin:
    msg = json.loads(line)
    if "id" not in msg:
        continue
    method = msg.get("method", "")
    if method == "server/discover":
        send({"jsonrpc": "2.0", "id": msg["id"], "result": {
            "resultType": "complete",
            "supportedVersions": ["2026-07-28"],
            "capabilities": {"tools": {}},
            "_meta": {"io.modelcontextprotocol/serverInfo": {
                "name": "fast", "version": "1.0"}}}})
    elif method == "tools/list":
        send({"jsonrpc": "2.0", "id": msg["id"], "result": {
            "resultType": "complete", "tools": [{
                "name": "quick", "description": "fast tool",
                "inputSchema": {"type": "object"}}]}})
    else:
        send({"jsonrpc": "2.0", "id": msg["id"],
              "error": {"code": -32601, "message": "unsupported"}})
"#;
        // BTreeMap 按名排序：a-slow 排在 b-fast 前，串行实现必然先卡 slow。
        let config = serde_json::json!({
            "a-slow": {
                "command": "python3",
                "args": ["-u", "-c", slow_script, gate_arg]
            },
            "b-fast": {
                "command": "python3",
                "args": ["-u", "-c", fast_script]
            }
        });
        fs::write(
            root.join("mcp.json"),
            serde_json::to_vec(&config).expect("serialize config"),
        )
        .expect("config");

        let catalog: Vec<Arc<dyn Plugin>> = vec![
            Arc::new(ToolRegistryPlugin),
            Arc::new(McpAdapterPlugin::new(
                root.clone(),
                Vec::new(),
                crate::plugin_host::PluginHostBridge::shared(),
            )),
        ];
        let mut manager = PluginManager::root(ScopeKind::TrustedProject);
        manager.mount_all(catalog).expect("mount plugins");
        let registry = manager.require(TOOL_SERVICE).expect("tool registry");

        // 门不放行：slow 永远 connecting；fast 必须已注册（并行收益）。
        let deadline = Instant::now() + Duration::from_secs(10);
        let fast_registered = loop {
            if registry.get("mcp_b_fast_quick").is_some() {
                break true;
            }
            if Instant::now() > deadline {
                break false;
            }
            std::thread::sleep(Duration::from_millis(50));
        };
        if !fast_registered {
            let status = manager.require(MCP_STATUS_SERVICE).expect("status service");
            let snapshot = status.snapshot();
            panic!(
                "the fast server's tool must register while the slow one is still gated; \
                 connected={} connecting={} failures={:?}",
                snapshot.connected, snapshot.connecting, snapshot.failures
            );
        }
        // 放行 slow，正常收尾。
        fs::write(&gate, b"").expect("release gate");
        let status = manager.require(MCP_STATUS_SERVICE).expect("status");
        assert!(status.wait_until_settled(Duration::from_secs(15)));
        manager.close().expect("close project scope");
        fs::remove_dir_all(root).expect("remove root");
    }
}
