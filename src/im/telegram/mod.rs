//! Telegram long-poll adapter for one bot token.
//!
//! This module owns the Bot API wire only. Pairing, sessions, and permission
//! decisions stay on the shared remote core. Groups, channels, and non-text
//! messages are discarded here.

mod client;
mod credentials;
mod host;
mod update;

pub(crate) use client::{Client, Error};
pub(crate) use credentials::{BindingStatus, Credentials, acceptable_token};
pub(crate) use host::{AuthorizedTextHandler, spawn_telegram_host};
pub(crate) use update::InboundText;
