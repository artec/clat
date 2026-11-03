//! Same-journal replacement for an explicitly stopped, tool-free text turn.
use super::*;
use crate::session::catalog::rewind::last_aborted_text_turn;

impl JsonlBackend {
    pub(crate) fn rewindable_tail(
        &self,
        key: &SessionKey,
        message_seq: u64,
    ) -> Result<Option<(String, u64)>, SessionError> {
        let read = self.read_events(key, true)?;
        Ok(rewind_boundary(&read, message_seq).and_then(|index| {
            Some((
                read.revision.0,
                read.events[index].data.get("turn")?.as_u64()?,
            ))
        }))
    }

    /// The active coordinator must be retired before taking the disk lease.
    pub(crate) fn rewind_tail(
        &self,
        key: &SessionKey,
        message_seq: u64,
        expected: &str,
    ) -> Result<(), SessionError> {
        let source = self
            .find_log(key)?
            .ok_or_else(|| SessionError::NotFound(key.id.to_string()))?;
        let dir = self.open_session_dir(key)?;
        let _lease = SessionWriteLease::try_acquire(
            &dir,
            &source.path.with_file_name(write_lease::LEASE_FILENAME),
        )
        .map_err(io)?
        .ok_or_else(|| SessionError::Io("session still has an active writer".into()))?;
        let read = self.read_events(key, true)?;
        let boundary = rewind_boundary(&read, message_seq)
            .filter(|_| read.revision.0 == expected)
            .ok_or_else(|| SessionError::Io("session tail is no longer eligible".into()))?;
        let prefix = &read.events[..boundary];
        let mut projection = crate::session::projection::ProjectionRegistry::clat();
        projection
            .fold_all(prefix)
            .map_err(SessionError::Corruption)?;
        if !interrupted_turn_closers(prefix).is_empty() {
            return Err(SessionError::Corruption(
                "rewind prefix is not balanced".into(),
            ));
        }
        let content = encode_prefix(&read.header, prefix, self.compression, self.pack_chunks)?;
        #[cfg(test)]
        if std::mem::take(
            &mut self
                .faults
                .lock()
                .expect("faults")
                .fail_rewind_before_publish,
        ) {
            return Err(SessionError::Io(
                "injected rewind publication failure".into(),
            ));
        }
        publish_prefix(&read.dir, &source.name, &content, &read.revision)
    }
}

fn rewind_boundary(read: &RawRead, message_seq: u64) -> Option<usize> {
    if read.header.version != compat::SESSION_FORMAT_VERSION
        || read.truncate_to.is_some()
        || read.stable_events != read.events.len()
    {
        return None;
    }
    last_aborted_text_turn(&read.events, message_seq)
}

fn encode_prefix(
    header: &SessionHeader,
    prefix: &[SessionEvent],
    compression: JsonlCompression,
    pack_chunks: bool,
) -> Result<Vec<u8>, SessionError> {
    let line = format!("{}\n", header.to_line());
    let mut content = match compression {
        JsonlCompression::None => line.into_bytes(),
        JsonlCompression::Zstd => {
            crate::session::zstd_frames::compress_frame(line.as_bytes()).map_err(io)?
        }
    };
    // Preserve append-sized frames rather than making the complete prefix a
    // single compressed frame that could exceed the reader's frame budget.
    for event in prefix {
        content.extend(
            jsonl::append_batch_bytes(std::slice::from_ref(event), compression, pack_chunks)
                .map_err(io)?,
        );
    }
    Ok(content)
}

fn publish_prefix(
    dir: &Dir,
    name: &str,
    content: &[u8],
    expected: &LogRevision,
) -> Result<(), SessionError> {
    let temp = format!("{name}.{}.tmp", uuid::Uuid::new_v4().simple());
    let mut options = cap_std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use cap_std::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    let result = (|| {
        let mut file = dir.open_with(&temp, &options).map_err(io)?;
        file.write_all(content).map_err(io)?;
        file.sync_all().map_err(io)?;
        drop(file);
        let current = open_read_no_follow(dir, name).map_err(io)?;
        if &matching_handle_and_path_revision(&current, dir, name).map_err(io)? != expected {
            return Err(SessionError::Io("session log revision changed".into()));
        }
        drop(current);
        dir.rename(&temp, dir, name).map_err(io)?;
        crate::session::root_dir::sync_dir(dir).map_err(|error| {
            SessionError::Io(format!(
                "rewind published but durability is unknown: {error}"
            ))
        })
    })();
    if result.is_err() {
        let _ = dir.remove_file(&temp);
    }
    result
}
