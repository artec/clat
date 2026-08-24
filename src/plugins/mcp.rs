//! MCP adapter plugin: mounts configured MCP servers (stdio/HTTP) after
//! trust and contributes their tools plus marked DSH system prompts through
//! revocable leases.

use super::services::{
    MCP_STATUS_SERVICE, MCP_STATUS_SERVICE_ID, McpServerStatus, McpStatus, PROMPT_SERVICE,
    PROMPT_SERVICE_ID, PromptRegistry, TOOL_SERVICE, TOOL_SERVICE_ID,
};
use crate::mcp::client::{
    McpServer, McpServerConfig, McpServerRequestHandler, McpTool, load_mcp_config,
    merge_vendor_pack,
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
const OPTIONAL: &[ServiceId] = &[PROMPT_SERVICE_ID];
const DESCRIPTOR: PluginDescriptor = PluginDescriptor {
    id: ID,
    scope: ScopeKind::TrustedProject,
    provides: PROVIDES,
    requires: REQUIRES,
    optional: OPTIONAL,
};

pub(crate) struct McpAdapterPlugin {
    storage_root: PathBuf,
    /// DSH `{{cwd}}` 变量的项目真值。子进程 cwd 仍是 storage_root，
    /// 防止不受信任项目劫持 cwd-sensitive launcher（如 npx）。
    project_root: PathBuf,
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
    #[cfg(test)]
    pub(crate) fn new(
        storage_root: PathBuf,
        vendor_pack: Vec<(String, crate::mcp::client::McpServerConfig)>,
        host: Arc<PluginHostBridge>,
    ) -> Self {
        Self {
            project_root: storage_root.clone(),
            storage_root,
            vendor_pack,
            host,
        }
    }

    /// 生产 Catalog 使用：控制面与 MCP 子进程 cwd 仍取 `storage_root`；
    /// 仅 DSH prompt 变量使用真实项目根。
    pub(crate) fn with_project_root(
        storage_root: PathBuf,
        project_root: PathBuf,
        vendor_pack: Vec<(String, crate::mcp::client::McpServerConfig)>,
        host: Arc<PluginHostBridge>,
    ) -> Self {
        Self {
            storage_root,
            project_root,
            vendor_pack,
            host,
        }
    }
}

/// worker 与 close 协作的共享状态：取消位 + 清理闭包列表。worker 在
/// server 间检查取消；清理按 push 序执行（**每 server 先工具/prompt
/// lease 后进程关闭**——线性 drain 等价旧 LIFO teardown 语义：贡献先撤、
/// 进程后关）。
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
        let prompts = context
            .try_require(PROMPT_SERVICE)
            .map_err(|error| PluginError::new(error.to_string()))?;
        // 本地配置读取保留 fail-fast（无网络/子进程 I/O，INV-M1）；
        // 连接与握手全部挪进后台 worker——mount 即返回，ready 不再被
        // MCP 阻塞（docs/todo/mcp-async-startup.md）。
        let effective = load_effective_mcp_config(&self.storage_root, &self.vendor_pack)
            .map_err(PluginError::new)?;
        let status = Arc::new(McpStatus::new(
            effective.servers.len() + effective.failures.len(),
        ));
        for failure in &effective.failures {
            status.record_failed_server(failure.clone());
        }
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
            let server_cwd = self.storage_root.clone();
            let project_root = self.project_root.clone();
            std::thread::Builder::new()
                .name("clat-mcp-startup".into())
                .spawn(move || {
                    run_startup(McpStartupInputs {
                        config: effective.servers,
                        manifest_prompts: effective.manifest_prompts,
                        server_cwd,
                        project_root,
                        registry,
                        prompt_registry: prompts,
                        owner,
                        status,
                        state,
                        host,
                    })
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

fn load_installed_mcp_config(
    storage_root: &std::path::Path,
    excluded_ids: &std::collections::BTreeSet<String>,
) -> Result<EffectiveMcpConfig, String> {
    let mut config = BTreeMap::new();
    let mut manifest_prompts = BTreeMap::new();
    let installed = crate::plugin::active_packages_for_runtime_excluding(
        storage_root,
        crate::plugin::PluginRuntimeKind::McpStdio,
        excluded_ids,
    )?;
    for package in installed.packages {
        let entry = package
            .manifest
            .verify_entry_digest(&package.manifest_path)?;
        let mut env = BTreeMap::from([
            ("CLAT_PLUGIN_ID".into(), package.id.clone()),
            (
                "CLAT_PLUGIN_VERSION".into(),
                package.manifest.version.clone(),
            ),
            ("CLAT_PLUGIN_TREE_SHA256".into(), package.tree_sha256),
            (
                "CLAT_PLUGIN_TRUST".into(),
                match package.trust {
                    crate::plugin::TrustLabel::LocalUnverified => "local/unverified".into(),
                    crate::plugin::TrustLabel::PublisherVerified => "publisher/verified".into(),
                },
            ),
        ]);
        if let Some(value) = package.config {
            env.insert(
                "CLAT_PLUGIN_CONFIG".into(),
                serde_json::to_string(&value)
                    .map_err(|error| format!("serialize plugin config: {error}"))?,
            );
        }
        if let Some(publisher) = package.publisher {
            env.insert("CLAT_PLUGIN_PUBLISHER".into(), publisher.publisher);
        }
        manifest_prompts.insert(package.id.clone(), package.manifest.prompts.clone());
        config.insert(
            package.id,
            McpServerConfig {
                command: entry.display().to_string(),
                args: package.manifest.runtime.args,
                env,
                cwd: package
                    .manifest_path
                    .parent()
                    .map(|path| path.display().to_string()),
                ..McpServerConfig::default()
            },
        );
    }
    Ok(EffectiveMcpConfig {
        servers: config,
        manifest_prompts,
        failures: installed.failures,
    })
}

struct EffectiveMcpConfig {
    servers: BTreeMap<String, McpServerConfig>,
    manifest_prompts: BTreeMap<String, Vec<crate::plugin::ManifestPrompt>>,
    failures: Vec<String>,
}

fn load_effective_mcp_config(
    storage_root: &std::path::Path,
    vendor_pack: &[(String, McpServerConfig)],
) -> Result<EffectiveMcpConfig, String> {
    let user = load_mcp_config(storage_root)?;
    let excluded = user.keys().cloned().collect();
    let mut effective = load_installed_mcp_config(storage_root, &excluded)?;
    // User mcp.json is the explicit escape hatch and wins by package id.
    effective.servers.extend(user);
    merge_vendor_pack(&mut effective.servers, vendor_pack);
    Ok(effective)
}

fn resolve_mcp_cwd(
    storage_root: &std::path::Path,
    configured: Option<&str>,
) -> Result<PathBuf, String> {
    let requested = configured
        .map(PathBuf::from)
        .unwrap_or_else(|| storage_root.to_owned());
    let candidate = if requested.is_absolute() {
        requested
    } else {
        if requested.components().any(|component| {
            matches!(
                component,
                std::path::Component::ParentDir
                    | std::path::Component::RootDir
                    | std::path::Component::Prefix(_)
            )
        }) {
            return Err("relative MCP cwd must not escape ~/.clat".into());
        }
        storage_root.join(requested)
    };
    let canonical = candidate
        .canonicalize()
        .map_err(|error| format!("resolve MCP cwd {}: {error}", candidate.display()))?;
    if !canonical.is_dir() {
        return Err(format!(
            "MCP cwd is not a directory: {}",
            canonical.display()
        ));
    }
    if configured.is_some_and(|cwd| !PathBuf::from(cwd).is_absolute()) {
        let root = storage_root
            .canonicalize()
            .map_err(|error| format!("resolve ~/.clat for MCP cwd: {error}"))?;
        if !canonical.starts_with(root) {
            return Err("relative MCP cwd resolves outside ~/.clat".into());
        }
    }
    Ok(canonical)
}

/// 后台启动 server：**并行**连接（W1-16/A2——此前串行逐 server，第一
/// 个坏 server 的握手/list 全额超时会顺延所有后续 server 的工具注册）。
/// 每 server 一线程做 connect（spawn/HTTP + initialize 握手）→
/// `list_tools` + 标记 prompt 解析（各超时有界：握手 10s / discover 3s /
/// list 总帽 30s），完成即经 channel 交回主 worker 按到达序注册贡献并
/// 上报状态；失败
/// （含 stderr 尾部）记入状态，不影响其余 server。全部落定（或取消）
/// 后 mark_settled——`start_run` 的有界等待以此为准（INV-M2/M3）。
struct McpStartupInputs {
    config: BTreeMap<String, crate::mcp::client::McpServerConfig>,
    manifest_prompts: BTreeMap<String, Vec<crate::plugin::ManifestPrompt>>,
    server_cwd: PathBuf,
    project_root: PathBuf,
    registry: Arc<ToolRegistry>,
    prompt_registry: Option<Arc<PromptRegistry>>,
    owner: PluginOwner,
    status: Arc<McpStatus>,
    state: Arc<McpStartupState>,
    host: Arc<PluginHostBridge>,
}

struct ConnectedContributions {
    server: crate::mcp::client::McpServer,
    tools: Vec<crate::mcp::client::McpToolInfo>,
    prompts: Vec<(crate::mcp::client::McpPromptInfo, String)>,
    prompt_failure: Option<String>,
}

fn run_startup(inputs: McpStartupInputs) {
    let McpStartupInputs {
        config,
        mut manifest_prompts,
        server_cwd,
        project_root,
        registry,
        prompt_registry,
        owner,
        status,
        state,
        host,
    } = inputs;
    struct ServerOutcome {
        name: String,
        transport: &'static str,
        result: Result<ConnectedContributions, String>,
    }
    let (tx, rx) = std::sync::mpsc::channel::<ServerOutcome>();
    let mut pending = 0usize;
    for (name, server_config) in &config {
        let valid = server_config.is_http() || !server_config.command.trim().is_empty();
        if !valid {
            status.record_failed_server(format!("mcp `{name}`: empty command and no url"));
            continue;
        }
        let resolved_server_cwd = match resolve_mcp_cwd(&server_cwd, server_config.cwd.as_deref()) {
            Ok(cwd) => cwd,
            Err(error) => {
                status.record_failed_server(format!("mcp `{name}`: {error}"));
                continue;
            }
        };
        pending += 1;
        // 每 server 一个 McpHostHandler：服务端请求带 server 名过桥。
        let server_handler = Arc::new(McpHostHandler::new(Arc::clone(&host), name));
        let server_requests: Option<Arc<dyn McpServerRequestHandler>> =
            Some(server_handler.clone());
        let tx = tx.clone();
        let thread_name = name.clone();
        let name = name.clone();
        let server_config = server_config.clone();
        let server_cwd = resolved_server_cwd;
        let project_root = project_root.clone();
        let server_handler = Arc::clone(&server_handler);
        let spawned = std::thread::Builder::new()
            .name(format!("clat-mcp-connect-{thread_name}"))
            .spawn(move || {
                let outcome =
                    match McpServer::connect(&name, &server_config, &server_cwd, server_requests) {
                        Ok(server) => {
                            if server.supports_clat_host_services() {
                                server_handler.enable_host_services();
                            }
                            match server.list_tools() {
                                Ok(infos) => {
                                    let mut arguments = BTreeMap::new();
                                    arguments.insert(
                                        "cwd".to_owned(),
                                        project_root.to_string_lossy().into_owned(),
                                    );
                                    let prompt_result =
                                        server.list_system_prompts().and_then(|items| {
                                            items
                                                .into_iter()
                                                .map(|prompt| {
                                                    server
                                                        .get_system_prompt(&prompt, &arguments)
                                                        .map(|text| (prompt, text))
                                                })
                                                .collect::<Result<Vec<_>, _>>()
                                        });
                                    match prompt_result {
                                        Ok(prompts) => Ok(ConnectedContributions {
                                            server,
                                            tools: infos,
                                            prompts,
                                            prompt_failure: None,
                                        }),
                                        Err(error) => Ok(ConnectedContributions {
                                            server,
                                            tools: infos,
                                            prompts: Vec::new(),
                                            prompt_failure: Some(format!(
                                                "mcp `{name}` prompts: {error}"
                                            )),
                                        }),
                                    }
                                }
                                Err(error) => {
                                    // 失败消息带上服务器 stderr 尾部——npx/npm 的
                                    // 报错就在这里；连上但列具失败的连接显式关闭。
                                    let tail = crate::mcp::client::format_stderr_tail_public(
                                        &server.stderr_tail(),
                                    );
                                    let _ = server.shutdown();
                                    Err(format!("mcp `{name}`: {error}{tail}"))
                                }
                            }
                        }
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
            if let Ok(connected) = outcome.result {
                let _ = connected.server.shutdown();
            }
            continue;
        }
        let ServerOutcome {
            name,
            transport,
            result,
        } = outcome;
        let ConnectedContributions {
            server,
            tools: infos,
            prompts: prompt_infos,
            prompt_failure,
        } = match result {
            Ok(connected) => connected,
            Err(message) => {
                status.record_failed_server(message);
                continue;
            }
        };
        if let Some(failure) = prompt_failure {
            status.record_failure(failure);
        }
        let server = Arc::new(server);
        if server.supports_clat_host_services() {
            let weak_server = Arc::downgrade(&server);
            let lease = host.subscribe_context(Arc::new(move |context| {
                if let Some(server) = weak_server.upgrade() {
                    let _ = server.notify_clat_host_context(context);
                }
            }));
            state.push_cleanup(Box::new(move || lease.revoke()));
        }
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
                    // 清理序（每 server）：先贡献 lease、后进程关闭。
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
        let static_prompts = manifest_prompts.remove(&name).unwrap_or_default();
        if let Some(prompt_registry) = &prompt_registry {
            for prompt in &static_prompts {
                if prompt.system.trim().is_empty() {
                    continue;
                }
                match prompt_registry.contribute(owner, prompt.system.clone()) {
                    Ok(lease) => state.push_cleanup(Box::new(move || {
                        let _ = lease.revoke();
                    })),
                    Err(error) => status.record_failure(format!(
                        "mcp `{name}` manifest prompt `{}`: {error}",
                        prompt.name
                    )),
                }
            }
            for (prompt, text) in prompt_infos {
                if text.trim().is_empty() {
                    continue;
                }
                match prompt_registry.contribute(owner, text) {
                    Ok(lease) => state.push_cleanup(Box::new(move || {
                        let _ = lease.revoke();
                    })),
                    Err(error) => status
                        .record_failure(format!("mcp `{name}` prompt `{}`: {error}", prompt.name)),
                }
            }
        } else if !static_prompts.is_empty() {
            status.record_failure(format!(
                "mcp `{name}` manifest declares prompts but PromptRegistry is unavailable"
            ));
        }
        let shutdown_server = Arc::clone(&server);
        state.push_cleanup(Box::new(move || {
            // 工具/prompt lease 已先行 revoke（McpTool 释放 Arc）；仍被放弃的
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
    use crate::plugins::{PromptRegistryPlugin, ToolRegistryPlugin};
    use sha2::{Digest as _, Sha256};
    use std::fs;
    use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

    fn installed_mcp_package(storage: &std::path::Path) -> PathBuf {
        let package = storage
            .parent()
            .expect("storage parent")
            .join("mcp-package-source");
        fs::create_dir_all(&package).expect("package");
        let entry = package.join(if cfg!(windows) {
            "fixture.exe"
        } else {
            "fixture"
        });
        fs::write(&entry, b"fixture executable").expect("entry");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            fs::set_permissions(&entry, fs::Permissions::from_mode(0o755)).expect("chmod");
        }
        let digest = Sha256::digest(b"fixture executable")
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        fs::write(
            package.join("clat-plugin.json"),
            serde_json::to_vec(&serde_json::json!({
                "manifestVersion": 1,
                "id": "dev.clat.mcp-fixture",
                "name": "MCP Fixture",
                "version": "1.0.0",
                "runtime": {
                    "kind": "mcp-stdio",
                    "entry": entry.file_name().and_then(|name| name.to_str()).expect("entry name"),
                    "sha256": digest,
                    "args": ["--fixture"],
                },
                "capabilities": { "tools": true, "prompts": true },
                "prompts": [{ "name": "fixture", "system": "Installed MCP prompt." }],
                "configSchema": { "type": "object", "required": ["answer"] },
            }))
            .expect("manifest"),
        )
        .expect("write manifest");
        package
    }

    #[test]
    fn installed_mcp_package_projects_config_and_user_override_wins() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let base = std::env::temp_dir().join(format!("clat-installed-mcp-{unique}"));
        let storage = base.join("storage");
        fs::create_dir_all(&base).expect("base");
        let package = installed_mcp_package(&storage);
        {
            let mut store = crate::plugin::PackageStore::open(&storage).expect("store");
            store
                .install(
                    &package,
                    Some(serde_json::json!({"answer": "configured"})),
                    true,
                    crate::plugin::InstallKind::Install,
                )
                .expect("install");
        }
        let effective = load_effective_mcp_config(&storage, &[]).expect("effective");
        let installed = effective
            .servers
            .get("dev.clat.mcp-fixture")
            .expect("installed");
        assert!(installed.command.contains("plugin-store"));
        assert!(
            installed
                .cwd
                .as_deref()
                .is_some_and(|cwd| cwd.contains("plugin-store"))
        );
        assert_eq!(installed.args, ["--fixture"]);
        assert_eq!(
            installed.env.get("CLAT_PLUGIN_CONFIG").map(String::as_str),
            Some(r#"{"answer":"configured"}"#)
        );
        assert_eq!(
            effective.manifest_prompts["dev.clat.mcp-fixture"][0].system,
            "Installed MCP prompt."
        );
        let installed_root = std::path::Path::new(&installed.command)
            .parent()
            .expect("artifact root")
            .to_owned();
        fs::write(installed_root.join("tampered-sidecar"), "tampered")
            .expect("tamper installed package");
        let damaged = load_effective_mcp_config(&storage, &[]).expect("isolated damage");
        assert!(damaged.servers.is_empty());
        assert!(
            damaged
                .failures
                .iter()
                .any(|failure| failure.contains("dev.clat.mcp-fixture")),
            "{:?}",
            damaged.failures
        );
        fs::write(
            storage.join("mcp.json"),
            serde_json::to_vec(&serde_json::json!({
                "dev.clat.mcp-fixture": { "command": "user-override" }
            }))
            .expect("user config"),
        )
        .expect("write user config");
        let effective = load_effective_mcp_config(&storage, &[]).expect("effective override");
        assert_eq!(
            effective
                .servers
                .get("dev.clat.mcp-fixture")
                .map(|config| config.command.as_str()),
            Some("user-override")
        );
        assert!(
            !effective
                .manifest_prompts
                .contains_key("dev.clat.mcp-fixture")
        );
        assert!(effective.failures.is_empty());
        fs::remove_dir_all(base).expect("cleanup");
    }

    #[test]
    fn relative_mcp_cwd_is_fenced_under_storage_root() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let root = std::env::temp_dir().join(format!("clat-mcp-cwd-{unique}"));
        fs::create_dir_all(root.join("inside")).expect("inside");
        assert_eq!(
            resolve_mcp_cwd(&root, Some("inside")).expect("inside cwd"),
            root.join("inside")
                .canonicalize()
                .expect("canonical inside")
        );
        assert!(resolve_mcp_cwd(&root, Some("../outside")).is_err());
        #[cfg(unix)]
        {
            use std::os::unix::fs::symlink;
            let outside = std::env::temp_dir();
            symlink(outside, root.join("escape")).expect("escape symlink");
            assert!(resolve_mcp_cwd(&root, Some("escape")).is_err());
            fs::remove_file(root.join("escape")).expect("remove escape symlink");
        }
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    #[ignore = "builds a Bun DSH package and spawns its MCP executable"]
    fn generated_dsh_package_installs_and_invokes_through_the_normal_mcp_adapter() {
        let repository = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let adapter = repository.join("sdk/dsh-adapter");
        let cli = adapter.join("dist/src/dsh-cli.js");
        assert!(
            cli.is_file(),
            "build the adapter first: cd sdk/dsh-adapter && npm test"
        );
        assert!(
            std::process::Command::new("bun")
                .arg("--version")
                .output()
                .is_ok_and(|output| output.status.success()),
            "Bun is required for this author-side packaging acceptance"
        );
        assert!(
            std::process::Command::new("minisign")
                .arg("-v")
                .output()
                .is_ok_and(|output| output.status.success()),
            "minisign is required for the signed-package acceptance"
        );
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let base = adapter.join(format!(".tmp-clat-dsh-rust-e2e-{unique}"));
        let source = base.join("source");
        let port = base.join("port");
        let artifact = base.join("artifact");
        let other_artifact = base.join("artifact-other-publisher");
        let storage = base.join("storage");
        let public_key = base.join("publisher.pub");
        let secret_key = base.join("publisher.key");
        let other_public_key = base.join("publisher-other.pub");
        let other_secret_key = base.join("publisher-other.key");
        fs::create_dir_all(source.join("src")).expect("source");
        let generated = std::process::Command::new("minisign")
            .args(["-G", "-W", "-p"])
            .arg(&public_key)
            .arg("-s")
            .arg(&secret_key)
            .output()
            .expect("generate minisign key");
        assert!(
            generated.status.success(),
            "key generation failed: {}",
            String::from_utf8_lossy(&generated.stderr)
        );
        let generated = std::process::Command::new("minisign")
            .args(["-G", "-W", "-p"])
            .arg(&other_public_key)
            .arg("-s")
            .arg(&other_secret_key)
            .output()
            .expect("generate other minisign key");
        assert!(generated.status.success(), "other key generation failed");
        fs::write(
            source.join("package.json"),
            serde_json::to_vec(&serde_json::json!({
                "name": "@fixture/dsh-rust-e2e",
                "version": "1.0.0",
                "type": "module",
                "exports": "./src/index.ts"
            }))
            .expect("package json"),
        )
        .expect("package json");
        fs::write(
            source.join("src/index.ts"),
            r#"export default {
  apply(ctx) {
    ctx.tools.register({
      name: 'fixture_echo',
      description: 'installed DSH fixture',
      parameters: { type: 'object', properties: {} },
      output: { render: (_args, value) => [{ type: 'text', text: JSON.stringify(value) }] },
      execute: async () => ({ ok: true, transport: 'installed-mcp' }),
    })
  },
}
"#,
        )
        .expect("plugin source");
        let run_cli = |arguments: &[String]| {
            let output = std::process::Command::new("node")
                .arg(&cli)
                .args(arguments)
                .current_dir(&adapter)
                .output()
                .expect("run clat-dsh");
            assert!(
                output.status.success(),
                "clat-dsh failed: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        };
        run_cli(&[
            "port".into(),
            source.display().to_string(),
            "--out".into(),
            port.display().to_string(),
        ]);
        run_cli(&[
            "package".into(),
            port.display().to_string(),
            "--out".into(),
            artifact.display().to_string(),
            "--publisher".into(),
            "dev.clat.tests".into(),
            "--publisher-key".into(),
            public_key.display().to_string(),
            "--minisign-key".into(),
            secret_key.display().to_string(),
        ]);
        run_cli(&[
            "package".into(),
            port.display().to_string(),
            "--out".into(),
            other_artifact.display().to_string(),
            "--publisher".into(),
            "dev.clat.other".into(),
            "--publisher-key".into(),
            other_public_key.display().to_string(),
            "--minisign-key".into(),
            other_secret_key.display().to_string(),
        ]);
        {
            let mut store = crate::plugin::PackageStore::open(&storage).expect("store");
            store
                .install(&artifact, None, true, crate::plugin::InstallKind::Install)
                .expect("install generated DSH package");
            assert_eq!(
                store.list()[0].trust,
                crate::plugin::TrustLabel::PublisherVerified
            );
            let error = store
                .install(
                    &other_artifact,
                    None,
                    false,
                    crate::plugin::InstallKind::Update,
                )
                .expect_err("publisher switch must fail");
            assert!(error.contains("publisher identity changed"), "{error}");
        }

        let catalog: Vec<Arc<dyn Plugin>> = vec![
            Arc::new(ToolRegistryPlugin),
            Arc::new(McpAdapterPlugin::new(
                storage.clone(),
                Vec::new(),
                crate::plugin_host::PluginHostBridge::shared(),
            )),
        ];
        let mut manager = PluginManager::root(ScopeKind::TrustedProject);
        manager
            .mount_all(catalog)
            .expect("mount installed MCP package");
        let status = manager.require(MCP_STATUS_SERVICE).expect("status");
        assert!(
            status.wait_until_settled(Duration::from_secs(30)),
            "MCP package startup did not settle"
        );
        assert_eq!(status.snapshot().connected, 1, "{:?}", status.snapshot());
        let tools = manager.require(TOOL_SERVICE).expect("tools");
        let output = tools
            .get("mcp_dsh_fixture_dsh_rust_e2e_fixture_echo")
            .expect("generated DSH tool")
            .invoke(
                &serde_json::json!({}),
                &crate::project::Project::new(&storage),
                &crate::model::CancelToken::new(),
            )
            .expect("invoke installed DSH tool");
        assert!(output.to_string().contains("installed-mcp"), "{output}");
        manager.close().expect("close");
        fs::remove_dir_all(base).expect("cleanup");
    }

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
            "capabilities": {"tools": {}, "prompts": {}},
            "_meta": {"io.modelcontextprotocol/serverInfo": {
                "name": "plugin-test", "version": "1.0"}}}})
    elif method == "tools/list":
        send({"jsonrpc": "2.0", "id": msg["id"], "result": {
            "resultType": "complete", "tools": [{
                "name": "echo", "description": "test tool",
                "inputSchema": {"type": "object"}}]}})
    elif method == "prompts/list":
        send({"jsonrpc": "2.0", "id": msg["id"], "result": {
            "resultType": "complete", "prompts": [{
                "name": "dsh-system-prompt", "description": "fixture prompt",
                "_meta": {"io.artec.clat/dshSystemPrompt": True}}]}})
    elif method == "prompts/get":
        send({"jsonrpc": "2.0", "id": msg["id"], "result": {
            "resultType": "complete", "messages": [],
            "_meta": {"io.artec.clat/systemPrompt": "fixture guidance"}}})
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
            Arc::new(PromptRegistryPlugin),
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
        let prompts = manager.require(PROMPT_SERVICE).expect("prompt registry");
        assert_eq!(prompts.instructions(), "fixture guidance");

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
