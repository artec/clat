//! 会话转录模型（TUI 重构架构级 P0-0，DSH `renderEvent` 的对应物）。
//!
//! 单一装配：live `RunEvent` 流与 journal `ReplayEvent` 回放汇入同一组
//! item 构造器，UI 不持有独立 durable 转录状态（G2/G8）。渲染是
//! `(state, width) → lines` 的纯函数，逐 item 缓存：命中当且仅当内容
//! 代数与宽度未变，任何 mutator 置脏，宽度变化全部失效（G3）——
//! 每帧只克隆视口行，不再全量深拷贝。
//!
//! 已知取舍：reasoning 与 tool_calls 存而不显（`/details` 展示是 P2）；
//! 工具卡在本模块累积状态但 B5 之前不渲染；turn-end 通知 B7 起渲染。

use crate::RunEvent;
use crate::ToolCall;
use crate::model::ModelEvent;
use crate::session::replay::{ReplayEvent, ReplayTurnEnd};
use crate::tui::markdown::render_markdown;
use crate::tui::theme;
use crate::tui::tool_argument_lines;
use crate::tui::wrap_text;
use ratatui::text::{Line, Span};
use serde_json::Value;
use std::collections::VecDeque;
use unicode_width::UnicodeWidthStr;

/// 工具卡状态：`Pending`（○ 模型已发起）→ `Settled`（● 已有结果）或
/// `Denied`（权限拒绝——journal 拒绝路径无 tool/call，回放侧以
/// isError 结果呈现，对拍按"错误类"归一）。
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum CardState {
    Pending,
    Settled { output: Value, is_error: bool },
    Denied { reason: String },
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum ConversationItem {
    User {
        text: String,
    },
    Assistant {
        text: String,
        reasoning: Option<String>,
        tool_calls: Vec<ToolCall>,
        provider: String,
        model: String,
    },
    ToolCard {
        call_id: String,
        tool: String,
        arguments: Value,
        state: CardState,
    },
    /// 压缩摘要（journal 的 compaction/summary 与 replace 载体各产生一项，
    /// 与旧 TranscriptUnit 的 keeps-everything 语义平价）。
    Compaction {
        text: String,
    },
    /// run 终态通知（B7 起生产构造并渲染；先入模型保证 live/replay 同形）。
    #[allow(dead_code)]
    TurnEnd {
        text: String,
    },
}

/// 逐 item 渲染缓存：`lines` 只在（内容代数, 宽度）匹配时有效。
#[derive(Clone, Debug, Default)]
struct ItemCache {
    dirty: bool,
    width: Option<usize>,
    /// 流式 assistant 项渲染时使用的活动帧字形（None=落定 ⏺）。帧变
    /// 化必须触发重渲染——动画帧与内容/宽度同属缓存键。
    marker: Option<&'static str>,
    lines: Vec<Line<'static>>,
}

impl ItemCache {
    fn fresh() -> Self {
        Self {
            dirty: true,
            ..Self::default()
        }
    }
}

/// 工具卡三态（Ctrl+O 循环）。纯呈现状态：从不持久化、不进会话日志
/// （G5）；resize/重绘后重推导。
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) enum ToolCardVisibility {
    /// 折叠预览：head/tail 各半 + 计数标记（默认）。
    #[default]
    Collapsed,
    /// 全文。
    Expanded,
    /// 整卡消失（0 行）。
    Hidden,
}

/// 折叠预览的行预算（phase-1 P1-4 默认 6 行；配置文件化后置）。
pub(crate) const MAX_TOOL_OUTPUT_LINES: usize = 6;

impl ToolCardVisibility {
    pub(crate) fn next(self) -> Self {
        match self {
            Self::Collapsed => Self::Expanded,
            Self::Expanded => Self::Hidden,
            Self::Hidden => Self::Collapsed,
        }
    }
}

#[derive(Default)]
pub(crate) struct ConversationModel {
    items: Vec<(ConversationItem, ItemCache)>,
    /// 未 claim 的 steering 回显尾部区（纯视图状态，INV-SV1）：FIFO 与
    /// core 队列同序，渲染在全部 items 之后（流式 assistant 仍是最后
    /// 一个 item，续写不被打断，INV-SV6）。claim → `confirm`（front 出
    /// 区、在 claim 时点的尾部落为正式用户项）；ESC 召回 → `recall`
    /// （back 出区回编辑框）；run 结束/取消 → `discard` 清区。
    pending_steering: VecDeque<String>,
    pending_lines: Vec<Vec<Line<'static>>>,
    pending_width: Option<usize>,
    /// pending 区内容代数（push/recall/discard 递增）——条数相同的
    /// 换血也必须重建缓存。
    pending_generation: u64,
    pending_rendered_generation: u64,
    /// 流式 assistant 是否可续写（下一个用户/卡片/压缩项关闭它）。
    assistant_open: bool,
    /// 流式 assistant 项前缀的当前活动帧（None=落定 ⏺）：由前端每帧
    /// 设置（run 活动时为 spinner 帧），见 `set_stream_marker`。
    stream_marker: Option<&'static str>,
    /// 最近一次 ToolRequested 的 call id：live `PermissionDenied` 不带
    /// call id，只能据此回指。
    last_call_id: Option<String>,
}

impl ConversationModel {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    // ---- 共享构造器（live 与 replay 的唯一汇合点，G8）----

    fn push_item(&mut self, item: ConversationItem) {
        self.assistant_open = false;
        self.items.push((item, ItemCache::fresh()));
    }

    pub(crate) fn push_user(&mut self, text: String) {
        self.push_item(ConversationItem::User { text });
    }

    pub(crate) fn push_compaction(&mut self, text: String) {
        self.push_item(ConversationItem::Compaction { text });
    }

    pub(crate) fn push_turn_end(&mut self, text: String) {
        self.push_item(ConversationItem::TurnEnd { text });
    }

