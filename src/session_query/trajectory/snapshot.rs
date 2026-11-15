use super::*;
use crate::session::catalog::search;
use crate::session::event::SurfaceOp;
use sha2::{Digest, Sha256};

impl SessionQueryService {
    pub(super) fn trajectory_scan(
        &self,
        reference: &HistoryReference,
        all: bool,
        expected: Option<&Snapshot>,
        cancel: &CancelToken,
        mut projection: Projection,
        imported: bool,
    ) -> Result<(Snapshot, Projection), QueryError> {
        if imported {
            let log = self.imported_log(reference, all, cancel)?;
            return imported_projection(reference, expected, cancel, projection, log.events);
        }
        let key = self.resolve(reference, all, false)?;
        let revision = self.reader.revision(&key).map_err(source_error)?;
        let deadline = Instant::now() + DEADLINE;
        let mut boundary = None;
        let mut hidden = Vec::new();
        let mut failure = None;
        let read = self.reader.visit(&key, &mut |event| {
            if let Err(error) = check(cancel, deadline) {
                failure = Some(error);
                return Err("cancelled".into());
            }
            boundary = Some(event.seq);
            hidden.extend(hidden_range(event));
            Ok(())
        });
        if let Some(error) = failure.take() {
            return Err(error);
        }
        read.map_err(source_error)?;
        let frozen = expected.map_or(boundary, |s| s.boundary);
        if expected.is_some() && frozen > boundary {
            return Err(QueryError::new("source_changed"));
        }
        let mut hash = Sha256::new();
        hash.update(
            serde_json::to_vec(&(&reference.project, &reference.session))
                .expect("history reference serializes"),
        );
        let read = self.reader.visit(&key, &mut |event| {
            if let Err(error) = check(cancel, deadline) {
                failure = Some(error);
                return Err("cancelled".into());
            }
            if frozen.is_none_or(|end| event.seq > end) {
                return Ok(());
            }
            let visible = !hidden.iter().any(|(a, b)| (*a..=*b).contains(&event.seq));
            hash.update(serde_json::to_vec(event).expect("event serializes"));
            hash.update([u8::from(visible)]);
            if visible {
                projection.push(event);
            }
            Ok(())
        });
        if let Some(error) = failure {
            return Err(error);
        }
        read.map_err(source_error)?;
        let snapshot = Snapshot {
            version: 1,
            boundary: frozen,
            digest: format!("{:x}", hash.finalize()),
        };
        if expected.is_some_and(|s| s != &snapshot)
            || self.reader.revision(&key).map_err(source_error)? != revision
        {
            return Err(QueryError::new("source_changed"));
        }
        self.resolve(reference, all, false)?;
        check(cancel, deadline)?;
        projection.finalize();
        Ok((snapshot, projection))
    }
}

fn imported_projection(
    reference: &HistoryReference,
    expected: Option<&Snapshot>,
    cancel: &CancelToken,
    mut projection: Projection,
    events: Vec<crate::session::event::SessionEvent>,
) -> Result<(Snapshot, Projection), QueryError> {
    let (digest, _) = reference
        .session
        .split_once('/')
        .ok_or_else(|| QueryError::new("invalid_reference"))?;
    let snapshot = Snapshot {
        version: 1,
        boundary: events.last().map(|event| event.seq),
        digest: super::super::index::digest(&format!("{digest}:{}", reference.session)),
    };
    if expected.is_some_and(|s| s != &snapshot) {
        return Err(QueryError::new("source_changed"));
    }
    let deadline = Instant::now() + DEADLINE;
    let hidden = events.iter().filter_map(hidden_range).collect::<Vec<_>>();
    for event in events {
        check(cancel, deadline)?;
        if !hidden
            .iter()
            .any(|(start, end)| (*start..=*end).contains(&event.seq))
        {
            projection.push_imported(&event);
        }
    }
    projection.finalize();
    Ok((snapshot, projection))
}

fn hidden_range(event: &SessionEvent) -> Option<(u64, u64)> {
    match event.surface_op {
        Some(SurfaceOp::Replace { start, end }) if !search::preserves_history(event) => {
            Some((start, end))
        }
        _ => None,
    }
}
fn source_error(error: impl ToString) -> QueryError {
    QueryError::new("source_unavailable").detail(error)
}
