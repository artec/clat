//! dsh 模式的转录装配（D-1 §4，INV-D6；D-2 §1.2 去模型化）：DSH wire
//! 事件 → CLAT `SessionEvent` → 既有 `ReplayAdapter`（流式 push）→
//! `ConversationModel::apply_replay`——与本地 journal 回放同管线。
//! 本结构只持折叠状态（adapter + last_seq）；**模型实例归 App 独占**
//! （App.conversation 是唯一模型——滚动/选区/折叠全挂它），方法签名
//! 显式收 `&mut ConversationModel`。独立补充只有一块：`assistant/chunk`
//! 的 chunk 级流式增量（ReplayEvent 是整消息粒度），整消息到达时丢弃
//! 流式预览、由 replay 通路落定权威形态。

use crate::session::event::SessionEvent;
use crate::session::replay::ReplayAdapter;
use crate::tui::conversation::ConversationModel;

/// 一条会话的 dsh 转录折叠状态（D-2 §1.2：不含模型实例）。
pub(crate) struct DshTranscript {
    adapter: ReplayAdapter,
    /// 已进入的最末事件 seq（间隙检测用；尚无事件时 `None`）。
    last_seq: Option<u64>,
}

impl DshTranscript {
    pub(crate) fn new() -> Self {
        Self {
            adapter: ReplayAdapter::new(),
            last_seq: None,
        }
    }

    /// 装载一页历史（`session.history` 返回的事件按 seq 升序进入；
    /// 历史页天然连续，重放整页即可）。整页重建：模型一并重置
    ///（调用方的 conversation 由这里接管为空白态）。
    pub(crate) fn load_history(&mut self, model: &mut ConversationModel, events: &[SessionEvent]) {
        self.adapter = ReplayAdapter::new();
        self.last_seq = None;
        *model = ConversationModel::new();
        for event in events {
            self.apply(model, event);
        }
    }

    /// 应用一条事件（历史与活流共用）。返回 true = 该事件推进了
    /// 转录（供上层决定重绘）。
    pub(crate) fn apply(&mut self, model: &mut ConversationModel, event: &SessionEvent) -> bool {
        let advanced = self.last_seq.is_none_or(|last| event.seq > last);
        if advanced {
            self.last_seq = Some(event.seq);
        }
        if event.event_type == "assistant/chunk" {
            self.apply_chunk(model, event);
            return true;
        }
        let mut replay_events = Vec::new();
        self.adapter.push(event, &mut replay_events);
        if replay_events.is_empty() {
            return advanced;
        }
        // 任何落定事件都会终结流式预览：丢弃开放态项，让权威形态进入。
        model.discard_open_stream_assistant();
        model.apply_replay(&replay_events);
        true
    }

    /// INV-D5 间隙判定：事件 seq 与已见最末 seq 之间的差 > 1 表示有
    /// 帧丢失（应触发 session.history 补齐）。
    pub(crate) fn gap_before(&self, event: &SessionEvent) -> Option<u64> {
        let last = self.last_seq?;
        (event.seq > last + 1).then_some(last + 1)
    }

    /// 订阅基线锚：`session/subscribed.lastSeq`（-1 = 空会话）。
    pub(crate) fn baseline(&mut self, last_seq: i64) {
        self.last_seq = if last_seq < 0 {
            None
        } else {
            Some(last_seq as u64)
        };
    }

