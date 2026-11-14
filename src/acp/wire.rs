use crate::{
    CancelToken,
    process::{ManagedStdioLease, ProcessService},
};
use serde_json::{Value, json};
use std::{
    sync::{Arc, mpsc},
    time::{Duration, Instant},
};

pub(super) const FRAME: usize = 4 * 1024 * 1024;
const POLL: Duration = Duration::from_millis(40);
const CALLBACK_LIMIT: usize = 16;

#[derive(Clone)]
pub(super) struct Handle {
    pub lease: ManagedStdioLease,
    pub process: Arc<ProcessService>,
}

impl Drop for Wire {
    fn drop(&mut self) {
        let _ = self.handle.process.close_managed_stdio(&self.handle.lease);
    }
}

pub(super) struct Wire {
    handle: Handle,
    buffer: Vec<u8>,
    cursor: usize,
    next: u64,
}

pub(super) type Reply = Result<Value, i64>;
pub(super) type Callback = Box<dyn FnOnce() -> Reply + Send>;

pub(super) trait Client {
    fn update(&mut self, params: Value) -> Result<(), String>;
    fn callback(
        &mut self,
        method: &str,
        params: Value,
        request_id: u64,
        cancel: &CancelToken,
    ) -> Result<Callback, i64>;
}

pub(super) struct Request<'a> {
    pub method: &'a str,
    pub params: Value,
    pub session: Option<&'a str>,
    pub cancel: &'a CancelToken,
    pub limit: Duration,
}

impl Wire {
    pub(super) fn new(handle: Handle) -> Self {
        Self {
            handle,
            buffer: Vec::new(),
            cursor: 0,
            next: 0,
        }
    }

    pub(super) fn is_connected(&self) -> bool {
        !self.handle.lease.is_terminal()
    }

    fn send(&self, payload: Value) -> Result<(), String> {
        let mut bytes = serde_json::to_vec(&payload).map_err(|_| "ACP encode failed")?;
        if bytes.len() > FRAME {
            return Err("ACP frame too large".into());
        }
        bytes.push(b'\n');
        self.handle.lease.write_all(&bytes)
    }

    fn frame(&mut self) -> Result<Option<Value>, String> {
        if let Some(offset) = self.buffer[self.cursor..]
            .iter()
            .position(|byte| *byte == b'\n')
        {
            let end = self.cursor + offset;
            if offset > FRAME {
                return Err("ACP frame too large".into());
            }
            let value = serde_json::from_slice(&self.buffer[self.cursor..end])
                .map_err(|_| "invalid ACP JSON frame")?;
            self.cursor = end + 1;
            return Ok(Some(value));
        }
        if self.cursor != 0 {
            self.buffer.drain(..self.cursor);
            self.cursor = 0;
        }
        if self.buffer.len() > FRAME {
            return Err("ACP frame too large".into());
        }
        let bytes = self.handle.lease.read_stdout(POLL, 64 * 1024)?;
        if bytes.is_empty() && self.handle.lease.is_terminal() {
            return Err("ACP process disconnected".into());
        }
        self.buffer.extend(bytes);
        Ok(None)
    }

    pub(super) fn request(
        &mut self,
        request: Request<'_>,
        client: &mut impl Client,
    ) -> Result<Value, String> {
        if request.cancel.is_cancelled() {
            return Err("ACP request cancelled".into());
        }
        self.next += 1;
        let id = self.next;
        self.send(
            json!({"jsonrpc":"2.0","id":id,"method":request.method,"params":request.params}),
        )?;
        let deadline = Instant::now() + request.limit;
        let callbacks_cancel = request.cancel.child_with_deadline(deadline);
        let mut cancellation = None;
        let prefix = request.session;
        let mut callbacks = Callbacks {
            request_id: id,
            cancel: Some(callbacks_cancel.clone()),
            pending: Vec::new(),
        };
        loop {
            if callbacks_cancel.is_cancelled() && cancellation.is_none() {
                callbacks_cancel.cancel();
                cancellation = Some(Instant::now() + Duration::from_secs(2));
                callbacks.cancel(self)?;
                self.send(cancel_frame(id, prefix))?;
            }
            if cancellation.is_none() {
                callbacks.flush(self)?;
            }
            if cancellation.is_some_and(|end| Instant::now() >= end) {
                return Err("ACP request did not settle after cancellation".into());
            }
            let Some(frame) = self.frame()? else { continue };
            if frame["jsonrpc"] != "2.0" {
                return Err("invalid ACP JSON-RPC envelope".into());
            }
            if let Some(method) = frame["method"].as_str().map(str::to_owned) {
                callbacks.handle(
                    self,
                    client,
                    &method,
                    frame,
                    cancellation.is_some(),
                    &callbacks_cancel,
                )?;
                continue;
            }
            if frame["id"] != id
                || !callbacks.pending.is_empty()
                || frame.get("result").is_some() == frame.get("error").is_some()
            {
                return Err("unexpected ACP response".into());
            }
            if cancellation.is_some() {
                return Err("ACP request cancelled".into());
            }
            return if let Some(result) = frame.get("result").filter(|result| result.is_object()) {
                Ok(result.clone())
            } else {
                let code = frame["error"]["code"]
                    .as_i64()
                    .ok_or("invalid ACP response")?;
                Err(format!("ACP request rejected ({code})"))
            };
        }
    }

