//! Bounded semantic entity decoding, never a guest node:zlib capability.
use super::{Failure, memory};
use std::io::{self, Read};
mod brotli;
pub(super) enum Encoding {
    Identity,
    Gzip,
    Deflate,
    Brotli,
}
impl Encoding {
    pub(super) fn parse(headers: &http::HeaderMap) -> Result<Self, Failure> {
        let mut values = headers.get_all("content-encoding").iter();
        let Some(value) = values.next() else {
            return Ok(Self::Identity);
        };
        if values.next().is_some() {
            return Err(Failure::Encoding);
        }
        match value
            .to_str()
            .map_err(|_| Failure::Encoding)?
            .trim()
            .to_ascii_lowercase()
            .as_str()
        {
            "identity" => Ok(Self::Identity),
            "gzip" | "x-gzip" => Ok(Self::Gzip),
            "deflate" => Ok(Self::Deflate),
            "br" => Ok(Self::Brotli),
            _ => Err(Failure::Encoding),
        }
    }
    pub(super) fn compressed(&self) -> bool {
        !matches!(self, Self::Identity)
    }
    pub(super) fn reader<'a>(
        &self,
        bytes: &'a [u8],
        check: &'a (dyn Fn() -> Result<(), Failure> + Sync),
    ) -> Result<Reader<'a>, Failure> {
        let workspace = match self {
            Self::Gzip => 9 * 1024 * 1024,
            Self::Deflate => 1024 * 1024,
            _ => 0,
        };
        let memory = memory::Permit::acquire(workspace)?;
        let inner: Box<dyn Read + Send + 'a> = match self {
            Self::Identity => Box::new(bytes),
            Self::Gzip => Box::new(flate2::read::MultiGzDecoder::new(CheckedInput {
                bytes,
                check,
            })),
            Self::Deflate if zlib_header(bytes) => {
                Box::new(flate2::read::ZlibDecoder::new(CheckedInput {
                    bytes,
                    check,
                }))
            }
            Self::Deflate => Box::new(flate2::read::DeflateDecoder::new(CheckedInput {
                bytes,
                check,
            })),
            Self::Brotli => Box::new(brotli::Reader::new(bytes, check)?),
        };
        Ok(Reader {
            inner,
            _memory: memory,
        })
    }
}
struct CheckedInput<'a> {
    bytes: &'a [u8],
    check: &'a (dyn Fn() -> Result<(), Failure> + Sync),
}
impl Read for CheckedInput<'_> {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        (self.check)().map_err(|_| io::ErrorKind::PermissionDenied)?;
        let count = output.len().min(self.bytes.len()).min(4096);
        output[..count].copy_from_slice(&self.bytes[..count]);
        self.bytes = &self.bytes[count..];
        Ok(count)
    }
}
pub(super) struct Reader<'a> {
    inner: Box<dyn Read + Send + 'a>,
    _memory: memory::Permit,
}
impl Read for Reader<'_> {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        self.inner.read(bytes)
    }
}
fn zlib_header(bytes: &[u8]) -> bool {
    bytes.len() >= 2
        && bytes[0] & 15 == 8
        && u16::from_be_bytes([bytes[0], bytes[1]]).is_multiple_of(31)
}
pub(super) async fn decode(
    encoding: &Encoding,
    bytes: &[u8],
    max: usize,
    check: impl Fn() -> Result<(), Failure> + Sync,
) -> Result<Vec<u8>, Failure> {
    check()?;
    let _scratch = memory::Permit::acquire(65536)?;
    let mut reader = encoding.reader(bytes, &check)?;
    let mut output = Vec::new();
    output.try_reserve_exact(max).map_err(|_| Failure::Limit)?;
    let mut chunk = [0; 65536];
    loop {
        check()?;
        let count = reader.read(&mut chunk).map_err(|error| {
            if let Err(failure) = check() {
                return failure;
            }
            if error.kind() == io::ErrorKind::OutOfMemory {
                Failure::Limit
            } else {
                Failure::Encoding
            }
        })?;
        if count == 0 {
            break;
        }
        if count > max - output.len() {
            return Err(Failure::Limit);
        }
        output.extend_from_slice(&chunk[..count]);
        tokio::task::yield_now().await;
    }
    check()?;
    Ok(output)
}
