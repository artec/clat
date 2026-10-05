use super::*;
use brotli_decompressor::{Allocator, BrotliState, SliceWrapper, SliceWrapperMut};
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};
const WORKSPACE: usize = 32 * 1024 * 1024;
#[derive(Default)]
struct Budget {
    used: AtomicUsize,
    failed: AtomicBool,
}
struct Cells<T> {
    data: Vec<T>,
    budget: Option<Arc<Budget>>,
    bytes: usize,
}
impl<T> Default for Cells<T> {
    fn default() -> Self {
        Self {
            data: Vec::new(),
            budget: None,
            bytes: 0,
        }
    }
}
impl<T> SliceWrapper<T> for Cells<T> {
    fn slice(&self) -> &[T] {
        &self.data
    }
}
impl<T> SliceWrapperMut<T> for Cells<T> {
    fn slice_mut(&mut self) -> &mut [T] {
        &mut self.data
    }
}
impl<T> Drop for Cells<T> {
    fn drop(&mut self) {
        if let Some(budget) = &self.budget {
            budget.used.fetch_sub(self.bytes, Ordering::AcqRel);
        }
    }
}
struct Allocate(Arc<Budget>);
impl<T: Default + Clone> Allocator<T> for Allocate {
    type AllocatedMemory = Cells<T>;
    fn alloc_cell(&mut self, len: usize) -> Cells<T> {
        let Some(bytes) = len.checked_mul(std::mem::size_of::<T>()) else {
            self.0.failed.store(true, Ordering::Release);
            return Cells::default();
        };
        if self
            .0
            .used
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |used| {
                used.checked_add(bytes)
                    .filter(|total| *total <= WORKSPACE - 65536)
            })
            .is_err()
        {
            self.0.failed.store(true, Ordering::Release);
            return Cells::default();
        }
        let mut data = Vec::new();
        if data.try_reserve_exact(len).is_err() {
            self.0.used.fetch_sub(bytes, Ordering::AcqRel);
            self.0.failed.store(true, Ordering::Release);
            return Cells::default();
        }
        data.resize(len, T::default());
        Cells {
            data,
            budget: Some(self.0.clone()),
            bytes,
        }
    }
    fn free_cell(&mut self, cells: Cells<T>) {
        drop(cells);
    }
}
pub(super) struct Reader<'a> {
    state: Box<BrotliState<Allocate, Allocate, Allocate>>,
    input: &'a [u8],
    offset: usize,
    total: usize,
    done: bool,
    budget: Arc<Budget>,
    check: &'a (dyn Fn() -> Result<(), Failure> + Sync),
    _memory: memory::Permit,
}
impl<'a> Reader<'a> {
    pub(super) fn new(
        input: &'a [u8],
        check: &'a (dyn Fn() -> Result<(), Failure> + Sync),
    ) -> Result<Self, Failure> {
        let memory = memory::Permit::acquire(WORKSPACE)?;
        let budget = Arc::new(Budget::default());
        let state = BrotliState::new_strict(
            Allocate(budget.clone()),
            Allocate(budget.clone()),
            Allocate(budget.clone()),
        );
        if budget.failed.load(Ordering::Acquire) {
            return Err(Failure::Limit);
        }
        Ok(Self {
            state: Box::new(state),
            input,
            offset: 0,
            total: 0,
            done: false,
            budget,
            check,
            _memory: memory,
        })
    }
}
impl Read for Reader<'_> {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        use brotli_decompressor::{BrotliDecompressStream, BrotliResult};
        if self.done || output.is_empty() {
            return Ok(0);
        }
        loop {
            (self.check)().map_err(|_| io::ErrorKind::PermissionDenied)?;
            let end = (self.offset + 4096).min(self.input.len());
            let input = &self.input[self.offset..end];
            let mut available_in = input.len();
            let mut consumed = 0;
            let mut available_out = output.len();
            let mut written = 0;
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                BrotliDecompressStream(
                    &mut available_in,
                    &mut consumed,
                    input,
                    &mut available_out,
                    &mut written,
                    output,
                    &mut self.total,
                    &mut self.state,
                )
            }));
            self.offset += consumed;
            if self.budget.failed.load(Ordering::Acquire) {
                return Err(io::ErrorKind::OutOfMemory.into());
            }
            return match result {
                Ok(BrotliResult::ResultSuccess)
                    if available_in == 0 && self.offset == self.input.len() =>
                {
                    self.done = true;
                    Ok(written)
                }
                Ok(BrotliResult::NeedsMoreOutput) if written > 0 => Ok(written),
                Ok(BrotliResult::NeedsMoreInput) if written > 0 => Ok(written),
                Ok(BrotliResult::NeedsMoreInput)
                    if self.offset < self.input.len() && consumed > 0 =>
                {
                    continue;
                }
                _ => Err(io::ErrorKind::InvalidData.into()),
            };
        }
    }
}
