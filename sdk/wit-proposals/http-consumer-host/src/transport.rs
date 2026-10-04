//! Bounded fake HTTP entity transport, with no DNS/socket/network implementation.
use bytes::Bytes;
use http_body::{Body, Frame};
use http_body_util::BodyExt;
use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::Duration;
use wasmtime_wasi_http::{Error, RequestOptions, WasiBody, WasiHttpHooks};

pub use crate::events::{EventLog, Events};
type Worker = Box<dyn Future<Output = Result<(), Error>> + Send>;
type Reply = Result<(http::Response<WasiBody>, Worker), Error>;

pub struct Hooks {
    pub events: Events,
}

struct Watch {
    events: Events,
    phase: String,
    completed: bool,
}

impl Watch {
    fn new(events: Events, phase: &str) -> Self {
        events.push(format!("{phase}:start"));
        Self {
            events,
            phase: phase.into(),
            completed: false,
        }
    }
    fn complete(&mut self) {
        self.completed = true;
        self.events.push(format!("{}:complete", self.phase));
    }
}

impl Drop for Watch {
    fn drop(&mut self) {
        if !self.completed {
            self.events.push(format!("{}:cancel", self.phase));
        }
    }
}

impl WasiHttpHooks for Hooks {
    fn send_request(
        &mut self,
        request: http::Request<WasiBody>,
        _: Option<RequestOptions>,
        _: Worker,
    ) -> Box<dyn Future<Output = Reply> + Send> {
        let uri = request.uri();
        if uri.scheme_str() != Some("https")
            || uri.host() != Some("api.deepseek.com")
            || request.method() != http::Method::GET
            || !matches!(uri.path(), "/dns" | "/headers" | "/body")
        {
            return Box::new(async { Err(Error::HttpRequestDenied) });
        }
        let stage = uri.path().trim_start_matches('/').to_string();
        let events = self.events.clone();
        let mut watch = Watch::new(events.clone(), &stage);
        Box::new(async move {
            if stage != "body" {
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
            watch.complete();
            let body = DelayedBody::new(events, stage == "body").boxed_unsync();
            let response = http::Response::builder().status(200).body(body).unwrap();
            Ok((response, Box::new(async { Ok(()) }) as Worker))
        })
    }
}

struct DelayedBody {
    delay: Option<Pin<Box<tokio::time::Sleep>>>,
    sent: bool,
    watch: Watch,
}

impl DelayedBody {
    fn new(events: Events, delay: bool) -> Self {
        Self {
            delay: delay.then(|| Box::pin(tokio::time::sleep(Duration::from_millis(100)))),
            sent: false,
            watch: Watch::new(events, "entity"),
        }
    }
}

impl Body for DelayedBody {
    type Data = Bytes;
    type Error = Error;
    fn poll_frame(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, Error>>> {
        if let Some(delay) = &mut self.delay {
            if delay.as_mut().poll(cx).is_pending() {
                return Poll::Pending;
            }
            self.delay = None;
        }
        if self.sent {
            return Poll::Ready(None);
        }
        self.sent = true;
        self.watch.complete();
        Poll::Ready(Some(Ok(Frame::data(Bytes::from_static(b"ok")))))
    }
}
