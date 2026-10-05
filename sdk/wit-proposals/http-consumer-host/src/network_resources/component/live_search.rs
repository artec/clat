//! Explicitly armed live-provider measurements; system DNS is independently waived.
use super::*;
use super::{invocation::Lane, plugin_interface::ToolsConsumer};
use crate::CancelToken;
use crate::dns_authority::Run;
use crate::http_authority::permission::Gate;
use std::net::IpAddr;

#[test]
#[ignore = "requires private PLG4_LIVE_CONFIG and externally supplied public address sets"]
fn original_search_only_live_provider_repeated_samples() {
    let config: serde_json::Value = serde_json::from_slice(
        &std::fs::read(std::env::var("PLG4_LIVE_CONFIG").expect("private config path")).unwrap(),
    )
    .unwrap();
    let key = config["apiKey"].as_str().expect("private API key");
    let mut addresses = Vec::new();
    for name in ["PLG4_LIVE_A", "PLG4_LIVE_AAAA"] {
        let packet: serde_json::Value =
            serde_json::from_slice(&std::fs::read(std::env::var(name).unwrap()).unwrap()).unwrap();
        assert_eq!(packet["Status"], 0);
        for answer in packet["Answer"].as_array().into_iter().flatten() {
            if matches!(answer["type"].as_u64(), Some(1 | 28)) {
                addresses.push(answer["data"].as_str().unwrap().parse::<IpAddr>().unwrap());
            }
        }
    }
    assert!(!addresses.is_empty());
    let bytes = std::fs::read(std::env::var("PLG4_SEARCH_COMPONENT").unwrap()).unwrap();
    let size = bytes.len();
    let start = Instant::now();
    let mut lane = Lane::new(bytes).unwrap();
    let compile_ms = start.elapsed().as_millis();
    let mut samples = Vec::new();
    for index in 0..5 {
        let run = Run::new(Duration::from_secs(120)).unwrap();
        let host = live_host(&run, addresses.clone(), &config);
        let start = Instant::now();
        let (fuel, result_bytes) = lane
            .invoke(host, |store, component, linker| {
                let plugin = ToolsConsumer::instantiate(&mut *store, component, linker)?;
                let result = plugin.clat_plugin_tools().call_call(
                    &mut *store,
                    "web_search",
                    r#"{"queries":["Rust programming language official website"]}"#,
                )?;
                assert!(
                    result.is_ok(),
                    "real provider search must succeed (details suppressed)"
                );
                let result = result.unwrap();
                assert!(!result.contains(key), "secret must not enter tool result");
                assert!(
                    result.contains("https://"),
                    "real search must return source URLs"
                );
                Ok((100_000_000_000 - store.get_fuel()?, result.len()))
            })
            .expect("live component execution must succeed; no guest diagnostics emitted");
        samples.push(serde_json::json!({"sample":index,"fuel":fuel,"wallUs":start.elapsed().as_micros(),"resultBytes":result_bytes}));
    }
    let report = serde_json::json!({"scope":"real DeepSeek HTTPS search; test-injected external A/AAAA, system DNS waived; production connector and TLS","componentBytes":size,"compileMs":compile_ms,"samples":samples});
    println!("LIVE_SEARCH_SAMPLES {report}");
    std::fs::write(
        std::env::var("PLG4_SEARCH_SAMPLES_OUT").unwrap(),
        report.to_string() + "\n",
    )
    .unwrap();
}

fn live_host(run: &Run, addresses: Vec<IpAddr>, config: &serde_json::Value) -> HostState {
    use clat_core::{PermissionDecision, PermissionMode, Project};
    let origin = "https://api.deepseek.com";
    let mut declaration = crate::capabilities::tests::descriptor(origin, true);
    declaration["capabilities"]["network"]["origins"][0]["methods"] = serde_json::json!(["POST"]);
    let policy =
        crate::capabilities::Policy::parse(&serde_json::to_vec(&declaration).unwrap(), None)
            .unwrap();
    let gate = Gate::for_test(
        PermissionMode::FullAccess,
        false,
        Arc::new(|_: clat_core::PermissionRequest, _: &CancelToken| PermissionDecision::Allow),
        Project::new("."),
    );
    let mut host = HostState::from_policy(
        run,
        Arc::new(gate),
        CancelToken::new(),
        Instant::now() + Duration::from_secs(60),
        SystemDns::fake(move |_| Ok(addresses.clone())),
        policy,
    )
    .unwrap();
    host.config = Some(config.to_string());
    host
}
