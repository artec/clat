use super::*;
pub(crate) fn try_mount(
    adapter: &WasmAdapterPlugin,
    context: &mut PluginContext<'_>,
    name: &str,
    config: &WasmPluginConfig,
) -> Option<Result<usize, String>> {
    let manifest_path = resolve_component_path(&adapter.storage_root, config.manifest.as_deref()?);
    let manifest = PluginPackageManifest::load(&manifest_path).ok()?;
    if manifest.manifest_version != 2 {
        return None;
    }
    Some(mount(
        adapter,
        context,
        name,
        config,
        &manifest,
        &manifest_path,
    ))
}
fn mount(
    adapter: &WasmAdapterPlugin,
    context: &mut PluginContext<'_>,
    name: &str,
    config: &WasmPluginConfig,
    manifest: &PluginPackageManifest,
    manifest_path: &std::path::Path,
) -> Result<usize, String> {
    if !config.dirs.is_empty() {
        return Err("network components cannot receive directory grants".into());
    }
    // Resolve identity, configuration and conflicting pins through the ordinary path.
    resolve_package(&adapter.storage_root, name, config)?;
    let path = manifest.verify_entry_digest(manifest_path)?;
    if std::fs::metadata(&path)
        .map_err(|_| "component unavailable")?
        .len()
        > MAX_COMPONENT_BYTES
    {
        return Err("network component exceeds size limit".into());
    }
    let bytes = verified_bytes(&path, &manifest.runtime.sha256)?;
    let mut runtime = Runtime::new(&bytes)?;
    let declaration = serde_json::to_vec(&manifest.capabilities.network_descriptor())
        .map_err(|_| "invalid declaration")?;
    let narrowing = manifest
        .capabilities
        .network_narrowing(config.config.as_ref())?;
    let plugin_config = config.config.as_ref().map(Value::to_string);
    let run = Run::new(Duration::from_secs(30)).map_err(|_| "discovery unavailable")?;
    let invocation = Invocation::new(
        &run,
        &declaration,
        narrowing.as_deref(),
        Arc::new(Discovery),
        clat_wasm_net::CancelToken::from_check(|| false),
        Instant::now() + Duration::from_secs(30),
        plugin_config.clone(),
    )?;
    let definitions = runtime.list(invocation)?;
    if definitions.len() > MAX_PLUGIN_TOOLS {
        return Err("network plugin exposes too many tools".into());
    }
    let (definitions, diagnostics) = sanitize_definitions(name, definitions);
    if !diagnostics.is_empty() {
        return Err("network plugin has invalid tool metadata".into());
    }
    if !manifest.capabilities.tools && !definitions.is_empty() {
        return Err("manifest does not declare tools".into());
    }
    let alive = Arc::new(AtomicBool::new(true));
    let instance = Arc::new(Instance {
        runtime: Mutex::new(runtime),
        bridge: adapter.host.clone(),
        name: name.to_owned(),
        declaration,
        narrowing,
        config: plugin_config,
        run: Mutex::new(None),
        alive: alive.clone(),
    });
    register(context, name, definitions, instance, alive)
}
fn register(
    context: &mut PluginContext<'_>,
    name: &str,
    definitions: Vec<ToolDefinition>,
    instance: Arc<Instance>,
    alive: Arc<AtomicBool>,
) -> Result<usize, String> {
    let registry = context
        .require(TOOL_SERVICE)
        .map_err(|_| "tool registry unavailable")?;
    let mut registration = Registration {
        leases: Vec::new(),
        alive,
    };
    for definition in definitions {
        let remote_name = definition.name.clone();
        let qualified = qualify_prefixed_tool_name("wasm", name, &remote_name)
            .ok_or("invalid network tool name")?;
        let tool = NetworkTool {
            remote_name,
            definition: ToolDefinition {
                name: qualified,
                ..definition
            },
            instance: instance.clone(),
        };
        registration.leases.push(
            registry
                .register(context.owner(), Arc::new(tool))
                .map_err(|_| "network tool registration failed")?,
        );
    }
    let count = registration.leases.len();
    context.defer(move || {
        drop(registration);
        drop(instance);
        Ok(())
    });
    Ok(count)
}

struct Registration {
    leases: Vec<crate::tool::ToolLease>,
    alive: Arc<AtomicBool>,
}
impl Drop for Registration {
    fn drop(&mut self) {
        self.alive.store(false, AtomicOrdering::Release);
        for lease in self.leases.drain(..).rev() {
            let _ = lease.revoke();
        }
    }
}

fn verified_bytes(path: &std::path::Path, expected: &str) -> Result<Vec<u8>, String> {
    use sha2::Digest;
    use std::io::Read;
    let file = std::fs::File::open(path).map_err(|_| "component unavailable")?;
    let mut bytes = Vec::new();
    file.take(MAX_COMPONENT_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| "component unavailable")?;
    if bytes.len() as u64 > MAX_COMPONENT_BYTES {
        return Err("network component exceeds size limit".into());
    }
    let digest = format!("{:x}", sha2::Sha256::digest(&bytes));
    if !digest.eq_ignore_ascii_case(expected.trim().trim_start_matches("sha256:")) {
        return Err("network component changed before compilation".into());
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn plg4_compilation_hashes_the_actual_bytes_not_a_previous_open() {
        use sha2::Digest;
        let path = std::env::temp_dir().join(format!("clat-net-bytes-{}", uuid::Uuid::new_v4()));
        let digest = format!("{:x}", sha2::Sha256::digest(b"approved"));
        std::fs::write(&path, b"approved").unwrap();
        assert_eq!(verified_bytes(&path, &digest).unwrap(), b"approved");
        std::fs::write(&path, b"replaced").unwrap();
        assert!(verified_bytes(&path, &digest).is_err());
        std::fs::remove_file(path).unwrap();
    }
}