    // ---- steering 回显尾部区（docs/todo/steering-visibility-recall.md）----

    /// steer 入队回显：FIFO 尾部（与 core 队列同序）。
    pub(crate) fn push_pending_steering(&mut self, text: String) {
        self.pending_steering.push_back(text);
        self.pending_generation += 1;
    }

    /// `SteeringApplied` 的 claim 升级（INV-SV2）：按事件文本出区、在
    /// 当前尾部落为正式用户项（事件文本是权威）——升级后的 items 序 ==
    /// journal 顺序 == 回放顺序。W1-29：确认**按文本匹配**出区——陈旧
    /// 事件（上一 run 收尾窗口送达，`RunEvent` 不携带 run 身份）失配时
    /// 不弹本 run 的队首，降级为普通用户项追加；区为空（测试直灌事件
    /// 等）同样 `push_user`，向后兼容。
    pub(crate) fn confirm_pending_steering(&mut self, text: String) {
        if let Some(position) = self
            .pending_steering
            .iter()
            .position(|pending| pending == &text)
        {
            self.pending_steering.remove(position);
        }
        self.pending_generation += 1;
        self.push_user(text);
    }

    /// ESC 召回（INV-SV4）：back 出区，文本退回调用方（编辑框）。
    pub(crate) fn recall_pending_steering(&mut self) -> Option<String> {
        let text = self.pending_steering.pop_back()?;
        self.pending_generation += 1;
        Some(text)
    }

    /// run 结束/取消：清区并返回条数（INV-SV5，丢弃提示的计数来源）。
    pub(crate) fn discard_pending_steering(&mut self) -> usize {
        let count = self.pending_steering.len();
        self.pending_steering.clear();
        self.pending_generation += 1;
        count
    }

    /// pending 条数（徽标的单一事实源，INV-SV1）。
    pub(crate) fn pending_steering_count(&self) -> usize {
        self.pending_steering.len()
    }

    /// 测试便利：压入一条已落定的 assistant（占位 provider/model）。
    #[cfg(test)]
    pub(crate) fn push_assistant_for_test(&mut self, text: &str) {
        self.push_item(ConversationItem::Assistant {
            text: text.to_owned(),
            reasoning: None,
            tool_calls: Vec::new(),
            provider: "application-test".into(),
            model: "deterministic".into(),
        });
    }

    fn open_assistant(&mut self, provider: String, model: String) {
        self.assistant_open = true;
        self.items.push((
            ConversationItem::Assistant {
                text: String::new(),
                reasoning: None,
                tool_calls: Vec::new(),
                provider,
                model,
            },
            ItemCache::fresh(),
        ));
    }

    /// 流式写入 assistant；仅在开放态（上一 item 为流式 assistant）生效。
    fn append_assistant(&mut self, with: impl FnOnce(&mut ConversationItem)) {
        if !self.assistant_open {
            return;
        }
        if let Some((item, cache)) = self.items.last_mut()
            && matches!(item, ConversationItem::Assistant { .. })
        {
            with(item);
            cache.dirty = true;
        }
    }

    pub(crate) fn open_tool_card(&mut self, call_id: String, tool: String, arguments: Value) {
        self.last_call_id = Some(call_id.clone());
        self.push_item(ConversationItem::ToolCard {
            call_id,
            tool,
            arguments,
            state: CardState::Pending,
        });
    }

    /// 落定已有卡；找不到对应 call id 时忽略（防御 journal 异态）。
    pub(crate) fn settle_tool_card(&mut self, call_id: &str, state: CardState) {
        for (item, cache) in self.items.iter_mut().rev() {
            if let ConversationItem::ToolCard {
                call_id: id,
                state: slot,
                ..
            } = item
                && id == call_id
            {
                *slot = state;
                cache.dirty = true;
                return;
            }
        }
    }

    /// 非流式 provider 的回填：run 完成时若开放态 assistant 仍无正文，
    /// 以最终输出填充（镜像旧 finish_run 兜底与 journal 侧 assistant/
    /// message 的 settled 文本，保持 live/replay 对拍成立）。
    pub(crate) fn settle_streamed_output(&mut self, output: &str) {
        self.append_assistant(|item| {
            if let ConversationItem::Assistant { text, .. } = item
                && text.trim().is_empty()
            {
                *text = output.to_owned();
            }
        });
    }

    // ---- live 入口 ----

    pub(crate) fn apply_run_event(&mut self, event: &RunEvent) {
        match event {
            RunEvent::ModelRequested {
                provider, model, ..
            } => {
                self.open_assistant(provider.clone(), model.clone());
            }
            RunEvent::ModelStream { event, .. } => match event {
                ModelEvent::TextDelta { delta } | ModelEvent::RefusalDelta { delta } => {
                    self.append_assistant(|item| {
                        if let ConversationItem::Assistant { text, .. } = item {
                            text.push_str(delta);
                        }
                    });
                }
                ModelEvent::ReasoningDelta { delta }
                | ModelEvent::ReasoningSummaryDelta { delta } => {
                    self.append_assistant(|item| {
                        if let ConversationItem::Assistant { reasoning, .. } = item {
                            reasoning.get_or_insert_with(String::new).push_str(delta);
                        }
                    });
                }
                ModelEvent::ToolCallCompleted { call } => {
                    self.append_assistant(|item| {
                        if let ConversationItem::Assistant { tool_calls, .. } = item {
                            tool_calls.push(call.clone());
                        }
                    });
                }
                _ => {}
            },
            RunEvent::ToolRequested { call } => {
                self.open_tool_card(call.id.clone(), call.name.clone(), call.arguments.clone());
            }
            RunEvent::ToolFinished { result } => {
                self.settle_tool_card(
                    &result.call_id,
                    CardState::Settled {
                        output: result.output.clone(),
                        is_error: result.is_error,
                    },
                );
            }
            RunEvent::PermissionDenied { reason, .. } => {
                if let Some(call_id) = self.last_call_id.clone() {
                    self.settle_tool_card(
                        &call_id,
                        CardState::Denied {
                            reason: reason.clone(),
                        },
                    );
                }
            }
            RunEvent::SteeringApplied { text } => {
                // 与 replay 侧的 UserMessage 同一位点：claim 发生在上一步
                // assistant/工具卡之后、下一步模型请求之前。pending 回显
                // 升级为正式用户项（INV-SV2）。
                self.confirm_pending_steering(text.clone());
            }
            _ => {}
        }
    }

