//! Feishu long-connection adapter for one enterprise self-built app.
//!
//! This module owns the official wire only. Pairing, sessions, and permission
//! decisions stay on the shared remote core.

mod api;
mod credentials;
mod event;
mod frame;
mod host;
mod socket;

pub(crate) use api::{Client, Error};
pub(crate) use credentials::{BindingStatus, Credentials, acceptable_credential};
pub(crate) use event::InboundText;
pub(crate) use host::{AuthorizedTextHandler, spawn_feishu_host};
