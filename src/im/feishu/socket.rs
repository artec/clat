//! Blocking Feishu WebSocket session.
//!
//! Event frames are acknowledged before the payload is queued. The agent run
//! never shares this thread, so the 3 second ack budget stays independent of
//! model time.

use super::api::{Client, Error};
use super::credentials::Credentials;
use super::event::{self, InboundText};
use super::frame::{self, Frame, HEADER_MESSAGE_ID, HEADER_SEQ, HEADER_SUM, HEADER_TYPE};
use std::collections::HashMap;
use std::io::ErrorKind;
use std::net::TcpStream;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::SyncSender;
use std::time::{Duration, Instant};
use tungstenite::stream::MaybeTlsStream;
use tungstenite::{Message, WebSocket};

const READ_WAIT: Duration = Duration::from_secs(1);
const PART_LIMIT: usize = 16;
const PART_TTL: Duration = Duration::from_secs(5);

pub(crate) enum Stop {
    Shutdown,
    Revoked,
}

pub(crate) fn drive(
    client: &Client,
    credentials: &Credentials,
    shutdown: &AtomicBool,
    bound: impl Fn() -> bool,
    events: &SyncSender<InboundText>,
    online: &AtomicBool,
) -> Result<Stop, Error> {
    if stopped(shutdown, &bound) {
        return Ok(stop_reason(shutdown));
    }
    let endpoint = client.discover(credentials)?;
    if stopped(shutdown, &bound) {
        return Ok(stop_reason(shutdown));
    }
    let mut socket = connect(&endpoint.url)?;
    set_read_timeout(socket.get_mut());
    if stopped(shutdown, &bound) {
        return Ok(stop_reason(shutdown));
    }
    let _connected = Connected::arm(online);
    let mut session = Session {
        service: endpoint.service,
        next_ping: Instant::now(),
        ping_every: endpoint.ping_interval,
        parts: HashMap::new(),
    };
    session.pump(&mut socket, shutdown, &bound, events)
}

fn stopped(shutdown: &AtomicBool, bound: &impl Fn() -> bool) -> bool {
    shutdown.load(Ordering::Acquire) || !bound()
}

fn stop_reason(shutdown: &AtomicBool) -> Stop {
    if shutdown.load(Ordering::Acquire) {
        Stop::Shutdown
    } else {
        Stop::Revoked
    }
}

struct Connected<'a>(&'a AtomicBool);

impl<'a> Connected<'a> {
    fn arm(flag: &'a AtomicBool) -> Self {
        flag.store(true, Ordering::Release);
        Self(flag)
    }
}

impl Drop for Connected<'_> {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

struct Session {
    service: i32,
    next_ping: Instant,
    ping_every: Duration,
    parts: HashMap<String, Parts>,
}

struct Parts {
    pieces: Vec<Option<Vec<u8>>>,
    expires: Instant,
}

impl Session {
    fn pump(
        &mut self,
        socket: &mut WebSocket<MaybeTlsStream<TcpStream>>,
        shutdown: &AtomicBool,
        bound: &impl Fn() -> bool,
        events: &SyncSender<InboundText>,
    ) -> Result<Stop, Error> {
        loop {
            if shutdown.load(Ordering::Acquire) {
                return Ok(Stop::Shutdown);
            }
            if !bound() {
                return Ok(Stop::Revoked);
            }
            self.ping(socket)?;
            match socket.read() {
                Ok(Message::Binary(bytes)) => self.on_binary(socket, &bytes, events)?,
                Ok(Message::Close(_)) => return Err(Error::Transport("connection closed".into())),
                Ok(_) => {}
                Err(tungstenite::Error::Io(error))
                    if matches!(error.kind(), ErrorKind::TimedOut | ErrorKind::WouldBlock) => {}
                Err(error) => return Err(Error::Transport(error.to_string())),
            }
        }
    }

    fn ping(&mut self, socket: &mut WebSocket<MaybeTlsStream<TcpStream>>) -> Result<(), Error> {
        if Instant::now() < self.next_ping {
            return Ok(());
        }
        let bytes = Frame::ping(self.service).encode();
        socket
            .send(Message::Binary(bytes.into()))
            .map_err(|error| Error::Transport(error.to_string()))?;
        self.next_ping = Instant::now() + self.ping_every;
        Ok(())
    }

