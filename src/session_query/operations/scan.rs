use super::*;
use crate::session::replay::{ReplayAdapter, ReplayEvent};
use sha2::{Digest, Sha256};
impl SessionQueryService {
    pub(super) fn scan_operations(
        &self,
        key: &SessionKey,
        cancel: &CancelToken,
        cursor: Option<&Cursor>,
        facts: &mut OperationScan,
    ) -> Result<(Option<u64>, String), QueryError> {
        let deadline = Instant::now() + DEADLINE;
        let revision = self.reader.revision(key).map_err(source_error)?;
        let mut adapter = ReplayAdapter::new();
        let mut replay = Vec::new();
        let mut boundary = None;
        let mut hash = Sha256::new();
        let mut failure = None;
        let read = self.reader.visit(key, &mut |event| {
            if let Err(error) = check(cancel, deadline) {
                failure = Some(error);
                return Err("query interrupted".into());
            }
            if cursor.is_some_and(|c| event.seq > c.boundary) {
                return Ok(());
            }
            boundary = Some(event.seq);
            hash.update(serde_json::to_vec(event).expect("event serializes"));
            adapter.push(event, &mut replay);
            for mut item in replay.drain(..) {
                // A late result retains its recorded owner even after a newer turn starts.
                if let Some(recorded) = event.data.get("turn").and_then(Value::as_u64) {
                    match &mut item {
                        ReplayEvent::ToolRequested { turn, .. }
                        | ReplayEvent::ToolFinished { turn, .. } => *turn = recorded,
                        _ => {}
                    }
                }
                facts.push(item);
            }
            Ok(())
        });
        if let Some(error) = failure {
            return Err(error);
        }
        read.map_err(source_error)?;
        let digest = format!("{:x}", hash.finalize());
        if cursor.is_some_and(|c| Some(c.boundary) != boundary || c.digest != digest)
            || self.reader.revision(key).map_err(source_error)? != revision
        {
            return Err(QueryError::new("source_changed"));
        }
        if !self.authorized(false).contains(&key.project) {
            return Err(QueryError::new("scope_changed"));
        }
        check(cancel, deadline)?;
        Ok((boundary, digest))
    }
}
fn source_error(error: impl ToString) -> QueryError {
    QueryError::new("source_unavailable").detail(error)
}
