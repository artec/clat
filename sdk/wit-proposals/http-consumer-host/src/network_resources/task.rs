//! One polling mechanism for DNS, HTTP headers, and entity reads; no spawn.
use crate::{
    dns_authority::{Failure, Resolution, Scope},
    http_authority::transport::{Failure as HttpFailure, StreamResponse},
};
use std::{
    future::Future,
    pin::Pin,
    time::{Duration, Instant},
};
use wasmtime_wasi::p2::Pollable;
pub(crate) enum Outcome {
    Resolution(Resolution),
    Response(StreamResponse),
    Read(StreamResponse, Vec<u8>),
}
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Error {
    Authority(Failure),
    Http(HttpFailure),
    Cancelled,
    Consumed,
}
type Operation = Pin<Box<dyn Future<Output = Result<Outcome, Error>> + Send>>;
enum State {
    Pending(Operation),
    Ready(Box<Result<Outcome, Error>>),
    Cancelled,
    Consumed,
}
pub struct Task {
    state: State,
    scope: Scope,
    deadline: Instant,
}
impl Task {
    pub(crate) fn new(
        scope: &Scope,
        deadline: Instant,
        operation: impl Future<Output = Result<Outcome, Error>> + Send + 'static,
    ) -> Self {
        Self {
            state: State::Pending(Box::pin(operation)),
            scope: scope.borrow_handle(),
            deadline,
        }
    }
    pub(crate) fn cancel(&mut self) {
        self.state = State::Cancelled;
    }
    pub(crate) fn get(&mut self) -> Option<Result<Outcome, Error>> {
        if let Err(error) = self.scope.check_active(self.deadline) {
            self.state = State::Cancelled;
            return Some(Err(Error::Authority(error)));
        }
        if matches!(self.state, State::Pending(_)) {
            return None;
        }
        Some(match std::mem::replace(&mut self.state, State::Consumed) {
            State::Ready(result) => *result,
            State::Cancelled => Err(Error::Cancelled),
            _ => Err(Error::Consumed),
        })
    }
    pub(crate) async fn ready(&mut self) {
        if !matches!(self.state, State::Pending(_)) {
            return;
        }
        let State::Pending(operation) = std::mem::replace(&mut self.state, State::Cancelled) else {
            unreachable!()
        };
        let result = tokio::select! {
            biased;
            error=self.scope.closed(self.deadline)=>Err(Error::Authority(error)),
            result=operation=>result,
        };
        self.state = State::Ready(Box::new(result));
    }
}
impl Drop for Task {
    fn drop(&mut self) {
        self.cancel();
    }
}
#[wasmtime_wasi::async_trait]
impl Pollable for Task {
    async fn ready(&mut self) {
        // WASI ready()/poll() drops incomplete readiness futures routinely.
        // Keep the operation in Task; dropping this wait is not cancellation.
        let State::Pending(operation) = &mut self.state else {
            return;
        };
        let result = tokio::select! {
            biased;
            error = self.scope.closed(self.deadline) => Err(Error::Authority(error)),
            result = operation => result,
        };
        self.state = State::Ready(Box::new(result));
    }
}
// The host, not the guest, must provide a tool-bound ceiling.
pub(crate) fn deadline(entered: Instant, timeout: Duration) -> Result<Instant, Error> {
    if timeout.is_zero() || timeout > Duration::from_secs(30) {
        return Err(Error::Authority(Failure::DeadlineExceeded));
    }
    Ok(entered + timeout)
}

impl Task {
    pub(crate) fn dns(
        network: std::sync::Arc<crate::http_authority::network::NetworkScope>,
        dns: std::sync::Arc<crate::dns_authority::SystemDns>,
        origin: crate::dns_authority::Origin,
        timeout: Duration,
    ) -> Result<Self, Error> {
        let ceiling = deadline(Instant::now(), timeout)?;
        let scope = network.scope().borrow_handle();
        Ok(Self::new(&scope, ceiling, async move {
            let job =
                network
                    .resolve(&dns, origin, timeout)
                    .await
                    .map_err(|error| match error {
                        crate::http_authority::network::Error::Authority(error) => {
                            Error::Authority(error)
                        }
                        crate::http_authority::network::Error::Permission(error) => {
                            Error::Http(HttpFailure::Permission(error))
                        }
                    })?;
            job.ready().await;
            job.get()
                .ok_or(Error::Consumed)?
                .map(Outcome::Resolution)
                .map_err(Error::Authority)
        }))
    }
    pub(crate) fn http(
        network: std::sync::Arc<crate::http_authority::network::NetworkScope>,
        resolution: Resolution,
        request: crate::http_authority::PreparedRequest,
        timeout: Duration,
        max: usize,
    ) -> Result<Self, Error> {
        let ceiling = deadline(Instant::now(), timeout)?;
        let scope = network.scope().borrow_handle();
        Ok(Self::new(&scope, ceiling, async move {
            network
                .http(request, &resolution, timeout, max)
                .await
                .map(Outcome::Response)
                .map_err(Error::Http)
        }))
    }
    pub(crate) fn read(
        scope: &Scope,
        mut response: StreamResponse,
        max_bytes: usize,
        ceiling: Instant,
    ) -> Self {
        Self::new(scope, ceiling, async move {
            let bytes = response.read(max_bytes).await.map_err(Error::Http)?;
            Ok(Outcome::Read(response, bytes))
        })
    }
}
