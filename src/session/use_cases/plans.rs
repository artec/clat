use super::*;
use crate::plan_mode::documents::{Change, Documents, EVENT};
impl SessionService {
    pub(crate) fn plan_documents(&self) -> Result<Documents, String> {
        let guard = self.active.lock().map_err(|_| "session lock")?;
        let Some(active) = guard.as_ref() else {
            return Ok(Documents::default());
        };
        fold_if_behind(active, &self.backend).map_err(|e| e.to_string())?;
        let value = active
            .projections
            .lock()
            .map_err(|_| "projections")?
            .state_snapshot("plan-documents");
        serde_json::from_value(value.unwrap_or_default()).map_err(|e| e.to_string())
    }
    pub(crate) fn commit_plan_change(
        &self,
        change: Change,
        tail: &[crate::session::run_journal::NewSessionEvent],
    ) -> Result<(), String> {
        let guard = self.active.lock().map_err(|_| "session lock")?;
        let active = guard.as_ref().ok_or("plan requires an active session")?;
        if let Some(doc) = &change.document
            && doc.session != active.key.id.as_str()
        {
            return Err("plan belongs to another session".into());
        }
        let mut state: Documents = serde_json::from_value(
            active
                .projections
                .lock()
                .map_err(|_| "projections")?
                .state_snapshot("plan-documents")
                .unwrap_or_default(),
        )
        .map_err(|e| e.to_string())?;
        let data = serde_json::to_value(change).map_err(|e| e.to_string())?;
        crate::plan_mode::documents::fold(&mut state, &data)?;
        let mut events =
            vec![crate::session::run_journal::NewSessionEvent::new(EVENT, data).log_only()];
        events.extend_from_slice(tail);
        active
            .shared_journal(&self.backend)
            .map_err(|e| e.to_string())?
            .append_atomic_durable(&events)?;
        let _ = checkpoint_active(active, &self.checkpoints);
        Ok(())
    }
    pub(crate) fn materialize_plan(
        &self,
        doc: &crate::plan_mode::documents::PlanDocument,
    ) -> Result<(), String> {
        let guard = self.active.lock().map_err(|_| "session lock")?;
        let active = guard.as_ref().ok_or("no active session")?;
        if active.coordinator.is_read_only() || doc.session != active.key.id.as_str() {
            return Err("plan session is not writable".into());
        }
        let root = self
            .backend
            .open_session_dir(&active.key)
            .map_err(|e| e.to_string())?;
        let plans = SessionRootDir::open_or_create_child(&root, std::path::Path::new("plans"))
            .map_err(|e| e.to_string())?;
        let name = format!("{}-r{}.md", doc.id, doc.revision);
        match plans.symlink_metadata(&name) {
            Ok(meta) if !meta.is_file() || meta.is_symlink() => {
                return Err("plan projection is not a regular file".into());
            }
            Ok(_) => return verify_projection(&plans, &name, &doc.text),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.to_string()),
        }
        crate::private_fs::write_text_new_atomic_in_dir(&plans, &name, &doc.text)
            .or_else(|error| verify_projection(&plans, &name, &doc.text).map_err(|_| error))
    }
}

fn verify_projection(dir: &cap_std::fs::Dir, name: &str, expected: &str) -> Result<(), String> {
    use std::io::Read;
    let meta = dir.symlink_metadata(name).map_err(|e| e.to_string())?;
    if !meta.is_file() || meta.is_symlink() || meta.len() != expected.len() as u64 {
        return Err("plan projection was externally modified; original preserved".into());
    }
    let mut options = cap_std::fs::OpenOptions::new();
    options
        .read(true)
        ._cap_fs_ext_follow(cap_primitives::fs::FollowSymlinks::No);
    let file = dir.open_with(name, &options).map_err(|e| e.to_string())?;
    if !file.metadata().map_err(|e| e.to_string())?.is_file() {
        return Err("plan is not a regular file".into());
    }
    let mut text = String::new();
    file.take(expected.len() as u64 + 1)
        .read_to_string(&mut text)
        .map_err(|e| e.to_string())?;
    if text != expected {
        return Err("plan projection was externally modified; original preserved".into());
    }
    Ok(())
}
