//! `project_workspace_state` (plan §13.1): the CLAT-only per-project
//! selection pointer in the control DB. It stores selection state and a
//! cwd witness — never session facts (title/messages/surface live in the
//! session log). Updates go through revision CAS with three-state commit.

use crate::session::id::SessionId;
use rusqlite::{Connection, OptionalExtension, params};

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum WorkspaceSelection {
    Fresh,
    Materializing(SessionId),
    Session(SessionId),
}

impl WorkspaceSelection {
    fn encode(&self) -> (&'static str, Option<&str>) {
        match self {
            Self::Fresh => ("fresh", None),
            Self::Materializing(id) => ("materializing", Some(id.as_str())),
            Self::Session(id) => ("session", Some(id.as_str())),
        }
    }

    fn decode(kind: &str, session_id: Option<String>) -> Option<Self> {
        match (kind, session_id) {
            ("fresh", _) => Some(Self::Fresh),
            ("materializing", Some(id)) => Some(Self::Materializing(SessionId::new(id))),
            ("session", Some(id)) => Some(Self::Session(SessionId::new(id))),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct WorkspaceSnapshot {
    pub(crate) selection: WorkspaceSelection,
    pub(crate) revision: i64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum CasOutcome {
    /// The revision moved or the row vanished; nothing was written.
    NotCommitted,
    /// The new state is durable.
    Committed { revision: i64 },
    /// The transaction may or may not have reached disk; callers must
    /// re-open read-only and normalize before using any selection.
    Unknown,
}

/// Row-level operations; the caller owns the connection (ControlStorage).
pub(crate) fn get(
    connection: &Connection,
    project_root: &str,
) -> Result<WorkspaceSnapshot, rusqlite::Error> {
    let row = connection
        .query_row(
            "SELECT selection, session_id, revision FROM project_workspace_state
             WHERE project_root = ?1",
            params![project_root],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, Option<String>>(1)?,
                    row.get::<_, i64>(2)?,
                ))
            },
        )
        .optional()?;
    Ok(match row {
        Some((kind, session_id, revision)) => match WorkspaceSelection::decode(&kind, session_id) {
            Some(selection) => WorkspaceSnapshot {
                selection,
                revision,
            },
            // Damaged row: treat as Fresh, but never "fix" it here — the
            // next successful CAS replaces it atomically (plan §13.1).
            None => WorkspaceSnapshot {
                selection: WorkspaceSelection::Fresh,
                revision,
            },
        },
        None => WorkspaceSnapshot {
            selection: WorkspaceSelection::Fresh,
            revision: 0,
        },
    })
}

pub(crate) fn compare_and_set(
    connection: &Connection,
    project_root: &str,
    expected_revision: i64,
    new_selection: &WorkspaceSelection,
    cwd_witness: &str,
) -> CasOutcome {
    let (kind, session_id) = new_selection.encode();
    let transaction = match connection.unchecked_transaction() {
        Ok(transaction) => transaction,
        Err(_) => return CasOutcome::Unknown,
    };
    let changed = match transaction.execute(
        "UPDATE project_workspace_state
         SET selection = ?3, session_id = ?4, cwd_witness = ?5, revision = revision + 1,
             updated_at = ?6
         WHERE project_root = ?1 AND revision = ?2",
        params![
            project_root,
            expected_revision,
            kind,
            session_id,
            cwd_witness,
            now_unix()
        ],
    ) {
        Ok(changed) => changed,
        // The statement itself failed before touching any page.
        Err(error) if is_statement_error(&error) => {
            let _ = transaction.rollback();
            return CasOutcome::NotCommitted;
        }
        Err(_) => return CasOutcome::Unknown,
    };
    if changed == 0 {
        // Row missing: insert with revision 1, but only when the caller
        // expected revision 0 (no prior state).
        if expected_revision != 0 {
            let _ = transaction.rollback();
            return CasOutcome::NotCommitted;
        }
        if let Err(error) = transaction.execute(
            "INSERT INTO project_workspace_state
             (project_root, selection, session_id, cwd_witness, revision, updated_at)
             VALUES (?1, ?2, ?3, ?4, 1, ?5)",
            params![project_root, kind, session_id, cwd_witness, now_unix()],
        ) {
            if is_statement_error(&error) {
                let _ = transaction.rollback();
                return CasOutcome::NotCommitted;
            }
            return CasOutcome::Unknown;
        }
    }
    match transaction.commit() {
        Ok(()) => match get(connection, project_root) {
            Ok(snapshot) => CasOutcome::Committed {
                revision: snapshot.revision,
            },
            // The commit itself succeeded but the read-back failed; treat
            // as Unknown until a fresh read says otherwise.
            Err(_) => CasOutcome::Unknown,
        },
        // A failed commit may or may not have been flushed: Unknown until
        // a fresh read says otherwise.
        Err(_) => CasOutcome::Unknown,
    }
}

fn is_statement_error(error: &rusqlite::Error) -> bool {
    matches!(
        error,
        rusqlite::Error::SqliteFailure(code, _)
            if code.code == rusqlite::ErrorCode::ConstraintViolation
    ) || matches!(
        error,
        rusqlite::Error::InvalidParameterName(_) | rusqlite::Error::ToSqlConversionFailure(_)
    )
}

fn now_unix() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

/// Convenience for diagnostics/tests: the stored witness of a project row.
#[allow(dead_code)]
pub(crate) fn cwd_witness(connection: &Connection, project_root: &str) -> Option<String> {
    connection
        .query_row(
            "SELECT cwd_witness FROM project_workspace_state WHERE project_root = ?1",
            params![project_root],
            |row| row.get::<_, String>(0),
        )
        .optional()
        .expect("workspace witness row")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::control_storage::sentinel::create_schema_sql;

    fn connection() -> Connection {
        let connection = Connection::open_in_memory().expect("memory db");
        connection.execute_batch(create_schema_sql()).unwrap();
        connection
    }

    #[test]
    fn missing_row_is_fresh_revision_zero() {
        let connection = connection();
        let snapshot = get(&connection, "/p").expect("get");
        assert_eq!(
            snapshot,
            WorkspaceSnapshot {
                selection: WorkspaceSelection::Fresh,
                revision: 0
            }
        );
    }

    #[test]
    fn cas_inserts_then_revises_and_rejects_stale_revisions() {
        let connection = connection();
        let id = SessionId::new("s-1");
        let outcome = compare_and_set(
            &connection,
            "/p",
            0,
            &WorkspaceSelection::Materializing(id.clone()),
            "/p",
        );
        assert_eq!(
            outcome,
            CasOutcome::Committed { revision: 1 },
            "fresh insert commits at revision 1"
        );
        // Stale revision loses.
        assert_eq!(
            compare_and_set(
                &connection,
                "/p",
                0,
                &WorkspaceSelection::Session(id.clone()),
                "/p"
            ),
            CasOutcome::NotCommitted
        );
        // Current revision wins.
        assert_eq!(
            compare_and_set(
                &connection,
                "/p",
                1,
                &WorkspaceSelection::Session(id.clone()),
                "/p"
            ),
            CasOutcome::Committed { revision: 2 }
        );
        assert_eq!(
            get(&connection, "/p").expect("get").selection,
            WorkspaceSelection::Session(id)
        );
    }

    #[test]
    fn two_projects_keep_independent_rows() {
        let connection = connection();
        compare_and_set(
            &connection,
            "/a",
            0,
            &WorkspaceSelection::Session(SessionId::new("s-a")),
            "/a",
        );
        compare_and_set(
            &connection,
            "/b",
            0,
            &WorkspaceSelection::Session(SessionId::new("s-b")),
            "/b",
        );
        assert_eq!(
            get(&connection, "/a").expect("get a").selection,
            WorkspaceSelection::Session(SessionId::new("s-a"))
        );
        assert_eq!(
            get(&connection, "/b").expect("get b").selection,
            WorkspaceSelection::Session(SessionId::new("s-b"))
        );
    }
}