    // ---- replay 入口 ----

    pub(crate) fn from_replay(events: &[ReplayEvent]) -> Self {
        let mut model = Self::new();
        model.apply_replay(events);
        model
    }

    pub(crate) fn apply_replay(&mut self, events: &[ReplayEvent]) {
        for event in events {
            match event {
                ReplayEvent::UserMessage { text, .. } => self.push_user(text.clone()),
                ReplayEvent::AssistantMessage {
                    reasoning,
                    text,
                    tool_calls,
                    provider,
                    model,
                    ..
                } => {
                    self.push_item(ConversationItem::Assistant {
                        text: text.clone(),
                        reasoning: reasoning.clone(),
                        tool_calls: tool_calls.clone(),
                        provider: provider.clone(),
                        model: model.clone(),
                    });
                }
                ReplayEvent::ToolRequested { call, .. } => {
                    self.open_tool_card(call.id.clone(), call.name.clone(), call.arguments.clone());
                }
                ReplayEvent::ToolFinished {
                    call_id,
                    output,
                    is_error,
                    ..
                } => {
                    self.settle_tool_card(
                        call_id,
                        CardState::Settled {
                            output: output.clone(),
                            is_error: *is_error,
                        },
                    );
                }
                ReplayEvent::Compaction { summary_text, .. } => {
                    self.push_compaction(summary_text.clone());
                }
                ReplayEvent::TurnEnded { reason, .. } => {
                    self.push_turn_end(turn_end_notice_text(reason));
                }
                // 权限与重试只影响状态行，不进转录。
                ReplayEvent::PermissionChecked { .. } | ReplayEvent::RetryScheduled { .. } => {}
            }
        }
    }

    // ---- 渲染（G3 缓存契约）----

    /// 重渲染脏项并缓存；宽度变化使全部缓存失效。摊销后每个 item 在
    /// 其（内容, 宽度）生命周期内只渲染一次。行数 = Σ(item 行数 + 分隔
    /// 空行)；零行项（B5 hidden 卡、B7 前的通知）不占分隔行。
    pub(crate) fn ensure_rendered(&mut self, width: usize) {
        let open_index = self.open_assistant_index();
        for index in 0..self.items.len() {
            let marker = if Some(index) == open_index {
                self.stream_marker
            } else {
                None
            };
            let needs = {
                let (_, cache) = &self.items[index];
                cache.dirty || cache.width != Some(width) || cache.marker != marker
            };
            if needs {
                let rendered = render_item(&self.items[index].0, width, marker);
                let (_, cache) = &mut self.items[index];
                cache.lines = rendered;
                cache.width = Some(width);
                cache.marker = marker;
                cache.dirty = false;
            }
        }
        // pending 区缓存：内容代数或宽度变化才重建（与 items 的 G3 纪律
        // 同构；变更只来自用户动作与 claim，低频）。
        if self.pending_rendered_generation != self.pending_generation
            || self.pending_width != Some(width)
        {
            self.pending_lines = self
                .pending_steering
                .iter()
                .map(|text| render_pending_steering(text, width))
                .collect();
            self.pending_width = Some(width);
            self.pending_rendered_generation = self.pending_generation;
        }
    }

    /// 流式 assistant 项的开放下标（assistant_open ⇒ 末项为 assistant）。
    fn open_assistant_index(&self) -> Option<usize> {
        self.assistant_open
            .then(|| self.items.len().checked_sub(1))
            .flatten()
    }

    /// 设置流式 assistant 前缀的活动帧（`None` = 落定 ⏺）。变化时使
    /// 开放项缓存失效（G3 的帧驱动例外：每帧至多重渲染一个 item）。
    pub(crate) fn set_stream_marker(&mut self, marker: Option<&'static str>) {
        if self.stream_marker != marker {
            self.stream_marker = marker;
            if let Some(index) = self.open_assistant_index() {
                self.items[index].1.dirty = true;
            }
        }
    }

    /// 卡片行数（不物化：折叠 = min(len, budget) + 可能的 1 行标记）。
    fn card_row_count(cache: &ItemCache, visibility: ToolCardVisibility) -> usize {
        match visibility {
            ToolCardVisibility::Hidden => 0,
            ToolCardVisibility::Expanded => cache.lines.len(),
            ToolCardVisibility::Collapsed => {
                let len = cache.lines.len();
                len.min(MAX_TOOL_OUTPUT_LINES) + usize::from(len > MAX_TOOL_OUTPUT_LINES)
            }
        }
    }

    /// 空会话（无 items 且无 pending）：draw 以 LOGO 欢迎页接管会话区。
    pub(crate) fn is_empty(&self) -> bool {
        self.items.is_empty() && self.pending_steering.is_empty()
    }

    /// 内容总行数（空会话为 0——空态由欢迎页渲染，不占内容行）。
    pub(crate) fn total_lines(&self, visibility: ToolCardVisibility) -> usize {
        if self.is_empty() {
            return 0;
        }
        let items = self
            .items
            .iter()
            .map(|(item, cache)| {
                let rows = match item {
                    ConversationItem::ToolCard { .. } => Self::card_row_count(cache, visibility),
                    _ => cache.lines.len(),
                };
                rows + usize::from(rows > 0)
            })
            .sum::<usize>();
        // pending 回显区：与 items 同构（行数 + 条目间分隔行）。
        let pending = self
            .pending_lines
            .iter()
            .map(|lines| lines.len() + usize::from(!lines.is_empty()))
            .sum::<usize>();
        items + pending
    }

