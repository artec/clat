//! One legacy contribution owner, shared by the adapter's per-package mount paths.
use super::*;

pub(super) struct LegacyMount {
    registry: Arc<crate::tool::ToolRegistry>,
    prompts: Option<Arc<crate::plugins::services::PromptRegistry>>,
    status: Option<Arc<crate::plugins::services::McpStatus>>,
    instances: Vec<Arc<WasmInstance>>,
    tools: Vec<crate::tool::ToolLease>,
    prompt_leases: Vec<crate::plugins::services::PromptLease>,
}
impl LegacyMount {
    pub(super) fn new(
        registry: Arc<crate::tool::ToolRegistry>,
        prompts: Option<Arc<crate::plugins::services::PromptRegistry>>,
        status: Option<Arc<crate::plugins::services::McpStatus>>,
    ) -> Self {
        Self {
            registry,
            prompts,
            status,
            instances: vec![],
            tools: vec![],
            prompt_leases: vec![],
        }
    }
    pub(super) fn observe_component(&self, name: &str, result: Result<usize, String>) {
        match result {
            Ok(tools) => self.connected(name, WASMTIME_VERSION, "clat-plugin-manifest/2", tools),
            Err(error) => self.failed(name, error),
        }
    }
    pub(super) fn failed(&self, name: &str, error: impl std::fmt::Display) {
        if let Some(status) = &self.status {
            status.record_capability_failed(
                "wasm",
                name,
                "component_unavailable",
                format!("wasm `{name}`: {error}"),
            );
        }
    }
    fn diagnostic(&self, message: String) {
        if let Some(status) = &self.status {
            status.record_failure(message);
        }
    }
    fn connected(&self, name: &str, version: &str, protocol: &str, tools: usize) {
        if let Some(status) = &self.status {
            status.record_connected(McpServerStatus {
                name: name.into(),
                server_version: version.into(),
                protocol_version: protocol.into(),
                tools,
                transport: "wasm".into(),
            });
        }
    }
    pub(super) fn mount(
        &mut self,
        adapter: &WasmAdapterPlugin,
        owner: crate::plugin::PluginOwner,
        name: &str,
        compiled: CompiledPlugin,
        engine: &wasmtime::Engine,
        linker: &Arc<wasmtime::component::Linker<PluginState>>,
    ) {
        for diagnostic in &compiled.diagnostics {
            self.diagnostic(diagnostic.clone());
        }
        self.contribute_prompts(owner, name, &compiled.prompts);
        let version = compiled.package_version.clone();
        let definitions = compiled.definitions.clone();
        let shared = adapter.legacy_instance(name, compiled, engine, linker);
        let tools = self.contribute_tools(owner, name, &shared, definitions);
        self.instances.push(shared);
        self.connected(
            name,
            version.as_deref().unwrap_or(WASMTIME_VERSION),
            if version.is_some() {
                "clat-plugin-manifest/1"
            } else {
                WIT_PROTOCOL
            },
            tools,
        );
    }
    fn contribute_prompts(
        &mut self,
        owner: crate::plugin::PluginOwner,
        name: &str,
        prompts: &[crate::plugin::ManifestPrompt],
    ) {
        if let Some(registry) = self.prompts.clone() {
            for prompt in prompts.iter().filter(|p| !p.system.trim().is_empty()) {
                match registry.contribute(owner, prompt.system.clone()) {
                    Ok(lease) => self.prompt_leases.push(lease),
                    Err(error) => {
                        self.diagnostic(format!("wasm `{name}` prompt `{}`: {error}", prompt.name))
                    }
                }
            }
        } else if !prompts.is_empty() {
            self.diagnostic(format!(
                "wasm `{name}` manifest declares prompts but PromptRegistry is unavailable"
            ));
        }
    }
    fn contribute_tools(
        &mut self,
        owner: crate::plugin::PluginOwner,
        name: &str,
        shared: &Arc<WasmInstance>,
        definitions: Vec<ToolDefinition>,
    ) -> usize {
        let mut tools = 0;
        for definition in definitions {
            let remote_name = definition.name.clone();
            let Some(qualified) = qualify_prefixed_tool_name("wasm", name, &remote_name) else {
                self.diagnostic(format!(
                    "wasm `{name}` tool `{remote_name}`: empty name segment"
                ));
                continue;
            };
            let tool = WasmTool {
                remote_name,
                definition: ToolDefinition {
                    name: qualified,
                    ..definition
                },
                instance: shared.clone(),
            };
            match self.registry.register(owner, Arc::new(tool)) {
                Ok(lease) => {
                    self.tools.push(lease);
                    tools += 1;
                }
                Err(error) => self.diagnostic(format!("wasm `{name}` tool registration: {error}")),
            }
        }
        tools
    }
    pub(super) fn close(self) {
        close_tool_contributions(self.tools, self.prompt_leases, self.instances);
    }
}
impl WasmAdapterPlugin {
    fn legacy_instance(
        &self,
        name: &str,
        compiled: CompiledPlugin,
        engine: &wasmtime::Engine,
        linker: &Arc<wasmtime::component::Linker<PluginState>>,
    ) -> Arc<WasmInstance> {
        let fs_cap = compiled.definitions.iter().any(|def| {
            matches!(
                def.effect,
                ToolEffect::Write | ToolEffect::Execute | ToolEffect::Destructive
            )
        });
        Arc::new(WasmInstance {
            name: name.into(),
            engine: engine.clone(),
            linker: linker.clone(),
            component: Arc::new(compiled.component),
            host: self.host.clone(),
            grants: GrantPolicy {
                mode: self.permission_mode.clone(),
                project_root: self.project_root.clone(),
                fs_cap,
                extra_dirs: compiled.extra_dirs,
            },
            config: compiled.config,
            capabilities: compiled.capabilities,
            fuel: self.fuel,
            digest: compiled.digest,
            grants_path: wasm_grants::grants_path(&self.storage_root),
            write_state: Mutex::new(None),
            slot: Mutex::new(None),
        })
    }
}
