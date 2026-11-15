//! DSH session-log ZIP codec. A conversion owns a detached copy of the journal.
mod attachments;
mod incoming;
mod normalize;
mod restore;
mod tools;

use super::{event::SessionEvent, header::SessionHeader};
use std::{
    collections::BTreeMap,
    io::{Cursor, Write},
};
use zip::{CompressionMethod, ZipWriter, write::SimpleFileOptions};

pub(crate) use attachments::{export_attachments, import_images};
pub(crate) use incoming::{ImportedLog, Incoming};
pub(crate) use normalize::normalize;
pub(crate) use restore::unpack_dsh_archive;

pub(crate) const LOG_NAME: &str = "session.v4.jsonl";
pub(crate) const MAX_ARCHIVE_BYTES: usize = 64 * 1024 * 1024;
pub(crate) const MAX_ARCHIVE_ENTRIES: usize = 2048;

pub(crate) fn safe_id(id: &str) -> String {
    id.encode_utf16()
        .map(|unit| {
            char::from_u32(unit as u32)
                .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'))
                .unwrap_or('_')
        })
        .collect()
}

pub(crate) fn log_bytes(header: &SessionHeader, events: &[SessionEvent]) -> Vec<u8> {
    let rows = super::jsonl::event_lines(events, false, header.version);
    let mut bytes = header.to_line().into_bytes();
    if !bytes.ends_with(b"\n") {
        bytes.push(b'\n');
    }
    if !rows.is_empty() {
        bytes.extend(rows.into_bytes());
        bytes.push(b'\n');
    }
    bytes
}

#[derive(Default)]
pub(crate) struct Archive {
    entries: BTreeMap<String, Vec<u8>>,
    bytes: usize,
}
impl Archive {
    pub(crate) fn insert(&mut self, path: String, bytes: Vec<u8>) -> Result<(), String> {
        if let Some(old) = self.entries.get(&path) {
            return if old == &bytes {
                Ok(())
            } else {
                Err(format!("Conflicting archive entry: {path}"))
            };
        }
        if self.entries.len() >= MAX_ARCHIVE_ENTRIES {
            return Err("Session archive has too many entries".into());
        }
        self.bytes = self.bytes.saturating_add(bytes.len());
        if self.bytes > MAX_ARCHIVE_BYTES {
            return Err("Session archive exceeds 64 MiB before compression".into());
        }
        self.entries.insert(path, bytes);
        Ok(())
    }

    pub(crate) fn zip(mut self) -> Result<Vec<u8>, String> {
        let primary = self
            .entries
            .remove(LOG_NAME)
            .ok_or("Missing main session log")?;
        let mut writer = ZipWriter::new(Cursor::new(Vec::new()));
        let options = SimpleFileOptions::default()
            .compression_method(CompressionMethod::Deflated)
            .unix_permissions(0o600);
        for (path, data) in std::iter::once((LOG_NAME.to_owned(), primary)).chain(self.entries) {
            writer
                .start_file(path, options)
                .map_err(|e| e.to_string())?;
            writer.write_all(&data).map_err(|e| e.to_string())?;
        }
        let zip = writer.finish().map_err(|e| e.to_string())?.into_inner();
        if zip.len() > MAX_ARCHIVE_BYTES {
            return Err("Session archive exceeds 64 MiB after compression".into());
        }
        Ok(zip)
    }
}
