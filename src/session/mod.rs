//! DSH-compatible session persistence (format pinned by `compat.rs`).
//!
//! Stage 1 of `docs/todo/dsh-session-persistence.md`: core types, event
//! catalog, path layout, zstd JSONL encoding, and the backend port. Every
//! format decision traces to `docs/research/dsh-session-compatibility.md`,
//! which records the pinned upstream facts — nothing here is invented.

pub(crate) mod adapter;
pub(crate) mod admission;
pub(crate) mod catalog;
pub(crate) mod checkpoint;
pub(crate) mod chunk_packing;
pub(crate) mod compat;
#[cfg(test)]
mod dsh_golden;
pub(crate) mod event;
pub(crate) mod header;
pub(crate) mod id;
mod interop;
pub(crate) mod jsonl;
pub(crate) mod key;
pub(crate) mod path_layout;
pub(crate) mod persistence;
pub(crate) mod preflight;
pub(crate) mod projection;
pub(crate) mod recorder;
pub(crate) mod recovery;
pub(crate) mod replay;
pub(crate) mod root_dir;
pub(crate) mod root_lease;
pub(crate) mod run_journal;
pub(crate) mod surface;
pub(crate) mod use_cases;
pub(crate) mod write_behind;
pub(crate) mod zstd_frames;