    /// 取视口行——消息行零拷贝借用，卡片行按 visibility 物化（折叠态
    /// 有界；展开态受 core 工具结果截断约束），每帧成本 O(viewport)。
    pub(crate) fn visible_lines(
        &mut self,
        start: usize,
        count: usize,
        width: usize,
        visibility: ToolCardVisibility,
    ) -> Vec<Line<'static>> {
        self.ensure_rendered(width);
        if self.is_empty() {
            return Vec::new();
        }
        let mut out = Vec::with_capacity(count.min(64));
        let mut row = 0usize;
        let end = start + count;
        'items: for (item, cache) in &self.items {
            let rows = item_rows(item, cache, visibility);
            for line in rows.as_slice() {
                if row >= end {
                    break 'items;
                }
                if row >= start {
                    out.push(line.clone());
                }
                row += 1;
            }
            if rows.as_slice().is_empty() {
                continue;
            }
            if row >= end {
                break 'items;
            }
            if row >= start {
                out.push(Line::from(""));
            }
            row += 1;
        }
        // pending 回显区续接在 items 之后（尾部，INV-SV1/SV6）。
        'pending: for lines in &self.pending_lines {
            for line in lines {
                if row >= end {
                    break 'pending;
                }
                if row >= start {
                    out.push(line.clone());
                }
                row += 1;
            }
            if lines.is_empty() {
                continue;
            }
            if row >= end {
                break;
            }
            if row >= start {
                out.push(Line::from(""));
            }
            row += 1;
        }
        out
    }

    /// 内容行纯文本（选区复制路径，G4：无行尾装饰）。
    pub(crate) fn row_plain_text(
        &mut self,
        row: usize,
        width: usize,
        visibility: ToolCardVisibility,
    ) -> String {
        self.ensure_rendered(width);
        if self.is_empty() {
            return String::new();
        }
        let mut current = 0usize;
        for (item, cache) in &self.items {
            let rows = item_rows(item, cache, visibility);
            for line in rows.as_slice() {
                if current == row {
                    // 复制出口统一裁尾（G4）：用户块的满宽背景填充是纯
                    // 视觉，不进复制文本。
                    let text: String = line
                        .spans
                        .iter()
                        .map(|span| span.content.as_ref())
                        .collect();
                    return text.trim_end().to_owned();
                }
                current += 1;
            }
            if rows.as_slice().is_empty() {
                continue;
            }
            if current == row {
                return String::new();
            }
            current += 1;
        }
        // pending 回显区同样可复制（用户自己的文本）。
        for lines in &self.pending_lines {
            for line in lines {
                if current == row {
                    let text: String = line
                        .spans
                        .iter()
                        .map(|span| span.content.as_ref())
                        .collect();
                    return text.trim_end().to_owned();
                }
                current += 1;
            }
            if lines.is_empty() {
                continue;
            }
            if current == row {
                return String::new();
            }
            current += 1;
        }
        String::new()
    }
}

/// 单 item 的展示行：消息直接借用缓存（零拷贝），卡片按 visibility
/// 物化（折叠标记行是构造出来的）。
enum Rows<'a> {
    Borrowed(&'a [Line<'static>]),
    Owned(Vec<Line<'static>>),
}

impl Rows<'_> {
    fn as_slice(&self) -> &[Line<'static>] {
        match self {
            Self::Borrowed(lines) => lines,
            Self::Owned(lines) => lines,
        }
    }
}

fn item_rows<'a>(
    item: &'a ConversationItem,
    cache: &'a ItemCache,
    visibility: ToolCardVisibility,
) -> Rows<'a> {
    match item {
        ConversationItem::ToolCard { .. } => Rows::Owned(card_display_lines(cache, visibility)),
        _ => Rows::Borrowed(&cache.lines),
    }
}

fn render_item(
    item: &ConversationItem,
    width: usize,
    stream_marker: Option<&'static str>,
) -> Vec<Line<'static>> {
    match item {
        ConversationItem::User { text } | ConversationItem::Compaction { text } => {
            render_user_block(text, width)
        }
        ConversationItem::Assistant { text, .. } => render_assistant(text, width, stream_marker),
        ConversationItem::ToolCard {
            tool,
            arguments,
            state,
            ..
        } => render_tool_card(tool, arguments, state, width),
        // run 终态通知（G7）：dim 单行，每个停止都解释自己。
        ConversationItem::TurnEnd { text } => vec![Line::from(Span::styled(
            format!("· {text}"),
            theme::style(theme::Role::Dim),
        ))],
    }
}

/// 终态原因 → 通知文本。终态是封闭集之外的兜底已在回放适配器侧
/// 归并为 Error（"unsupported turn-end kind: …"），此处任何变体都
/// 产出非空解释（G7：未知终态显示通用解释而非静默）。
pub(crate) fn turn_end_notice_text(reason: &ReplayTurnEnd) -> String {
    match reason {
        ReplayTurnEnd::Completed => "completed".into(),
        ReplayTurnEnd::Aborted { cause } if cause == "user" => "cancelled".into(),
        ReplayTurnEnd::Aborted { cause } => format!("aborted ({cause})"),
        ReplayTurnEnd::Blocked => "blocked".into(),
        ReplayTurnEnd::MaxTokens => "max tokens".into(),
        ReplayTurnEnd::Interrupted => "interrupted".into(),
        ReplayTurnEnd::Error { message } => format!("error: {message}"),
    }
}

