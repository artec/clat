//! Read-only turn navigation from replay facts already owned by the transcript.
use super::*;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct RoundSummary {
    pub turn: Option<u64>,
    pub identity: u64,
    pub question: String,
    pub answer: String,
}

/// Bounds are based only on durable replay facts. A tail page's first turn can
/// begin with a steering message, so it is never assumed complete until an
/// earlier page proves the boundary.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct RoundProjection {
    pub turn: u64,
    pub start: usize,
    pub end: usize,
    pub final_answer: Option<usize>,
    pub complete: bool,
    pub estimated_duration_ms: Option<u64>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct RoundHeader {
    elapsed_ms: Option<u64>,
    expanded: bool,
    running: bool,
    estimated: bool,
}

pub(super) struct LiveRound {
    start: u64,
    end: Option<u64>,
    started: std::time::Instant,
    elapsed_ms: Option<u64>,
    expanded: bool,
}

impl RoundHeader {
    pub(super) fn line(&self, locale: LocaleContext) -> Line<'static> {
        let text = self
            .elapsed_ms
            .map(|elapsed| {
                let seconds = elapsed / 1000;
                let duration = UiMessage::new("round.duration")
                    .arg("minutes", seconds / 60)
                    .arg("seconds", seconds % 60);
                let key = if self.running {
                    "round.processing"
                } else if self.estimated {
                    "round.elapsed_estimated"
                } else {
                    "round.elapsed"
                };
                locale.render(&UiMessage::new(key).arg_message("duration", duration))
            })
            .unwrap_or_else(|| locale.text("round.unknown").to_owned());
        Line::from(Span::styled(
            format!("{text} {}", if self.expanded { "▾" } else { "▸" }),
            theme::style(theme::Role::Dim),
        ))
    }
}

impl ConversationModel {
    /// DSH supplies durable turn ids on replay events. Never infer an active
    /// round from an unrelated or metadata-free last user message.
    pub(crate) fn begin_live_replay_round(&mut self, turn: u64) -> bool {
        if turn == 0
            || !self.items.last().is_some_and(|(item, cache)| {
                matches!(item, ConversationItem::User { .. }) && cache.replay_turn == Some(turn)
            })
        {
            return false;
        }
        self.begin_live_round();
        true
    }

    pub(crate) fn begin_live_round(&mut self) {
        if self
            .live_rounds
            .last()
            .is_some_and(|round| round.end.is_none() && round.elapsed_ms.is_none())
        {
            return;
        }
        let Some((ConversationItem::User { .. }, cache)) = self.items.last() else {
            return;
        };
        self.live_rounds.push(LiveRound {
            start: cache.identity,
            end: None,
            started: std::time::Instant::now(),
            elapsed_ms: None,
            expanded: true,
        });
    }

    pub(crate) fn finish_live_round(&mut self) {
        let Some(end) = self.items.last().map(|(_, cache)| cache.identity) else {
            return;
        };
        let Some(round) = self
            .live_rounds
            .last_mut()
            .filter(|round| round.end.is_none() && round.elapsed_ms.is_none())
        else {
            return;
        };
        round.elapsed_ms = Some(
            round
                .started
                .elapsed()
                .as_millis()
                .min(u128::from(u64::MAX)) as u64,
        );
        round.end = Some(end);
        round.expanded = false;
    }

    pub(crate) fn live_clock_running(&self) -> bool {
        self.live_rounds
            .last()
            .is_some_and(|round| round.end.is_none() && round.elapsed_ms.is_none())
    }

    pub(crate) fn stop_live_clock(&mut self) {
        if let Some(round) = self
            .live_rounds
            .last_mut()
            .filter(|round| round.end.is_none() && round.elapsed_ms.is_none())
        {
            round.elapsed_ms = Some(
                round
                    .started
                    .elapsed()
                    .as_millis()
                    .min(u128::from(u64::MAX)) as u64,
            );
        }
    }

