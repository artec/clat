//! Process-local pending input. A claim is reversible until durable admission.
//! No journal vocabulary, frontend I/O or execution policy lives in the view DTO.
use crate::message::PendingMessage;
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

mod actions;
#[cfg(test)]
mod tests;

#[derive(Clone, Default, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InputDraft {
    pub draft_scope_id: Option<String>,
    #[serde(default)]
    pub uploads: Vec<String>,
    #[serde(default)]
    pub references: Vec<serde_json::Value>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PendingInputEdit {
    pub text: String,
    pub keep_uploads: Option<Vec<String>>,
    #[serde(default)]
    pub attachments: Vec<String>,
    pub draft_scope_id: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PendingInputView {
    pub id: String,
    pub version: u64,
    pub phase: &'static str,
    pub text: String,
    pub images: Vec<crate::message::AttachmentDescriptor>,
    pub draft: InputDraft,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PendingInputsView {
    pub session_id: Option<String>,
    pub items: Vec<PendingInputView>,
    pub paused: bool,
    pub interrupting: bool,
}

#[derive(Clone)]
pub(crate) struct Entry {
    pub view: PendingInputView,
    pub message: PendingMessage,
}

#[derive(Default)]
struct State {
    entries: VecDeque<Entry>,
    paused: bool,
    handoff: Option<String>,
    initial: Option<String>,
    terminal: bool,
    resume_after_terminal: bool,
    receipts: VecDeque<(String, String, serde_json::Value)>,
    references: VecDeque<(String, Vec<serde_json::Value>)>,
}

#[derive(Clone, Default)]
pub(crate) struct InputQueue(Arc<Mutex<State>>);

#[derive(Clone, Copy, PartialEq)]
pub(crate) enum InputSettlement {
    Completed,
    Cancelled,
    Failed,
}

impl InputQueue {
    pub(crate) fn same_owner(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
    pub(crate) fn snapshot(&self) -> PendingInputsView {
        let state = self.0.lock().expect("pending input");
        PendingInputsView {
            session_id: None,
            items: state.entries.iter().map(|e| e.view.clone()).collect(),
            paused: state.paused,
            interrupting: state.handoff.is_some(),
        }
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.0.lock().expect("pending input").entries.is_empty()
    }

    pub(crate) fn ready(&self) -> bool {
        let state = self.0.lock().expect("pending input");
        !state.paused
            && state.initial.is_none()
            && state.handoff.is_none()
            && state
                .entries
                .front()
                .is_some_and(|e| e.view.phase == "queued")
    }

    pub(crate) fn existing(&self, id: &str) -> Option<PendingMessage> {
        self.0
            .lock()
            .expect("pending input")
            .entries
            .iter()
            .find(|e| e.view.id == id)
            .map(|e| e.message.clone())
    }

    pub(crate) fn enqueue(
        &self,
        id: String,
        message: PendingMessage,
        draft: InputDraft,
    ) -> Result<(), String> {
        let mut state = self.0.lock().map_err(|_| "pending input poisoned")?;
        if let Some(e) = state.entries.iter().find(|e| e.view.id == id) {
            return if e.message.request_digest() == message.request_digest()
                && e.view.draft == draft
            {
                Ok(())
            } else {
                Err("input id already has different content".into())
            };
        }
        if message.client_message_id.as_deref() != Some(id.as_str()) {
            return Err("input id does not match its message".into());
        }
        if id.is_empty() || id.len() > 200 || state.entries.len() >= 64 {
            return Err("pending input limit reached".into());
        }
        let images = message
            .content
            .image_descriptors()
            .into_iter()
            .cloned()
            .collect();
        let view = PendingInputView {
            id,
            version: 1,
            phase: "queued",
            text: message.content.plain_text(),
            images,
            draft,
        };
        if state.terminal || state.paused {
            state.resume_after_terminal = true;
        }
        state.entries.push_back(Entry { view, message });
        state.paused = false;
        Ok(())
    }

    pub(crate) fn claim(&self, ordinary: bool) -> Option<PendingMessage> {
        let mut state = self.0.lock().ok()?;
        if state.paused {
            return None;
        }
        let index = match state.handoff.as_ref().or(state.initial.as_ref()) {
            Some(id) if ordinary => state.entries.iter().position(|e| &e.view.id == id)?,
            Some(_) => return None,
            None => 0,
        };
        let entry = state.entries.get_mut(index)?;
        if !matches!(entry.view.phase, "queued" | "interrupting") {
            return None;
        }
        entry.view.phase = "claiming";
        Some(entry.message.clone())
    }

    pub(crate) fn accepts_initial(&self, message: &PendingMessage) -> bool {
        let state = self.0.lock().expect("pending input");
        let entry = message
            .client_message_id
            .as_ref()
            .and_then(|id| state.entries.iter().find(|entry| &entry.view.id == id));
        entry.is_none_or(|entry| {
            entry.view.phase == "claiming"
                && entry.message.request_digest() == message.request_digest()
        })
    }

    pub(crate) fn committed(&self, id: &str, digest: &str) -> Option<InputDraft> {
        let mut state = self.0.lock().ok()?;
        let index = state.entries.iter().position(|e| {
            e.view.id == id && e.view.phase == "claiming" && e.message.request_digest() == digest
        })?;
        let entry = state.entries.remove(index)?;
        if !entry.view.draft.references.is_empty() {
            state
                .references
                .push_back((id.to_owned(), entry.view.draft.references.clone()));
        }
        if state.handoff.as_deref() == Some(id) {
            state.handoff = None;
        }
        if state.initial.as_deref() == Some(id) {
            state.initial = None;
        }
        Some(entry.view.draft)
    }

    pub(crate) fn settled(&self, outcome: InputSettlement) {
        let mut state = self.0.lock().expect("pending input");
        let failed_handoff = outcome == InputSettlement::Failed && state.handoff.take().is_some();
        if failed_handoff {
            state.paused = true;
        }
        for entry in &mut state.entries {
            if entry.view.phase == "claiming"
                || (failed_handoff && entry.view.phase == "interrupting")
            {
                entry.view.phase = "queued";
            }
        }
        if outcome != InputSettlement::Completed
            && state.handoff.is_none()
            && !state.resume_after_terminal
        {
            state.paused = true;
        }
    }

    pub(crate) fn pause(&self) {
        let mut state = self.0.lock().expect("pending input");
        state.paused = true;
        state.resume_after_terminal = false;
        state.handoff = None;
        state.initial = None;
        for entry in &mut state.entries {
            if entry.view.phase == "interrupting" {
                entry.view.phase = "queued";
            }
        }
    }

    pub(crate) fn begin_run(&self) {
        let mut state = self.0.lock().expect("pending input");
        state.terminal = false;
        state.resume_after_terminal = false;
    }

    pub(crate) fn prefer_initial(&self, id: String) {
        let mut state = self.0.lock().expect("pending input");
        if state.initial.is_none() && state.handoff.is_none() {
            state.initial = Some(id);
        }
    }

    pub(crate) fn agent_started(&self) {
        self.0.lock().expect("pending input").terminal = false;
    }

    pub(crate) fn terminal(&self) {
        self.0.lock().expect("pending input").terminal = true;
    }
}
