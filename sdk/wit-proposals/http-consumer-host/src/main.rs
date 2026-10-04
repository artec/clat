//! Standalone author harness. Neither registered nor depended upon by CLAT.
mod events;
mod transport;
use serde_json::{Value, json};
use std::sync::Arc;
use std::time::Instant;
use transport::{EventLog, Events, Hooks};
use wasmtime::Result;
use wasmtime::component::{Component, Linker, ResourceTable};
use wasmtime::{Engine, Store};
use wasmtime_wasi::{WasiCtx, WasiCtxView, WasiView};
use wasmtime_wasi_http::{WasiHttpCtx, WasiHttpCtxView, WasiHttpView};

wasmtime::component::bindgen!({ path: "../http-consumer", world: "consumer" });

struct State {
    table: ResourceTable,
    wasi: WasiCtx,
    http: WasiHttpCtx,
    hooks: Hooks,
}
impl WasiView for State {
    fn ctx(&mut self) -> WasiCtxView<'_> {
        WasiCtxView {
            ctx: &mut self.wasi,
            table: &mut self.table,
        }
    }
}
impl WasiHttpView for State {
    fn http(&mut self) -> WasiHttpCtxView<'_> {
        WasiHttpCtxView {
            ctx: &mut self.http,
            table: &mut self.table,
            hooks: &mut self.hooks,
        }
    }
}

fn state(events: Events) -> State {
    State {
        table: ResourceTable::new(),
        wasi: WasiCtx::builder().build(),
        http: WasiHttpCtx::new(),
        hooks: Hooks { events },
    }
}

fn case(
    engine: &Engine,
    component: &Component,
    linker: &Linker<State>,
    stage: &str,
    order: &str,
    trigger: &str,
) -> Result<Value> {
    let events = Arc::new(EventLog::default());
    let mut store = Store::new(engine, state(events.clone()));
    store.set_fuel(100_000_000_000)?;
    let instance = Consumer::instantiate(&mut store, component, linker)?;
    let mut result = invoke(&mut store, &instance, &events, stage, order, trigger)?;
    drop(store);
    result["host_after_store_drop"] = json!(events.snapshot());
    Ok(result)
}

fn invoke(
    store: &mut Store<State>,
    instance: &Consumer,
    events: &Events,
    stage: &str,
    order: &str,
    trigger: &str,
) -> Result<Value> {
    let started = Instant::now();
    let guest: Value =
        serde_json::from_str(&instance.call_run(&mut *store, stage, order, trigger)?)?;
    // A scheduled Tokio abort needs terminal observation while Store stays alive.
    if order == "during" {
        events.wait_terminal(if stage.starts_with("body") {
            "entity"
        } else {
            stage
        });
    }
    Ok(
        json!({ "guest": guest, "elapsed_ms": started.elapsed().as_millis(),
        "host_before_store_drop": events.snapshot() }),
    )
}

fn sequence(engine: &Engine, component: &Component, linker: &Linker<State>) -> Result<Vec<Value>> {
    let mut store = Store::new(engine, state(Arc::new(EventLog::default())));
    store.set_fuel(100_000_000_000)?;
    let instance = Consumer::instantiate(&mut store, component, linker)?;
    let mut results = Vec::new();
    for (stage, order, trigger) in [
        ("body-parallel", "during", "timer"),
        ("body", "during", "timer"),
        ("headers", "during", "promise"),
        ("body", "none", "promise"),
        ("headers", "none", "promise"),
        ("body", "after", "promise"),
    ] {
        let events = Arc::new(EventLog::default());
        store.data_mut().hooks.events = events.clone();
        let result = invoke(&mut store, &instance, &events, stage, order, trigger)?;
        println!("HTTP_SEQUENCE {result}");
        results.push(result);
    }
    Ok(results)
}

fn main() -> Result<()> {
    let path = std::env::args()
        .nth(1)
        .ok_or_else(|| wasmtime::format_err!("usage: host COMPONENT [--require-cancel]"))?;
    let require = std::env::args().any(|arg| arg == "--require-cancel");
    let mut config = wasmtime::Config::new();
    config.consume_fuel(true);
    let engine = Engine::new(&config)?;
    let component = Component::from_file(&engine, path)?;
    let mut linker = Linker::new(&engine);
    wasmtime_wasi_http::p2::add_to_linker_sync(&mut linker)?;
    let mut results = Vec::new();
    for stage in ["dns", "headers", "body"] {
        for (order, trigger) in [
            ("none", "promise"),
            ("before", "promise"),
            ("during", "promise"),
            ("during", "timer"),
            ("after", "promise"),
        ] {
            let result = case(&engine, &component, &linker, stage, order, trigger)?;
            println!("HTTP_CONSUMER {result}");
            results.push(result);
        }
    }
    if std::env::args().any(|arg| arg == "--reuse-store") {
        results.extend(sequence(&engine, &component, &linker)?);
    }
    if require {
        wasmtime::ensure!(
            results.iter().all(cancellation_matches),
            "consumer pre-red: native fetch must honor before/during cancellation and cancel host work before Store drop"
        );
    }
    Ok(())
}

fn cancellation_matches(result: &Value) -> bool {
    let guest = &result["guest"];
    let order = guest["order"].as_str().unwrap();
    if matches!(order, "none" | "after") {
        return guest["outcome"]["kind"] == "success"
            && guest["outcome"]["body"] == "ok"
            && guest["aborted"] == (order == "after");
    }
    if guest["outcome"]["kind"] != "error"
        || guest["outcome"]["name"] != "AbortError"
        || guest["aborted"] != true
    {
        return false;
    }
    let events = result["host_before_store_drop"].as_array().unwrap();
    if order == "before" {
        return events.is_empty();
    }
    let phase = if guest["stage"].as_str().unwrap().starts_with("body") {
        "entity"
    } else {
        guest["stage"].as_str().unwrap()
    };
    events.contains(&json!(format!("{phase}:cancel")))
        && !events.contains(&json!(format!("{phase}:complete")))
        && result["elapsed_ms"].as_u64().unwrap() <= 250
}
