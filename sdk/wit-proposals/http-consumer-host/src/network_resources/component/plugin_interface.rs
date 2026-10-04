//! Existing CLAT tools/config contract on the isolated network author lane.
use super::HostState;
use wasmtime::component::Linker;
wasmtime::component::bindgen!({
    path: "../../dsh-wasm-flavor/plugin-wit", world: "tools-consumer",
    imports: { default: trappable },
});
pub(super) fn link(linker: &mut Linker<HostState>) -> wasmtime::Result<()> {
    clat::plugin::config::add_to_linker::<_, wasmtime::component::HasSelf<_>>(linker, |s| s)
}
impl clat::plugin::config::Host for HostState {
    fn get(&mut self) -> wasmtime::Result<Result<String, String>> {
        self.owner.check().map_err(super::trap)?;
        Ok(self
            .config
            .clone()
            .ok_or_else(|| "plugin configuration missing".into()))
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::network_resources::component::{invocation::Lane, tests::host};
    #[test]
    #[ignore = "requires original quartet tools component; no actual DNS"]
    fn original_quartet_uses_existing_tools_contract() {
        let path = std::env::var("PLG4_TOOLS_COMPONENT").expect("build tools component");
        let mut lane = Lane::new(std::fs::read(path).unwrap()).unwrap();
        let (_run, mut host, calls) = host("https://typed.example.com");
        host.config =
            Some(serde_json::json!({"origins":["https://undeclared.invalid"]}).to_string());
        lane.invoke(host, |store, component, linker| {
            let instance = ToolsConsumer::instantiate(&mut *store, component, linker)?;
            let tools = instance.clat_plugin_tools().call_list_tools(&mut *store);
            if tools.is_err() {
                println!(
                    "TOOLS_DIAGNOSTIC {}",
                    String::from_utf8_lossy(&store.data().owner.diagnostics.contents())
                )
            }
            let tools = tools?;
            assert_eq!(
                tools.iter().map(|t| t.name.as_str()).collect::<Vec<_>>(),
                ["web_search", "web_fetch"]
            );
            for tool in &tools {
                assert!(matches!(
                    tool.effect,
                    exports::clat::plugin::tools::Effect::Network
                ));
                let schema: serde_json::Value = serde_json::from_str(&tool.input_schema).unwrap();
                assert_eq!(schema["type"], "object");
            }
            let error = instance
                .clat_plugin_tools()
                .call_call(
                    &mut *store,
                    "web_fetch",
                    r#"{"url":"https://undeclared.invalid/path"}"#,
                )?
                .unwrap_err();
            assert!(error.contains("capability-denied"), "{error}");
            assert_eq!(store.data().dns_submitted, 1);
            assert_eq!(store.data().http_submitted, 0);
            let unknown = instance
                .clat_plugin_tools()
                .call_call(&mut *store, "unknown", "{}")?
                .unwrap_err();
            assert!(!unknown.is_empty());
            Ok(())
        })
        .unwrap();
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 0);
    }
}
