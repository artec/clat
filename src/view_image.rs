//! MM-2/W5 core-native visual inspection tool.
//!
//! Authority is intentionally narrower than ordinary native reads:
//! attachment ids must already be reachable from the active session,
//! project paths must be relative and cross the project's no-follow reader,
//! and run-scratch refs must have been minted into the current run's bounded
//! registry by trusted core code. Full Access never changes these rules.

use crate::message::{AttachmentDescriptor, ContentBlock, JournalImage};
use crate::model::{CancelToken, ContentPart};
use crate::project::Project;
use crate::session::use_cases::SessionService;
use crate::tool::{Tool, ToolDefinition, ToolEffect, ToolError, ToolResult, ToolResultTransformer};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex};

const SCRATCH_PREFIX: &str = "run-scratch:";
const MAX_SCRATCH_IMAGES: usize = 8;
const MAX_SCRATCH_BYTES: usize = 32 * 1024 * 1024;
/// A single provider turn cannot carry more than twelve image blocks. Pending
/// entries exist only between a successful tool invocation and its result
/// transformer, so this is an in-flight bound rather than a per-run quota.
const MAX_PENDING_IMAGES: usize = 12;

#[derive(Clone)]
struct ScratchImage {
    bytes: Vec<u8>,
    display_name: String,
}

#[derive(Default)]
struct State {
    active: bool,
    /// One attachment may be requested by several parallel tool calls. Keep a
    /// small stack per id so each result consumes exactly one authority entry,
    /// rather than letting the last invocation overwrite the earlier ones.
    pending: HashMap<String, Vec<JournalImage>>,
    pending_images: usize,
    scratch: HashMap<String, ScratchImage>,
    scratch_bytes: usize,
}

/// Project-owned runtime slot shared by the tool, its result transformer, and
/// future trusted screenshot producers. It stores only bounded current-run
/// bytes and transient attachment paths; `clear` revokes the whole generation.
#[derive(Default)]
pub(crate) struct ViewImageState {
    inner: Mutex<State>,
}

impl ViewImageState {
    pub(crate) fn begin_run(&self) {
        let mut state = self.inner.lock().expect("view-image state");
        *state = State {
            active: true,
            ..State::default()
        };
    }

    pub(crate) fn clear(&self) {
        *self.inner.lock().expect("view-image state") = State::default();
    }

    fn remember(&self, image: JournalImage) -> Result<(), ToolError> {
        let mut state = self.inner.lock().expect("view-image state");
        if !state.active {
            return Err(ToolError::new(
                "view_image is unavailable outside an active run",
            ));
        }
        if state.pending_images >= MAX_PENDING_IMAGES {
            return Err(ToolError::new(
                "view_image has too many pending image results in this model step",
            ));
        }
        state
            .pending
            .entry(image.descriptor.attachment_id.clone())
            .or_default()
            .push(image);
        state.pending_images += 1;
        Ok(())
    }

    fn take_pending(&self, attachment_id: &str) -> Option<JournalImage> {
        let mut state = self.inner.lock().ok()?;
        let image = state.pending.get_mut(attachment_id)?.pop();
        if state.pending.get(attachment_id).is_some_and(Vec::is_empty) {
            state.pending.remove(attachment_id);
        }
        if image.is_some() {
            state.pending_images = state.pending_images.saturating_sub(1);
        }
        image
    }

    fn scratch(&self, reference: &str) -> Option<ScratchImage> {
        self.inner.lock().ok()?.scratch.get(reference).cloned()
    }

    /// Mint a current-run opaque reference from bytes already produced by a
    /// trusted core tool. No model/user string can add entries to this map.
    #[allow(dead_code)]
    pub(crate) fn mint_scratch(
        &self,
        bytes: Vec<u8>,
        display_name: impl Into<String>,
    ) -> Result<String, ToolError> {
        let mut state = self.inner.lock().expect("view-image state");
        if !state.active {
            return Err(ToolError::new(
                "cannot mint image scratch outside an active run",
            ));
        }
        if state.scratch.len() >= MAX_SCRATCH_IMAGES
            || state.scratch_bytes.saturating_add(bytes.len()) > MAX_SCRATCH_BYTES
        {
            return Err(ToolError::new("run image scratch budget exceeded"));
        }
        let reference = format!("{SCRATCH_PREFIX}{}", uuid::Uuid::new_v4().simple());
        state.scratch_bytes += bytes.len();
        state.scratch.insert(
            reference.clone(),
            ScratchImage {
                bytes,
                display_name: display_name.into(),
            },
        );
        Ok(reference)
    }
}

