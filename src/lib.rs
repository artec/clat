//! CLAT public facade and terminal client. Runtime ownership lives in clat-core.
pub(crate) use clat_core::client_ports::{SessionId, SessionSummary, permission};
pub(crate) use clat_core::client_ports::{command, draft, interaction, private_fs, session};
pub use clat_core::*;
mod dsh;
pub mod tui;
