//! HTTP entity only: headers first, bounded sequential chunks, one authority chain.
use super::*;
use crate::dns_authority::{ConnectionPins, HttpPermit};
use http_body_util::BodyExt;
use hyper_util::rt::TokioIo;
use std::{future::Future, pin::Pin};
use tokio::io::{AsyncRead, AsyncWrite};
type Driver = Pin<Box<dyn Future<Output = Result<(), hyper::Error>> + Send>>;
struct Body {
    incoming: hyper::body::Incoming,
    driver: Option<Driver>,
    pending: Bytes,
    received: usize,
}
pub(crate) struct StreamResponse {
    status: u16,
    headers: http::HeaderMap,
    body: Option<Body>,
    pins: ConnectionPins,
    scope: Scope,
    gate: Gate,
    cancel: CancelToken,
    deadline: Instant,
    max: usize,
    terminal: Option<Failure>,
    eof: bool,
    process: Option<OwnedSemaphorePermit>,
    resource: Option<HttpPermit>,
}
pub(crate) async fn open(
    request: PreparedRequest,
    resolution: &Resolution,
    scope: &Scope,
    gate: &Gate,
    cancel: &CancelToken,
    timeout: Duration,
    max: usize,
) -> Result<StreamResponse, Failure> {
    if max > LIMIT {
        return Err(Failure::Limit);
    }
    let (deadline, resource) = scope.admit_http(Instant::now(), timeout)?;
    let mut connection = ConnectionRequest::bind(request, resolution, scope)?;
    connection.pins.restrict_deadline(deadline);
    let (_, _, deadline) = connection.pins.snapshot()?;
    let permit = reserve()?;
    let operation = async {
        gate.authorize(&connection.request, cancel, deadline)
            .await
            .map_err(Failure::Permission)?;
        gate.check(cancel, deadline).map_err(Failure::Permission)?;
        let stream = connection.connect().await.map_err(|_| Failure::Transport)?;
        gate.check(cancel, deadline).map_err(Failure::Permission)?;
        match stream {
            Connection::Plain(stream) => head(stream, &connection.request, max).await,
            Connection::Tls(stream) => head(stream, &connection.request, max).await,
        }
    };
    let (parts, body) = tokio::select! {
        biased;
        _ = cancelled(gate,cancel,deadline) => return Err(Failure::Permission(gate.check(cancel,deadline).unwrap_err())),
        error = connection.pins.closed() => return Err(error.into()),
        result = operation => result?,
    };
    connection.pins.snapshot()?;
    gate.check(cancel, deadline).map_err(Failure::Permission)?;
    Ok(StreamResponse {
        status: parts.status.as_u16(),
        headers: parts.headers,
        body: Some(body),
        pins: connection.pins,
        scope: scope.borrow_handle(),
        gate: gate.clone(),
        cancel: cancel.clone(),
        deadline,
        max,
        terminal: None,
        eof: false,
        process: Some(permit),
        resource: Some(resource),
    })
}
async fn head<T: AsyncRead + AsyncWrite + Unpin + Send + 'static>(
    stream: T,
    request: &PreparedRequest,
    max: usize,
) -> Result<(http::response::Parts, Body), Failure> {
    let (mut sender, driver) = hyper::client::conn::http1::Builder::new()
        .max_headers(HEADER_COUNT)
        .max_buf_size(32768)
        .handshake(TokioIo::new(stream))
        .await
        .map_err(|_| Failure::Transport)?;
    let fetch = sender.send_request(wire_request(request)?);
    tokio::pin!(fetch);
    let mut driver: Driver = Box::pin(driver);
    let (response, driver) = tokio::select! {
        biased;
        result = &mut fetch => (result.map_err(|_| Failure::Transport)?, Some(driver)),
        result = &mut driver => { result.map_err(|_| Failure::Transport)?; (fetch.await.map_err(|_| Failure::Transport)?, None) }
    };
    let (parts, incoming) = response.into_parts();
    validate_head(&parts, max)?;
    Ok((
        parts,
        Body {
            incoming,
            driver,
            pending: Bytes::new(),
            received: 0,
        },
    ))
}
fn validate_head(parts: &http::response::Parts, max: usize) -> Result<(), Failure> {
    if parts.status == http::StatusCode::SWITCHING_PROTOCOLS {
        return Err(Failure::Transport);
    }
    if parts.headers.len() > HEADER_COUNT
        || parts
            .headers
            .iter()
            .map(|(n, v)| n.as_str().len() + v.as_bytes().len())
            .sum::<usize>()
            > 32768
    {
        return Err(Failure::Limit);
    }
    for value in parts.headers.get_all("content-encoding") {
        if value.as_bytes() != b"identity" {
            return Err(Failure::Encoding);
        }
    }
    if let Some(length) = parts.headers.get("content-length") {
        let length = length
            .to_str()
            .ok()
            .and_then(|n| n.parse::<u64>().ok())
            .ok_or(Failure::Transport)?;
        if length > max as u64 {
            return Err(Failure::Limit);
        }
    }
    Ok(())
}
impl Body {
    async fn next(&mut self) -> Result<Option<Bytes>, Failure> {
        loop {
            let Some(driver) = &mut self.driver else {
                return frame(self.incoming.frame().await);
            };
            tokio::select! {
                biased;
                data = self.incoming.frame() => return frame(data),
                result = driver => {
                    result.map_err(|_| Failure::Transport)?;
                    self.driver = None;
                }
            }
        }
    }
}
fn frame(
    data: Option<Result<http_body::Frame<Bytes>, hyper::Error>>,
) -> Result<Option<Bytes>, Failure> {
    data.map(|frame| {
        frame
            .map_err(|_| Failure::Transport)?
            .into_data()
            .map_err(|_| Failure::Transport)
    })
    .transpose()
}
impl StreamResponse {
    fn check(&self) -> Result<(), Failure> {
        if let Some(error) = &self.terminal {
            return Err(error.clone());
        }
        self.gate
            .check(&self.cancel, self.deadline)
            .map_err(Failure::Permission)?;
        self.scope.check_active(self.deadline)?;
        self.pins.snapshot()?;
        Ok(())
    }
    fn validate(&mut self) -> Result<(), Failure> {
        if let Err(error) = self.check() {
            self.cancel();
            self.terminal = Some(error.clone());
            return Err(error);
        }
        Ok(())
    }
    pub(crate) fn status(&mut self) -> Result<u16, Failure> {
        self.validate()?;
        Ok(self.status)
    }
    pub(crate) fn headers(&mut self) -> Result<&http::HeaderMap, Failure> {
        self.validate()?;
        Ok(&self.headers)
    }
    pub(crate) fn cancel(&mut self) {
        self.body = None;
        self.process = None;
        self.resource = None;
        self.terminal = Some(Failure::Authority(AuthorityFailure::Cancelled));
    }
    pub(crate) async fn read(&mut self, max_bytes: usize) -> Result<Vec<u8>, Failure> {
        if !(1..=65536).contains(&max_bytes) {
            return Err(Failure::Permission(permission::Failure::InvalidRequest));
        }
        let mut reading = ReadGuard {
            response: self,
            complete: false,
        };
        let result = reading.response.read_inner(max_bytes).await;
        reading.complete = true;
        if let Err(error) = &result {
            reading.response.cancel();
            reading.response.terminal = Some(error.clone());
        }
        result
    }
    async fn read_inner(&mut self, max_bytes: usize) -> Result<Vec<u8>, Failure> {
        self.check()?;
        if self.eof {
            return Ok(vec![]);
        }
        let body = self.body.as_mut().ok_or(Failure::Transport)?;
        while body.pending.is_empty() {
            let frame = tokio::select! {
                biased;
                _ = cancelled(&self.gate,&self.cancel,self.deadline) => return Err(Failure::Permission(self.gate.check(&self.cancel,self.deadline).unwrap_err())),
                error = self.pins.closed() => return Err(error.into()),
                result = body.next() => result?,
            };
            let Some(data) = frame else {
                self.check()?;
                self.eof = true;
                self.body = None;
                return Ok(vec![]);
            };
            if data.len() > self.max - body.received {
                return Err(Failure::Limit);
            }
            body.received += data.len();
            body.pending = data;
        }
        let bytes = body
            .pending
            .split_to(max_bytes.min(body.pending.len()))
            .to_vec();
        self.check()?;
        Ok(bytes)
    }
}

struct ReadGuard<'a> {
    response: &'a mut StreamResponse,
    complete: bool,
}
impl Drop for ReadGuard<'_> {
    fn drop(&mut self) {
        if !self.complete {
            self.response.cancel();
        }
    }
}
