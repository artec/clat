use super::*;
use sha2::{Digest, Sha256};
use std::io::Read;
use zip::ZipArchive;

pub(crate) struct ImportedLog {
    pub(crate) header: SessionHeader,
    pub(crate) events: Vec<SessionEvent>,
    pub(crate) bytes: Vec<u8>,
}

pub(crate) struct Incoming {
    pub(crate) logs: Vec<ImportedLog>,
    pub(crate) objects: BTreeMap<String, Vec<u8>>,
}

impl Incoming {
    pub(crate) fn parse(
        bytes: &[u8],
        mut check: impl FnMut() -> Result<(), String>,
    ) -> Result<Self, String> {
        if bytes.len() > MAX_ARCHIVE_BYTES {
            return Err("ZIP exceeds 64 MiB".into());
        }
        let declared = declared_entries(bytes)?;
        let mut zip = ZipArchive::new(Cursor::new(bytes)).map_err(|error| error.to_string())?;
        if zip.len() != declared || zip.offset() != 0 {
            return Err("ZIP contains duplicate entries or a leading payload".into());
        }
        let mut result = Self {
            logs: Vec::new(),
            objects: BTreeMap::new(),
        };
        let mut total = 0u64;
        let mut root_found = false;
        for index in 0..zip.len() {
            check()?;
            let file = zip.by_index(index).map_err(|error| error.to_string())?;
            let path = file.name().to_owned();
            if file.is_symlink() || !file.is_file() {
                return Err("ZIP contains a link or directory".into());
            }
            let kind = entry_kind(&path)?;
            let size = file.size();
            total = total
                .checked_add(size)
                .filter(|total| *total <= MAX_ARCHIVE_BYTES as u64)
                .ok_or("Expanded ZIP exceeds 64 MiB")?;
            let mut data = Vec::new();
            file.take(size + 1)
                .read_to_end(&mut data)
                .map_err(|error| error.to_string())?;
            if data.len() as u64 != size {
                return Err("Invalid ZIP entry length".into());
            }
            match kind {
                Entry::Root => {
                    result.logs.insert(0, parse_log(data)?);
                    root_found = true;
                }
                Entry::Child(segment) => {
                    let log = parse_log(data)?;
                    if safe_id(log.header.id.as_str()) != segment
                        || log.header.origin != Some(super::super::header::SessionOrigin::Subagent)
                    {
                        return Err("Subagent archive path and header disagree".into());
                    }
                    result.logs.push(log);
                }
                Entry::Object(digest) => {
                    if format!("{:x}", Sha256::digest(&data)) != digest {
                        return Err("Attachment digest mismatch".into());
                    }
                    result.objects.insert(path, data);
                }
                Entry::Missing => {
                    std::str::from_utf8(&data).map_err(|error| error.to_string())?;
                }
            }
        }
        if !root_found {
            return Err("Missing main session log".into());
        }
        result.check_lineage()?;
        Ok(result)
    }

    fn check_lineage(&self) -> Result<(), String> {
        let root = &self.logs[0].header;
        let lookup: BTreeMap<_, _> = self
            .logs
            .iter()
            .map(|log| (&log.header.id, &log.header))
            .collect();
        if lookup.len() != self.logs.len() {
            return Err("Archive repeats a session ID".into());
        }
        for log in &self.logs[1..] {
            let mut child = &log.header;
            for _ in 0..lookup.len() {
                if child.id == root.id {
                    break;
                }
                child = *lookup
                    .get(
                        child
                            .parent_session
                            .as_ref()
                            .ok_or("Missing subagent parent")?,
                    )
                    .ok_or("Subagent parent is outside the archive")?;
            }
            if child.id != root.id {
                return Err("Archive contains a lineage cycle".into());
            }
        }
        Ok(())
    }
}

// Compare the ordinary DSH ZIP directory count with the library's name index,
// which would otherwise silently collapse duplicate entries.
fn declared_entries(bytes: &[u8]) -> Result<usize, String> {
    let tail = &bytes[bytes.len().saturating_sub(22 + u16::MAX as usize)..];
    let ending = tail
        .windows(22)
        .enumerate()
        .rev()
        .find_map(|(offset, row)| {
            let short = |start: usize| u16::from_le_bytes([row[start], row[start + 1]]) as usize;
            (row[..4] == [0x50, 0x4b, 0x05, 0x06] && offset + 22 + short(20) == tail.len())
                .then(|| (short(4), short(6), short(8), short(10)))
        })
        .ok_or("ZIP has no end-of-directory record")?;
    let (disk, directory_disk, count, total) = ending;
    if disk != 0
        || directory_disk != 0
        || count != total
        || !(1..=MAX_ARCHIVE_ENTRIES).contains(&count)
    {
        return Err("ZIP has an invalid disk or entry count".into());
    }
    Ok(count)
}

enum Entry<'a> {
    Root,
    Child(&'a str),
    Object(&'a str),
    Missing,
}

fn entry_kind(path: &str) -> Result<Entry<'_>, String> {
    use super::attachments::is_digest;
    if path.is_empty()
        || path.len() > 4096
        || path.chars().any(|c| c.is_ascii_control() || c == '\\')
    {
        return Err("Invalid ZIP entry name".into());
    }
    let parts = path.split('/').collect::<Vec<_>>();
    match parts.as_slice() {
        [LOG_NAME] => Ok(Entry::Root),
        ["attachments-missing.txt"] => Ok(Entry::Missing),
        ["subagents", id, LOG_NAME] if !id.is_empty() && safe_id(id) == *id => Ok(Entry::Child(id)),
        ["files", prefix, digest, name]
            if is_digest(digest)
                && *prefix == &digest[..2]
                && !matches!(*name, "" | "." | "..") =>
        {
            Ok(Entry::Object(digest))
        }
        ["media", name] => name
            .strip_prefix("sha256:")
            .and_then(|file| file.rsplit_once('.'))
            .filter(|(digest, extension)| {
                is_digest(digest) && matches!(*extension, "png" | "jpg" | "webp" | "gif")
            })
            .map(|(digest, _)| Entry::Object(digest))
            .ok_or_else(|| "Invalid image entry name".into()),
        _ => Err("ZIP contains a path outside the DSH trajectory format".into()),
    }
}

fn parse_log(bytes: Vec<u8>) -> Result<ImportedLog, String> {
    let scan = crate::session::jsonl::scan_raw(&bytes)?;
    if scan.header.version != crate::session::compat::SESSION_FORMAT_VERSION
        || scan.committed_plain_bytes != bytes.len()
        || scan.events.len() + 1 != bytes.iter().filter(|byte| **byte == b'\n').count()
        || scan.header.id.as_str().is_empty()
        || scan.header.id.as_str().len() > 1024
        || scan.header.cwd.as_deref() == Some("")
    {
        return Err("Invalid or incomplete DSH v4 session log".into());
    }
    Ok(ImportedLog {
        header: scan.header,
        events: scan.events,
        bytes,
    })
}
