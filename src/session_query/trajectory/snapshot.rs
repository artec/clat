use super::*;
use crate::session::{catalog::search, event::SurfaceOp};
use sha2::{Digest, Sha256};

impl SessionQueryService {
    pub(super) fn trajectory_scan(
        &self,
        reference: &HistoryReference,
        all: bool,
        expected: Option<&Snapshot>,
        cancel: &CancelToken,
        mut projection: Projection,
    ) -> Result<(Snapshot, Projection), QueryError> {
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
            if let Some(SurfaceOp::Replace { start, end }) = event.surface_op
                && !search::preserves_history(event)
            {
                hidden.push((start, end));
            }
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
fn source_error(error: impl ToString) -> QueryError {
    QueryError::new("source_unavailable").detail(error)
}
