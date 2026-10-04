//! Fresh guest lifetime for the author network lane; compiled code alone is cached.
use super::HostState;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use wasmtime::component::{Component, Linker};
use wasmtime::{Config, Engine, Store, StoreLimitsBuilder, UpdateDeadline};

pub(super) struct Lane {
    engine: Engine,
    linker: Linker<HostState>,
    component: Component,
    _ticker: Ticker,
}
impl Lane {
    pub(super) fn new(bytes: impl AsRef<[u8]>) -> wasmtime::Result<Self> {
        let mut config = Config::new();
        config.consume_fuel(true).epoch_interruption(true);
        config.cranelift_opt_level(wasmtime::OptLevel::None);
        let engine = Engine::new(&config)?;
        let component = Component::new(&engine, bytes)?;
        let mut linker = Linker::new(&engine);
        HostState::link_scheduler(&mut linker)?;
        let ticker = Ticker::start(&engine)?;
        Ok(Self {
            engine,
            linker,
            component,
            _ticker: ticker,
        })
    }
    pub(super) fn invoke<R>(
        &mut self,
        host: HostState,
        call: impl FnOnce(&mut Store<HostState>, &Component, &Linker<HostState>) -> wasmtime::Result<R>,
    ) -> wasmtime::Result<R> {
        let mut store = Store::new(&self.engine, host);
        store.limiter(|host| &mut host.limits);
        store.set_fuel(100_000_000_000)?;
        store.epoch_deadline_callback(|mut context| {
            context.data_mut().owner.check().map_err(super::trap)?;
            Ok(UpdateDeadline::Continue(1))
        });
        store.set_epoch_deadline(1);
        let mut boundary = Boundary(&mut store);
        boundary.0.data_mut().owner.check().map_err(super::trap)?;
        let result = call(boundary.0, &self.component, &self.linker);
        let active = boundary.0.data_mut().owner.check().map_err(super::trap);
        let closed = boundary.close();
        // Preserve guest failure, but never deliver a revoked successful result.
        let value = result?;
        active?;
        closed?;
        Ok(value)
    }
}
struct Boundary<'a>(&'a mut Store<HostState>);
impl Boundary<'_> {
    fn close(&mut self) -> wasmtime::Result<()> {
        let host = self.0.data_mut();
        let result = host.owner.close().map_err(super::trap);
        host.network.scope().invalidate();
        #[cfg(test)]
        if let Some(probe) = host.close_probe.take() {
            probe(host);
        }
        result
    }
}
impl Drop for Boundary<'_> {
    fn drop(&mut self) {
        let _ = self.close();
    }
}
struct Ticker {
    stopped: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}
impl Ticker {
    fn start(engine: &Engine) -> std::io::Result<Self> {
        let stopped = Arc::new(AtomicBool::new(false));
        let stop = stopped.clone();
        let engine = engine.clone();
        let thread = std::thread::Builder::new()
            .name("plg4-author-epoch".into())
            .spawn(move || {
                while !stop.load(Ordering::Acquire) {
                    std::thread::park_timeout(std::time::Duration::from_millis(5));
                    if !stop.load(Ordering::Acquire) {
                        engine.increment_epoch();
                    }
                }
            })?;
        Ok(Self {
            stopped,
            thread: Some(thread),
        })
    }
}
impl Drop for Ticker {
    fn drop(&mut self) {
        self.stopped.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take() {
            thread.thread().unpark();
            let _ = thread.join();
        }
    }
}
pub(super) fn limits() -> wasmtime::StoreLimits {
    StoreLimitsBuilder::new()
        .memory_size(256 * 1024 * 1024)
        .memories(1)
        .build()
}
#[cfg(test)]
mod tests;

#[cfg(test)]
mod quartet;

#[cfg(test)]
mod http_quartet;