    fn on_binary(
        &mut self,
        socket: &mut WebSocket<MaybeTlsStream<TcpStream>>,
        bytes: &[u8],
        events: &SyncSender<InboundText>,
    ) -> Result<(), Error> {
        let Ok(frame) = Frame::decode(bytes) else {
            return Ok(());
        };
        if frame.method == frame::CONTROL {
            return Ok(());
        }
        if frame.method != frame::DATA || frame.header(HEADER_TYPE) != Some("event") {
            return Ok(());
        }
        let Some(payload) = self.assemble(&frame) else {
            return Ok(());
        };
        let started = Instant::now();
        let code = if serde_json::from_slice::<serde_json::Value>(&payload).is_ok() {
            200
        } else {
            500
        };
        let ack = frame.acknowledge(started.elapsed().as_millis(), code);
        socket
            .send(Message::Binary(ack.encode().into()))
            .map_err(|error| Error::Transport(error.to_string()))?;
        if code != 200 {
            return Ok(());
        }
        let Ok(value) = serde_json::from_slice(&payload) else {
            return Ok(());
        };
        if let Some(inbound) = event::p2p_user_text(&value) {
            let _ = events.try_send(inbound);
        }
        Ok(())
    }

    fn assemble(&mut self, frame: &Frame) -> Option<Vec<u8>> {
        let message_id = frame.header(HEADER_MESSAGE_ID)?.to_owned();
        let sum = frame
            .header(HEADER_SUM)
            .and_then(|value| value.parse().ok())
            .unwrap_or(1usize);
        let seq = frame
            .header(HEADER_SEQ)
            .and_then(|value| value.parse().ok())
            .unwrap_or(0usize);
        let piece = frame.payload.clone().unwrap_or_default();
        if sum <= 1 {
            return Some(piece);
        }
        if sum > PART_LIMIT || seq >= sum {
            return None;
        }
        let now = Instant::now();
        self.parts.retain(|_, parts| parts.expires > now);
        let parts = self
            .parts
            .entry(message_id.clone())
            .or_insert_with(|| Parts {
                pieces: vec![None; sum],
                expires: now + PART_TTL,
            });
        if parts.pieces.len() != sum {
            parts.pieces = vec![None; sum];
        }
        parts.pieces[seq] = Some(piece);
        parts.expires = now + PART_TTL;
        if parts.pieces.iter().any(Option::is_none) {
            return None;
        }
        let parts = self.parts.remove(&message_id)?;
        Some(parts.pieces.into_iter().flatten().flatten().collect())
    }
}

fn connect(url: &str) -> Result<WebSocket<MaybeTlsStream<TcpStream>>, Error> {
    match tungstenite::connect(url) {
        Ok((socket, _)) => Ok(socket),
        Err(tungstenite::Error::Http(response)) if handshake_rejected(&response) => {
            Err(Error::InvalidCredential)
        }
        Err(error) => Err(Error::Transport(safe_socket_error(&error))),
    }
}

fn safe_socket_error(error: &tungstenite::Error) -> String {
    let text = error.to_string();
    if text.contains("wss://") || text.contains("ws://") {
        "websocket connection failed".to_owned()
    } else {
        text
    }
}

fn handshake_rejected(response: &tungstenite::http::Response<Option<Vec<u8>>>) -> bool {
    let header = |name: &str| {
        response
            .headers()
            .get(name)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.parse::<i64>().ok())
    };
    matches!(header("handshake-status"), Some(403 | 514))
}

fn set_read_timeout(stream: &mut MaybeTlsStream<TcpStream>) {
    let tcp = match stream {
        MaybeTlsStream::Plain(tcp) => tcp,
        MaybeTlsStream::Rustls(tls) => tls.get_mut(),
        #[allow(unreachable_patterns)]
        _ => return,
    };
    let _ = tcp.set_read_timeout(Some(READ_WAIT));
}
