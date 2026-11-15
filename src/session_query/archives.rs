use super::*;
use crate::session::{
    event::SessionEvent,
    header::SessionHeader,
    portable::{self, Archive, LOG_NAME, MAX_ARCHIVE_BYTES},
};
use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ArchiveRequest {
    pub(crate) reference: HistoryReference,
    #[serde(default)]
    pub(crate) trusted_projects: bool,
    #[serde(default)]
    pub(crate) imported: bool,
}

pub(crate) struct HistoryArchive {
    pub(crate) filename: String,
    pub(crate) bytes: Vec<u8>,
}

pub(super) fn archive_error(error: impl ToString) -> QueryError {
    QueryError::new("archive_failed").detail(error)
}

impl SessionQueryService {
    pub(crate) fn export_archive(
        &self,
        request: ArchiveRequest,
        cancel: &CancelToken,
    ) -> Result<HistoryArchive, QueryError> {
        if request.imported {
            return self.download_import(request, cancel);
        }
        let key = self.resolve(&request.reference, request.trusted_projects, false)?;
        let deadline = Instant::now() + DEADLINE;
        let mut archive = Archive::default();
        let mut missing = BTreeMap::new();
        let mut versions = Vec::new();
        for (key, path) in self.archive_tree(&key, cancel, deadline)? {
            let revision = self.reader.revision(&key).map_err(archive_error)?;
            let (header, mut events) = self.archive_log(&key, cancel, deadline)?;
            portable::normalize(&mut events).map_err(archive_error)?;
            for event in &mut events {
                portable::export_attachments(event, &mut archive, &mut missing, |desc| {
                    self.reader.attachment(&key, desc).ok()
                })
                .map_err(archive_error)?;
            }
            archive
                .insert(path, portable::log_bytes(&header, &events))
                .map_err(archive_error)?;
            versions.push((key, revision));
        }
        if !missing.is_empty() {
            let entries = missing
                .into_iter()
                .map(|(path, reason)| format!("{path}\t{reason}"))
                .collect::<Vec<_>>();
            archive
                .insert(
                    "attachments-missing.txt".into(),
                    (entries.join("\n") + "\n").into_bytes(),
                )
                .map_err(archive_error)?;
        }
        for (key, revision) in versions {
            if self.reader.revision(&key).map_err(archive_error)? != revision {
                return Err(QueryError::new("source_changed"));
            }
        }
        check(cancel, deadline)?;
        let bytes = archive.zip().map_err(archive_error)?;
        self.resolve(&request.reference, request.trusted_projects, false)?;
        Ok(HistoryArchive {
            filename: filename(key.id.as_str()),
            bytes,
        })
    }

    fn archive_tree(
        &self,
        root: &SessionKey,
        cancel: &CancelToken,
        deadline: Instant,
    ) -> Result<Vec<(SessionKey, String)>, QueryError> {
        let mut children: HashMap<_, Vec<_>> = HashMap::new();
        for (key, header, _) in self.reader.list(&root.project).map_err(archive_error)? {
            check(cancel, deadline)?;
            if header.origin == Some(crate::session::header::SessionOrigin::Subagent)
                && let Some(parent) = header.parent_session
            {
                children.entry(parent).or_default().push(key);
            }
        }
        let mut queue = VecDeque::from([(root.clone(), LOG_NAME.to_owned())]);
        let mut seen = HashSet::new();
        let mut paths = HashSet::new();
        let mut rows = Vec::new();
        while let Some((key, path)) = queue.pop_front() {
            if !seen.insert(key.id.clone())
                || !paths.insert(path.clone())
                || rows.len() >= portable::MAX_ARCHIVE_ENTRIES
            {
                return Err(archive_error("Invalid or oversized subagent lineage"));
            }
            for child in children.remove(&key.id).unwrap_or_default() {
                let segment = portable::safe_id(child.id.as_str());
                queue.push_back((child, format!("subagents/{segment}/{LOG_NAME}")));
            }
            rows.push((key, path));
        }
        Ok(rows)
    }

    fn archive_log(
        &self,
        key: &SessionKey,
        cancel: &CancelToken,
        deadline: Instant,
    ) -> Result<(SessionHeader, Vec<SessionEvent>), QueryError> {
        let mut events = Vec::new();
        let mut bytes = 0usize;
        let mut failure = None;
        let result = self.reader.visit(key, &mut |event| {
            let admission = check(cancel, deadline).and_then(|()| {
                bytes = bytes
                    .saturating_add(serde_json::to_vec(event).map_err(archive_error)?.len() + 1);
                (bytes <= MAX_ARCHIVE_BYTES)
                    .then_some(())
                    .ok_or_else(|| archive_error("Session log exceeds 64 MiB"))
            });
            if let Err(error) = admission {
                failure = Some(error);
                return Err("Archive scan stopped".into());
            }
            events.push(event.clone());
            Ok(())
        });
        if let Some(error) = failure {
            return Err(error);
        }
        let header = result.map_err(archive_error)?;
        if header.version == crate::session::compat::SESSION_FORMAT_VERSION {
            Ok((header, events))
        } else {
            crate::session::upgrade::ensure_current(&header, &events).map_err(archive_error)
        }
    }
}

pub(super) fn filename(id: &str) -> String {
    let segment = portable::safe_id(id);
    let end = segment.len().min(160);
    format!("dsh-session-{}.zip", &segment[..end])
}

mod imports;
pub(crate) use imports::{ArchiveListRequest, ArchiveRow};

#[cfg(test)]
mod tests;