/// 工具卡 = 单行状态头 + dim 正文（参数 + 结果）。缓存持有**展开全文**，
/// 折叠/隐藏在展平阶段按 visibility 裁剪（先渲染后截断——上游 07-23
/// 教训：先截断源文本会撕开多行结构）。
fn render_tool_card(
    tool: &str,
    arguments: &Value,
    state: &CardState,
    width: usize,
) -> Vec<Line<'static>> {
    let (glyph, role) = match state {
        CardState::Pending => ("○", theme::Role::Warning),
        CardState::Settled {
            is_error: false, ..
        } => ("●", theme::Role::Success),
        CardState::Settled { is_error: true, .. } | CardState::Denied { .. } => {
            ("✗", theme::Role::Error)
        }
    };
    let mut lines = vec![Line::from(Span::styled(
        format!("{glyph} Tool / {tool}"),
        theme::style(role),
    ))];
    if let Some(argument_lines) = tool_argument_lines(tool, arguments, width) {
        lines.extend(argument_lines);
    } else {
        // 未知工具：参数 JSON 兜底（dim）。
        lines.push(Line::from(Span::styled(
            serde_json::to_string_pretty(arguments).unwrap_or_default(),
            theme::style(theme::Role::Dim),
        )));
    }
    match state {
        CardState::Pending => {}
        CardState::Settled { output, is_error } => {
            let mark = if *is_error { "✗" } else { "✓" };
            let role = if *is_error {
                theme::Role::Error
            } else {
                theme::Role::Success
            };
            lines.push(Line::from(Span::styled(
                format!("{mark} {}", value_display_text(output)),
                theme::style(role),
            )));
        }
        CardState::Denied { reason } => {
            lines.push(Line::from(Span::styled(
                format!("✗ permission denied — {reason}"),
                theme::style(theme::Role::Error),
            )));
        }
    }
    lines
}

fn value_display_text(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        other => serde_json::to_string(other).unwrap_or_default(),
    }
}

/// 卡片在当前 visibility 下的展示行。折叠 = head/tail 各
/// `MAX_TOOL_OUTPUT_LINES/2` + 计数标记；隐藏 = 0 行（G5）。
fn card_display_lines(cache: &ItemCache, visibility: ToolCardVisibility) -> Vec<Line<'static>> {
    match visibility {
        ToolCardVisibility::Hidden => Vec::new(),
        ToolCardVisibility::Expanded => cache.lines.clone(),
        ToolCardVisibility::Collapsed => {
            let budget = MAX_TOOL_OUTPUT_LINES;
            if cache.lines.len() <= budget {
                return cache.lines.clone();
            }
            let head = budget.div_ceil(2);
            let tail = budget - head;
            let mut out: Vec<Line<'static>> = cache.lines[..head].to_vec();
            out.push(Line::from(Span::styled(
                format!(
                    "  … +{} lines (Ctrl+O to expand)",
                    cache.lines.len() - budget
                ),
                theme::style(theme::Role::Dim),
            )));
            out.extend(cache.lines[cache.lines.len() - tail..].to_vec());
            out
        }
    }
}

/// 用户消息块：`❯ ` 前缀 + 背景填充到内容区满宽（横贯左右的视觉长条，
/// 用户 2026-08-19 反馈恢复）。行尾填充是**纯视觉**：复制出口
/// [`ConversationModel::row_plain_text`] 统一裁尾，复制文本保持干净（G4
/// 守在出口而非行内容）。
/// pending steering 回显块：与用户块同构但整体 dim、尾行追加 queued
/// 标记——它是"已入队、下一模型步生效"的视图状态（INV-SV1），claim
/// 后由 `confirm_pending_steering` 升级为正式用户块。
fn render_pending_steering(text: &str, width: usize) -> Vec<Line<'static>> {
    let style = theme::style(theme::Role::Dim);
    let marker = theme::style(theme::Role::Faint);
    let text_width = width.saturating_sub(2).max(1);
    let wrapped = wrap_text(text, text_width.saturating_sub(2).max(1));
    let last = wrapped.len().saturating_sub(1);
    wrapped
        .into_iter()
        .enumerate()
        .map(|(index, line)| {
            let (prefix, prefix_style) = if index == 0 {
                ("❯ ", marker)
            } else {
                ("  ", style)
            };
            let queued = if index == last { " · queued" } else { "" };
            let used = UnicodeWidthStr::width(prefix)
                + UnicodeWidthStr::width(line.as_str())
                + UnicodeWidthStr::width(queued);
            let padding = " ".repeat(width.saturating_sub(used));
            Line::from(vec![
                Span::styled(prefix.to_owned(), prefix_style),
                Span::styled(line, style),
                Span::styled(queued.to_owned(), marker),
                Span::styled(padding, style),
            ])
        })
        .collect()
}

fn render_user_block(text: &str, width: usize) -> Vec<Line<'static>> {
    let style = theme::style(theme::Role::UserBlock);
    let marker = theme::style(theme::Role::UserMarker);
    let text_width = width.saturating_sub(2).max(1);
    wrap_text(text, text_width.saturating_sub(2).max(1))
        .into_iter()
        .enumerate()
        .map(|(index, line)| {
            let (prefix, prefix_style) = if index == 0 {
                ("❯ ", marker)
            } else {
                ("  ", style)
            };
            let used = UnicodeWidthStr::width(prefix) + UnicodeWidthStr::width(line.as_str());
            let padding = " ".repeat(width.saturating_sub(used));
            Line::from(vec![
                Span::styled(prefix.to_owned(), prefix_style),
                Span::styled(line, style),
                Span::styled(padding, style),
            ])
        })
        .collect()
}

