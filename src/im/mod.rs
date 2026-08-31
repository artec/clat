//! Instant-messaging adapters shared by every frontend.
//!
//! The WeChat/iLink transport is deliberately a core protocol module: serve,
//! the TUI, and future frontends may drive binding or consume messages, but
//! none of them owns credentials, retry semantics, or wire interpretation.

mod binding;
mod host;
pub(crate) mod ilink;
mod qr;
mod state;

pub(crate) use binding::{BindingSession, BindingStep};
pub(crate) use host::{AuthorizedMessageHandler, delivery_id, spawn_wechat_host};
pub(crate) use qr::{qr_svg, qr_terminal};
pub(crate) use state::{PairingAttempt, PairingChallenge, WechatBindingStatus, WechatChatBinding};
