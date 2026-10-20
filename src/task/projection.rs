use crate::session::event::SessionEvent;
use crate::session::projection::{CheckpointRow, ProjectionUnit};
use serde_json::Value;
pub(crate) struct TaskUnit {
    state: Option<super::TaskState>,
    as_of: i64,
}
impl Default for TaskUnit {
    fn default() -> Self {
        Self {
            state: None,
            as_of: -1,
        }
    }
}
impl ProjectionUnit for TaskUnit {
    fn key(&self) -> &'static str {
        "task"
    }
    fn state_version(&self) -> u64 {
        1
    }
    fn as_of(&self) -> i64 {
        self.as_of
    }
    fn fold(&mut self, event: &SessionEvent) -> Result<(), String> {
        if event.event_type == "clat/task" {
            super::fold_change(&mut self.state, &event.data)?;
        }
        self.as_of = event.seq as i64;
        Ok(())
    }
    fn snapshot(&self) -> Value {
        serde_json::to_value(&self.state).expect("task serializes")
    }
    fn restore(&mut self, row: &CheckpointRow) -> Result<(), String> {
        self.state = if row.val.is_null() {
            None
        } else {
            Some(super::decode_change(&row.val)?)
        };
        self.as_of = row.seq;
        Ok(())
    }
}
