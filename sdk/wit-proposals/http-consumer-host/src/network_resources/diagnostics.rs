//! Bounded in-memory stderr only; no ambient streams and no hidden error entries.
use super::{Owner, poll::trap};
use wasmtime::component::Resource;
use wasmtime_wasi::p2::{
    DynPollable, StreamError, StreamResult,
    bindings::{
        cli::stderr,
        sync::io::{
            error,
            streams::{self, InputStream, OutputStream},
        },
    },
};
impl stderr::Host for Owner {
    fn get_stderr(&mut self) -> wasmtime::Result<Resource<OutputStream>> {
        let stream: OutputStream = Box::new(self.diagnostics.clone());
        self.insert(stream).map(Self::resource).map_err(trap)
    }
}
impl Owner {
    fn output(&mut self, stream: Resource<OutputStream>) -> StreamResult<&mut OutputStream> {
        let handle = self
            .binding(&stream)
            .map_err(|e| StreamError::Trap(trap(e)))?;
        self.get_mut(&handle)
            .map_err(|e| StreamError::Trap(trap(e)))
    }
}
impl streams::Host for Owner {
    fn convert_stream_error(&mut self, err: StreamError) -> wasmtime::Result<streams::StreamError> {
        match err {
            StreamError::Trap(error) => Err(error),
            // The only output is the finite memory sink. Never allocate error resources.
            StreamError::Closed | StreamError::LastOperationFailed(_) => {
                Ok(streams::StreamError::Closed)
            }
        }
    }
}
impl error::Host for Owner {}
impl error::HostError for Owner {
    fn to_debug_string(&mut self, _resource: Resource<streams::Error>) -> wasmtime::Result<String> {
        wasmtime::bail!("diagnostic error resources are unavailable")
    }
    fn drop(&mut self, resource: Resource<streams::Error>) -> wasmtime::Result<()> {
        self.drop_binding(resource).map_err(trap)
    }
}
impl streams::HostOutputStream for Owner {
    fn drop(&mut self, stream: Resource<OutputStream>) -> wasmtime::Result<()> {
        self.drop_binding(stream).map_err(trap)
    }
    fn check_write(&mut self, stream: Resource<OutputStream>) -> StreamResult<u64> {
        Ok(self.output(stream)?.check_write()? as u64)
    }
    fn write(&mut self, stream: Resource<OutputStream>, bytes: Vec<u8>) -> StreamResult<()> {
        let output = self.output(stream)?;
        if bytes.len() > output.check_write()? {
            return Err(StreamError::Closed);
        }
        output.write(bytes.into())
    }
    fn blocking_write_and_flush(
        &mut self,
        stream: Resource<OutputStream>,
        bytes: Vec<u8>,
    ) -> StreamResult<()> {
        let rep = stream.rep();
        Self::write(self, stream, bytes)?;
        Self::flush(self, Resource::new_borrow(rep))
    }
    fn write_zeroes(&mut self, stream: Resource<OutputStream>, len: u64) -> StreamResult<()> {
        let output = self.output(stream)?;
        if len > output.check_write()? as u64 {
            return Err(StreamError::Closed);
        }
        output.write(bytes::Bytes::from(vec![0; len as usize]))
    }
    fn blocking_write_zeroes_and_flush(
        &mut self,
        stream: Resource<OutputStream>,
        len: u64,
    ) -> StreamResult<()> {
        let rep = stream.rep();
        Self::write_zeroes(self, stream, len)?;
        Self::flush(self, Resource::new_borrow(rep))
    }
    fn flush(&mut self, stream: Resource<OutputStream>) -> StreamResult<()> {
        self.output(stream)?.flush()
    }
    fn blocking_flush(&mut self, stream: Resource<OutputStream>) -> StreamResult<()> {
        Self::flush(self, stream)
    }
    fn subscribe(
        &mut self,
        stream: Resource<OutputStream>,
    ) -> wasmtime::Result<Resource<DynPollable>> {
        let handle = self.binding(&stream).map_err(trap)?;
        self.subscribe(&handle).map(Self::resource).map_err(trap)
    }
    fn splice(
        &mut self,
        _dst: Resource<OutputStream>,
        _src: Resource<InputStream>,
        _len: u64,
    ) -> StreamResult<u64> {
        Err(StreamError::Closed)
    }
    fn blocking_splice(
        &mut self,
        dst: Resource<OutputStream>,
        src: Resource<InputStream>,
        len: u64,
    ) -> StreamResult<u64> {
        Self::splice(self, dst, src, len)
    }
}
impl streams::HostInputStream for Owner {
    fn drop(&mut self, resource: Resource<InputStream>) -> wasmtime::Result<()> {
        self.drop_binding(resource).map_err(trap)
    }
    fn read(&mut self, _stream: Resource<InputStream>, _len: u64) -> StreamResult<Vec<u8>> {
        Err(StreamError::Closed)
    }
    fn blocking_read(&mut self, stream: Resource<InputStream>, len: u64) -> StreamResult<Vec<u8>> {
        Self::read(self, stream, len)
    }
    fn skip(&mut self, _stream: Resource<InputStream>, _len: u64) -> StreamResult<u64> {
        Err(StreamError::Closed)
    }
    fn blocking_skip(&mut self, stream: Resource<InputStream>, len: u64) -> StreamResult<u64> {
        Self::skip(self, stream, len)
    }
    fn subscribe(
        &mut self,
        _stream: Resource<InputStream>,
    ) -> wasmtime::Result<Resource<DynPollable>> {
        wasmtime::bail!("input streams are unavailable")
    }
}
#[cfg(test)]
mod tests;