    fn respond(&self, id: Value, result: Reply) -> Result<(), String> {
        let response = match result {
            Ok(value) => json!({"jsonrpc":"2.0","id":id,"result":value}),
            Err(code) => json!({"jsonrpc":"2.0","id":id,
                "error":{"code":code,"message":error_message(code)}}),
        };
        self.send(response)
    }
}

fn error_message(code: i64) -> &'static str {
    match code {
        -32601 => "Method not found",
        -32602 => "Invalid params",
        -32800 => "Request cancelled",
        _ => "Request denied or unavailable",
    }
}

fn cancel_frame(id: u64, session: Option<&str>) -> Value {
    match session {
        Some(id) => json!({"jsonrpc":"2.0","method":"session/cancel","params":{"sessionId":id}}),
        None => json!({"jsonrpc":"2.0","method":"$/cancel_request","params":{"requestId":id}}),
    }
}

#[derive(Default)]
struct Callbacks {
    request_id: u64,
    cancel: Option<CancelToken>,
    pending: Vec<(Value, String, mpsc::Receiver<Reply>)>,
}

impl Drop for Callbacks {
    fn drop(&mut self) {
        if let Some(cancel) = &self.cancel {
            cancel.cancel();
        }
    }
}

impl Callbacks {
    fn flush(&mut self, wire: &Wire) -> Result<(), String> {
        let mut index = 0;
        while let Some((_, _, receiver)) = self.pending.get(index) {
            let result = match receiver.try_recv() {
                Ok(result) => result,
                Err(mpsc::TryRecvError::Disconnected) => Err(-32603),
                Err(mpsc::TryRecvError::Empty) => {
                    index += 1;
                    continue;
                }
            };
            let (id, _, _) = self.pending.swap_remove(index);
            wire.respond(id, result)?;
        }
        Ok(())
    }

    fn cancel(&mut self, wire: &Wire) -> Result<(), String> {
        for (id, method, _) in self.pending.drain(..) {
            wire.respond(id, cancelled(&method))?;
        }
        Ok(())
    }

    fn handle(
        &mut self,
        wire: &Wire,
        client: &mut impl Client,
        method: &str,
        frame: Value,
        cancelling: bool,
        cancel: &CancelToken,
    ) -> Result<(), String> {
        let params = frame.get("params").cloned().unwrap_or(Value::Null);
        let Some(id) = frame.get("id") else {
            if method == "session/update" && !cancelling {
                client.update(params)?;
            }
            return Ok(());
        };
        if (!id.is_string() && !id.is_i64() && !id.is_u64())
            || self.pending.iter().any(|(pending, _, _)| pending == id)
        {
            return Err("invalid ACP callback identifier".into());
        }
        if cancelling {
            return wire.respond(id.clone(), cancelled(method));
        }
        if self.pending.len() >= CALLBACK_LIMIT {
            return wire.respond(id.clone(), Err(-32603));
        }
        let callback = match client.callback(method, params, self.request_id, cancel) {
            Ok(callback) => callback,
            Err(code) => return wire.respond(id.clone(), Err(code)),
        };
        let (sender, receive) = mpsc::sync_channel(1);
        std::thread::Builder::new()
            .name("clat-acp-callback".into())
            .spawn(move || {
                let _ = sender.send(callback());
            })
            .map_err(|_| "cannot start ACP callback")?;
        self.pending.push((id.clone(), method.into(), receive));
        Ok(())
    }
}

fn cancelled(method: &str) -> Reply {
    if method == "session/request_permission" {
        Ok(json!({"outcome":{"outcome":"cancelled"}}))
    } else if method == "elicitation/create" {
        Ok(json!({"action":"cancel"}))
    } else {
        Err(-32800)
    }
}
