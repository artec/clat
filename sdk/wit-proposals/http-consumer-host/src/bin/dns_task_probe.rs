//! Real system DNS on the accepted native scheduling component; no HTTP mapping.
pub use clat_plg4_http_consumer_host::dns_authority::DnsJob;
use clat_plg4_http_consumer_host::dns_authority::{
    Fence, Origin, Resolution, Run, Scope, SystemDns,
};
use serde_json::{Value, json};
use std::sync::Arc;
use std::time::Duration;
use wasmtime::component::{Component, Linker, Resource, ResourceTable};
use wasmtime::{Engine, Result, Store};
use wasmtime_wasi::p2::{DynPollable, subscribe};
use wasmtime_wasi::{WasiCtx, WasiCtxView, WasiView};

wasmtime::component::bindgen!({
    path: "../task-consumer", world: "consumer", imports: { default: trappable },
    with: { "wasi:io/poll@0.2.10": wasmtime_wasi::p2::bindings::sync::io::poll,
        "clat:net-task-probe/tasks.job": DnsJob },
});
use clat::net_task_probe::tasks::{Failure, Host, HostJob, Phase};
struct State {
    table: ResourceTable,
    wasi: WasiCtx,
    scope: Scope,
    _run: Run,
    dns: Arc<SystemDns>,
    origin: Origin,
    resolution: Option<Resolution>,
    events: Vec<&'static str>,
    policy_failure: Option<clat_plg4_http_consumer_host::dns_authority::Failure>,
}
impl WasiView for State {
    fn ctx(&mut self) -> WasiCtxView<'_> {
        WasiCtxView {
            ctx: &mut self.wasi,
            table: &mut self.table,
        }
    }
}
fn failure(error: clat_plg4_http_consumer_host::dns_authority::Failure) -> Failure {
    use clat_plg4_http_consumer_host::dns_authority::Failure as E;
    match error {
        E::Cancelled => Failure::Cancelled,
        E::DeadlineExceeded => Failure::DeadlineExceeded,
        E::LimitExceeded => Failure::LimitExceeded,
        // The scheduling-only WIT cannot represent the final policy error enum.
        // Never collapse a policy rejection into a successful guest result.
        _ => Failure::LimitExceeded,
    }
}
impl Host for State {
    fn start(&mut self, phase: Phase) -> Result<Result<Resource<DnsJob>, Failure>> {
        if !matches!(phase, Phase::Dns) {
            return Ok(Err(Failure::LimitExceeded));
        }
        self.events.push("dns:start");
        let job = match self
            .dns
            .start(&self.scope, self.origin.clone(), Duration::from_secs(30))
        {
            Ok(job) => job,
            Err(error) => return Ok(Err(failure(error))),
        };
        Ok(Ok(self.table.push(job)?))
    }
}
impl HostJob for State {
    fn get(&mut self, job: Resource<DnsJob>) -> Result<Option<Result<String, Failure>>> {
        let result = match self.table.get(&job)?.get() {
            None => return Ok(None),
            Some(result) => result,
        };
        match result {
            Ok(resolution) => {
                wasmtime::ensure!(
                    self.resolution.is_none(),
                    "one private grant per probe call"
                );
                self.resolution = Some(resolution);
                self.events.push("dns:complete");
                Ok(Some(Ok("ok".into())))
            }
            Err(error) => {
                self.policy_failure = Some(error);
                Ok(Some(Err(failure(error))))
            }
        }
    }
    fn subscribe(&mut self, job: Resource<DnsJob>) -> Result<Resource<DynPollable>> {
        subscribe(&mut self.table, job)
    }
    fn cancel(&mut self, job: Resource<DnsJob>) -> Result<()> {
        self.table.get(&job)?.cancel();
        self.events.push("dns:cancel");
        Ok(())
    }
    fn drop(&mut self, job: Resource<DnsJob>) -> Result<()> {
        self.table.delete(job)?;
        Ok(())
    }
}
fn state(text: &str) -> Result<State> {
    let origin = Origin::parse(text).map_err(|e| wasmtime::format_err!("{e:?}"))?;
    let run = Run::new(Duration::from_secs(120)).map_err(|e| wasmtime::format_err!("{e:?}"))?;
    let scope = Scope::new(
        &run,
        Fence::new(Some(&[text]), None).map_err(|e| wasmtime::format_err!("{e:?}"))?,
    );
    Ok(State {
        table: ResourceTable::new(),
        wasi: WasiCtx::builder().build(),
        scope,
        _run: run,
        dns: SystemDns::shared().map_err(|e| wasmtime::format_err!("{e:?}"))?,
        origin,
        resolution: None,
        events: vec![],
        policy_failure: None,
    })
}
fn invoke(
    store: &mut Store<State>,
    instance: &Consumer,
    order: &str,
    expect_blocked: bool,
) -> Result<()> {
    store.data_mut().events.clear();
    store.data_mut().policy_failure = None;
    let guest: Value =
        serde_json::from_str(&instance.call_run(&mut *store, "dns", order, "promise")?)?;
    wasmtime::ensure!(
        store.data().table.is_empty(),
        "native job/pollable must close before Store drop"
    );
    let resolution = store.data_mut().resolution.take();
    let evidence = if expect_blocked && matches!(order, "none" | "after") {
        wasmtime::ensure!(
            guest["outcome"]["kind"] == "error" && resolution.is_none(),
            "blocked DNS grants nothing"
        );
        wasmtime::ensure!(
            store.data().policy_failure
                == Some(clat_plg4_http_consumer_host::dns_authority::Failure::BlockedAddress),
            "actual address policy rejection, not a linker/compiler error"
        );
        json!({ "policy_failure": "BlockedAddress", "private_grant": false })
    } else if matches!(order, "none" | "after") {
        wasmtime::ensure!(guest["outcome"]["body"] == "ok", "real DNS completion");
        let resolution =
            resolution.ok_or_else(|| wasmtime::format_err!("missing private grant"))?;
        let state = store.data();
        let addresses = resolution
            .addresses(&state.scope)
            .map_err(|e| wasmtime::format_err!("{e:?}"))?;
        let discovery = resolution
            .discovery_answers(&state.scope)
            .map_err(|e| wasmtime::format_err!("{e:?}"))?;
        let pins = resolution
            .consume(&state.scope, &state.origin)
            .map_err(|e| wasmtime::format_err!("{e:?}"))?;
        wasmtime::ensure!(
            resolution.consume(&state.scope, &state.origin).is_err(),
            "single consume"
        );
        wasmtime::ensure!(
            pins.snapshot()
                .map_err(|e| wasmtime::format_err!("{e:?}"))?
                .1
                == addresses,
            "same host pins"
        );
        json!({ "addresses": addresses, "discovery_answers": discovery, "single_consume": true })
    } else {
        wasmtime::ensure!(
            guest["outcome"]["name"] == "AbortError" && resolution.is_none(),
            "cancel grants nothing"
        );
        if order == "before" {
            wasmtime::ensure!(store.data().events.is_empty(), "pre-abort no submit");
        } else {
            wasmtime::ensure!(
                store.data().events.contains(&"dns:cancel"),
                "native host cancellation"
            );
        }
        Value::Null
    };
    println!(
        "DNS_NATIVE {}",
        json!({ "guest": guest, "host": store.data().events,
        "resource_table_empty": true, "real_system_dns": true, "evidence": evidence })
    );
    Ok(())
}
fn main() -> Result<()> {
    let path = std::env::args()
        .nth(1)
        .ok_or_else(|| wasmtime::format_err!("usage: dns_task_probe COMPONENT [ORIGIN]"))?;
    let origin = std::env::args()
        .nth(2)
        .unwrap_or_else(|| "https://example.com".into());
    let expect_blocked = std::env::args().nth(3).as_deref() == Some("--expect-blocked");
    let mut config = wasmtime::Config::new();
    config.consume_fuel(true);
    let engine = Engine::new(&config)?;
    let component = Component::from_file(&engine, path)?;
    let mut linker = Linker::new(&engine);
    wasmtime_wasi::p2::add_to_linker_sync(&mut linker)?;
    clat::net_task_probe::tasks::add_to_linker::<_, wasmtime::component::HasSelf<_>>(
        &mut linker,
        |s| s,
    )?;
    let mut store = Store::new(&engine, state(&origin)?);
    store.set_fuel(100_000_000_000)?;
    let instance = Consumer::instantiate(&mut store, &component, &linker)?;
    for order in ["none", "before", "during", "none", "after"] {
        invoke(&mut store, &instance, order, expect_blocked)?;
    }
    Ok(())
}
