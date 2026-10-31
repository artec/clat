//! Optional transient-image capability. Empty cleanup has no resource owner.
use super::super::{ApplicationError, missing_builtin};
use crate::draft::DraftImageStore;
use crate::message::{DraftScope, DraftTarget};
use std::{
    path::{Path, PathBuf},
    sync::Arc,
};
#[derive(Clone, Default)]
pub(crate) struct DraftImages(pub(crate) Option<Arc<DraftImageStore>>);
impl DraftImages {
    pub(crate) fn require(&self) -> Result<Arc<DraftImageStore>, ApplicationError> {
        self.0
            .clone()
            .ok_or_else(|| missing_builtin("builtin.draft_images"))
    }
    pub(crate) fn has_live_drafts(&self) -> bool {
        self.0.as_ref().is_some_and(|store| store.has_live_drafts())
    }
    pub(crate) fn reserve_uploads(
        &self,
        scope: &str,
        selection: u64,
        token: u64,
        ids: &[String],
    ) -> Result<Vec<PathBuf>, String> {
        if ids.is_empty() {
            return Ok(vec![]);
        }
        self.require()
            .map_err(|e| e.to_string())?
            .reserve_uploads(scope, selection, token, ids)
    }
    pub(crate) fn transfer_uploads(
        &self,
        source: &str,
        dest: &str,
        authority: (u64, u64),
        ids: &[String],
        reserved: bool,
        destination_ids: Option<&[String]>,
    ) -> Result<(), String> {
        if ids.is_empty() {
            return Ok(());
        }
        self.require().map_err(|e| e.to_string())?.transfer_uploads(
            source,
            dest,
            authority,
            ids,
            reserved,
            destination_ids,
        )
    }
    pub(crate) fn commit_uploads(&self, scope: &str, ids: &[String]) {
        if !ids.is_empty()
            && let Some(store) = &self.0
        {
            store.commit_uploads(scope, ids);
        }
    }
    pub(crate) fn rollback_uploads(&self, scope: &str, ids: &[String]) {
        if !ids.is_empty()
            && let Some(store) = &self.0
        {
            store.rollback_uploads(scope, ids);
        }
    }
    pub(crate) fn release_clipboard_path(&self, path: &Path) -> bool {
        self.0
            .as_ref()
            .is_some_and(|store| store.release_clipboard_path(path))
    }
    pub(crate) fn open_scope(
        &self,
        client: &str,
        selection: u64,
        token: u64,
        target: DraftTarget,
    ) -> Result<DraftScope, String> {
        self.require()
            .map_err(|e| e.to_string())?
            .open_scope(client, selection, token, target)
    }
    pub(crate) fn stage_remote_image(
        &self,
        bytes: &[u8],
        extension: &str,
    ) -> Result<PathBuf, String> {
        self.require()
            .map_err(|e| e.to_string())?
            .stage_remote_image(bytes, extension)
    }
}
