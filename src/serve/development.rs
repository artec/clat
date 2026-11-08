//! Authenticated human development adapter; no session selection/run lock is held during I/O.
use super::{protocol::RpcError, state::ServeShared};
use serde_json::Value;
use std::{io::Write, net::TcpStream, sync::Arc, time::Duration};
pub(super) fn dispatch(
    method: &str,
    value: &Value,
    shared: &Arc<ServeShared>,
) -> Result<Value, RpcError> {
    if shared.is_shutting_down() {
        return Err(error("host_closed"));
    }
    let (service, model) = {
        let app = shared.app.lock().expect("application");
        let service = app.development()?;
        let model = if method == "git.start"
            && matches!(value["action"].as_str(), Some("commit" | "generate"))
            && (value["action"] == "generate"
                || value["message"].as_str().unwrap_or("").trim().is_empty())
        {
            app.development_model().ok()
        } else {
            None
        };
        (service, model)
    };
    service
        .request(method, value.clone(), model)
        .map_err(|e| error(&e))
}
fn error(reason: &str) -> RpcError {
    RpcError::bad_request(reason.to_owned()).with_presentation(
        crate::application::DevelopmentService::error_presentation(reason),
    )
}

pub(super) fn stream(stream: &mut TcpStream, path: &str, shared: &Arc<ServeShared>) {
    let parts: Vec<_> = path
        .trim_start_matches("/api/terminal/")
        .split('/')
        .collect();
    if parts.len() != 2 {
        return;
    }
    let id = parts[0];
    let Ok(mut offset) = parts[1].parse::<u64>() else {
        return;
    };
    let service = shared.app.lock().expect("application").development();
    let service = match service {
        Ok(service) => service,
        Err(error) => {
            let _ = super::write_json(
                stream,
                400,
                super::protocol::rpc_result_json(&Err(error.into())),
            );
            return;
        }
    };
    if service.terminal_output(id, offset).is_err() {
        return;
    }
    let _ = stream.set_write_timeout(Some(Duration::from_secs(3)));
    if super::http::write_sse_head(stream).is_err() {
        return;
    }
    let mut heartbeat = std::time::Instant::now();
    while !shared.is_shutting_down() {
        // Event-driven pickup: wake on append/output_done/closed, bounded by
        // this timeout so shutdown detection and heartbeats stay responsive.
        let _ = service.terminal_wait(id, offset, Duration::from_millis(250));
        let Ok(chunk) = service.terminal_output(id, offset) else {
            break;
        };
        let end = chunk["offset"].as_u64().unwrap_or(offset);
        if end != offset || chunk["reset"] == true || chunk["ended"] == true {
            if writeln!(stream, "event: terminal\ndata: {chunk}\n")
                .and_then(|_| stream.flush())
                .is_err()
            {
                break;
            }
            offset = end;
            if chunk["ended"] == true {
                break;
            }
        } else if heartbeat.elapsed() > Duration::from_secs(10) {
            if stream
                .write_all(b": heartbeat\n\n")
                .and_then(|_| stream.flush())
                .is_err()
            {
                break;
            }
            heartbeat = std::time::Instant::now();
        }
    }
}
