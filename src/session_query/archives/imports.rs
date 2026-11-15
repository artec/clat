use super::*;
use cap_std::fs::{Dir, File, OpenOptions};
use portable::{ImportedLog, Incoming};
use sha2::{Digest, Sha256};
use std::{io::Read, path::Path};

#[derive(Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct ArchiveListRequest {
    pub(crate) trusted_projects: bool,
    pub(crate) query: String,
}

#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct ArchiveRow {
    pub(crate) reference: HistoryReference,
    title: String,
    time: i64,
    source: String,
    main: bool,
}

impl SessionQueryService {
    pub(crate) fn import_archive(
        &self,
        bytes: &[u8],
        cancel: &CancelToken,
    ) -> Result<Vec<ArchiveRow>, QueryError> {
        let scope = self.authorized(false);
        if !scope.contains(&self.project) {
            return Err(QueryError::new("unauthorized"));
        }
        let incoming = parse(bytes, cancel)?;
        let digest = digest(bytes);
        let rows = describe(&self.project, &digest, &incoming);
        let metadata = serde_json::to_vec(&rows).map_err(archive_error)?;
        let dir = self
            .import_dir(&self.project, true)
            .map_err(archive_error)?;
        self.check_scope(&scope, false)?;
        put_once(&dir, &format!("{digest}.zip"), bytes)?;
        put_once(&dir, &format!("{digest}.json"), &metadata)?;
        Ok(rows)
    }

    pub(crate) fn list_archives(
        &self,
        request: ArchiveListRequest,
        cancel: &CancelToken,
    ) -> Result<Vec<ArchiveRow>, QueryError> {
        if request.query.len() > 2048 {
            return Err(QueryError::new("invalid_query"));
        }
        let projects = self.authorized(request.trusted_projects);
        let query = request.query.trim().to_lowercase();
        let deadline = Instant::now() + DEADLINE;
        let mut rows = Vec::new();
        for project in &projects {
            match self.import_dir(project, false) {
                Ok(dir) => {
                    list_project(&dir, project, &query, &mut rows, || check(cancel, deadline))?
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(archive_error(e)),
            }
        }
        self.check_scope(&projects, request.trusted_projects)?;
        rows.sort_by_key(|row| {
            (
                std::cmp::Reverse(row.time),
                !row.main,
                row.reference.session.clone(),
            )
        });
        Ok(rows)
    }

    pub(crate) fn imported_log(
        &self,
        reference: &HistoryReference,
        all: bool,
        cancel: &CancelToken,
    ) -> Result<ImportedLog, QueryError> {
        let (bytes, index) = self.import_bytes(reference, all)?;
        let log = parse(&bytes, cancel)?
            .logs
            .into_iter()
            .find(|log| log.header.id.as_str() == index)
            .ok_or_else(|| QueryError::new("not_found"))?;
        self.archive_project(reference, all)?;
        Ok(log)
    }

    pub(crate) fn import_image(
        &self,
        reference: &HistoryReference,
        all: bool,
        attachment: &str,
        cancel: &CancelToken,
    ) -> Result<(Vec<u8>, &'static str), QueryError> {
        if !valid_digest(attachment) {
            return Err(QueryError::new("invalid_reference"));
        }
        let (bytes, _) = self.import_bytes(reference, all)?;
        let mut incoming = parse(&bytes, cancel)?;
        for (extension, media) in [
            ("png", "image/png"),
            ("jpg", "image/jpeg"),
            ("webp", "image/webp"),
            ("gif", "image/gif"),
        ] {
            if let Some(bytes) = incoming
                .objects
                .remove(&format!("media/sha256:{attachment}.{extension}"))
            {
                self.archive_project(reference, all)?;
                return Ok((bytes, media));
            }
        }
        Err(QueryError::new("not_found"))
    }

    pub(super) fn download_import(
        &self,
        request: ArchiveRequest,
        cancel: &CancelToken,
    ) -> Result<HistoryArchive, QueryError> {
        let (bytes, _) = self.import_bytes(&request.reference, request.trusted_projects)?;
        let parsed = parse(&bytes, cancel)?;
        self.archive_project(&request.reference, request.trusted_projects)?;
        Ok(HistoryArchive {
            filename: filename(parsed.logs[0].header.id.as_str()),
            bytes,
        })
    }

    fn import_bytes<'a>(
        &self,
        reference: &'a HistoryReference,
        all: bool,
    ) -> Result<(Vec<u8>, &'a str), QueryError> {
        let (hash, id) = reference
            .session
            .split_once('/')
            .filter(|(hash, id)| valid_digest(hash) && !id.is_empty())
            .ok_or_else(|| QueryError::new("invalid_reference"))?;
        let project = self.archive_project(reference, all)?;
        let dir = self.import_dir(&project, false).map_err(archive_error)?;
        open_file(&dir, &format!("{hash}.json")).map_err(archive_error)?;
        let bytes =
            read_file(&dir, &format!("{hash}.zip"), MAX_ARCHIVE_BYTES).map_err(archive_error)?;
        if digest(&bytes) != hash {
            return Err(QueryError::new("source_changed"));
        }
        Ok((bytes, id))
    }

