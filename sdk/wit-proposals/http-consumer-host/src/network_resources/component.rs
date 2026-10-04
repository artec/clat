//! Generated typed Host over the same private task/credential/transport authority.
use super::{Error, Owner, Task, task};
use crate::{
    dns_authority::{Origin, Resolution, SystemDns},
    http_authority::{HttpFence, network::NetworkScope, transport::StreamResponse},
};
use std::{
    sync::Arc,
    time::{Duration, Instant},
};
use wasmtime::component::{Linker, Resource};
use wasmtime_wasi::p2::{DynPollable, bindings::sync::io::poll};
wasmtime::component::bindgen!({
    path: "../typed-task", world: "typed-client",
    imports: { default: trappable },
    with: {
        "wasi:io/poll@0.2.10": wasmtime_wasi::p2::bindings::sync::io::poll,
        "clat:net-task/egress.task": crate::network_resources::task::Task,
        "clat:net-task/egress.resolution": crate::dns_authority::Resolution,
        "clat:net-task/egress.response": crate::http_authority::transport::StreamResponse,
    },
});
use clat::net_task::egress::*;
#[cfg(test)]
type CloseProbe = Box<dyn FnOnce(&HostState) + Send>;
// Constructed only by trusted host, per tool. No ambient WASI or guest fences.
pub(super) struct HostState {
    owner: Owner,
    limits: wasmtime::StoreLimits,
    network: Arc<NetworkScope>,
    dns: Arc<SystemDns>,
    fence: HttpFence,
    config: Option<String>,
    #[cfg(test)]
    dns_submitted: u32,
    #[cfg(test)]
    http_submitted: u32,
    #[cfg(test)]
    transport_fixture: bool,
    #[cfg(test)]
    public_transport_fixture: bool,
    #[cfg(test)]
    close_probe: Option<CloseProbe>,
}
impl HostState {
    pub(super) fn from_policy(
        run: &crate::dns_authority::Run,
        gate: Arc<crate::http_authority::permission::Gate>,
        cancel: clat_core::CancelToken,
        deadline: Instant,
        dns: Arc<SystemDns>,
        policy: crate::capabilities::Policy,
    ) -> Result<Self, crate::http_authority::network::Error> {
        let (fence, http, clock) = policy.into_parts();
        let network = Arc::new(NetworkScope::begin_tool(
            run, fence, gate, cancel, deadline,
        )?);
        Ok(Self {
            owner: Owner::with_clock(network.scope(), clock),
            limits: invocation::limits(),
            network,
            dns,
            fence: http,
            config: None,
            #[cfg(test)]
            dns_submitted: 0,
            #[cfg(test)]
            http_submitted: 0,
            #[cfg(test)]
            transport_fixture: false,
            #[cfg(test)]
            public_transport_fixture: false,
            #[cfg(test)]
            close_probe: None,
        })
    }
    pub(super) fn link(linker: &mut Linker<Self>) -> wasmtime::Result<()> {
        clat::net_task::egress::add_to_linker::<_, wasmtime::component::HasSelf<_>>(linker, |s| s)?;
        poll::add_to_linker::<_, wasmtime::component::HasSelf<_>>(linker, |s| &mut s.owner)
    }
    pub(super) fn link_scheduler(linker: &mut Linker<Self>) -> wasmtime::Result<()> {
        use wasmtime_wasi::p2::bindings::{
            cli::stderr,
            clocks,
            sync::io::{error, streams},
        };
        type Data = wasmtime::component::HasSelf<Owner>;
        Self::link(linker)?;
        plugin_interface::link(linker)?;
        clocks::monotonic_clock::add_to_linker::<_, Data>(linker, |s| &mut s.owner)?;
        clocks::wall_clock::add_to_linker::<_, Data>(linker, |s| &mut s.owner)?;
        stderr::add_to_linker::<_, Data>(linker, |s| &mut s.owner)?;
        error::add_to_linker::<_, Data>(linker, |s| &mut s.owner)?;
        streams::add_to_linker::<_, Data>(linker, |s| &mut s.owner)
    }
    fn insert<T: Send + 'static>(&mut self, value: T) -> Result<Resource<T>, Failure> {
        self.owner
            .insert(value)
            .map(Owner::resource)
            .map_err(owner_error)
    }
    fn take<T: 'static>(&mut self, value: Resource<T>) -> Result<T, Failure> {
        let handle = self.owner.binding(&value).map_err(owner_error)?;
        self.owner.take(&handle).map_err(owner_error)
    }
    fn get_mut<T: 'static>(&mut self, resource: Resource<T>) -> Result<&mut T, Failure> {
        let handle = self.owner.binding(&resource).map_err(owner_error)?;
        self.owner.get_mut(&handle).map_err(owner_error)
    }
    fn drop_resource<T: 'static>(&mut self, resource: Resource<T>) -> wasmtime::Result<()> {
        self.owner.drop_binding(resource).map_err(trap)
    }
    fn outcome(&mut self, outcome: task::Outcome) -> Result<Outcome, Failure> {
        Ok(match outcome {
            task::Outcome::Resolution(value) => Outcome::Resolved(self.insert(value)?),
            task::Outcome::Response(value) => Outcome::Headers(self.insert(value)?),
            task::Outcome::Read(value, bytes) => Outcome::Body(Chunk {
                response: self.insert(value)?,
                bytes,
            }),
        })
    }
}
impl Host for HostState {
    fn dns_start(
        &mut self,
        origin: String,
        timeout_ms: u32,
    ) -> wasmtime::Result<Result<Resource<Task>, Failure>> {
        #[cfg(test)]
        {
            self.dns_submitted += 1;
        }
        Ok((|| {
            self.owner.check().map_err(owner_error)?;
            let origin = Origin::parse(&origin).map_err(dns_error)?;
            #[cfg(test)]
            if self.transport_fixture {
                // Private transport fixture, never a DNS policy bypass in a built host.
                let deadline = Instant::now() + Duration::from_secs(2);
                let mut resolution = crate::dns_authority::test_resolution(
                    self.network.scope(),
                    origin,
                    vec![
                        if self.public_transport_fixture {
                            "8.8.8.8"
                        } else {
                            "127.0.0.1"
                        }
                        .parse()
                        .unwrap(),
                    ],
                    deadline,
                );
                if self.public_transport_fixture {
                    resolution = resolution.with_loopback_fixture();
                }
                let task = Task::new(self.network.scope(), deadline, async move {
                    Ok(task::Outcome::Resolution(resolution))
                });
                return self.insert(task);
            }
            let task = Task::dns(
                self.network.clone(),
                self.dns.clone(),
                origin,
                Duration::from_millis(timeout_ms.into()),
            )
            .map_err(task_error)?;
            self.insert(task)
        })())
    }
    fn http_start(
        &mut self,
        target: Resource<Resolution>,
        request: Request,
    ) -> wasmtime::Result<Result<Resource<Task>, Failure>> {
        #[cfg(test)]
        {
            self.http_submitted += 1;
        }
        Ok((|| {
            // Owned input is released even if validation/admission fails.
            let resolution = self.take(target)?;
            let headers: Vec<_> = request
                .headers
                .iter()
                .map(|h| (h.name.as_str(), h.value.as_str()))
                .collect();
            let prepared = self
                .fence
                .prepare(&request.url, method(request.verb), &headers, &request.body)
                .map_err(request_error)?;
            let task = Task::http(
                self.network.clone(),
                resolution,
                prepared,
                Duration::from_millis(request.timeout_ms.into()),
                request.max_response_bytes as usize,
            )
            .map_err(task_error)?;
            self.insert(task)
        })())
    }
    fn read_start(
        &mut self,
        response: Resource<StreamResponse>,
        max_bytes: u32,
    ) -> wasmtime::Result<Result<Resource<Task>, Failure>> {
        Ok((|| {
            let response = self.take(response)?;
            if !(1..=65536).contains(&max_bytes) {
                return Err(Failure::InvalidRequest);
            }
            let task = Task::read(
                self.network.scope(),
                response,
                max_bytes as usize,
                Instant::now() + Duration::from_secs(30),
            );
            self.insert(task)
        })())
    }
}
impl HostTask for HostState {
    fn get(
        &mut self,
        resource: Resource<Task>,
    ) -> wasmtime::Result<Option<Result<Outcome, Failure>>> {
        Ok(match self.get_mut(resource) {
            Err(error) => Some(Err(error)),
            Ok(task) => match task.get() {
                None => None,
                Some(Err(error)) => Some(Err(task_error(error))),
                Some(Ok(result)) => Some(self.outcome(result)),
            },
        })
    }
    fn subscribe(
        &mut self,
        resource: Resource<Task>,
    ) -> wasmtime::Result<Result<Resource<DynPollable>, Failure>> {
        Ok((|| {
            let handle = self.owner.binding(&resource).map_err(owner_error)?;
            self.owner
                .subscribe(&handle)
                .map(Owner::resource)
                .map_err(owner_error)
        })())
    }
    fn cancel(&mut self, resource: Resource<Task>) -> wasmtime::Result<()> {
        // Cleanup after invalidation is safe: check may already have cleared it.
        if self.owner.check().is_ok() {
            self.get_mut(resource)
                .map_err(|e| wasmtime::format_err!("{e:?}"))?
                .cancel();
        }
        Ok(())
    }
    fn drop(&mut self, resource: Resource<Task>) -> wasmtime::Result<()> {
        self.drop_resource(resource)
    }
}
impl HostResolution for HostState {
    fn addresses(
        &mut self,
        resource: Resource<Resolution>,
    ) -> wasmtime::Result<Result<Vec<Address>, Failure>> {
        Ok((|| {
            self.owner.check().map_err(owner_error)?;
            let handle = self.owner.binding(&resource).map_err(owner_error)?;
            self.owner
                .get_mut(&handle)
                .map_err(owner_error)?
                .addresses(self.network.scope())
                .map(addresses)
                .map_err(dns_error)
        })())
    }
    fn dns64_answers(
        &mut self,
        resource: Resource<Resolution>,
    ) -> wasmtime::Result<Result<Vec<Address>, Failure>> {
        Ok((|| {
            self.owner.check().map_err(owner_error)?;
            let handle = self.owner.binding(&resource).map_err(owner_error)?;
            self.owner
                .get_mut(&handle)
                .map_err(owner_error)?
                .discovery_answers(self.network.scope())
                .map(addresses)
                .map_err(dns_error)
        })())
    }
    fn drop(&mut self, resource: Resource<Resolution>) -> wasmtime::Result<()> {
        self.drop_resource(resource)
    }
}
impl HostResponse for HostState {
    fn status(
        &mut self,
        resource: Resource<StreamResponse>,
    ) -> wasmtime::Result<Result<u16, Failure>> {
        Ok(self
            .get_mut(resource)
            .and_then(|r| r.status().map_err(http_error)))
    }
    fn headers(
        &mut self,
        resource: Resource<StreamResponse>,
    ) -> wasmtime::Result<Result<Vec<Header>, Failure>> {
        Ok(self.get_mut(resource).and_then(|r| {
            let result = r
                .headers()
                .map_err(http_error)?
                .iter()
                .map(|(name, value)| {
                    Ok(Header {
                        name: name.as_str().into(),
                        value: value.to_str().map_err(|_| Failure::Unsupported)?.into(),
                    })
                })
                .collect::<Result<Vec<_>, Failure>>();
            if result.is_err() {
                r.cancel();
            }
            result
        }))
    }
    fn cancel(&mut self, resource: Resource<StreamResponse>) -> wasmtime::Result<()> {
        if self.owner.check().is_ok() {
            self.get_mut(resource)
                .map_err(|e| wasmtime::format_err!("{e:?}"))?
                .cancel();
        }
        Ok(())
    }
    fn drop(&mut self, resource: Resource<StreamResponse>) -> wasmtime::Result<()> {
        self.drop_resource(resource)
    }
}
fn trap(error: Error) -> wasmtime::Error {
    wasmtime::format_err!("network resource: {error:?}")
}
fn addresses(values: Vec<std::net::IpAddr>) -> Vec<Address> {
    values
        .into_iter()
        .map(|ip| Address {
            family: if ip.is_ipv4() {
                AddressFamily::Ipv4
            } else {
                AddressFamily::Ipv6
            },
            ip: ip.to_string(),
        })
        .collect()
}
fn method(value: Method) -> &'static str {
    match value {
        Method::Get => "GET",
        Method::Head => "HEAD",
        Method::Post => "POST",
        Method::Put => "PUT",
        Method::Patch => "PATCH",
        Method::Delete => "DELETE",
        Method::Options => "OPTIONS",
    }
}
mod errors;
use errors::*;
#[cfg(test)]
mod tests;

#[cfg(test)]
mod native_tests;

#[cfg(test)]
mod native_transport_tests;

mod invocation;

mod plugin_interface;
