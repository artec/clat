//! Manifest generation and signed semantic-network ceiling.
use super::*;
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct NetworkCapability {
    pub protocol: String,
    pub origins: Vec<NetworkOrigin>,
}
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct NetworkOrigin {
    pub scheme: String,
    pub host: String,
    pub port: u16,
    pub methods: Vec<String>,
}
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ClockCapability {
    pub protocol: String,
}
pub(super) fn present<'de, T: Deserialize<'de>, D: serde::Deserializer<'de>>(
    d: D,
) -> Result<Option<T>, D::Error> {
    T::deserialize(d).map(Some)
}
impl PluginCapabilities {
    pub(crate) fn network_descriptor(&self) -> Value {
        let mut caps = serde_json::json!({"network":self.network,"sampling":self.sampling,"hostTools":self.host_tools});
        if let Some(clock) = &self.clock {
            caps["clock"] = serde_json::to_value(clock).expect("clock JSON");
        }
        serde_json::json!({"manifestVersion":2,"capabilities":caps})
    }
    pub(crate) fn network_labels(&self) -> Vec<String> {
        let mut labels = Vec::new();
        if let Some(network) = &self.network {
            for origin in &network.origins {
                for method in &origin.methods {
                    labels.push(format!(
                        "network.{}://{}:{}/{}",
                        origin.scheme, origin.host, origin.port, method
                    ));
                }
            }
        }
        if let Some(clock) = &self.clock {
            labels.push(format!("clock.{}", clock.protocol));
        }
        labels
    }
    pub(crate) fn validate_network(&self, config: Option<&Value>) -> Result<(), String> {
        let Some(_) = self.network else {
            return Ok(());
        };
        let declaration =
            serde_json::to_vec(&self.network_descriptor()).map_err(|_| "invalid declaration")?;
        let narrowing = self.network_narrowing(config)?;
        clat_wasm_net::runtime::validate_policy(&declaration, narrowing.as_deref())
    }
    pub(crate) fn network_narrowing(
        &self,
        config: Option<&Value>,
    ) -> Result<Option<Vec<u8>>, String> {
        let Some(policy) = config.and_then(|c| c.get("networkPolicy")) else {
            return Ok(None);
        };
        serde_json::to_vec(policy)
            .map(Some)
            .map_err(|_| "invalid networkPolicy".into())
    }
}
pub(super) fn validate_generation(manifest: &PluginPackageManifest) -> Result<(), String> {
    match manifest.manifest_version {
        MANIFEST_VERSION
            if manifest.capabilities.network.is_none() && manifest.capabilities.clock.is_none() =>
        {
            Ok(())
        }
        2 if manifest.runtime.kind == PluginRuntimeKind::WasmComponent
            && manifest.capabilities.network.is_some()
            && manifest.runtime.args.is_empty()
            && !manifest.capabilities.sampling
            && !manifest.capabilities.elicitation
            && !manifest.capabilities.host_context
            && !manifest.capabilities.prompts
            && manifest.prompts.is_empty() =>
        {
            manifest.capabilities.validate_network(None)
        }
        _ => Err("unsupported manifest generation or runtime/capability combination".into()),
    }
}