    fn archive_project(
        &self,
        reference: &HistoryReference,
        all: bool,
    ) -> Result<ProjectKey, QueryError> {
        self.authorized(all)
            .into_iter()
            .find(|project| project.header_cwd == reference.project)
            .ok_or_else(|| QueryError::new("unauthorized"))
    }

    fn import_dir(&self, project: &ProjectKey, create: bool) -> std::io::Result<Dir> {
        let token = digest(project.header_cwd.as_deref().unwrap_or("").as_bytes());
        self.control.history_archives_dir(&token, create)
    }
}

fn parse(bytes: &[u8], cancel: &CancelToken) -> Result<Incoming, QueryError> {
    let deadline = Instant::now() + DEADLINE;
    let mut failure = None;
    let result = Incoming::parse(bytes, || {
        check(cancel, deadline).map_err(|error| {
            failure = Some(error);
            "Archive read cancelled".into()
        })
    });
    if let Some(error) = failure {
        return Err(error);
    }
    check(cancel, deadline)?;
    result.map_err(archive_error)
}

fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn valid_digest(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

fn open_file(dir: &Dir, name: &str) -> std::io::Result<File> {
    if !dir.symlink_metadata(name)?.is_file() {
        return Err(std::io::Error::other("Expected a regular archive file"));
    }
    let mut options = OpenOptions::new();
    options
        .read(true)
        ._cap_fs_ext_follow(cap_primitives::fs::FollowSymlinks::No);
    let file = dir.open_with(Path::new(name), &options)?;
    if !file.metadata()?.is_file() {
        return Err(std::io::Error::other("Expected a regular archive file"));
    }
    Ok(file)
}

fn read_file(dir: &Dir, name: &str, max_bytes: usize) -> std::io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    open_file(dir, name)?
        .take(max_bytes as u64 + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() > max_bytes {
        return Err(std::io::Error::other("Archive file exceeds its size limit"));
    }
    Ok(bytes)
}

fn put_once(dir: &Dir, name: &str, bytes: &[u8]) -> Result<(), QueryError> {
    crate::private_fs::write_bytes_new_atomic_in_dir(dir, name, bytes)
        .or_else(|error| {
            if read_file(dir, name, bytes.len()).is_ok_and(|old| old == bytes) {
                Ok(())
            } else {
                Err(error)
            }
        })
        .map_err(archive_error)
}

fn list_project(
    dir: &Dir,
    project: &ProjectKey,
    query: &str,
    rows: &mut Vec<ArchiveRow>,
    mut check: impl FnMut() -> Result<(), QueryError>,
) -> Result<(), QueryError> {
    for entry in dir.entries().map_err(archive_error)? {
        check()?;
        let name = entry.map_err(archive_error)?.file_name();
        let Some((name, hash)) = name
            .to_str()
            .and_then(|name| name.strip_suffix(".json").map(|hash| (name, hash)))
            .filter(|(_, hash)| valid_digest(hash))
        else {
            continue;
        };
        let data = read_file(dir, name, 8 * 1024 * 1024).map_err(archive_error)?;
        let file: Vec<ArchiveRow> = serde_json::from_slice(&data).map_err(archive_error)?;
        rows.extend(file.into_iter().filter(|row| {
            row.reference.project == project.header_cwd
                && row
                    .reference
                    .session
                    .split_once('/')
                    .is_some_and(|(digest, _)| digest == hash)
                && (query.is_empty()
                    || row.title.to_lowercase().contains(query)
                    || row.source.to_lowercase().contains(query))
        }));
        if rows.len() > 8192 {
            return Err(archive_error(
                "Too many imported trajectories match the search",
            ));
        }
    }
    Ok(())
}

fn describe(project: &ProjectKey, digest: &str, incoming: &Incoming) -> Vec<ArchiveRow> {
    incoming
        .logs
        .iter()
        .enumerate()
        .map(|(i, log)| {
            let mut tracker = crate::session::projection::TitleTracker::default();
            for event in &log.events {
                let _ = tracker.push(event);
            }
            let title = tracker.explicit_title().unwrap_or_else(|| {
                let first = log.events.iter().find_map(|event| {
                    crate::session::catalog::search::document(event).filter(|d| d.kind == "user")
                });
                crate::session::projection::fallback_title(first.as_ref().map_or("", |d| &d.text))
            });
            let source = log.header.id.as_str().to_owned();
            ArchiveRow {
                reference: HistoryReference {
                    project: project.header_cwd.clone(),
                    session: format!("{digest}/{source}"),
                    seq: None,
                },
                title: crate::redact::redact_secrets(&title),
                time: log
                    .events
                    .last()
                    .map_or(log.header.created_at, |event| event.time),
                source,
                main: i == 0,
            }
        })
        .collect()
}