pub(crate) struct ViewImageTool {
    sessions: Arc<SessionService>,
    state: Arc<ViewImageState>,
}

impl ViewImageTool {
    pub(crate) fn new(sessions: Arc<SessionService>, state: Arc<ViewImageState>) -> Self {
        Self { sessions, state }
    }

    fn select_image(
        &self,
        arguments: &Value,
        project: &Project,
    ) -> Result<JournalImage, ToolError> {
        let attachment_id = arguments.get("attachment_id").and_then(Value::as_str);
        let project_path = arguments
            .get("project_relative_path")
            .and_then(Value::as_str);
        let scratch_ref = arguments.get("run_scratch_ref").and_then(Value::as_str);
        if [attachment_id, project_path, scratch_ref]
            .into_iter()
            .flatten()
            .count()
            != 1
        {
            return Err(ToolError::new(
                "view_image requires exactly one of attachment_id, project_relative_path, or run_scratch_ref",
            ));
        }

        if let Some(id) = attachment_id {
            return self
                .sessions
                .resolve_active_attachment(id)
                .map_err(|error| ToolError::new(format!("view_image: {error}")));
        }
        if let Some(relative) = project_path {
            let relative = Path::new(relative);
            if relative.is_absolute() {
                return Err(ToolError::new(
                    "view_image project_relative_path must be project-relative; Full Access does not permit absolute image reads",
                ));
            }
            let bytes = project
                .read_file_limited(relative, crate::media::MAX_ATTACHMENT_BYTES as usize + 1)
                .map_err(|error| ToolError::new(format!("view_image: {error}")))?
                .ok_or_else(|| ToolError::new("view_image: project image does not exist"))?;
            if bytes.len() as u64 > crate::media::MAX_ATTACHMENT_BYTES {
                return Err(ToolError::new(
                    "view_image: project image exceeds the byte limit",
                ));
            }
            let display_name = relative
                .file_name()
                .and_then(|name| name.to_str())
                .ok_or_else(|| ToolError::new("view_image: image path has no UTF-8 file name"))?;
            return self
                .sessions
                .import_attachment_bytes(&bytes, display_name)
                .map_err(|error| ToolError::new(format!("view_image: {error}")));
        }

        let reference = scratch_ref.expect("one selector was present");
        if !reference.starts_with(SCRATCH_PREFIX) {
            return Err(ToolError::new("view_image: malformed run_scratch_ref"));
        }
        let scratch = self.state.scratch(reference).ok_or_else(|| {
            ToolError::new("view_image: run_scratch_ref is not valid for this run")
        })?;
        self.sessions
            .import_attachment_bytes(&scratch.bytes, &scratch.display_name)
            .map_err(|error| ToolError::new(format!("view_image: {error}")))
    }
}

impl Tool for ViewImageTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "view_image".into(),
            description: "Inspect one image with the active visual model. The image is sent to the configured model provider. Accepts only an attachment id reachable from this session, a project-relative path, or a core-minted current-run scratch ref; absolute paths are never accepted, including in Full Access.".into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "attachment_id": {
                        "type": "string",
                        "description": "Opaque image attachment id already reachable from this session"
                    },
                    "project_relative_path": {
                        "type": "string",
                        "description": "Project-relative PNG or JPEG path; absolute paths are rejected"
                    },
                    "run_scratch_ref": {
                        "type": "string",
                        "description": "Opaque image ref minted by a trusted tool in this run"
                    }
                },
                "additionalProperties": false
            }),
            effect: ToolEffect::Read,
            strict: false,
        }
    }

    fn invoke(
        &self,
        arguments: &Value,
        project: &Project,
        cancel: &CancelToken,
    ) -> Result<Value, ToolError> {
        if cancel.is_cancelled() {
            return Err(ToolError::new("view_image cancelled"));
        }
        let image = self.select_image(arguments, project)?;
        self.state.remember(image.clone())?;
        Ok(json!({
            "viewed": true,
            "attachment": image.descriptor,
            "note": "image attached to this tool result"
        }))
    }

    fn result_blocks(&self, output: &Value) -> Vec<ContentBlock> {
        output
            .get("attachment")
            .cloned()
            .and_then(|value| serde_json::from_value::<AttachmentDescriptor>(value).ok())
            .map(|attachment| vec![ContentBlock::Image { attachment }])
            .unwrap_or_default()
    }
}

