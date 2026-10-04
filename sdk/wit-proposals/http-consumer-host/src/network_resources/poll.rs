//! Delegate WASI polling, intercept deletion to keep the actual-entry ledger exact.
use super::*;
use wasmtime_wasi::p2::bindings::sync::io::poll::{Host, HostPollable};
pub(super) fn trap(error: Error) -> wasmtime::Error {
    wasmtime::format_err!("network resource: {error:?}")
}
impl Host for Owner {
    fn poll(&mut self, pollables: Vec<Resource<DynPollable>>) -> wasmtime::Result<Vec<u32>> {
        self.check().map_err(trap)?;
        let mapped = pollables
            .iter()
            .map(|r| {
                self.binding(r)
                    .map(|h| Resource::new_borrow(h.rep))
                    .map_err(trap)
            })
            .collect::<wasmtime::Result<Vec<_>>>()?;
        let result = Host::poll(&mut self.table, mapped)?;
        self.check().map_err(trap)?;
        Ok(result)
    }
}
impl HostPollable for Owner {
    fn ready(&mut self, resource: Resource<DynPollable>) -> wasmtime::Result<bool> {
        self.check().map_err(trap)?;
        let handle = self.binding(&resource).map_err(trap)?;
        let result = HostPollable::ready(&mut self.table, Resource::new_borrow(handle.rep))?;
        self.check().map_err(trap)?;
        Ok(result)
    }
    fn block(&mut self, resource: Resource<DynPollable>) -> wasmtime::Result<()> {
        self.check().map_err(trap)?;
        let handle = self.binding(&resource).map_err(trap)?;
        HostPollable::block(&mut self.table, Resource::new_borrow(handle.rep))?;
        self.check().map_err(trap)
    }
    fn drop(&mut self, resource: Resource<DynPollable>) -> wasmtime::Result<()> {
        self.drop_binding(resource).map_err(trap)
    }
}
