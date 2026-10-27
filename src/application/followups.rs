//! Ephemeral followups: never journaled, never automatically submitted.
use super::*;
use crate::plugins::services::UtilityTask;
use serde::Serialize;

#[derive(Clone, Default, Serialize)]
pub struct FollowupReply {
    pub run_id: String,
    pub status: String,
    pub items: Vec<String>,
}
#[derive(Default)]
pub(super) struct FollowupStore {
    sessions: std::collections::BTreeMap<SessionId, FollowupSlot>,
}
#[derive(Default)]
struct FollowupSlot {
    run: String,
    seq: Option<u64>,
    attempted: bool,
    reply: FollowupReply,
    cancel: crate::CancelToken,
}
pub struct PreparedFollowups {
    suggestion: super::suggestion::PreparedPromptSuggestion,
    store: Arc<Mutex<FollowupStore>>,
    run: String,
    session: SessionId,
    cancel: crate::CancelToken,
}
impl TrustedProjectApplication {
    pub(crate) fn register_followup_run(&self, run: &str, completed: bool) {
        let Some(session) = self.current_session_id() else {
            return;
        };
        let mut store = self.followups.lock().expect("followups");
        if let Some(old) = store.sessions.remove(&session) {
            old.cancel.cancel();
        }
        if completed {
            // Bound ephemeral memory independently of the durable conversation catalog.
            if store.sessions.len() >= 64 {
                if let Some(key) = store.sessions.keys().next().cloned() {
                    if let Some(old) = store.sessions.remove(&key) {
                        old.cancel.cancel();
                    }
                }
            }
            store.sessions.insert(
                session,
                FollowupSlot {
                    run: run.into(),
                    seq: self.followup_content_seq(),
                    reply: FollowupReply {
                        run_id: run.into(),
                        status: "empty".into(),
                        items: vec![],
                    },
                    ..FollowupSlot::default()
                },
            );
        }
    }
    pub fn followup_snapshot(&self) -> FollowupReply {
        let Some(session) = self.current_session_id() else {
            return FollowupReply::default();
        };
        let mut store = self.followups.lock().expect("followups");
        let Some(slot) = store.sessions.get_mut(&session) else {
            return FollowupReply::default();
        };
        if slot.seq != self.followup_content_seq() || !self.utility.enabled(UtilityTask::Followups)
        {
            slot.cancel.cancel();
            slot.reply.status = "empty".into();
            slot.reply.items.clear();
            return FollowupReply::default();
        }
        slot.reply.clone()
    }
    fn followup_content_seq(&self) -> Option<u64> {
        // Selection/title metadata can advance the journal without a new conversation turn.
        self.session_message_outline()
            .ok()?
            .last()
            .map(|item| item.seq)
    }
    pub(super) fn cancel_followups(&self) {
        for slot in self
            .followups
            .lock()
            .expect("followups")
            .sessions
            .values_mut()
        {
            slot.cancel.cancel();
            slot.reply.status = "empty".into();
            slot.reply.items.clear();
        }
    }
    pub fn prepare_followups(
        &mut self,
        run: &str,
        retry: bool,
    ) -> Result<Option<PreparedFollowups>, ApplicationError> {
        let snapshot = self.followup_snapshot();
        if snapshot.run_id != run || run.is_empty() {
            return Ok(None);
        }
        let session = self.current_session_id().expect("snapshot session");
        let mut store = self.followups.lock().expect("followups");
        let slot = store.sessions.get_mut(&session).expect("snapshot slot");
        if slot.attempted && !(retry && slot.reply.status == "failed") {
            return Ok(None);
        }
        // Preparation checks run/compaction and freezes the bounded conversation.
        drop(store);
        let suggestion = self.prepare_suggestion(UtilityTask::Followups)?;
        store = self.followups.lock().expect("followups");
        let Some(slot) = store.sessions.get_mut(&session) else {
            return Ok(None);
        };
        if slot.run != run {
            return Ok(None);
        }
        slot.attempted = true;
        slot.reply.status = "pending".into();
        Ok(Some(PreparedFollowups {
            suggestion,
            store: Arc::clone(&self.followups),
            run: run.into(),
            session,
            cancel: slot.cancel.clone(),
        }))
    }
}
impl PreparedFollowups {
    pub fn generate(self) {
        let result = self
            .suggestion
            .generate(&self.cancel)
            .and_then(|output| parse_followups(&output.text));
        let mut store = self.store.lock().expect("followups");
        let Some(slot) = store.sessions.get_mut(&self.session) else {
            return;
        };
        if slot.run != self.run || self.cancel.is_cancelled() {
            return;
        }
        match result {
            Ok(items) => {
                slot.reply.items = items;
                slot.reply.status = "ready".into();
            }
            Err(_) => slot.reply.status = "failed".into(),
        }
    }
}
fn parse_followups(text: &str) -> Result<Vec<String>, String> {
    let values: Vec<String> = serde_json::from_str(text).map_err(|_| "invalid followups")?;
    let mut items = Vec::new();
    for value in values.into_iter().take(12) {
        let text: String = value.trim().chars().take(300).collect();
        if !text.is_empty() && !items.contains(&text) {
            items.push(text);
        }
        if items.len() == 3 {
            break;
        }
    }
    if items.is_empty() {
        return Err("empty followups".into());
    }
    Ok(items)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn followups_require_a_bounded_text_array_and_remove_duplicates() {
        assert_eq!(
            parse_followups(r#"[" next ","next","","another","third","fourth"]"#).unwrap(),
            ["next", "another", "third"]
        );
        for invalid in ["raw model explanation", "{}", "[1]", "[\" \" ]"] {
            assert!(parse_followups(invalid).is_err());
        }
    }
}
