//! Real system DNS author check, separate from the component scheduling fixture.
use clat_plg4_http_consumer_host::dns_authority::{Fence, Origin, Run, Scope, SystemDns};
use serde_json::json;
use std::time::Duration;

fn main() -> wasmtime::Result<()> {
    let text = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "https://example.com".into());
    let origin = Origin::parse(&text).map_err(|e| wasmtime::format_err!("{e:?}"))?;
    let run = Run::new(Duration::from_secs(30)).map_err(|e| wasmtime::format_err!("{e:?}"))?;
    let scope = Scope::new(
        &run,
        Fence::new(Some(&[&text]), None).map_err(|e| wasmtime::format_err!("{e:?}"))?,
    );
    let dns = SystemDns::shared().map_err(|e| wasmtime::format_err!("{e:?}"))?;
    let job = dns
        .start(&scope, origin.clone(), Duration::from_secs(30))
        .map_err(|e| wasmtime::format_err!("{e:?}"))?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()?;
    runtime.block_on(job.ready());
    let resolution = job
        .get()
        .ok_or_else(|| wasmtime::format_err!("pending after ready"))?
        .map_err(|e| wasmtime::format_err!("{e:?}"))?;
    drop(job);
    let addresses = resolution
        .addresses(&scope)
        .map_err(|e| wasmtime::format_err!("{e:?}"))?;
    let discovery = resolution
        .discovery_answers(&scope)
        .map_err(|e| wasmtime::format_err!("{e:?}"))?;
    let pins = resolution
        .consume(&scope, &origin)
        .map_err(|e| wasmtime::format_err!("{e:?}"))?;
    wasmtime::ensure!(
        resolution.consume(&scope, &origin).is_err(),
        "single consume"
    );
    wasmtime::ensure!(
        pins.snapshot()
            .map_err(|e| wasmtime::format_err!("{e:?}"))?
            .1
            == addresses,
        "same pins"
    );
    println!(
        "DNS_REAL {}",
        json!({ "host": origin.host(), "resolver": "system getaddrinfo",
        "addresses": addresses, "discovery_domain": (!discovery.is_empty()).then_some("ipv4only.arpa"),
        "discovery_answers": discovery, "workers": 8, "queue_capacity": 8, "single_consume": true,
        "http_connector_executed": false, "production_permission_wired": false })
    );
    Ok(())
}
