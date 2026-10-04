//! Private bounded HTTP/1 candidate; no production linker entry.
use super::{
    PreparedRequest,
    connector::{Connection, ConnectionRequest},
    permission::{self, Gate},
};
use crate::dns_authority::{Failure as AuthorityFailure, Resolution, Scope};
use bytes::Bytes;
use clat_core::CancelToken;
use http_body_util::Full;

use std::sync::Arc;
use std::{
    sync::OnceLock,
    time::{Duration, Instant},
};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
const HEADER_COUNT: usize = 64;
const LIMIT: usize = 8 * 1024 * 1024;
#[derive(Clone, Debug, PartialEq)]
pub(super) enum Failure {
    Authority(AuthorityFailure),
    Permission(permission::Failure),
    Transport,
    Limit,
    Encoding,
}
pub(super) struct Response {
    pub status: u16,
    pub headers: http::HeaderMap,
    pub body: Vec<u8>,
    _stream: StreamResponse,
}
impl From<AuthorityFailure> for Failure {
    fn from(e: AuthorityFailure) -> Self {
        Self::Authority(e)
    }
}
mod stream;
pub(super) use stream::{StreamResponse, open};
pub(super) async fn execute(
    request: PreparedRequest,
    resolution: &Resolution,
    scope: &Scope,
    gate: &Gate,
    cancel: &CancelToken,
) -> Result<Response, Failure> {
    let mut stream = open(
        request,
        resolution,
        scope,
        gate,
        cancel,
        Duration::from_secs(30),
        LIMIT,
    )
    .await?;
    let status = stream.status()?;
    let headers = stream.headers()?.clone();
    let mut body = Vec::new();
    body.try_reserve_exact(LIMIT).map_err(|_| Failure::Limit)?;
    loop {
        let data = stream.read(65536).await?;
        if data.is_empty() {
            break;
        }
        body.extend_from_slice(&data);
    }
    Ok(Response {
        status,
        headers,
        body,
        _stream: stream,
    })
}
fn reserve() -> Result<OwnedSemaphorePermit, Failure> {
    static BUDGET: OnceLock<Arc<Semaphore>> = OnceLock::new();
    BUDGET
        .get_or_init(|| Arc::new(Semaphore::new(8)))
        .clone()
        .try_acquire_owned()
        .map_err(|_| Failure::Limit)
}
async fn cancelled(gate: &Gate, cancel: &CancelToken, deadline: Instant) {
    while gate.check(cancel, deadline).is_ok() {
        tokio::time::sleep_until(
            (Instant::now() + Duration::from_millis(50))
                .min(deadline)
                .into(),
        )
        .await;
    }
}
fn wire_request(request: &PreparedRequest) -> Result<http::Request<Full<Bytes>>, Failure> {
    let origin = request.origin();
    let target = match request.url().query() {
        Some(query) => format!("{}?{query}", request.url().path()),
        None => request.url().path().into(),
    };
    let mut output = http::Request::builder()
        .method(request.method().clone())
        .uri(target)
        .header("host", format!("{}:{}", origin.host(), origin.port()))
        .header("connection", "close")
        .body(Full::new(Bytes::copy_from_slice(request.body())))
        .map_err(|_| Failure::Transport)?;
    for (name, value) in request.headers() {
        output.headers_mut().append(name, value.clone());
    }
    Ok(output)
}
#[cfg(test)]
mod tests;
