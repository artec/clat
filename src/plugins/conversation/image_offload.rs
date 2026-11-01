//! Optional decision owner. Durable interpretation always belongs to Session.
use crate::plugin::{Plugin, PluginContext, PluginDescriptor, PluginError, PluginId, ScopeKind};
use crate::plugins::services::{IMAGE_OFFLOAD_SERVICE, IMAGE_OFFLOAD_SERVICE_ID};
use std::sync::Arc;
const DESCRIPTOR: PluginDescriptor = PluginDescriptor {
    id: PluginId::new("builtin.compaction_image_offload"),
    scope: ScopeKind::TrustedProject,
    provides: &[IMAGE_OFFLOAD_SERVICE_ID],
    requires: &[],
    optional: &[],
};
pub(crate) struct ImageOffloadPolicy;
impl ImageOffloadPolicy {
    pub(crate) fn oldest(
        &self,
        events: &[crate::session::event::SessionEvent],
        protected: u64,
        nodes: &[(u64, crate::model::ModelItem)],
    ) -> Option<(u64, usize)> {
        events
            .iter()
            .filter(|event| event.seq < protected)
            .find_map(|event| {
                let content = match event.event_type.as_str() {
                    "user/message" => &event.data["content"],
                    "tool/result" if event.data["message"]["role"] == "tool" => {
                        &event.data["message"]["content"]
                    }
                    "tool/result" => &event.data["message"]["content"][0]["content"],
                    _ => return None,
                };
                content
                    .as_array()?
                    .iter()
                    .filter(|b| b["type"] == "image")
                    .enumerate()
                    .find(|(_, b)| b["offloaded"] != true && retained(event.seq, b, nodes))
                    .map(|(index, _)| (event.seq, index))
            })
    }
}
fn retained(seq: u64, block: &serde_json::Value, nodes: &[(u64, crate::model::ModelItem)]) -> bool {
    nodes
        .iter()
        .filter(|(source, _)| *source == seq)
        .flat_map(|(_, item)| crate::model::model_item_image_parts(item))
        .any(|part| {
            let crate::model::ContentPart::Image { path, .. } = part else {
                return false;
            };
            block["path"].as_str() == Some(path.as_str())
                || block["attachmentId"].as_str().is_some_and(|id| {
                    std::path::Path::new(path)
                        .file_name()
                        .and_then(|file| file.to_str())
                        == Some(id)
                })
        })
}
pub(crate) struct CompactionImageOffloadPlugin;
impl Plugin for CompactionImageOffloadPlugin {
    fn descriptor(&self) -> &'static PluginDescriptor {
        &DESCRIPTOR
    }
    fn mount(&self, context: &mut PluginContext<'_>) -> Result<(), PluginError> {
        #[cfg(test)]
        crate::test_support::execution_observation::record("compaction.image_offload.create");
        context
            .provide(IMAGE_OFFLOAD_SERVICE, Arc::new(ImageOffloadPolicy))
            .map_err(|error| PluginError::new(error.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn phase8_offload_selects_retained_occurrences_instead_of_fenced_out_images() {
        let event = crate::session::event::SessionEvent::new(
            "user/message",
            0,
            0,
            serde_json::json!({"content":[{"type":"image","attachmentId":"missing"},{"type":"image","attachmentId":"valid"}]}),
        );
        let nodes = vec![(
            0,
            crate::ModelItem::User {
                content: vec![
                    crate::model::ContentPart::Text("unreachable".into()),
                    crate::model::ContentPart::Image {
                        path: "/session/blobs/valid".into(),
                        media_type: "image/png".into(),
                    },
                ],
            },
        )];
        assert_eq!(
            ImageOffloadPolicy.oldest(std::slice::from_ref(&event), 1, &nodes),
            Some((0, 1))
        );
        assert!(
            ImageOffloadPolicy.oldest(&[event], 0, &nodes).is_none(),
            "current round stays protected"
        );
    }
}
