use super::*;
use crate::session::{
    catalog::{ReplayKind, replay_kind},
    replay::ReplayTurnEnd,
};
impl Projection {
    pub(super) fn push(&mut self, event: &SessionEvent) {
        if replay_kind(&event.event_type) == ReplayKind::TurnStart {
            self.turn = event.data["turn"].as_u64().filter(|t| *t > 0);
        }
        self.metadata(event);
        let mut replay = Vec::new();
        self.adapter.push(event, &mut replay);
        for item in replay {
            self.item(event, item);
        }
        self.raw(event);
    }
    fn item(&mut self, event: &SessionEvent, item: ReplayEvent) {
        let turn = event.data["turn"]
            .as_u64()
            .filter(|t| *t > 0)
            .or_else(|| (item.turn() > 0).then_some(item.turn()));
        match item {
            ReplayEvent::UserMessage {
                text,
                content_blocks,
                ..
            } => {
                let explicit = event.data["turn"].as_u64().filter(|t| *t > 0);
                let i = self.add(event, explicit.or(self.turn), "user", &text);
                self.attachments(i, content_blocks);
            }
            ReplayEvent::AssistantMessage {
                turn,
                step,
                reasoning,
                text,
                provider,
                model,
                ..
            } => self.assistant(event, turn, step, reasoning, text, provider, model),
            ReplayEvent::ToolRequested { call, .. } => self.tool_call(event, turn, call),
            ReplayEvent::ToolFinished {
                call_id,
                output,
                is_error,
                content_blocks,
                ..
            } => {
                let i = self.tool_result(event, turn, call_id, output, is_error);
                self.attachments(i, content_blocks);
            }
            ReplayEvent::PermissionChecked { decision, tool, .. } => {
                self.permission(event, turn, tool, decision);
            }
            ReplayEvent::RetryScheduled { failure, .. } => {
                let i = self.add(event, turn, "retry", &failure.message);
                self.entries[i].row.status = Some("failed".into());
            }
            ReplayEvent::TurnEnded { reason, .. } => self.end(event, turn, reason),
            ReplayEvent::Compaction {
                summary_text,
                disclosure,
                ..
            } => self.compaction(event, &summary_text, disclosure),
        }
    }
    fn compaction(
        &mut self,
        event: &SessionEvent,
        summary: &str,
        disclosure: Option<crate::session::replay::CompactionDisclosure>,
    ) {
        let id = disclosure.as_ref().and_then(|d| d.id.as_deref());
        if let Some(index) = id.and_then(|id| self.compactions.get(id)) {
            self.entries[*index].row.source_seqs.push(event.seq);
            return;
        }
        let index = self.add(event, None, "compaction", summary);
        if let Some(id) = id {
            self.compactions.insert(id.to_owned(), index);
        }
        if let Some(detail) = &mut self.entries[index].detail {
            detail.disclosure = disclosure.and_then(|d| serde_json::to_value(d).ok());
        }
    }
    fn permission(
        &mut self,
        event: &SessionEvent,
        turn: Option<u64>,
        tool: String,
        decision: crate::permission::PermissionDecision,
    ) {
        use crate::permission::PermissionDecision;
        let (status, body) = match decision {
            PermissionDecision::Allow => ("succeeded", String::new()),
            PermissionDecision::Ask { reason } => ("unfinished", reason),
            PermissionDecision::Deny { reason } | PermissionDecision::Unavailable { reason } => {
                ("blocked", reason)
            }
        };
        let i = self.add(event, turn, "approval", &body);
        self.entries[i].row.tool = Some(tool);
        self.entries[i].row.status = Some(status.into());
    }
    fn end(&mut self, event: &SessionEvent, turn: Option<u64>, reason: ReplayTurnEnd) {
        let (status, body) = match reason {
            ReplayTurnEnd::Completed => ("succeeded", String::new()),
            ReplayTurnEnd::Error { message } => ("failed", message),
            ReplayTurnEnd::Aborted { cause } => ("cancelled", cause),
            ReplayTurnEnd::Interrupted => ("cancelled", String::new()),
            ReplayTurnEnd::Blocked => ("blocked", String::new()),
            ReplayTurnEnd::MaxTokens => ("max_tokens", String::new()),
        };
        if turn.is_some()
            && let Some(entry) = self
                .entries
                .iter_mut()
                .rev()
                .find(|e| e.row.turn == turn && e.row.kind == "message")
        {
            entry.row.final_answer = true;
        }
        if self.turn == turn {
            self.turn = None;
        }
        let i = self.add(event, turn, "terminal", &body);
        self.entries[i].row.status = Some(status.into());
    }
    fn attachments(&mut self, index: usize, blocks: Vec<crate::message::ContentBlock>) {
        if let Some(detail) = &mut self.entries[index].detail {
            detail
                .attachments
                .extend(blocks.into_iter().filter_map(|b| match b {
                    crate::message::ContentBlock::Image { attachment } => Some(attachment),
                    _ => None,
                }));
        }
    }
    pub(super) fn finalize(&mut self) {
        self.entries.sort_by_key(|e| e.row.seq);
        for entry in &mut self.entries {
            if entry
                .row
                .started_at
                .zip(entry.row.ended_at)
                .is_some_and(|(a, b)| b < a)
            {
                entry.row.started_at = None;
                entry.row.ended_at = None;
            }
            if let Some(detail) = &mut entry.detail {
                detail.record = entry.row.clone();
            }
        }
    }
}
