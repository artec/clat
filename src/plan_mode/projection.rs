use super::documents::{Documents, EVENT, fold};
use crate::session::{
    event::SessionEvent,
    projection::{CheckpointRow, ProjectionUnit},
};
use serde_json::Value;
pub(crate) struct DocumentUnit {
    state: Documents,
    as_of: i64,
}
impl Default for DocumentUnit {
    fn default() -> Self {
        Self {
            state: Documents::default(),
            as_of: -1,
        }
    }
}
impl ProjectionUnit for DocumentUnit {
    fn key(&self) -> &'static str {
        "plan-documents"
    }
    fn state_version(&self) -> u64 {
        1
    }
    fn as_of(&self) -> i64 {
        self.as_of
    }
    fn fold(&mut self, event: &SessionEvent) -> Result<(), String> {
        if event.event_type == EVENT {
            fold(&mut self.state, &event.data)?;
        }
        self.as_of = event.seq as i64;
        Ok(())
    }
    fn snapshot(&self) -> Value {
        serde_json::to_value(&self.state).expect("documents serialize")
    }
    fn restore(&mut self, row: &CheckpointRow) -> Result<(), String> {
        self.state = serde_json::from_value(row.val.clone()).map_err(|e| e.to_string())?;
        super::documents::validate_snapshot(&self.state)?;
        self.as_of = row.seq;
        Ok(())
    }
}
