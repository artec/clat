//! Author owner of actual Wasmtime entries; no production linker export.
use crate::dns_authority::{Failure, Scope};
use std::{any::TypeId, collections::BTreeMap, marker::PhantomData, time::Instant};
use wasmtime::component::{Resource, ResourceTable};
use wasmtime_wasi::p2::{DynPollable, subscribe};
mod clocks;
pub(crate) mod component;
mod diagnostics;
mod poll;
pub(crate) mod task;
#[cfg(test)]
pub(crate) use task::Outcome;
pub(crate) use task::Task;
#[derive(Debug, Eq, PartialEq)]
pub(crate) enum Error {
    Authority(Failure),
    Limit,
    InvalidHandle,
    Table,
}
struct Entry {
    serial: u64,
    kind: TypeId,
    parent: Option<u32>,
    retire_parent: bool,
    delete: fn(&mut ResourceTable, u32) -> Result<(), Error>,
}
pub(crate) struct Handle<T> {
    rep: u32,
    serial: u64,
    owner: u64,
    marker: PhantomData<fn() -> T>,
}
impl<T> Clone for Handle<T> {
    fn clone(&self) -> Self {
        Self {
            rep: self.rep,
            serial: self.serial,
            owner: self.owner,
            marker: PhantomData,
        }
    }
}
pub(crate) struct Owner {
    table: ResourceTable,
    entries: BTreeMap<u32, Entry>,
    serial: u64,
    identity: u64,
    scope: Scope,
    closed: bool,
    clocks: wasmtime_wasi::clocks::WasiClocksCtx,
    diagnostics: wasmtime_wasi::p2::pipe::MemoryOutputPipe,
    clock_grant: Option<crate::capabilities::ClockGrant>,
}
impl Owner {
    pub(crate) fn new(scope: &Scope) -> Self {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
        Self {
            table: ResourceTable::new(),
            entries: BTreeMap::new(),
            serial: 0,
            identity: NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
            scope: scope.borrow_handle(),
            closed: false,
            clock_grant: None,
            clocks: Default::default(),
            diagnostics: wasmtime_wasi::p2::pipe::MemoryOutputPipe::new(32768),
        }
    }
    pub(crate) fn with_clock(
        scope: &Scope,
        grant: Option<crate::capabilities::ClockGrant>,
    ) -> Self {
        let mut owner = Self::new(scope);
        owner.clock_grant = grant;
        owner
    }
    pub(crate) fn check(&mut self) -> Result<(), Error> {
        if self.closed {
            return Err(Error::Authority(Failure::Cancelled));
        }
        if let Err(error) = self
            .scope
            .check_active(Instant::now() + std::time::Duration::from_secs(120))
        {
            self.close()?;
            return Err(Error::Authority(error));
        }
        Ok(())
    }
    fn admit(&mut self) -> Result<u64, Error> {
        self.check()?;
        if self.entries.len() >= 16 {
            return Err(Error::Limit);
        }
        // One namespace survives tool-owner replacement in a live Store.
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
        NEXT.fetch_update(
            std::sync::atomic::Ordering::Relaxed,
            std::sync::atomic::Ordering::Relaxed,
            |value| {
                value
                    .checked_add(1)
                    .filter(|next| *next <= u64::from(u32::MAX))
            },
        )
        .map_err(|_| Error::Limit)
    }
    pub(crate) fn insert<T: Send + 'static>(&mut self, value: T) -> Result<Handle<T>, Error> {
        let serial = self.admit()?;
        let resource = self.table.push(value).map_err(|_| Error::Table)?;
        Ok(self.track(resource, None, serial))
    }
    fn track<T: 'static>(
        &mut self,
        resource: Resource<T>,
        parent: Option<u32>,
        serial: u64,
    ) -> Handle<T> {
        self.serial = serial;
        let rep = resource.rep();
        self.entries.insert(
            rep,
            Entry {
                serial: self.serial,
                kind: TypeId::of::<T>(),
                parent,
                retire_parent: false,
                delete: delete::<T>,
            },
        );
        Handle {
            rep,
            serial: self.serial,
            owner: self.identity,
            marker: PhantomData,
        }
    }
    fn validate<T: 'static>(&self, handle: &Handle<T>) -> Result<(), Error> {
        let entry = self.entries.get(&handle.rep).ok_or(Error::InvalidHandle)?;
        if handle.owner != self.identity
            || entry.serial != handle.serial
            || entry.kind != TypeId::of::<T>()
        {
            return Err(Error::InvalidHandle);
        }
        Ok(())
    }
    pub(crate) fn get_mut<T: 'static>(&mut self, handle: &Handle<T>) -> Result<&mut T, Error> {
        self.check()?;
        self.validate(handle)?;
        self.table
            .get_mut(&Resource::new_borrow(handle.rep))
            .map_err(|_| Error::InvalidHandle)
    }
    pub(crate) fn subscribe<T: wasmtime_wasi::p2::Pollable>(
        &mut self,
        task: &Handle<T>,
    ) -> Result<Handle<DynPollable>, Error> {
        let serial = self.admit()?;
        self.validate(task)?;
        let pollable = subscribe(&mut self.table, Resource::<T>::new_borrow(task.rep))
            .map_err(|_| Error::Table)?;
        Ok(self.track(pollable, Some(task.rep), serial))
    }
    pub(crate) fn remove<T: 'static>(&mut self, handle: &Handle<T>) -> Result<(), Error> {
        self.validate(handle)?;
        self.remove_rep(handle.rep)
    }
    // Only canonical ABI resources may enter here: Wasmtime validates their
    // ownership/generation before invoking Host. Raw guest integers cannot.
    fn binding<T: 'static>(&self, resource: &Resource<T>) -> Result<Handle<T>, Error> {
        // Canonical reps are monotonic IDs, never recyclable ResourceTable indexes.
        let (rep, entry) = self
            .entries
            .iter()
            .find(|(_, e)| e.serial == u64::from(resource.rep()))
            .ok_or(Error::InvalidHandle)?;
        let handle = Handle {
            rep: *rep,
            serial: entry.serial,
            owner: self.identity,
            marker: PhantomData,
        };
        self.validate(&handle)?;
        Ok(handle)
    }
    fn resource<T>(handle: Handle<T>) -> Resource<T> {
        Resource::new_own(handle.serial as u32)
    }
    fn drop_binding<T: 'static>(&mut self, resource: Resource<T>) -> Result<(), Error> {
        match self.binding(&resource) {
            Ok(handle) => self.remove(&handle),
            Err(Error::InvalidHandle) if u64::from(resource.rep()) <= self.serial => Ok(()),
            Err(error) => Err(error),
        }
    }
    fn take<T: 'static>(&mut self, handle: &Handle<T>) -> Result<T, Error> {
        self.check()?;
        self.validate(handle)?;
        self.remove_children(handle.rep)?;
        let value = self
            .table
            .delete(Resource::<T>::new_own(handle.rep))
            .map_err(|_| Error::Table)?;
        self.entries.remove(&handle.rep);
        Ok(value)
    }
    fn remove_children(&mut self, rep: u32) -> Result<(), Error> {
        let children: Vec<_> = self
            .entries
            .iter()
            .filter_map(|(id, e)| (e.parent == Some(rep)).then_some(*id))
            .collect();
        for child in children {
            self.entries
                .get_mut(&child)
                .ok_or(Error::InvalidHandle)?
                .retire_parent = false;
            self.remove_rep(child)?;
        }
        Ok(())
    }
    fn remove_rep(&mut self, rep: u32) -> Result<(), Error> {
        self.remove_children(rep)?;
        let entry = self.entries.get(&rep).ok_or(Error::InvalidHandle)?;
        let retire = entry.parent.filter(|_| entry.retire_parent);
        (entry.delete)(&mut self.table, rep)?;
        self.entries.remove(&rep);
        if let Some(parent) = retire {
            self.remove_rep(parent)?;
        }
        Ok(())
    }
    pub(crate) fn close(&mut self) -> Result<(), Error> {
        self.closed = true;
        while let Some(rep) = self.entries.keys().next().copied() {
            self.remove_rep(rep)?;
        }
        Ok(())
    }
    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.entries.len()
    }
}
impl Drop for Owner {
    fn drop(&mut self) {
        let _ = self.close();
    }
}
fn delete<T: 'static>(table: &mut ResourceTable, rep: u32) -> Result<(), Error> {
    drop(
        table
            .delete(Resource::<T>::new_own(rep))
            .map_err(|_| Error::Table)?,
    );
    Ok(())
}
#[cfg(test)]
mod tests;