pub(crate) struct ViewImageResultTransformer {
    state: Arc<ViewImageState>,
}

impl ViewImageResultTransformer {
    pub(crate) fn new(state: Arc<ViewImageState>) -> Self {
        Self { state }
    }
}

impl ToolResultTransformer for ViewImageResultTransformer {
    fn transform_result(&self, result: &mut ToolResult) {
        if result.tool_name != "view_image" || result.is_error {
            return;
        }
        let mut parts = Vec::new();
        for block in &result.blocks {
            let ContentBlock::Image { attachment } = block else {
                continue;
            };
            let Some(image) = self.state.take_pending(&attachment.attachment_id) else {
                result.is_error = true;
                result.output = json!({
                    "error": "view_image result lost its current-run attachment authority"
                });
                result.blocks.clear();
                result.image_parts.clear();
                return;
            };
            parts.push(ContentPart::Image {
                path: image.path,
                media_type: image.descriptor.media_type,
            });
        }
        result.image_parts = parts;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pending_image(id: &str) -> JournalImage {
        JournalImage {
            descriptor: AttachmentDescriptor {
                attachment_id: id.into(),
                media_type: "image/png".into(),
                width: 1,
                height: 1,
                bytes: 1,
                display_name: Some("fixture.png".into()),
                original_width: None,
                original_height: None,
            },
            path: format!("/private/fixture-{id}.png"),
        }
    }

    #[test]
    fn scratch_refs_are_current_run_bounded_and_unforgeable() {
        let state = ViewImageState::default();
        assert!(state.mint_scratch(vec![1], "x.png").is_err());
        state.begin_run();
        let minted = state.mint_scratch(vec![1, 2], "x.png").expect("mint");
        assert!(minted.starts_with(SCRATCH_PREFIX));
        assert!(state.scratch(&minted).is_some());
        assert!(state.scratch("run-scratch:forged").is_none());
        state.clear();
        assert!(state.scratch(&minted).is_none());
    }

    /// The authority cache is only a handoff between invoke and result
    /// transformation. A long tool loop must therefore not retain every
    /// historic image; consuming one result frees precisely one slot. The
    /// same attachment may still be in flight for more than one call.
    #[test]
    fn pending_image_authority_is_bounded_and_consumed_per_result() {
        let state = ViewImageState::default();
        state.begin_run();
        for index in 0..MAX_PENDING_IMAGES {
            state
                .remember(pending_image(&format!("image-{index}")))
                .expect("within in-flight bound");
        }
        assert!(
            state.remember(pending_image("over-limit")).is_err(),
            "a model step cannot accumulate unbounded pending image authority"
        );

        assert!(state.take_pending("image-0").is_some());
        assert!(
            state.remember(pending_image("replacement")).is_ok(),
            "consuming a result must release exactly one slot"
        );
        assert!(state.take_pending("missing").is_none());

        let repeated = ViewImageState::default();
        repeated.begin_run();
        repeated
            .remember(pending_image("same-attachment"))
            .expect("first parallel call");
        repeated
            .remember(pending_image("same-attachment"))
            .expect("second parallel call");
        assert!(repeated.take_pending("same-attachment").is_some());
        assert!(repeated.take_pending("same-attachment").is_some());
        assert!(repeated.take_pending("same-attachment").is_none());
    }
}
