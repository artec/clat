//! Native semantic-task fixture host. No resolver/connector or production grants.
#[path = "../events.rs"]
mod events;
use events::{EventLog, Events};
use serde_json::{Value, json};
use std::sync::Arc;
use std::time::{Duration, Instant};
use wasmtime::Result;
use wasmtime::component::{Component, Linker, Resource, ResourceTable};
use wasmtime::{Engine, Store};
use wasmtime_wasi::p2::{DynPollable, Pollable, subscribe};
use wasmtime_wasi::{WasiCtx, WasiCtxView, WasiView};

wasmtime::component::bindgen!({
    path: "../task-consumer", world: "consumer",
    imports: { default: trappable },
    with: {
        "wasi:io/poll@0.2.10": wasmtime_wasi::p2::bindings::sync::io::poll,
        "clat:net-task-probe/tasks.job": Job,
    },
});
use clat::net_task_probe::tasks::{Failure, Host, HostJob, Phase};

struct State {
    table: ResourceTable,
    wasi: WasiCtx,
    events: Events,
}
impl WasiView for State {
    fn ctx(&mut self) -> WasiCtxView<'_> {
        WasiCtxView {
            ctx: &mut self.wasi,
            table: &mut self.table,
        }
    }
}
pub struct Job {
    deadline: Instant,
    events: Events,
    phase: &'static str,
    terminal: bool,
}
impl Job {
    fn cancel(&mut self) {
        if !self.terminal {
            self.events.push(format!("{}:cancel", self.phase));
            self.terminal = true;
        }
    }
}
impl Drop for Job {
    fn drop(&mut self) {
        self.cancel();
    }
}
#[wasmtime_wasi::async_trait]
impl Pollable for Job {
    async fn ready(&mut self) {
        tokio::time::sleep_until(self.deadline.into()).await;
    }
}
impl Host for State {
    fn start(&mut self, phase: Phase) -> Result<Result<Resource<Job>, Failure>> {
        let phase = match phase {
            Phase::Dns => "dns",
            Phase::Headers => "headers",
            Phase::Body => "entity",
        };
        self.events.push(format!("{phase}:start"));
        Ok(Ok(self.table.push(Job {
            deadline: Instant::now() + Duration::from_millis(100),
            events: self.events.clone(),
            phase,
            terminal: false,
        })?))
    }
}
impl HostJob for State {
    fn get(&mut self, job: Resource<Job>) -> Result<Option<Result<String, Failure>>> {
        let job = self.table.get_mut(&job)?;
        if job.terminal {
            return Ok(Some(Err(Failure::Cancelled)));
        }
        if Instant::now() < job.deadline {
            return Ok(None);
        }
        job.terminal = true;
        job.events.push(format!("{}:complete", job.phase));
        Ok(Some(Ok("ok".into())))
    }
    fn subscribe(&mut self, job: Resource<Job>) -> Result<Resource<DynPollable>> {
        subscribe(&mut self.table, job)
    }
    fn cancel(&mut self, job: Resource<Job>) -> Result<()> {
        self.table.get_mut(&job)?.cancel();
        Ok(())
    }
    fn drop(&mut self, job: Resource<Job>) -> Result<()> {
        self.table.delete(job)?;
        Ok(())
    }
}

fn state(events: Events) -> State {
    State {
        table: ResourceTable::new(),
        wasi: WasiCtx::builder().build(),
        events,
    }
}
fn invoke(
    store: &mut Store<State>,
    instance: &Consumer,
    stage: &str,
    order: &str,
    trigger: &str,
) -> Result<Value> {
    let started = Instant::now();
    let guest: Value =
        serde_json::from_str(&instance.call_run(&mut *store, stage, order, trigger)?)?;
    if order == "during" {
        store
            .data()
            .events
            .wait_terminal(if stage.starts_with("body") {
                "entity"
            } else {
                stage
            });
    }
    let result = json!({ "guest": guest, "elapsed_ms": started.elapsed().as_millis(),
        "host_before_store_drop": store.data().events.snapshot(), "resource_table_empty": store.data().table.is_empty() });
    Ok(result)
}
fn verify(result: &Value) -> Result<()> {
    let guest = &result["guest"];
    let order = guest["order"].as_str().unwrap();
    let events = result["host_before_store_drop"].as_array().unwrap();
    wasmtime::ensure!(
        result["resource_table_empty"] == true,
        "job/subscription must be released before Store drop"
    );
    if matches!(order, "none" | "after") {
        wasmtime::ensure!(
            guest["outcome"]["body"] == "ok" && guest["aborted"] == (order == "after"),
            "completion contract"
        );
    } else {
        wasmtime::ensure!(
            guest["outcome"]["name"] == "AbortError" && guest["aborted"] == true,
            "AbortSignal contract"
        );
        if order == "before" {
            wasmtime::ensure!(events.is_empty(), "pre-abort must not submit");
        } else {
            let stage = guest["stage"].as_str().unwrap();
            let phase = if stage.starts_with("body") {
                "entity"
            } else {
                stage
            };
            let expected = if stage == "body-parallel" { 2 } else { 1 };
            wasmtime::ensure!(
                events
                    .iter()
                    .filter(|e| **e == json!(format!("{phase}:cancel")))
                    .count()
                    == expected
                    && !events.contains(&json!(format!("{phase}:complete")))
                    && result["elapsed_ms"].as_u64().unwrap() <= 250,
                "physical task cancellation contract"
            );
        }
    }
    Ok(())
}
fn main() -> Result<()> {
    let path = std::env::args()
        .nth(1)
        .ok_or_else(|| wasmtime::format_err!("usage: task_probe COMPONENT"))?;
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
    let mut results = matrix(&engine, &component, &linker)?;
    results.extend(sequence(&engine, &component, &linker)?);
    for result in &results {
        verify(result)?;
    }
    Ok(())
}

fn matrix(engine: &Engine, component: &Component, linker: &Linker<State>) -> Result<Vec<Value>> {
    let mut results = Vec::new();
    for phase in ["dns", "headers", "body"] {
        for (order, trigger) in [
            ("none", "promise"),
            ("before", "promise"),
            ("during", "promise"),
            ("during", "timer"),
            ("after", "promise"),
        ] {
            let mut store = Store::new(engine, state(Arc::new(EventLog::default())));
            store.set_fuel(100_000_000_000)?;
            let instance = Consumer::instantiate(&mut store, component, linker)?;
            let result = invoke(&mut store, &instance, phase, order, trigger)?;
            println!("TASK_CONSUMER {result}");
            results.push(result);
        }
    }
    Ok(results)
}

fn sequence(engine: &Engine, component: &Component, linker: &Linker<State>) -> Result<Vec<Value>> {
    let mut results = Vec::new();
    let mut store = Store::new(engine, state(Arc::new(EventLog::default())));
    store.set_fuel(100_000_000_000)?;
    let instance = Consumer::instantiate(&mut store, component, linker)?;
    for (phase, order, trigger) in [
        ("body-parallel", "during", "timer"),
        ("dns", "during", "timer"),
        ("headers", "during", "promise"),
        ("body", "none", "promise"),
        ("dns", "none", "promise"),
        ("body", "after", "promise"),
    ] {
        store.data_mut().events = Arc::new(EventLog::default());
        let result = invoke(&mut store, &instance, phase, order, trigger)?;
        println!("TASK_SEQUENCE {result}");
        results.push(result);
    }
    Ok(results)
}
