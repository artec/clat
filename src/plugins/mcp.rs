use super::services::{
    MCP_STATUS_SERVICE, MCP_STATUS_SERVICE_ID, McpServerStatus, McpStatus, TOOL_SERVICE,
    TOOL_SERVICE_ID,
};
use crate::mcp_client::{McpServer, McpTool, load_mcp_config};
use crate::plugin::{
    DisposeError, Plugin, PluginContext, PluginDescriptor, PluginError, PluginId, ScopeKind,
    ServiceId,
};
use crate::{Tool, ToolRegistry};
use std::path::PathBuf;
use std::sync::Arc;

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
}

impl McpAdapterPlugin {
    pub(crate) fn new(storage_root: PathBuf) -> Self {
        Self { storage_root }
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
        let config = load_mcp_config(&self.storage_root).map_err(PluginError::new)?;
        let mut status = McpStatus {
            configured: config.len(),
            ..McpStatus::default()
        };

        for (name, server_config) in &config {
            if server_config.command.trim().is_empty() {
                status.failures.push(format!("mcp `{name}`: empty command"));
                continue;
            }
            let server = match McpServer::connect(name, server_config, &self.storage_root) {
                Ok(server) => server,
                Err(error) => {
                    status.failures.push(format!("mcp `{name}`: {error}"));
                    continue;
                }
            };
            let infos = match server.list_tools() {
                Ok(infos) => infos,
                Err(error) => {
                    status.failures.push(format!("mcp `{name}`: {error}"));
                    let _ = server.shutdown();
                    continue;
                }
            };
            status.servers.push(McpServerStatus {
                name: name.clone(),
                server_version: server.server_version().to_owned(),
                protocol_version: server.negotiated_version().to_owned(),
            });
            let server = Arc::new(server);
            let shutdown_server = Arc::clone(&server);
            // Registered before leases so LIFO teardown revokes tools first.
            context.defer(move || {
                Arc::try_unwrap(shutdown_server)
                    .map_err(|_| DisposeError::new("MCP server still has active owners"))?
                    .shutdown()
                    .map_err(|error| DisposeError::new(error.to_string()))
            });
            for info in infos {
                let remote_name = info.name.clone();
                let tool: Arc<dyn Tool> = Arc::new(McpTool::new(&server, info));
                if let Err(error) = register_tool(context, &registry, tool) {
                    status
                        .failures
                        .push(format!("mcp `{name}` tool `{remote_name}`: {error}"));
                }
            }
            status.connected += 1;
        }
        context
            .provide(MCP_STATUS_SERVICE, Arc::new(status))
            .map_err(|error| PluginError::new(error.to_string()))
    }
}

fn register_tool(
    context: &mut PluginContext<'_>,
    registry: &Arc<ToolRegistry>,
    tool: Arc<dyn Tool>,
) -> Result<(), String> {
    let lease = registry
        .register(context.owner(), tool)
        .map_err(|error| error.to_string())?;
    context.defer(move || {
        lease
            .revoke()
            .map_err(|error| DisposeError::new(error.to_string()))
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugin::{Plugin, PluginManager};
    use crate::plugins::ToolRegistryPlugin;
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

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
        let script = r#"
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
                "args": ["-u", "-c", script, marker.to_string_lossy()]
            }
        });
        fs::write(
            root.join("mcp.json"),
            serde_json::to_vec(&config).expect("serialize config"),
        )
        .expect("config");

        let catalog: Vec<Arc<dyn Plugin>> = vec![
            Arc::new(ToolRegistryPlugin),
            Arc::new(McpAdapterPlugin::new(root.clone())),
        ];
        let mut manager = PluginManager::root(ScopeKind::TrustedProject);
        manager.mount_all(catalog).expect("mount plugins");
        let registry = manager.require(TOOL_SERVICE).expect("tool registry");
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
}
