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
    pub(super) fn sweep_tool_groups(&self, items: &mut [ModelItem]) -> Result<usize, String> {
        self.maintenance().sweep_tool_groups(items)
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

/// INV-CP5（CP-C4）：微压缩清组保留的最近完整工具交互组数。
pub(in crate::application) const MICRO_SWEEP_KEEP_GROUPS: usize = 5;

/// 清组占位符行前缀（既是模型可见的一行注记，也是二次清扫的"已清"标记）。
const MICRO_SWEEP_MARKER: &str = "[micro-compaction] old tool result cleared";

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
            let Some(mut pruned) = pruner.prune(blocks) else {
                continue;
            };
            let before = self.sessions.surface_nodes().map_err(|e| e.to_string())?;
            let index = live_index(items, &before, event.seq)?;
            if let Some((_, evidence)) = self.source_note(event.seq) {
                pruned.insert(0, json!({"type":"text","text":evidence}));
            }
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
    /// INV-CP5（CP-C4）：微压缩清组——无模型泄压层。把"最近
    /// `MICRO_SWEEP_KEEP_GROUPS` 个完整工具交互组"之前的旧**文本**工具
    /// 结果整组替换为一行占位符（保留工具名/call_id/原文 seq 与字符数
    /// 注记；原文永在 journal）。保护项：含图结果、错误结果、已清结果、
    /// `protected_floor` 之内的结果一律不清。持久化沿用 `compaction/prune`
    /// （log-only 影子计价）+ `tool/result` replace 事件对——与单条剪枝
    /// 同词汇，零新增 catalog 座位。挂接位置：trim 之后、付钱摘要之前。
    pub(in crate::application) fn sweep_tool_groups(
        &self,
        items: &mut [ModelItem],
    ) -> Result<usize, String> {
        let results = self.sweep_candidates();
        let mut count = 0;
        for event in results {
            if self.cancel.is_cancelled() {
                break;
            }
            let before = self.sessions.surface_nodes().map_err(|e| e.to_string())?;
            let index = live_index(items, &before, event.seq)?;
            let tool_name = match &items[index] {
                ModelItem::ToolResult(result) => result.tool_name.clone(),
                _ => continue,
            };
            // 注记字符数取事件内现存文本块的实际长度（剪枝后再清扫时
            // 反映剪后体量，而非原始体量）。
            let chars = event
                .data
                .pointer(sweep_pointer(&event.data))
                .and_then(Value::as_array)
                .map(|blocks| {
                    blocks
                        .iter()
                        .filter_map(|block| block["text"].as_str())
                        .map(|text| text.chars().count())
                        .sum::<usize>()
                })
                .unwrap_or(0);
            let tokens = before
                .iter()
                .filter(|(seq, _)| *seq == event.seq)
                .map(|(_, item)| crate::model::estimate_model_item_tokens(item))
                .sum::<u64>();
            let (original_seq, evidence) = self
                .source_note(event.seq)
                .unwrap_or_else(|| (event.seq, String::new()));
            let placeholder = json!([{
                "type": "text",
                "text": format!(
                    "{MICRO_SWEEP_MARKER}: {tool_name} call {} seq {original_seq} chars {chars}. {evidence}",
                    event.data["message"]["toolCallId"].as_str().unwrap_or_default(),
                ),
            }]);
            let mut data = event.data;
            *data
                .pointer_mut(sweep_pointer(&data))
                .ok_or("missing tool result content")? = placeholder;
            let projected = crate::session::adapter::model_tool_result(&data)?;
            if crate::model::estimate_model_item_tokens(&projected) >= tokens {
                continue;
            }
            let revision = self.journal.committed_seq();
            let range = self.journal.append_atomic_durable_at(revision, &[
                NewSessionEvent::new("compaction/prune",json!({"shadowedRange":{"start":event.seq,"end":event.seq},"shadowedSeqs":[event.seq],"shadowedTokenCount":tokens})).log_only(),
                NewSessionEvent::new("tool/result",data).replace(event.seq,event.seq,vec![event.seq]),
            ])?;
            self.refresh_item(items, index, range.start + 1)?;
            count += 1;
        }
        Ok(count)
    }

    fn source_note(&self, seq: u64) -> Option<(u64, String)> {
        let (seq, source) = self.sessions.original_tool_result(seq)?;
        let session_id = self.sessions.active_id()?;
        let reference = json!({"session_id":session_id.as_str(),"event_seq":seq});
        let fields = if source.is_error {
            serde_json::Map::new()
        } else {
            crate::tool::evidence::tool_facts(&source.output)
        };
        let mut note = format!(
            "For original evidence, use session_event_read with {reference} when that tool is available."
        );
        if !fields.is_empty() {
            let fields = Value::Object(fields);
            note = format!("Recorded tool fields: {fields}. {note}");
        }
        Some((seq, note))
    }

    /// 清组候选：按 seq 升序的 tool/result 事件，保留最近
    /// `MICRO_SWEEP_KEEP_GROUPS` 组；逐条应用保护项（含图/错误/已清/
    /// protected_floor 之内，见 `sweep_protected`）。
    fn sweep_candidates(&self) -> Vec<crate::session::event::SessionEvent> {
        let mut results: Vec<_> = self
            .sessions
            .maintenance_events()
            .into_iter()
            .filter(|event| event.event_type == "tool/result")
            .collect();
        results.sort_by_key(|event| event.seq);
        let Some(old_count) = old_result_group_prefix(&results, MICRO_SWEEP_KEEP_GROUPS) else {
            return Vec::new();
        };
        results[..old_count]
            .iter()
            .filter(|event| event.seq < self.protected_floor)
            .filter(|event| !sweep_protected(&event.data))
            .cloned()
            .collect()
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
/// Preserve the last N *model steps*, not N individual results. One model
/// response may issue multiple tool calls, and cutting it in half corrupts
/// the recent evidence boundary. Legacy/malformed metadata fails closed.
fn old_result_group_prefix(
    results: &[crate::session::event::SessionEvent],
    keep: usize,
) -> Option<usize> {
    let mut groups = Vec::<(u64, u64)>::new();
    for result in results {
        let group = (
            result.data.get("turn")?.as_u64()?,
            result.data.get("step")?.as_u64()?,
        );
        if groups.last() != Some(&group) {
            // A non-contiguous reused (turn,step) is ambiguous; keep evidence.
            if groups.contains(&group) {
                return None;
            }
            groups.push(group);
        }
    }
    let first_kept_group = *groups.get(groups.len().checked_sub(keep)?)?;
    results.iter().position(|result| {
        (result.data["turn"].as_u64(), result.data["step"].as_u64())
            == (Some(first_kept_group.0), Some(first_kept_group.1))
    })
}

/// INV-CP5 保护项（纯函数，判别测试锁死）：含图结果（媒体证据）、
/// 错误结果（失败事实是证据）、已清结果（占位符标记即幂等门）一律
/// 不清。protected_floor 由调用方过滤。
fn sweep_protected(data: &Value) -> bool {
    let pointer = sweep_pointer(data);
    let Some(blocks) = data.pointer(pointer).and_then(Value::as_array) else {
        return true;
    };
    // Only explicitly well-formed text is discardable. The old image-only
    // check silently erased audio, attachment and unknown future media types.
    if blocks.is_empty()
        || !blocks
            .iter()
            .all(|block| block["type"] == "text" && block["text"].as_str().is_some())
    {
        return true;
    }
    let result = if data["message"]["role"] == "tool" {
        &data["message"]
    } else {
        &data["message"]["content"][0]
    };
    // Missing/ambiguous error metadata must not be treated as success.
    if result["isError"] != json!(false) {
        return true;
    }
    blocks
        .first()
        .and_then(|block| block["text"].as_str())
        .is_some_and(|text| text.starts_with(MICRO_SWEEP_MARKER))
}

/// tool/result 内容在事件内的位置（与剪枝/catalog 同口径：role=tool 直挂，
/// 否则嵌套在首块内）。
fn sweep_pointer(data: &Value) -> &'static str {
    if data["message"]["role"] == "tool" {
        "/message/content"
    } else {
        "/message/content/0/content"
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

#[cfg(test)]
mod cp1_tests {
    use super::*;

    #[test]
    fn cp1_review_keeps_entire_recent_model_steps_not_five_result_messages() {
        let events: Vec<_> = (1..=6)
            .flat_map(|step| {
                (0..2).map(move |index| {
                    crate::session::event::SessionEvent::new(
                        "tool/result",
                        (step * 2 + index) as u64,
                        0,
                        json!({"turn": 1, "step": step, "message": {
                            "role": "tool", "toolCallId": format!("{step}-{index}"),
                            "content": [{"type": "text", "text": "evidence"}],
                            "isError": false
                        }}),
                    )
                })
            })
            .collect();
        assert_eq!(old_result_group_prefix(&events, 5), Some(2));
        assert_eq!(old_result_group_prefix(&events[2..], 5), Some(0));
        let mut malformed = events.clone();
        malformed[1].data.as_object_mut().unwrap().remove("step");
        assert_eq!(old_result_group_prefix(&malformed, 5), None);
    }

    /// INV-CP5 保护项判别：含图/错误/已清不清，普通文本可清。
    #[test]
    fn sweep_protects_media_errors_and_already_cleared_results() {
        let event = |blocks: Value, is_error: bool| {
            json!({
                "message": {
                    "role": "tool",
                    "toolCallId": "call-1",
                    "content": blocks,
                    "isError": is_error,
                }
            })
        };
        let text = |content: &str| json!([{ "type": "text", "text": content }]);
        // 普通旧文本结果：可清。
        assert!(!sweep_protected(&event(text("plain observation"), false)));
        // 含图（媒体证据）：不清。
        assert!(sweep_protected(&event(
            json!([
                { "type": "text", "text": "caption" },
                { "type": "image", "source": {} }
            ]),
            false
        )));
        // 错误（失败事实是证据）：不清。
        assert!(sweep_protected(&event(text("command failed"), true)));
        // Unknown media and malformed error metadata cannot be discarded.
        assert!(sweep_protected(&event(
            json!([{ "type": "audio", "source": { "id": "recording" } }]),
            false
        )));
        assert!(sweep_protected(&event(
            json!([{ "type": "text", "text": "metadata" },
                   { "type": "file", "path": "artifact.bin" }]),
            false
        )));
        assert!(sweep_protected(&event(json!([]), false)));
        let mut unspecified = event(text("unclassified"), false);
        unspecified["message"]
            .as_object_mut()
            .unwrap()
            .remove("isError");
        assert!(sweep_protected(&unspecified));
        // 已清（占位符幂等门）：不清。
        assert!(sweep_protected(&event(
            text(&format!(
                "{MICRO_SWEEP_MARKER}: read_file call c seq 3 chars 42; kept in journal."
            )),
            false
        )));
        // 内容缺失的畸形事件按保护处理（fail-closed）。
        assert!(sweep_protected(&json!({"message": {}})));
    }

    /// 占位符注记（工具名/call_id/seq/字符数）一行成形。
    #[test]
    fn sweep_placeholder_names_the_cleared_group() {
        let data = json!({
            "message": {
                "role": "tool",
                "toolCallId": "read-3",
                "content": [{ "type": "text", "text": "x".repeat(93) }],
                "isError": false,
            }
        });
        let chars = data
            .pointer(sweep_pointer(&data))
            .and_then(Value::as_array)
            .map(|blocks| {
                blocks
                    .iter()
                    .filter_map(|block| block["text"].as_str())
                    .map(|text| text.chars().count())
                    .sum::<usize>()
            })
            .unwrap_or(0);
        let line = format!(
            "{MICRO_SWEEP_MARKER}: read_file call read-3 seq 7 chars {chars}; the original is preserved in the session journal."
        );
        assert!(line.starts_with(MICRO_SWEEP_MARKER));
        assert!(line.contains("read-3"));
        assert!(line.contains("chars 93"));
        assert!(!line.contains('\n'));
    }
}