fn render_assistant(
    text: &str,
    width: usize,
    stream_marker: Option<&'static str>,
) -> Vec<Line<'static>> {
    // 流式进行中的 assistant 以"太阳"四分圆帧作前缀（灰色，保持圆形
    // 字形——与落定 ⏺ 同色族，与状态栏蓝色盲文 spinner 不同形不同
    // 色），等待首 token / 长思考时不再是静止的 ⏺；落定后（run 结束）
    // 回到常驻 ⏺。
    let marker = match stream_marker {
        Some(frame) => Span::styled(
            format!("{frame} "),
            theme::style(theme::Role::AssistantMarker),
        ),
        None => Span::styled("⏺ ", theme::style(theme::Role::AssistantMarker)),
    };
    let text_width = width.saturating_sub(2).max(1);
    render_markdown(text, text_width)
        .into_iter()
        .enumerate()
        .map(|(index, line)| {
            let mut spans = vec![if index == 0 {
                marker.clone()
            } else {
                Span::raw("  ")
            }];
            spans.extend(line.spans);
            Line::from(spans)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{
        SharedEvents, TestBehavior, TestProviderPlugin, configure_test_model, roots,
    };
    use crate::{BootstrapApplication, Project};
    use std::sync::{Arc, Mutex};

    #[test]
    fn streaming_assistant_marker_spins_while_open_and_settles_when_closed() {
        // 2026-08-19 用户反馈：等待首 token / 长思考时，转录区是一动不
        // 动的 ⏺。不变量：run 进行中（前端设置活动帧），开放 assistant
        // 项的前缀是 spinner 帧（品牌蓝）；帧切换触发重渲染（缓存键含
        // 帧）；run 结束落定回 ⏺。
        let mut model = ConversationModel::new();
        model.push_user("hello".into());
        model.apply_run_event(&RunEvent::ModelRequested {
            turn: 1,
            provider: "p".into(),
            model: "m".into(),
        });
        let has = |model: &mut ConversationModel, glyph: &str| {
            let lines = model.visible_lines(0, 20, 40, ToolCardVisibility::Collapsed);
            lines
                .iter()
                .any(|line| line.spans.iter().any(|span| span.content == glyph))
        };
        model.set_stream_marker(Some("◐"));
        assert!(
            has(&mut model, "◐ "),
            "open assistant carries the sun frame"
        );
        // 换帧必须重渲染（缓存键含帧），不能停留在旧帧。
        model.set_stream_marker(Some("◓"));
        assert!(
            has(&mut model, "◓ "),
            "frame change re-renders the open item"
        );
        // 落定：run 结束后回到常驻 ⏺。
        model.set_stream_marker(None);
        assert!(
            has(&mut model, "⏺ "),
            "settled assistant keeps the static marker"
        );
    }

    #[test]
    fn cache_hits_until_content_or_width_changes() {
        let mut model = ConversationModel::new();
        model.push_user("hello".into());
        model.ensure_rendered(40);
        let first = model.visible_lines(0, 10, 40, ToolCardVisibility::Collapsed);
        assert_eq!(first.len(), 2, "one text row plus the separator");
        model.ensure_rendered(40);
        let second = model.visible_lines(0, 10, 40, ToolCardVisibility::Collapsed);
        assert_eq!(first, second);
        // 新内容置脏：重渲染后总行数增长（total_rows 随 ensure 刷新）。
        model.push_user("again".into());
        model.ensure_rendered(40);
        assert!(model.total_lines(ToolCardVisibility::Collapsed) > 2);
        // 宽度变化使缓存失效但不崩溃。
        let wide = model.visible_lines(0, 50, 80, ToolCardVisibility::Collapsed);
        assert!(!wide.is_empty());
    }

    /// pending steering 区生命周期（docs/todo/steering-visibility-recall.md
    /// INV-SV1/SV2/SV4/SV5）：入队即刻可见（dim + queued 标记，位于
    /// 流式 assistant 之后）；claim 升级 front、落为正式用户项；召回
    /// LIFO；区为空时直灌事件向后兼容；run 结束清区。
    #[test]
    fn pending_steering_zone_lifecycle() {
        let plain = |model: &mut ConversationModel| {
            model
                .visible_lines(0, 40, 60, ToolCardVisibility::Collapsed)
                .iter()
                .map(|line| {
                    line.spans
                        .iter()
                        .map(|span| span.content.as_ref())
                        .collect::<String>()
                })
                .collect::<Vec<_>>()
                .join("\n")
        };
        let mut model = ConversationModel::new();
        model.push_user("start".into());
        model.apply_run_event(&RunEvent::ModelRequested {
            turn: 1,
            provider: "p".into(),
            model: "m".into(),
        });
        model.apply_run_event(&RunEvent::ModelStream {
            turn: 1,
            event: ModelEvent::TextDelta {
                delta: "working".into(),
            },
        });

        // 入队即刻可见（INV-SV1），徽标计数派生自同一区。
        model.push_pending_steering("first".into());
        model.push_pending_steering("second".into());
        assert_eq!(model.pending_steering_count(), 2);
        let joined = plain(&mut model);
        assert!(joined.contains("❯ first · queued"), "{joined:?}");
        assert!(joined.contains("❯ second · queued"), "{joined:?}");

        // LIFO 召回（INV-SV4 区侧半边）。
        assert_eq!(model.recall_pending_steering(), Some("second".to_owned()));
        assert_eq!(model.pending_steering_count(), 1);
        let joined = plain(&mut model);
        assert!(
            !joined.contains("second"),
            "the recalled echo disappears: {joined:?}"
        );

        // claim 升级（INV-SV2）：front 出区、落为正式用户项——无 queued
        // 标记，且位置在 assistant 之后（journal/回放同序）。
        model.apply_run_event(&RunEvent::SteeringApplied {
            text: "first".into(),
        });
        assert_eq!(model.pending_steering_count(), 0);
        let joined = plain(&mut model);
        assert!(
            joined.contains("❯ first") && !joined.contains("queued"),
            "the confirmed message renders as a regular user block: {joined:?}"
        );

        // 区为空时直灌事件（向后兼容：不 panic，直接 push_user）。
        model.apply_run_event(&RunEvent::SteeringApplied {
            text: "direct".into(),
        });
        assert!(plain(&mut model).contains("❯ direct"));

        // 丢弃（INV-SV5）。
        model.push_pending_steering("gone".into());
        assert_eq!(model.discard_pending_steering(), 1);
        assert_eq!(model.pending_steering_count(), 0);
        assert!(!plain(&mut model).contains("gone"));
    }

    /// W1-29/W1-13：陈旧的 `SteeringApplied`（上一 run 收尾窗口送达，
    /// 事件不携带 run 身份）不得弹掉**本** run 的队首 pending——确认按
    /// 文本匹配出区，失配时降级为普通用户项追加，队列不动。pre-fix 红：
    /// 无条件 `pop_front` 会把 "second" 弹掉。
    #[test]
    fn stale_steering_applied_does_not_pop_the_newer_runs_pending_queue() {
        let plain = |model: &mut ConversationModel| {
            model
                .visible_lines(0, 40, 60, ToolCardVisibility::Collapsed)
                .iter()
                .map(|line| {
                    line.spans
                        .iter()
                        .map(|span| span.content.as_ref())
                        .collect::<String>()
                })
                .collect::<Vec<_>>()
                .join("\n")
        };
        let mut model = ConversationModel::default();
        model.push_pending_steering("second".into());
        model.confirm_pending_steering("first".into());
        assert_eq!(
            model.pending_steering_count(),
            1,
            "a mismatched confirmation must leave the queue untouched"
        );
        let joined = plain(&mut model);
        assert!(
            joined.contains("❯ second · queued"),
            "the pending echo survives the stale event: {joined:?}"
        );
        assert!(
            joined.contains("❯ first") && !joined.contains("first · queued"),
            "the stale text still lands as a regular user item (journal 顺序权威): {joined:?}"
        );
    }

    /// INV-SV6：pending 区不进 items——流式 assistant 仍是最后一个
    /// item 且继续续写。
    #[test]
    fn pending_steering_does_not_break_streaming_appends() {
        let mut model = ConversationModel::new();
        model.apply_run_event(&RunEvent::ModelRequested {
            turn: 1,
            provider: "p".into(),
            model: "m".into(),
        });
        model.apply_run_event(&RunEvent::ModelStream {
            turn: 1,
            event: ModelEvent::TextDelta {
                delta: "working".into(),
            },
        });
        model.push_pending_steering("queued".into());
        model.apply_run_event(&RunEvent::ModelStream {
            turn: 1,
            event: ModelEvent::TextDelta {
                delta: " more".into(),
            },
        });
        let joined = model
            .visible_lines(0, 40, 60, ToolCardVisibility::Collapsed)
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|span| span.content.as_ref())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            joined.contains("working more"),
            "the open assistant still appends after the pending zone: {joined:?}"
        );
        assert!(joined.contains("❯ queued · queued"), "{joined:?}");
    }

    /// G4 双面不变式：复制出口无行尾空白；用户块渲染满宽（视觉长条，
    /// 2026-08-19 用户反馈恢复）。
    #[test]
    fn user_block_spans_full_width_but_copies_clean() {
        let mut model = ConversationModel::new();
        model.push_user("hello world".into());
        model.ensure_rendered(30);
        // 视觉：每行总显示宽度 = 内容区满宽（含背景填充）。
        let lines = model.visible_lines(0, 4, 30, ToolCardVisibility::Collapsed);
        let row = &lines[0];
        let total: usize = row
            .spans
            .iter()
            .map(|span| UnicodeWidthStr::width(span.content.as_ref()))
            .sum();
        assert_eq!(total, 30, "user block fills the content width");
        // 复制：row_plain_text（选区复制的唯一出口）无行尾空白。
        for row_index in 0..model.total_lines(ToolCardVisibility::Collapsed) {
            let plain = model.row_plain_text(row_index, 30, ToolCardVisibility::Collapsed);
            assert_eq!(plain.trim_end(), plain, "G4 copy boundary: {plain:?}");
        }
    }

    /// G7（停止原因全覆盖）：每个终态变体（含上游未知 kind 的兜底
    /// Error）都产出非空通知——没有静默的停止。
    #[test]
    fn every_turn_end_reason_has_a_notice() {
        let reasons = [
            ReplayTurnEnd::Completed,
            ReplayTurnEnd::Aborted {
                cause: "user".into(),
            },
            ReplayTurnEnd::Aborted {
                cause: "disposed".into(),
            },
            ReplayTurnEnd::Blocked,
            ReplayTurnEnd::MaxTokens,
            ReplayTurnEnd::Interrupted,
            ReplayTurnEnd::Error {
                message: "boom".into(),
            },
            // 回放适配器对未知 kind 的兜底形态。
            ReplayTurnEnd::Error {
                message: "unsupported turn-end kind: future".into(),
            },
        ];
        for reason in &reasons {
            let text = turn_end_notice_text(reason);
            assert!(!text.trim().is_empty(), "{reason:?} must explain itself");
        }
        // 通知渲染为 dim 单行。
        let mut model = ConversationModel::new();
        model.push_turn_end(turn_end_notice_text(&ReplayTurnEnd::Completed));
        model.ensure_rendered(40);
        // 通知 1 行 + 统一分隔空行。
        assert_eq!(model.total_lines(ToolCardVisibility::Collapsed), 2);
        let line = model.visible_lines(0, 1, 40, ToolCardVisibility::Collapsed);
        assert_eq!(
            line[0]
                .spans
                .iter()
                .map(|s| s.content.as_ref())
                .collect::<String>(),
            "· completed"
        );
    }

    #[test]
    fn zero_row_items_do_not_contribute_separator_rows() {
        let mut model = ConversationModel::new();
        model.push_user("hi".into());
        model.open_tool_card("call-1".into(), "read_file".into(), Value::Null);
        model.settle_tool_card(
            "call-1",
            CardState::Settled {
                output: Value::String("ok".into()),
                is_error: false,
            },
        );
        model.ensure_rendered(40);
        // B5 之前卡片不渲染：行数 = 用户块 1 行 + 分隔 1 行。
        assert_eq!(
            model.total_lines(ToolCardVisibility::Hidden),
            2,
            "hidden cards contribute zero rows (G5)"
        );
        assert!(model.total_lines(ToolCardVisibility::Collapsed) > 2);
        assert!(
            model.total_lines(ToolCardVisibility::Expanded)
                >= model.total_lines(ToolCardVisibility::Collapsed)
        );
    }

    /// G2（live = replay，前端侧）：真实会话经完整 application 跑一遍，
    /// live `RunEvent` 流与 journal 回放分别喂两个模型，item 事实必须
    /// 相等。呈现性差异按下列规则归一（与 core 侧 T1 对拍的规则一致）：
    /// - 拒绝路径 live 记 `Denied{reason}`、journal 只落 isError 结果
    ///   （decided 无理由字段、无被拒参数）→ 双方归一为"错误卡"；
    /// - 工具输出的字符串/JSON 形态差异 → 统一比较文本形式。
    #[derive(Debug, PartialEq)]
    enum Fact {
        User(String),
        Assistant {
            text: String,
            reasoning: Option<String>,
            tool_calls: usize,
        },
        Card {
            call_id: String,
            tool: String,
            errored: bool,
        },
    }

    fn facts(model: &ConversationModel) -> Vec<Fact> {
        model
            .items
            .iter()
            .map(|(item, _)| match item {
                ConversationItem::User { text } => Fact::User(text.clone()),
                ConversationItem::Compaction { text } => Fact::User(format!("<compaction> {text}")),
                ConversationItem::Assistant {
                    text,
                    reasoning,
                    tool_calls,
                    ..
                } => Fact::Assistant {
                    text: text.clone(),
                    reasoning: reasoning.clone(),
                    tool_calls: tool_calls.len(),
                },
                ConversationItem::ToolCard {
                    call_id,
                    tool,
                    state,
                    ..
                } => Fact::Card {
                    call_id: call_id.clone(),
                    tool: tool.clone(),
                    errored: !matches!(
                        state,
                        CardState::Settled {
                            is_error: false,
                            ..
                        }
                    ),
                },
                ConversationItem::TurnEnd { .. } => Fact::User("<turn-end>".into()),
            })
            .collect()
    }

    fn load_replay(storage_root: &std::path::Path) -> Vec<ReplayEvent> {
        let backend = crate::session::persistence::JsonlBackend::new(
            storage_root.join("sessions"),
            crate::session::persistence::JsonlCompression::Zstd,
            false,
        );
        let headers = backend.list_headers().expect("headers");
        let header = headers.first().expect("one session");
        let key = crate::session::key::SessionKey {
            project: crate::session::key::ProjectKey::from_cwd(&header.cwd.clone().expect("cwd")),
            id: header.id.clone(),
        };
        let mut events = Vec::new();
        backend
            .visit_from(&key, 0, &mut |event| {
                events.push(event.clone());
                Ok(())
            })
            .expect("visit");
        crate::session::replay::ReplayAdapter::fold(&events)
    }

    fn assert_frontend_parity(tag: &str, behavior: TestBehavior, deny: bool) {
        let (storage_root, project_root) = roots(tag);
        std::fs::create_dir_all(&project_root).expect("project dir");
        let bootstrap =
            BootstrapApplication::open(Project::new(&project_root), storage_root.clone())
                .expect("bootstrap");
        let mut application = bootstrap
            .authorize_and_mount_with_provider(Arc::new(TestProviderPlugin { behavior }))
            .expect("mount");
        configure_test_model(&application);

        let prompt = "please write the file";
        let live = Arc::new(Mutex::new(Vec::new()));
        let (completion, receiver) = std::sync::mpsc::channel();
        let approver: Arc<dyn crate::PermissionApprover> = if deny {
            Arc::new(
                |_request: crate::PermissionRequest| crate::PermissionDecision::Deny {
                    reason: "not allowed".into(),
                },
            )
        } else {
            Arc::new(|_request: crate::PermissionRequest| crate::PermissionDecision::Allow)
        };
        let handle = application
            .start_run(crate::ApplicationRunRequest {
                attachments: Vec::new(),
                asker: None,
                prompt: prompt.into(),
                approver,
                events: Box::new(SharedEvents(Arc::clone(&live))),
                completion,
            })
            .expect("start");
        handle.join().expect("run joins");
        let result = receiver.recv().expect("completion");
        application.close().expect("close");

        // live：用户消息由提交路径压入（RunEvent 不携带用户输入），
        // 其余事件逐条喂模型。
        let mut live_model = ConversationModel::new();
        live_model.push_user(prompt.into());
        for event in live.lock().expect("live events").iter() {
            live_model.apply_run_event(event);
        }
        live_model.settle_streamed_output("write attempted");
        // 镜像 finish_run 的终态通知（B7/G7：live 与 replay 同源文本）。
        let notice = match &result {
            Ok(done) if done.cancelled => "cancelled".to_owned(),
            Ok(_) => "completed".to_owned(),
            Err(failure) => format!("error: {}", failure.error),
        };
        live_model.push_turn_end(notice);

        let replay_events = load_replay(&storage_root);
        let replay_model = ConversationModel::from_replay(&replay_events);

        assert_eq!(
            facts(&live_model),
            facts(&replay_model),
            "G2: live 与回放必须构造同一组转录事实（{tag}）"
        );
        std::fs::remove_dir_all(storage_root.parent().expect("base")).ok();
    }

    #[test]
    fn live_and_replay_agree_for_an_allowed_tool_run() {
        assert_frontend_parity("conv-parity-allow", TestBehavior::WriteFile, false);
    }

    #[test]
    fn live_and_replay_agree_for_a_denied_tool_run() {
        assert_frontend_parity("conv-parity-deny", TestBehavior::WriteFile, true);
    }

    #[test]
    fn live_and_replay_agree_for_a_failed_run() {
        assert_frontend_parity("conv-parity-fail", TestBehavior::Failure, false);
    }
}