    fn apply_chunk(&mut self, model: &mut ConversationModel, event: &SessionEvent) {
        let Some(chunk) = event.data.get("chunk") else {
            return;
        };
        // reasoning/tool-call delta 存而不显（与既有取舍一致）；
        // block-start/block-end/usage/finish 不驱动渲染。
        if chunk.get("type").and_then(|value| value.as_str()) == Some("text-delta")
            && let Some(text) = chunk.get("text").and_then(|value| value.as_str())
        {
            model.open_stream_assistant("dsh", "streaming");
            model.append_stream_text(text);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::conversation::ToolCardVisibility;
    use serde_json::json;

    fn event(kind: &str, seq: u64, data: serde_json::Value) -> SessionEvent {
        SessionEvent::new(kind, seq, 1_700_000_000_000 + seq as i64 * 1000, data)
    }

    fn surface(kind: &str, seq: u64, data: serde_json::Value) -> SessionEvent {
        event(kind, seq, data).append(Vec::new())
    }

    #[test]
    fn live_stream_preview_is_replaced_by_the_settled_message() {
        let mut transcript = DshTranscript::new();
        let mut model = ConversationModel::new();
        transcript.apply(
            &mut model,
            &surface(
                "user/message",
                0,
                json!({"content": [{"type": "text", "text": "hello"}]}),
            ),
        );
        // 流式 chunk：预览文本累积。
        transcript.apply(
            &mut model,
            &event(
                "assistant/chunk",
                1,
                json!({"turn": 1, "step": 1, "chunk": {"type": "text-delta", "index": 0, "text": "par"}}),
            ),
        );
        transcript.apply(
            &mut model,
            &event(
                "assistant/chunk",
                2,
                json!({"turn": 1, "step": 1, "chunk": {"type": "text-delta", "index": 0, "text": "tial"}}),
            ),
        );
        // 落定：整消息经 replay 通路进入，预览被丢弃（不重复）。
        transcript.apply(
            &mut model,
            &surface(
                "assistant/message",
                3,
                json!({"turn": 1, "step": 1, "message": {
                    "role": "assistant",
                    "content": [{"type": "text", "text": "partial and final"}],
                    "source": {"provider": "deepseek", "model": "test-model"}
                }}),
            ),
        );
        // 逐行纯文本检查：用户项与落定消息在，且无重复预览残迹
        //（"partial" 只以完整形态出现一次）。
        let width = 80;
        model.ensure_rendered(width);
        let total = model.total_lines(ToolCardVisibility::Collapsed);
        let rows: Vec<String> = (0..total)
            .map(|row| model.row_plain_text(row, width, ToolCardVisibility::Collapsed))
            .collect();
        let joined = rows.join("\n");
        assert!(joined.contains("hello"), "{joined}");
        assert!(joined.contains("partial and final"), "{joined}");
        assert_eq!(
            joined.matches("partial").count(),
            1,
            "the settled message appears exactly once: {joined}"
        );
    }

    #[test]
    fn gap_detection_and_baseline_anchor() {
        let mut transcript = DshTranscript::new();
        transcript.baseline(3);
        let next = event("turn/start", 4, json!({"turn": 2}));
        assert_eq!(transcript.gap_before(&next), None);
        let skipped = event(
            "turn/end",
            8,
            json!({"turn": 2, "reason": {"kind": "completed"}}),
        );
        assert_eq!(transcript.gap_before(&skipped), Some(4));
        let mut model = ConversationModel::new();
        transcript.apply(&mut model, &skipped);
        let after = event("session/title", 9, json!({"title": "t"}));
        assert_eq!(transcript.gap_before(&after), None);
        // 空会话基线。
        transcript.baseline(-1);
        assert_eq!(transcript.last_seq, None);
    }

    /// §1.2 去模型化判别：load_history 整页重置把调用方模型接管为
    /// 空白态（App.conversation 唯一模型的重建路径）。
    #[test]
    fn load_history_rebuilds_the_caller_owned_model() {
        let mut transcript = DshTranscript::new();
        let mut model = ConversationModel::new();
        transcript.apply(
            &mut model,
            &surface(
                "user/message",
                0,
                json!({"content": [{"type": "text", "text": "old"}]}),
            ),
        );
        assert!(!model.is_empty());
        transcript.load_history(
            &mut model,
            &[surface(
                "user/message",
                0,
                json!({"content": [{"type": "text", "text": "new"}]}),
            )],
        );
        assert!(!model.is_empty());
        model.ensure_rendered(80);
        let total = model.total_lines(ToolCardVisibility::Collapsed);
        let rows: Vec<String> = (0..total)
            .map(|row| model.row_plain_text(row, 80, ToolCardVisibility::Collapsed))
            .collect();
        let joined = rows.join("\n");
        assert!(joined.contains("new"), "{joined}");
        assert!(!joined.contains("old"), "{joined}");
    }
}
