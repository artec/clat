//! Standard WASI clocks with every hidden timer entry owned by the network scope.
use super::{Error, Owner, poll::trap};
use crate::dns_authority::Scope;
use std::time::{Duration, Instant};
use wasmtime::component::Resource;
use wasmtime_wasi::{
    clocks::WasiClocksCtxView,
    p2::{
        DynPollable, Pollable,
        bindings::clocks::{monotonic_clock, wall_clock},
    },
};
struct Timer {
    when: Option<tokio::time::Instant>,
    yielded: bool,
    scope: Scope,
}
#[wasmtime_wasi::async_trait]
impl Pollable for Timer {
    async fn ready(&mut self) {
        // Even an overflowing timer is bounded by the same tool/run authority.
        let limit = Instant::now() + Duration::from_secs(120);
        tokio::select! {
            biased;
            _ = self.scope.closed(limit) => {},
            _ = async {
                match self.when {
                    Some(when) if when <= tokio::time::Instant::now() => {
                        if !self.yielded {
                            self.yielded = true;
                            tokio::task::yield_now().await;
                        }
                    }
                    Some(when) => tokio::time::sleep_until(when).await,
                    None => std::future::pending::<()>().await,
                }
            } => {},
        }
    }
}
impl Owner {
    fn require_clock(&mut self) -> Result<(), Error> {
        self.check()?;
        if self.clock_grant.is_none() {
            return Err(Error::Authority(
                crate::dns_authority::Failure::CapabilityDenied,
            ));
        }
        Ok(())
    }
    fn clock_view(&mut self) -> WasiClocksCtxView<'_> {
        WasiClocksCtxView {
            ctx: &mut self.clocks,
            table: &mut self.table,
        }
    }
    fn timer(&mut self, duration: Duration) -> Result<Resource<DynPollable>, Error> {
        self.require_clock()?;
        let root = self.insert(Timer {
            when: tokio::time::Instant::now().checked_add(duration),
            yielded: !duration.is_zero(),
            scope: self.scope.borrow_handle(),
        })?;
        match self.subscribe(&root) {
            Ok(child) => {
                self.entries
                    .get_mut(&child.rep)
                    .ok_or(Error::InvalidHandle)?
                    .retire_parent = true;
                Ok(Self::resource(child))
            }
            Err(error) => {
                self.remove(&root)?;
                Err(error)
            }
        }
    }
}
impl monotonic_clock::Host for Owner {
    fn now(&mut self) -> wasmtime::Result<u64> {
        self.require_clock().map_err(trap)?;
        monotonic_clock::Host::now(&mut self.clock_view())
    }
    fn resolution(&mut self) -> wasmtime::Result<u64> {
        self.require_clock().map_err(trap)?;
        monotonic_clock::Host::resolution(&mut self.clock_view())
    }
    fn subscribe_instant(&mut self, when: u64) -> wasmtime::Result<Resource<DynPollable>> {
        let now = monotonic_clock::Host::now(self)?;
        self.timer(Duration::from_nanos(when.saturating_sub(now)))
            .map_err(trap)
    }
    fn subscribe_duration(&mut self, duration: u64) -> wasmtime::Result<Resource<DynPollable>> {
        self.timer(Duration::from_nanos(duration)).map_err(trap)
    }
}
impl wall_clock::Host for Owner {
    fn now(&mut self) -> wasmtime::Result<wall_clock::Datetime> {
        self.require_clock().map_err(trap)?;
        wall_clock::Host::now(&mut self.clock_view())
    }
    fn resolution(&mut self) -> wasmtime::Result<wall_clock::Datetime> {
        self.require_clock().map_err(trap)?;
        wall_clock::Host::resolution(&mut self.clock_view())
    }
}
#[cfg(test)]
mod tests;
