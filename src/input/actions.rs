use super::*;

impl InputQueue {
    pub(crate) fn replace(
        &self,
        id: &str,
        version: u64,
        message: PendingMessage,
        draft: InputDraft,
        transfer: impl FnOnce() -> Result<(), String>,
    ) -> Result<(), String> {
        let mut state = self.0.lock().map_err(|_| "pending input poisoned")?;
        let entry = state
            .entries
            .iter_mut()
            .find(|e| e.view.id == id)
            .ok_or("input already received")?;
        check(entry, version)?;
        transfer()?;
        entry.view.version += 1;
        entry.view.text = message.content.plain_text();
        entry.view.images = message
            .content
            .image_descriptors()
            .into_iter()
            .cloned()
            .collect();
        entry.view.draft = draft;
        entry.message = message;
        Ok(())
    }
    pub(crate) fn references(&self) -> Vec<(String, Vec<serde_json::Value>)> {
        self.0
            .lock()
            .expect("pending input")
            .references
            .iter()
            .cloned()
            .collect()
    }

    pub(crate) fn references_done(&self, id: &str) {
        self.0
            .lock()
            .expect("pending input")
            .references
            .retain(|(key, _)| key != id);
    }
    pub(crate) fn recall(
        &self,
        id: &str,
        version: u64,
        transfer: impl FnOnce(&Entry) -> Result<(), String>,
    ) -> Result<PendingInputView, String> {
        let mut state = self.0.lock().map_err(|_| "pending input poisoned")?;
        let index = state
            .entries
            .iter()
            .position(|e| e.view.id == id)
            .ok_or("input already received")?;
        check(&state.entries[index], version)?;
        transfer(&state.entries[index])?;
        Ok(state.entries.remove(index).expect("checked input").view)
    }

    pub(crate) fn receipt(
        &self,
        operation: &str,
        digest: &str,
    ) -> Result<Option<serde_json::Value>, String> {
        let state = self.0.lock().map_err(|_| "pending input poisoned")?;
        match state.receipts.iter().find(|(id, _, _)| id == operation) {
            Some((_, old, result)) if old == digest => Ok(Some(result.clone())),
            Some(_) => Err("input operation id has different arguments".into()),
            None => Ok(None),
        }
    }

    pub(crate) fn remember(&self, operation: String, digest: String, result: serde_json::Value) {
        let mut state = self.0.lock().expect("pending input");
        state.receipts.push_back((operation, digest, result));
        while state.receipts.len() > 128 {
            state.receipts.pop_front();
        }
    }

    pub(crate) fn interrupt(&self, id: &str, version: u64) -> Result<(), String> {
        let mut state = self.0.lock().map_err(|_| "pending input poisoned")?;
        if state.handoff.is_some() {
            return Err("an input interruption is already in progress".into());
        }
        let entry = state
            .entries
            .iter_mut()
            .find(|e| e.view.id == id)
            .ok_or("input already received")?;
        check(entry, version)?;
        entry.view.phase = "interrupting";
        state.handoff = Some(id.to_owned());
        state.paused = false;
        Ok(())
    }

    pub(crate) fn remove(&self, id: &str, version: u64) -> Result<Entry, String> {
        let mut state = self.0.lock().map_err(|_| "pending input poisoned")?;
        let index = state
            .entries
            .iter()
            .position(|e| e.view.id == id)
            .ok_or("input already received")?;
        check(&state.entries[index], version)?;
        Ok(state.entries.remove(index).expect("checked input"))
    }

    pub(crate) fn entry(&self, id: &str, version: u64) -> Result<Entry, String> {
        let state = self.0.lock().map_err(|_| "pending input poisoned")?;
        let entry = state
            .entries
            .iter()
            .find(|e| e.view.id == id)
            .ok_or("input already received")?;
        check(entry, version)?;
        Ok(entry.clone())
    }
}

fn check(entry: &Entry, version: u64) -> Result<(), String> {
    if entry.view.version != version {
        return Err("input changed; refresh before editing".into());
    }
    if entry.view.phase != "queued" {
        return Err("input is being received".into());
    }
    Ok(())
}