    pub(crate) fn set_live_host_started(&mut self, started_ms: u64) {
        let Some(round) = self
            .live_rounds
            .last_mut()
            .filter(|round| round.end.is_none() && round.elapsed_ms.is_none())
        else {
            return;
        };
        let Ok(now) = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH) else {
            return;
        };
        let now_ms = now.as_millis().min(u128::from(u64::MAX)) as u64;
        let Some(elapsed) = now_ms.checked_sub(started_ms) else {
            return;
        };
        if let Some(start) =
            std::time::Instant::now().checked_sub(std::time::Duration::from_millis(elapsed))
        {
            round.started = start;
        }
    }

    pub(crate) fn row_anchor(
        &self,
        target: usize,
        visibility: ToolCardVisibility,
    ) -> Option<(u64, usize)> {
        let mut row = 0;
        for (item, cache) in &self.items {
            let count = item_row_count(item, cache, visibility);
            if count > 0 && target >= row && target <= row + count {
                return Some((cache.identity, target - row));
            }
            row += count + usize::from(count > 0);
        }
        None
    }

    pub(crate) fn row_for_anchor(
        &self,
        anchor: (u64, usize),
        visibility: ToolCardVisibility,
    ) -> Option<usize> {
        let mut row = 0;
        for (item, cache) in &self.items {
            let count = item_row_count(item, cache, visibility);
            if cache.identity == anchor.0 && count > 0 {
                return Some(row + anchor.1.min(count));
            }
            row += count + usize::from(count > 0);
        }
        None
    }

    pub(crate) fn set_rounds_context(&mut self, has_more: bool) {
        self.round_history_more = Some(has_more);
    }

    pub(super) fn refresh_replay_rounds(&mut self) {
        let Some(has_more) = self.round_history_more else {
            return;
        };
        let projections = self.replay_round_projections(has_more);
        let mut desired = vec![(false, false, None); self.items.len()];
        for round in projections.iter().filter(|round| round.complete) {
            let Some(key) = self.items[round.start].1.replay_seq else {
                continue;
            };
            let expanded = self.round_expanded.get(&key).copied().unwrap_or(false);
            desired[round.start].2 = Some(RoundHeader {
                elapsed_ms: round.estimated_duration_ms,
                expanded,
                running: false,
                estimated: true,
            });
            if expanded {
                continue;
            }
            for (index, flags) in desired
                .iter_mut()
                .enumerate()
                .take(round.end)
                .skip(round.start + 1)
            {
                match &self.items[index].0 {
                    ConversationItem::Assistant { reasoning, .. }
                        if Some(index) == round.final_answer =>
                    {
                        flags.1 = reasoning
                            .as_ref()
                            .is_some_and(|value| !value.trim().is_empty())
                    }
                    ConversationItem::Assistant { .. } => flags.0 = true,
                    ConversationItem::ToolCard {
                        tool,
                        state:
                            CardState::Settled {
                                is_error: false, ..
                            },
                        ..
                    } if !matches!(tool.as_str(), "ask_user" | "exit_plan_mode") => flags.0 = true,
                    ConversationItem::ProcessNotice { foldable: true, .. } => flags.0 = true,
                    _ => {}
                }
            }
        }
        self.apply_live_rounds(&mut desired);
        for ((_, cache), (hidden, hide_reasoning, header)) in self.items.iter_mut().zip(desired) {
            if cache.round_hidden != hidden
                || cache.round_hide_reasoning != hide_reasoning
                || cache.round_header != header
            {
                cache.round_hidden = hidden;
                cache.round_hide_reasoning = hide_reasoning;
                cache.round_header = header;
                cache.dirty = true;
            }
        }
    }

    fn apply_live_rounds(&self, desired: &mut [(bool, bool, Option<RoundHeader>)]) {
        for round in &self.live_rounds {
            let Some(start) = self
                .items
                .iter()
                .position(|(_, cache)| cache.identity == round.start)
            else {
                continue;
            };
            let end = round
                .end
                .and_then(|id| {
                    self.items
                        .iter()
                        .position(|(_, cache)| cache.identity == id)
                        .map(|index| index + 1)
                })
                .unwrap_or(self.items.len());
            let elapsed = round
                .elapsed_ms
                .unwrap_or_else(|| round.started.elapsed().as_secs().saturating_mul(1000));
            desired[start].2 = Some(RoundHeader {
                elapsed_ms: Some(elapsed),
                expanded: round.expanded,
                running: round.end.is_none(),
                estimated: false,
            });
            // Live and replay facts can refer to the same DSH turn. Its live
            // expansion choice owns the whole range, including when reopening.
            for flags in &mut desired[start + 1..end] {
                flags.0 = false;
                flags.1 = false;
            }
            if round.end.is_some() && !round.expanded {
                let final_answer = (start + 1..end).rev().find(|&index| matches!(&self.items[index].0, ConversationItem::Assistant { text, .. } if !text.trim().is_empty()));
                for (offset, (flags, item)) in desired[start + 1..end]
                    .iter_mut()
                    .zip(&self.items[start + 1..end])
                    .enumerate()
                {
                    let index = start + 1 + offset;
                    match &item.0 {
                        ConversationItem::Assistant { reasoning, .. }
                            if Some(index) == final_answer =>
                        {
                            flags.1 = reasoning
                                .as_ref()
                                .is_some_and(|value| !value.trim().is_empty())
                        }
                        ConversationItem::Assistant { .. } => flags.0 = true,
                        ConversationItem::ToolCard {
                            tool,
                            state:
                                CardState::Settled {
                                    is_error: false, ..
                                },
                            ..
                        } if !matches!(tool.as_str(), "ask_user" | "exit_plan_mode") => {
                            flags.0 = true
                        }
                        ConversationItem::ProcessNotice { foldable: true, .. } => flags.0 = true,
                        _ => {}
                    }
                }
            }
        }
    }

    pub(crate) fn expand_round_for_identity(&mut self, identity: u64) -> bool {
        self.expand_round_identity(identity, false)
    }

    pub(crate) fn preserve_round_for_identity(&mut self, identity: u64) -> bool {
        self.expand_round_identity(identity, true)
    }

    fn expand_round_identity(&mut self, identity: u64, include_reasoning: bool) -> bool {
        let Some(index) = self.items.iter().position(|(_, cache)| {
            cache.identity == identity
                && (cache.round_hidden || include_reasoning && cache.round_hide_reasoning)
        }) else {
            return false;
        };
        for round in &mut self.live_rounds {
            let Some(start) = self
                .items
                .iter()
                .position(|(_, cache)| cache.identity == round.start)
            else {
                continue;
            };
            let Some(end) = round.end.and_then(|id| {
                self.items
                    .iter()
                    .position(|(_, cache)| cache.identity == id)
            }) else {
                continue;
            };
            if (start..=end).contains(&index) {
                round.expanded = true;
                return true;
            }
        }
        let Some(has_more) = self.round_history_more else {
            return false;
        };
        let Some(round) = self
            .replay_round_projections(has_more)
            .into_iter()
            .find(|round| round.complete && (round.start..round.end).contains(&index))
        else {
            return false;
        };
        let Some(key) = self.items[round.start].1.replay_seq else {
            return false;
        };
        self.round_expanded.insert(key, true);
        true
    }

    pub(crate) fn toggle_replay_round_at(
        &mut self,
        start: usize,
        end: usize,
        visibility: ToolCardVisibility,
    ) -> Option<usize> {
        let rounds = self.replay_round_projections(self.round_history_more?);
        let mut prefix = Vec::with_capacity(self.items.len() + 1);
        prefix.push(0usize);
        for (item, cache) in &self.items {
            let count = item_row_count(item, cache, visibility);
            prefix.push(prefix.last().copied().unwrap() + count + usize::from(count > 0));
        }
        for round in rounds {
            if self
                .live_rounds
                .iter()
                .any(|live| live.start == self.items[round.start].1.identity)
            {
                continue;
            }
            let row = prefix[round.start];
            let count = prefix[round.end] - row;
            if round.complete && row < end && row + count > start {
                let cache = &self.items[round.start].1;
                let key = cache.replay_seq?;
                let anchor = row + cache.lines.len().saturating_sub(1);
                let expanded = self.round_expanded.get(&key).copied().unwrap_or(false);
                self.round_expanded.insert(key, !expanded);
                return Some(anchor);
            }
        }
        for round in &mut self.live_rounds {
            let Some(first) = self
                .items
                .iter()
                .position(|(_, cache)| cache.identity == round.start)
            else {
                continue;
            };
            let Some(last) = round.end.and_then(|id| {
                self.items
                    .iter()
                    .position(|(_, cache)| cache.identity == id)
            }) else {
                continue;
            };
            let row = prefix[first];
            if row < end && prefix[last + 1] > start {
                let anchor = row + self.items[first].1.lines.len().saturating_sub(1);
                round.expanded = !round.expanded;
                return Some(anchor);
            }
        }
        None
    }
    pub(super) fn replay_round_projections(&self, has_more: bool) -> Vec<RoundProjection> {
        let mut rounds = Vec::new();
        let mut index = 0;
        let mut first_known = true;
        while index < self.items.len() {
            let Some(turn) = self.items[index].1.replay_turn else {
                index += 1;
                continue;
            };
            let start = index;
            while index < self.items.len() && self.items[index].1.replay_turn == Some(turn) {
                index += 1;
            }
            let end = index;
            let items = &self.items[start..end];
            let begins_with_user = matches!(
                items.first().map(|(item, _)| item),
                Some(ConversationItem::User { .. })
            );
            let terminal = items.iter().rposition(|(item, cache)| {
                matches!(item, ConversationItem::TurnEnd { .. }) && cache.replay_seq.is_some()
            });
            let response_end = terminal.map_or(items.len(), |position| position + 1);
            let final_answer = items[..response_end].iter().rposition(|(item, _)| matches!(item, ConversationItem::Assistant { text, .. } if !text.trim().is_empty())).map(|i| start + i);
            let complete = begins_with_user && terminal.is_some() && !(has_more && first_known);
            let estimated_duration_ms = terminal.filter(|_| complete).and_then(|last| {
                let start_time = items.first()?.1.replay_time_ms?;
                let end_time = items[last].1.replay_time_ms?;
                u64::try_from(end_time.checked_sub(start_time)?).ok()
            });
            if begins_with_user {
                rounds.push(RoundProjection {
                    turn,
                    start,
                    end,
                    final_answer,
                    complete,
                    estimated_duration_ms,
                });
            }
            first_known = false;
        }
        rounds
    }
    pub(crate) fn round_summaries(
        &mut self,
        width: usize,
        _visibility: ToolCardVisibility,
    ) -> Vec<RoundSummary> {
        self.ensure_rendered(width);
        let projections = self.replay_round_projections(false);
        let mut next = projections.iter().peekable();
        let mut result: Vec<RoundSummary> = Vec::new();
        for (index, (item, cache)) in self.items.iter().enumerate() {
            if next.peek().is_some_and(|round| round.start == index) {
                let round = next.next().expect("peeked round");
                if let ConversationItem::User { text } = item
                    && !text.trim().is_empty()
                    && !self
                        .live_rounds
                        .iter()
                        .any(|live| live.start == cache.identity)
                {
                    let answer = round
                        .final_answer
                        .and_then(|i| match &self.items[i].0 {
                            ConversationItem::Assistant { text, .. } => Some(text.clone()),
                            _ => None,
                        })
                        .unwrap_or_default();
                    result.push(RoundSummary {
                        turn: Some(round.turn),
                        identity: cache.identity,
                        question: text.clone(),
                        answer,
                    });
                }
            }
            if let Some(round) = self
                .live_rounds
                .iter()
                .find(|round| round.start == cache.identity)
                && let ConversationItem::User { text } = item
            {
                let end = round
                    .end
                    .and_then(|id| {
                        self.items
                            .iter()
                            .position(|(_, cache)| cache.identity == id)
                            .map(|i| i + 1)
                    })
                    .unwrap_or(self.items.len());
                let answer = self.items[index + 1..end]
                    .iter()
                    .rev()
                    .find_map(|(item, _)| match item {
                        ConversationItem::Assistant { text, .. } if !text.trim().is_empty() => {
                            Some(text.clone())
                        }
                        _ => None,
                    })
                    .unwrap_or_default();
                result.push(RoundSummary {
                    turn: None,
                    identity: cache.identity,
                    question: text.clone(),
                    answer,
                });
            }
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn user(seq: u64, turn: u64, text: &str) -> ReplayEvent {
        ReplayEvent::UserMessage {
            seq,
            turn,
            time_ms: seq as i64,
            text: text.into(),
            content_blocks: vec![],
            client_message_id: None,
            receipt: None,
        }
    }

    fn answer(seq: u64, turn: u64, text: &str) -> ReplayEvent {
        ReplayEvent::AssistantMessage {
            seq,
            turn,
            step: 1,
            time_ms: seq as i64,
            reasoning: None,
            text: text.into(),
            tool_calls: vec![],
            provider: String::new(),
            model: String::new(),
            replay_state: None,
        }
    }

    #[test]
    fn live_round_folds_only_after_real_settlement_and_can_be_reopened() {
        let mut model = ConversationModel::default();
        model.push_user("question".into());
        model.begin_live_round();
        model.live_rounds[0].started =
            std::time::Instant::now() - std::time::Duration::from_secs(3);
        model.push_assistant_for_test("intermediate");
        model.push_user("steering".into());
        model.push_assistant_for_test("final answer");
        model.set_rounds_context(false);
        model.ensure_rendered(80);
        assert!(model.items[0].1.round_header.as_ref().unwrap().running);
        assert!(!model.items[1].1.round_hidden);
        let summaries = model.round_summaries(80, ToolCardVisibility::Collapsed);
        assert_eq!(summaries.len(), 1);
        assert_eq!(summaries[0].question, "question");
        assert_eq!(summaries[0].answer, "final answer");
        model.push_turn_end("completed".into());
        model.finish_live_round();
        model.ensure_rendered(80);
        let header = model.items[0].1.round_header.as_ref().unwrap();
        assert!(!header.running);
        assert!(header.elapsed_ms.unwrap() >= 3000);
        assert!(model.items[1].1.round_hidden);
        assert!(!model.items[2].1.round_hidden);
        assert!(!model.items[3].1.round_hidden);
        let identity = model.items[1].1.identity;
        assert!(model.expand_round_for_identity(identity));
        model.ensure_rendered(80);
        assert!(!model.items[1].1.round_hidden);
    }

    #[test]
    fn replay_backed_live_round_has_one_navigation_entry_and_one_fold_state() {
        let mut model = ConversationModel::from_replay(&[user(1, 3, "question")]);
        model.set_rounds_context(false);
        assert!(!model.begin_live_replay_round(2));
        assert!(model.begin_live_replay_round(3));
        model.apply_replay(&[answer(2, 3, "working"), answer(3, 3, "answer")]);
        model.ensure_rendered(80);
        assert!(model.items[0].1.round_header.as_ref().unwrap().running);
        model.apply_replay(&[ReplayEvent::TurnEnded {
            seq: 4,
            turn: 3,
            time_ms: 4,
            reason: crate::session::replay::ReplayTurnEnd::Completed,
        }]);
        model.finish_live_round();
        model.ensure_rendered(80);
        assert_eq!(
            model
                .round_summaries(80, ToolCardVisibility::Collapsed)
                .len(),
            1
        );
        assert!(model.items[1].1.round_hidden);
        assert!(
            model
                .toggle_replay_round_at(0, usize::MAX, ToolCardVisibility::Collapsed)
                .is_some()
        );
        model.ensure_rendered(80);
        assert!(!model.items[1].1.round_hidden);
    }

    #[test]
    fn disconnected_live_round_stops_clock_without_inventing_terminal() {
        let mut model = ConversationModel::default();
        model.push_user("question".into());
        model.begin_live_round();
        model.push_assistant_for_test("working");
        model.stop_live_clock();
        assert!(!model.live_clock_running());
        model.set_rounds_context(false);
        model.ensure_rendered(80);
        assert!(model.items[0].1.round_header.as_ref().unwrap().running);
        assert!(!model.items[1].1.round_hidden);
    }

    #[test]
    fn round_navigation_uses_replay_identity_and_ignores_steering() {
        let events = [
            user(1, 1, "first"),
            answer(2, 1, "intermediate"),
            user(3, 1, "steering"),
            answer(4, 1, "final"),
            user(5, 2, "second"),
            answer(6, 2, "answer"),
        ];
        let mut model = ConversationModel::from_replay(&events);
        let rounds = model.round_summaries(80, ToolCardVisibility::Collapsed);
        assert_eq!(rounds.len(), 2);
        assert_eq!(rounds[0].question, "first");
        assert_eq!(rounds[0].answer, "final");
        assert_eq!(rounds[1].question, "second");
        assert!(
            model.row_for_anchor((rounds[1].identity, 0), ToolCardVisibility::Collapsed)
                > model.row_for_anchor((rounds[0].identity, 0), ToolCardVisibility::Collapsed)
        );
    }

    #[test]
    fn unknown_turns_and_orphaned_answers_are_not_invented() {
        let events = [
            user(1, 0, "unknown"),
            answer(2, 2, "orphaned answer"),
            user(3, 3, "known question"),
            answer(4, 3, "known answer"),
        ];
        let mut model = ConversationModel::from_replay(&events);
        let rounds = model.round_summaries(80, ToolCardVisibility::Collapsed);
        assert_eq!(rounds.len(), 1);
        assert_eq!(rounds[0].turn, Some(3));
        assert_eq!(rounds[0].question, "known question");
    }

    #[test]
    fn replay_boundaries_require_terminal_and_do_not_fold_a_partial_first_page() {
        let end = |seq, turn, time_ms| ReplayEvent::TurnEnded {
            seq,
            turn,
            time_ms,
            reason: crate::session::replay::ReplayTurnEnd::Completed,
        };
        let events = [
            user(1, 1, "question"),
            answer(2, 1, "draft"),
            user(3, 1, "steer"),
            answer(4, 1, "final"),
            end(5, 1, 5001),
            user(6, 2, "running"),
            answer(7, 2, "partial"),
        ];
        let model = ConversationModel::from_replay(&events);
        let rounds = model.replay_round_projections(false);
        assert_eq!(rounds.len(), 2);
        assert!(rounds[0].complete);
        assert_eq!(rounds[0].estimated_duration_ms, Some(5000));
        assert_eq!(rounds[0].final_answer, Some(3));
        assert!(!rounds[1].complete);
        let partial = &model.replay_round_projections(true)[0];
        assert!(!partial.complete);
        assert_eq!(partial.estimated_duration_ms, None);
    }

    #[test]
    fn missing_or_reversed_timestamps_never_invent_duration() {
        let end = ReplayEvent::TurnEnded {
            seq: 3,
            turn: 1,
            time_ms: 0,
            reason: crate::session::replay::ReplayTurnEnd::Completed,
        };
        let model =
            ConversationModel::from_replay(&[user(1, 1, "question"), answer(2, 1, "answer"), end]);
        let round = &model.replay_round_projections(false)[0];
        assert!(round.complete);
        assert_eq!(round.estimated_duration_ms, None);
        let end = ReplayEvent::TurnEnded {
            seq: 4,
            turn: 1,
            time_ms: 1,
            reason: crate::session::replay::ReplayTurnEnd::Completed,
        };
        let mut question = user(1, 1, "question");
        if let ReplayEvent::UserMessage { time_ms, .. } = &mut question {
            *time_ms = 200;
        }
        let model = ConversationModel::from_replay(&[
            question,
            answer(2, 1, "answer"),
            end,
            answer(5, 1, "late"),
        ]);
        let round = &model.replay_round_projections(false)[0];
        assert_eq!(round.estimated_duration_ms, None);
        assert_eq!(round.final_answer, Some(1));
    }

    #[test]
    fn folded_turn_keeps_approval_and_question_but_hides_retries() {
        use crate::session::replay::ReplayRetryFailure;
        let events = vec![
            user(1, 1, "question"),
            ReplayEvent::PermissionChecked {
                seq: 2,
                turn: 1,
                time_ms: 2,
                tool: "write_file".into(),
                decision: crate::PermissionDecision::Deny {
                    reason: "user declined".into(),
                },
            },
            ReplayEvent::RetryScheduled {
                seq: 3,
                turn: 1,
                step: 1,
                time_ms: 3,
                retry: 1,
                max_retries: 2,
                delay_ms: 10,
                failure: ReplayRetryFailure {
                    message: "temporary".into(),
                    code: "transport".into(),
                    provider_retry_after_ms: None,
                },
            },
            ReplayEvent::ToolRequested {
                seq: 4,
                turn: 1,
                time_ms: 4,
                call: crate::ToolCall {
                    id: "question-call".into(),
                    name: "ask_user".into(),
                    arguments: serde_json::json!({"question":"Which choice?"}),
                },
            },
            ReplayEvent::ToolFinished {
                seq: 5,
                turn: 1,
                time_ms: 5,
                call_id: "question-call".into(),
                tool: "ask_user".into(),
                output: serde_json::json!({"answer":"yes"}),
                is_error: false,
                content_blocks: vec![],
            },
            answer(6, 1, "final"),
            ReplayEvent::TurnEnded {
                seq: 7,
                turn: 1,
                time_ms: 7,
                reason: crate::session::replay::ReplayTurnEnd::Completed,
            },
        ];
        let mut model = ConversationModel::from_replay(&events);
        model.set_rounds_context(false);
        model.ensure_rendered(80);
        let permission = model
            .items
            .iter()
            .find(|(_, cache)| cache.replay_seq == Some(2))
            .unwrap();
        assert!(!permission.1.round_hidden);
        let retry = model
            .items
            .iter()
            .find(|(_, cache)| cache.replay_seq == Some(3))
            .unwrap();
        assert!(retry.1.round_hidden);
        let question = model
            .items
            .iter()
            .find(|(_, cache)| cache.replay_seq == Some(4))
            .unwrap();
        assert!(!question.1.round_hidden);
        assert!(item_row_count(&question.0, &question.1, ToolCardVisibility::Hidden) > 0);
        let id = retry.1.identity;
        assert!(model.expand_round_for_identity(id));
        model.ensure_rendered(80);
        assert!(
            !model
                .items
                .iter()
                .find(|(_, cache)| cache.identity == id)
                .unwrap()
                .1
                .round_hidden
        );
    }

    #[test]
    fn completed_replay_folds_process_but_keeps_steering_and_answer_searchable() {
        let end = ReplayEvent::TurnEnded {
            seq: 5,
            turn: 1,
            time_ms: 5000,
            reason: crate::session::replay::ReplayTurnEnd::Error {
                message: "failed".into(),
            },
        };
        let mut final_answer = answer(4, 1, "final");
        if let ReplayEvent::AssistantMessage { reasoning, .. } = &mut final_answer {
            *reasoning = Some("private reasoning".into());
        }
        let mut model = ConversationModel::from_replay(&[
            user(1, 1, "question"),
            answer(2, 1, "draft needle"),
            user(3, 1, "steering"),
            final_answer,
            end,
        ]);
        model.set_rounds_context(false);
        model.ensure_rendered(80);
        assert!(model.items[1].1.round_hidden);
        assert!(!model.items[2].1.round_hidden);
        assert!(!model.items[3].1.round_hidden);
        assert!(model.items[3].1.round_hide_reasoning);
        assert!(!model.items[4].1.round_hidden);
        let hits = model.find_body("needle", 80, ToolCardVisibility::Collapsed);
        assert_eq!(hits.len(), 1);
        assert!(model.expand_round_for_identity(hits[0].identity));
        model.ensure_rendered(80);
        assert!(!model.items[1].1.round_hidden);
        assert!(!model.items[3].1.round_hide_reasoning);
        assert!(
            model
                .toggle_replay_round_at(0, usize::MAX, ToolCardVisibility::Collapsed)
                .is_some()
        );
        model.ensure_rendered(80);
        assert!(model.items[1].1.round_hidden);
    }

    #[test]
    fn older_page_keeps_item_anchor_and_reveals_newly_foldable_process() {
        let end = ReplayEvent::TurnEnded {
            seq: 15,
            turn: 1,
            time_ms: 15,
            reason: crate::session::replay::ReplayTurnEnd::Completed,
        };
        let mut model = ConversationModel::from_replay(&[
            user(10, 1, "steer"),
            answer(11, 1, "reading this"),
            answer(12, 1, "final"),
            end,
        ]);
        model.set_rounds_context(true);
        model.ensure_rendered(40);
        let identity = model.items[1].1.identity;
        let anchor = (identity, 0);
        assert!(
            model
                .row_for_anchor(anchor, ToolCardVisibility::Collapsed)
                .is_some()
        );
        model.prepend_replay(&[user(1, 1, "original"), answer(2, 1, "earlier")]);
        model.set_rounds_context(false);
        model.ensure_rendered(40);
        assert!(
            model
                .row_for_anchor(anchor, ToolCardVisibility::Collapsed)
                .is_none()
        );
        assert!(model.expand_round_for_identity(identity));
        model.ensure_rendered(40);
        let row = model
            .row_for_anchor(anchor, ToolCardVisibility::Collapsed)
            .unwrap();
        assert!(
            model
                .row_plain_text(row, 40, ToolCardVisibility::Collapsed)
                .contains("reading this")
        );
    }

    #[test]
    fn failed_tool_result_remains_visible_even_when_tools_are_hidden() {
        let events = [
            user(1, 1, "question"),
            ReplayEvent::ToolRequested {
                seq: 2,
                turn: 1,
                time_ms: 2,
                call: ToolCall {
                    id: "call".into(),
                    name: "run_command".into(),
                    arguments: serde_json::json!({}),
                },
            },
            ReplayEvent::ToolFinished {
                seq: 3,
                turn: 1,
                time_ms: 3,
                call_id: "call".into(),
                tool: "run_command".into(),
                output: serde_json::json!("diagnostic"),
                is_error: true,
                content_blocks: vec![],
            },
            answer(4, 1, "answer"),
            ReplayEvent::TurnEnded {
                seq: 5,
                turn: 1,
                time_ms: 5,
                reason: crate::session::replay::ReplayTurnEnd::Completed,
            },
        ];
        let mut model = ConversationModel::from_replay(&events);
        model.set_rounds_context(false);
        model.ensure_rendered(80);
        let (item, cache) = &model.items[1];
        assert!(!cache.round_hidden);
        assert!(item_row_count(item, cache, ToolCardVisibility::Hidden) > 0);
    }
}
