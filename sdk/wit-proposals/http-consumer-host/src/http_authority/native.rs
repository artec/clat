//! Native tools enter only after CLAT's ordinary tool permission admission.
//! Reuses the guest transport's public-address checks, DNS pins and byte budget;
//! this entry grants no guest origin, environment or capability access.
use super::{
    Declaration, HttpFence,
    network::NetworkScope,
    permission::{Authority, Failure, Gate, Pending},
};
use crate::{
    CancelToken,
    dns_authority::{Fence, Origin, Run, SystemDns},
};
use std::{
    sync::Arc,
    time::{Duration, Instant},
};

pub struct Request<'a> {
    pub url: &'a str,
    pub method: &'a str,
    pub headers: &'a [(&'a str, &'a str)],
    pub body: &'a [u8],
    pub timeout: Duration,
    pub max_bytes: usize,
}
#[derive(Debug)]
pub struct Response {
    pub status: u16,
    pub content_type: String,
    pub location: Option<String>,
    pub body: Vec<u8>,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    InvalidRequest,
    Cancelled,
    Deadline,
    Blocked,
    Transport,
    Limit,
    Encoding,
}
struct Admitted(CancelToken);
struct Ready;
impl Pending for Ready {
    fn get(&self) -> Option<Result<(), Failure>> {
        Some(Ok(()))
    }
}
impl Authority for Admitted {
    fn check(&self) -> Result<(), Failure> {
        if self.0.is_cancelled() {
            Err(Failure::Cancelled)
        } else {
            Ok(())
        }
    }
    fn start(&self, _: &Origin, _: Option<&str>, _: Instant) -> Result<Box<dyn Pending>, Failure> {
        self.check()?;
        Ok(Box::new(Ready))
    }
}
pub fn execute(input: Request<'_>, cancel: CancelToken) -> Result<Response, Error> {
    if cancel.is_cancelled() {
        return Err(Error::Cancelled);
    }
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|_| Error::Transport)?;
    runtime.block_on(execute_async(input, cancel))
}
async fn execute_async(input: Request<'_>, cancel: CancelToken) -> Result<Response, Error> {
    let parsed = url::Url::parse(input.url).map_err(|_| Error::InvalidRequest)?;
    let origin = parsed.origin().ascii_serialization();
    let declaration = [Declaration {
        origin: &origin,
        methods: &[input.method],
    }];
    let request = HttpFence::new(Some(&declaration), None)
        .and_then(|f| f.prepare(input.url, input.method, input.headers, input.body))
        .map_err(|_| Error::InvalidRequest)?;
    let run = Run::new(input.timeout).map_err(|_| Error::Deadline)?;
    let fence = Fence::new(Some(&[&origin]), None).map_err(|_| Error::InvalidRequest)?;
    let gate = Arc::new(Gate::new(Arc::new(Admitted(cancel.clone()))));
    let scope = NetworkScope::begin_tool(
        &run,
        fence,
        gate,
        cancel.clone(),
        Instant::now() + input.timeout,
    )
    .map_err(|e| network_error(&cancel, e))?;
    let dns = SystemDns::shared().map_err(|_| Error::Transport)?;
    let job = scope
        .resolve(&dns, request.origin().clone(), input.timeout)
        .await
        .map_err(|e| network_error(&cancel, e))?;
    job.ready().await;
    let resolution = job
        .get()
        .ok_or(Error::Transport)?
        .map_err(|e| dns_error(&cancel, e))?;
    let mut stream = scope
        .http(request, &resolution, input.timeout, input.max_bytes)
        .await
        .map_err(|e| transport_error(&cancel, e))?;
    let status = stream.status().map_err(|e| transport_error(&cancel, e))?;
    let headers = stream.headers().map_err(|e| transport_error(&cancel, e))?;
    let content_type = headers
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_owned();
    let location = headers
        .get("location")
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned);
    let mut body = Vec::new();
    loop {
        let chunk = stream
            .read(65536)
            .await
            .map_err(|e| transport_error(&cancel, e))?;
        if chunk.is_empty() {
            break;
        }
        body.extend_from_slice(&chunk);
    }
    Ok(Response {
        status,
        content_type,
        location,
        body,
    })
}
fn network_error(cancel: &CancelToken, error: super::network::Error) -> Error {
    match error {
        super::network::Error::Authority(error) => dns_error(cancel, error),
        super::network::Error::Permission(error) => {
            transport_error(cancel, super::transport::Failure::Permission(error))
        }
    }
}
fn dns_error(cancel: &CancelToken, error: crate::dns_authority::Failure) -> Error {
    use crate::dns_authority::Failure as F;
    if cancel.is_cancelled() {
        return Error::Cancelled;
    }
    match error {
        F::Cancelled => Error::Cancelled,
        F::DeadlineExceeded => Error::Deadline,
        F::ResolutionFailed => Error::Transport,
        F::LimitExceeded => Error::Limit,
        _ => Error::Blocked,
    }
}
fn transport_error(cancel: &CancelToken, error: super::transport::Failure) -> Error {
    use super::transport::Failure as F;
    if cancel.is_cancelled() {
        return Error::Cancelled;
    }
    match error {
        F::Limit => Error::Limit,
        F::Encoding => Error::Encoding,
        F::Authority(crate::dns_authority::Failure::DeadlineExceeded)
        | F::Permission(Failure::Deadline) => Error::Deadline,
        F::Authority(_) | F::Permission(_) => Error::Blocked,
        F::Transport => Error::Transport,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn native_web_transport_rejects_non_public_url_shapes_and_precancel() {
        for url in [
            "http://127.0.0.1:1/",
            "http://[::1]:1/",
            "file:///tmp/file",
            "https://user:pass@example.com/",
        ] {
            let result = execute(
                Request {
                    url,
                    method: "GET",
                    headers: &[],
                    body: &[],
                    timeout: Duration::from_secs(1),
                    max_bytes: 1024,
                },
                CancelToken::new(),
            );
            assert_eq!(result.unwrap_err(), Error::InvalidRequest);
        }
        let cancel = CancelToken::new();
        cancel.cancel();
        let result = execute(
            Request {
                url: "https://example.com/",
                method: "GET",
                headers: &[],
                body: &[],
                timeout: Duration::from_secs(1),
                max_bytes: 1024,
            },
            cancel,
        );
        assert_eq!(result.unwrap_err(), Error::Cancelled);
    }
}
