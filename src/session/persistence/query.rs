//! Read-only enumeration restricted before any foreign bucket is opened.
use super::*;
impl JsonlBackend {
    pub(crate) fn query_snapshots(
        &self,
        project: &crate::session::key::ProjectKey,
    ) -> Result<Vec<(SessionKey, SessionHeader, LogRevision)>, SessionError> {
        let bucket = match self.root_dir.open_bucket(&project.bucket) {
            Ok(dir) => dir,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(io(e)),
        };
        let mut out = Vec::new();
        for entry in bucket.entries().map_err(io)? {
            let entry = entry.map_err(io)?;
            let name = entry
                .file_name()
                .into_string()
                .map_err(|_| SessionError::Corruption("non-UTF-8 session id".into()))?;
            if name == ".DS_Store" && entry.file_type().map_err(io)?.is_file() {
                continue;
            }
            if !entry.file_type().map_err(io)?.is_dir() {
                return Err(SessionError::Corruption("invalid session directory".into()));
            }
            let id = path_layout::decode_segment(&name)
                .filter(|id| path_layout::encode_segment(id) == name)
                .ok_or_else(|| SessionError::Corruption("noncanonical session directory".into()))?;
            let key = SessionKey {
                project: project.clone(),
                id: crate::session::id::SessionId::new(id),
            };
            if self.find_log(&key)?.is_none() {
                continue;
            }
            let header = self.header_snapshot(&key)?;
            let revision = self.query_revision(&key)?;
            out.push((key, header, revision));
        }
        out.sort_by(|a, b| a.0.id.as_str().cmp(b.0.id.as_str()));
        Ok(out)
    }
    pub(crate) fn query_revision(&self, key: &SessionKey) -> Result<LogRevision, SessionError> {
        let resolved = self
            .find_log(key)?
            .ok_or_else(|| SessionError::NotFound(key.id.to_string()))?;
        let dir = self.root_dir.open_session(key).map_err(io)?;
        let log = open_read_no_follow(&dir, &resolved.name).map_err(io)?;
        matching_handle_and_path_revision(&log, &dir, &resolved.name).map_err(io)
    }
}
impl LogRevision {
    pub(crate) fn cache_token(&self) -> &str {
        &self.0
    }
}
