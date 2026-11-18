//! Desktop-only Host activity feed and cancellation, separate from workbench SSE.
use super::{http, protocol::RpcError, state::ServeShared, workspaces::WorkspaceHost};
use crate::native_tools::ui_activity::ComputerUseTarget;
use serde_json::json;
use std::io::Write;
use std::net::TcpStream;
use std::time::Duration;

const EVENTS: &str = "/api/computer-use/events";
const STOP: &str = "/api/computer-use/stop";

pub(super) fn handles(path: &str) -> bool {
    matches!(path, EVENTS | STOP)
}

pub(super) fn handle(
    stream: &mut TcpStream,
    head: &mut http::HttpRequestHead,
    shared: &ServeShared,
) {
    let local = stream.peer_addr().is_ok_and(|peer| peer.ip().is_loopback());
    let bearer = http::bearer_token(head.authorization.as_deref());
    let host = shared.host.lock().expect("host link").upgrade();
    let Some(host) = host.filter(|host| {
        local
            && head.origin.is_none()
            && head.fetch_site.is_none()
            && bearer.as_deref() == Some(shared.token.as_str())
            && head
                .expected_instance
                .as_deref()
                .is_some_and(|id| host.matches_instance(id))
    }) else {
        super::auth::reject(stream, 403, "local_host_owner_required");
        return;
    };
    match (head.method.as_str(), head.path.as_str()) {
        ("GET", EVENTS) => events(stream, &host),
        ("POST", STOP) => stop(stream, head, &host),
        _ => reject(stream, 404, RpcError::not_found("no such route")),
    }
}

fn reject(stream: &mut TcpStream, status: u16, error: RpcError) {
    let _ = super::write_json(
        stream,
        status,
        super::protocol::rpc_result_json(&Err(error)),
    );
}

fn events(stream: &mut TcpStream, host: &WorkspaceHost) {
    let _ = stream.set_write_timeout(Some(Duration::from_secs(3)));
    if http::write_sse_head(stream).is_err() {
        return;
    }
    let mut revision = None;
    while !host.is_stopping() {
        let result = match host.computer_use.next(revision, Duration::from_secs(1)) {
            Some(snapshot) => {
                revision = Some(snapshot.revision);
                serde_json::to_string(&snapshot)
                    .map_err(std::io::Error::other)
                    .and_then(|data| write!(stream, "event: computer-use\ndata: {data}\n\n"))
            }
            None => stream.write_all(b": heartbeat\n\n"),
        };
        if result.and_then(|_| stream.flush()).is_err() {
            break;
        }
    }
}

fn stop(stream: &mut TcpStream, head: &mut http::HttpRequestHead, host: &WorkspaceHost) {
    let target = http::read_body(stream, head, 16 * 1024)
        .ok()
        .and_then(|body| serde_json::from_slice::<ComputerUseTarget>(&body).ok());
    let Some(target) = target else {
        reject(
            stream,
            400,
            RpcError::bad_request("invalid computer use target"),
        );
        return;
    };
    let stopped = !host.is_stopping() && host.computer_use.stop(&target);
    let _ = super::write_json(stream, 200, json!({ "stopped": stopped }).to_string());
}
