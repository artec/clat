//! One leased append transaction: validate, write/fsync, roll back or advance its witness.
use super::*;

pub(super) fn append_batch(
    backend: &JsonlBackend,
    session: PreparedSession,
    expected_next_seq: u64,
    events: &[SessionEvent],
) -> Result<PreparedSession, AppendFailure> {
    if backend
        .poisoned
        .lock()
        .expect("poisoned")
        .contains(&session.key)
    {
        return Err(AppendFailure::Unknown {
            error: format!(
                "session \"{}\" is poisoned after an indeterminate commit; cold load(repair) required",
                session.key.id
            ),
        });
    }
    if events.is_empty() {
        return Ok(session);
    }
    if expected_next_seq != session.next_seq {
        return Err(not_committed(
            session.clone(),
            format!(
                "append seq mismatch: expected {}, got {expected_next_seq}",
                session.next_seq
            ),
        ));
    }
    for (index, event) in events.iter().enumerate() {
        if event.seq != expected_next_seq + index as u64 {
            return Err(not_committed(
                session,
                format!(
                    "append seq mismatch: expected {} at index {index}, got {}",
                    expected_next_seq + index as u64,
                    event.seq
                ),
            ));
        }
    }
    if !session.materialized {
        return materialize_batch(backend, session, events);
    }
    append_existing(backend, session, events)
}

fn materialize_batch(
    backend: &JsonlBackend,
    mut session: PreparedSession,
    events: &[SessionEvent],
) -> Result<PreparedSession, AppendFailure> {
    let (dir, lease, revision) = match backend.materialize(&session, events) {
        Ok(committed) => committed,
        Err(MaterializeFailure::NotCommitted(error)) => return Err(not_committed(session, error)),
        Err(MaterializeFailure::Unknown(error)) => return Err(unknown(backend, &session, error)),
    };
    let digest = content::read_digest(&dir, compat::log_file_name(backend.compression), &revision)
        .map_err(|error| unknown(backend, &session, error.to_string()))?;
    session.dir = Some(dir);
    session.lease = Some(lease);
    session.revision = revision;
    session.digest = digest;
    advance(backend, session, events.len())
}

fn append_existing(
    backend: &JsonlBackend,
    session: PreparedSession,
    events: &[SessionEvent],
) -> Result<PreparedSession, AppendFailure> {
    let hooks = {
        let mut armed = backend.faults.lock().expect("faults");
        std::mem::take(&mut *armed)
    };
    let (mut file, before) = open_checked(backend, &session)?;
    let bytes = jsonl::append_batch_bytes(events, backend.compression, backend.pack_chunks)
        .map_err(|error| not_committed(session.clone(), error.to_string()))?;
    let result = (|| -> std::io::Result<()> {
        if hooks.fail_batch_write {
            return Err(std::io::Error::other("injected batch write failure"));
        }
        file.write_all(&bytes)?;
        if hooks.fail_batch_fsync {
            return Err(std::io::Error::other("injected batch fsync failure"));
        }
        file.sync_all()
    })();
    if let Err(error) = result {
        return Err(rollback(backend, session, &file, before, hooks, error));
    }
    let dir = session.dir.as_ref().expect("checked prepared directory");
    let revision =
        matching_handle_and_path_revision(&file, dir, compat::log_file_name(backend.compression))
            .map_err(|error| {
            unknown(
                backend,
                &session,
                format!(
                    "append committed but cannot restat {:?}: {error}",
                    session.path
                ),
            )
        })?;
    let mut session = session;
    session.revision = revision;
    session.digest.update(&bytes);
    advance(backend, session, events.len())
}

fn open_checked(
    backend: &JsonlBackend,
    session: &PreparedSession,
) -> Result<(std::fs::File, u64), AppendFailure> {
    let dir = session.dir.as_ref().ok_or_else(|| {
        unknown(
            backend,
            session,
            "materialized session has no capability-held directory".into(),
        )
    })?;
    if session.lease.is_none() {
        return Err(unknown(
            backend,
            session,
            "materialized session has no active cross-process write lease".into(),
        ));
    }
    let name = compat::log_file_name(backend.compression);
    let file = open_append_no_follow(dir, name).map_err(|error| {
        unknown(
            backend,
            session,
            format!(
                "cannot reopen prepared session log {}: {error}",
                session.path.display()
            ),
        )
    })?;
    content::verify_append(&file, dir, name, session)
        .map_err(|error| unknown(backend, session, error.to_string()))?;
    let before = file
        .metadata()
        .map_err(|error| unknown(backend, session, error.to_string()))?
        .len();
    Ok((file, before))
}

fn rollback(
    backend: &JsonlBackend,
    mut session: PreparedSession,
    file: &std::fs::File,
    before: u64,
    hooks: FaultHooks,
    error: std::io::Error,
) -> AppendFailure {
    let dir = session.dir.as_ref().expect("checked prepared directory");
    let name = compat::log_file_name(backend.compression);
    if let Err(rollback_error) =
        truncate_after_failed_append(file, dir, name, before, hooks.fail_rollback_fsync)
    {
        return unknown(
            backend,
            &session,
            format!(
                "failed to roll back append to {:?}: {error}; rollback failed too: {rollback_error}",
                session.path
            ),
        );
    }
    match matching_handle_and_path_revision(file, dir, name) {
        Ok(revision) => {
            // Rollback changes timestamps, but restores the exact pre-append digest/cursor.
            session.revision = revision;
            not_committed(session, error.to_string())
        }
        Err(stat_error) => unknown(
            backend,
            &session,
            format!(
                "rolled back but cannot restat {}: {stat_error}",
                session.path.display()
            ),
        ),
    }
}

fn advance(
    backend: &JsonlBackend,
    mut session: PreparedSession,
    count: usize,
) -> Result<PreparedSession, AppendFailure> {
    session.next_seq += count as u64;
    session.materialized = true;
    if let Some(state) = backend.states.lock().expect("states").get_mut(&session.key) {
        state.materialized = true;
        state.cursor = session.next_seq;
    }
    Ok(session)
}

fn not_committed(session: PreparedSession, error: String) -> AppendFailure {
    AppendFailure::NotCommitted {
        session: Box::new(session),
        error,
    }
}

fn unknown(backend: &JsonlBackend, session: &PreparedSession, error: String) -> AppendFailure {
    backend
        .poisoned
        .lock()
        .expect("poisoned")
        .insert(session.key.clone());
    AppendFailure::Unknown { error }
}
