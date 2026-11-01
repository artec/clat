//! Optional maintenance decisions; only Application commits durable replacements.
use super::*;
use crate::model::{ModelItem, ModelOptions};
use crate::tool::ToolDefinition;

impl ActiveContext {
    pub(super) fn notify_maintenance(&self, pruned: usize, images: usize) {
        if pruned + images == 0 {
            return;
        }
        broadcast_to(
            &self.subscribers,
            ApplicationEvent::CompactionUpdated(CompactionStatus::Finished {
                note: UiMessage::new("command.context_maintained")
                    .arg("pruned", pruned)
                    .arg("images", images)
                    .into(),
                succeeded: true,
            }),
        );
    }

    fn maintenance(&self) -> DurableMaintenance<'_> {
        DurableMaintenance {
            sessions: &self.sessions,
            journal: self.journal.as_ref(),
            cancel: &self.cancel,
            config: &self.config,
            pruner: self.pruner.as_deref(),
            image_offload: self.image_offload.as_deref(),
            protected_floor: self.protected_floor,
        }
    }
    pub(super) fn prune_results(&self, items: &mut [ModelItem]) -> Result<usize, String> {
        self.maintenance().prune_results(items)
    }
    pub(super) fn maintain_images(
        &self,
        items: &mut [ModelItem],
        instructions: Option<&str>,
        definitions: &[ToolDefinition],
        options: &ModelOptions,
        force: bool,
    ) -> Result<usize, String> {
        if self.image_offload.is_none() {
            return Ok(0);
        }
        self.boundary()?;
        self.maintenance()
            .maintain_images(items, instructions, definitions, options, force)
    }
}
/// Shared idle/active decisions. This port owns no service, recorder, worker or authority.
pub(in crate::application) struct DurableMaintenance<'a> {
    pub sessions: &'a SessionService,
    pub journal: &'a dyn RunJournal,
    pub cancel: &'a CancelToken,
    pub config: &'a ModelConfig,
    pub pruner: Option<&'a crate::plugins::ResultPruner>,
    pub image_offload: Option<&'a crate::plugins::ImageOffloadPolicy>,
    pub protected_floor: u64,
}
impl DurableMaintenance<'_> {
    pub(in crate::application) fn prune_results(
        &self,
        items: &mut [ModelItem],
    ) -> Result<usize, String> {
        let Some(pruner) = &self.pruner else {
            return Ok(0);
        };
        let mut count = 0;
        for event in self
            .sessions
            .maintenance_events()
            .into_iter()
            .filter(|event| event.event_type == "tool/result")
        {
            if self.cancel.is_cancelled() {
                break;
            }
            let pointer = if event.data["message"]["role"] == "tool" {
                "/message/content"
            } else {
                "/message/content/0/content"
            };
            let Some(blocks) = event
                .data
                .pointer(pointer)
                .and_then(serde_json::Value::as_array)
            else {
                continue;
            };
            let Some(pruned) = pruner.prune(blocks) else {
                continue;
            };
            let before = self.sessions.surface_nodes().map_err(|e| e.to_string())?;
            let index = live_index(items, &before, event.seq)?;
            let tokens = before
                .iter()
                .filter(|(seq, _)| *seq == event.seq)
                .map(|(_, item)| crate::model::estimate_model_item_tokens(item))
                .sum::<u64>();
            let mut data = event.data;
            *data
                .pointer_mut(pointer)
                .ok_or("missing tool result content")? = pruned.into();
            let revision = self.journal.committed_seq();
            let range=self.journal.append_atomic_durable_at(revision,&[
                NewSessionEvent::new("compaction/prune",json!({"shadowedRange":{"start":event.seq,"end":event.seq},"shadowedSeqs":[event.seq],"shadowedTokenCount":tokens})).log_only(),
                NewSessionEvent::new("tool/result",data).replace(event.seq,event.seq,vec![event.seq]),
            ])?;
            self.refresh_item(items, index, range.start + 1)?;
            count += 1;
        }
        Ok(count)
    }
    pub(in crate::application) fn maintain_images(
        &self,
        items: &mut [ModelItem],
        instructions: Option<&str>,
        definitions: &[ToolDefinition],
        options: &ModelOptions,
        force: bool,
    ) -> Result<usize, String> {
        let Some(policy) = &self.image_offload else {
            return Ok(0);
        };
        let mut count = 0;
        loop {
            if self.cancel.is_cancelled() {
                break;
            }
            let images = items
                .iter()
                .flat_map(crate::model::model_item_image_parts)
                .count();
            let pressure = options.image_projection.as_ref().is_some_and(|budget| {
                crate::model::image_projection_is_over_budget(
                    items,
                    instructions,
                    definitions,
                    options,
                    budget,
                )
            });
            if images == 0
                || (!force && !pressure && self.config.capabilities.accepts_image_input())
            {
                break;
            }
            let before = self.sessions.surface_nodes().map_err(|e| e.to_string())?;
            let Some((seq, image)) = policy.oldest(
                &self.sessions.maintenance_events(),
                self.protected_floor,
                &before,
            ) else {
                break;
            };
            let index = live_index(items, &before, seq)?;
            let revision = self.journal.committed_seq();
            self.journal.append_atomic_durable_at(
                revision,
                &[NewSessionEvent::new(
                    "image/offload",
                    json!({"targets":[{"seq":seq,"imageIndexes":[image]}]}),
                )
                .log_only()],
            )?;
            self.refresh_item(items, index, seq)?;
            let after = items
                .iter()
                .flat_map(crate::model::model_item_image_parts)
                .count();
            if after >= images {
                return Err("image offload made no progress".into());
            }
            count += 1;
            if force {
                break;
            }
        }
        Ok(count)
    }
    fn refresh_item(&self, items: &mut [ModelItem], index: usize, seq: u64) -> Result<(), String> {
        let mut next = self
            .sessions
            .surface_nodes()
            .map_err(|e| e.to_string())?
            .into_iter()
            .find(|(s, _)| *s == seq)
            .map(|(_, item)| item)
            .ok_or("maintenance result is not projected")?;
        if let (ModelItem::ToolResult(old), ModelItem::ToolResult(new)) = (&items[index], &mut next)
        {
            new.tool_name = old.tool_name.clone();
        }
        items[index] = next;
        Ok(())
    }
}
fn live_index(
    items: &[ModelItem],
    nodes: &[(u64, ModelItem)],
    target: u64,
) -> Result<usize, String> {
    let old = nodes
        .iter()
        .find(|(seq, _)| *seq == target)
        .map(|(_, item)| item)
        .ok_or("maintenance target changed")?;
    match old {
        ModelItem::ToolResult(result) => {
            let occurrence=nodes.iter().take_while(|(seq,_)|*seq!=target).filter(|(_,item)|matches!(item,ModelItem::ToolResult(other) if other.call_id==result.call_id)).count();
            items.iter().enumerate().filter(|(_,item)|matches!(item,ModelItem::ToolResult(other) if other.call_id==result.call_id)).nth(occurrence).map(|(index,_)|index).ok_or("maintenance tool result is absent".into())
        }
        ModelItem::User { .. } => {
            let occurrence = nodes
                .iter()
                .take_while(|(seq, _)| *seq != target)
                .filter(|(_, item)| item == old)
                .count();
            items
                .iter()
                .enumerate()
                .filter(|(_, item)| *item == old)
                .nth(occurrence)
                .map(|(index, _)| index)
                .ok_or("maintenance user message is absent".into())
        }
        _ => Err("maintenance target is not an input message".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn phase8_maintenance_maps_reused_call_ids_by_durable_occurrence() {
        let item = |text: &str| {
            ModelItem::ToolResult(crate::ToolResult {
                call_id: "reused".into(),
                tool_name: "evidence".into(),
                output: text.into(),
                is_error: false,
                blocks: vec![],
                image_parts: vec![],
            })
        };
        let items = vec![item("first"), item("second")];
        let nodes = vec![(10, items[0].clone()), (20, items[1].clone())];
        assert_eq!(live_index(&items, &nodes, 20).unwrap(), 1);
        assert_eq!(live_index(&items, &nodes, 10).unwrap(), 0);
    }
}
