use super::*;
impl Projection {
    pub(super) fn add(
        &mut self,
        event: &SessionEvent,
        turn: Option<u64>,
        kind: &str,
        body: &str,
    ) -> usize {
        let body = crate::redact::redact_secrets(body);
        let id = format!("{kind}:{}", event.seq);
        let row = Record {
            id: id.clone(),
            seq: event.seq,
            turn,
            kind: kind.into(),
            preview: excerpt(&body),
            tool: None,
            status: None,
            time: valid_time(event.time),
            started_at: None,
            ended_at: None,
            estimated: false,
            model: None,
            provider: None,
            source_seqs: vec![event.seq],
            final_answer: false,
        };
        let detail = (self.selected.as_ref() == Some(&id)).then(|| TrajectoryDetail {
            record: row.clone(),
            text: body.clone(),
            arguments: None,
            output: None,
            raw: Vec::new(),
            disclosure: None,
            attachments: Vec::new(),
        });
        if detail.is_some() {
            self.detail_index = Some(self.entries.len());
        }
        self.entries.push(Entry {
            row,
            matches: !self.query.is_empty() && body.to_lowercase().contains(&self.query),
            is_call: false,
            detail,
        });
        self.entries.len() - 1
    }
    pub(super) fn tool_call(
        &mut self,
        event: &SessionEvent,
        turn: Option<u64>,
        call: crate::tool::ToolCall,
    ) {
        let args = super::super::inspect::redact_trajectory_value(&call.arguments);
        let i = self.add(event, turn, "tool", &text(&args));
        let entry = &mut self.entries[i];
        entry.is_call = true;
        entry.row.tool = Some(call.name.clone());
        entry.matches |= !self.query.is_empty() && call.name.to_lowercase().contains(&self.query);
        entry.row.status = Some("unfinished".into());
        entry.row.started_at = valid_time(event.time);
        if let Some(detail) = &mut entry.detail {
            detail.arguments = Some(args);
        }
        self.pending
            .entry((turn, call.id))
            .or_default()
            .push((event.data["step"].as_u64(), i));
    }
    pub(super) fn tool_result(
        &mut self,
        event: &SessionEvent,
        turn: Option<u64>,
        id: String,
        output: Value,
        failed: bool,
    ) -> usize {
        let safe = super::super::inspect::redact_trajectory_value(&output);
        let matched = self
            .pending
            .get_mut(&(turn, id))
            .and_then(|calls| take_call(calls, event.data["step"].as_u64()));
        let i = matched.unwrap_or_else(|| self.add(event, turn, "tool", &text(&safe)));
        let entry = &mut self.entries[i];
        if !entry.row.source_seqs.contains(&event.seq) {
            entry.row.source_seqs.push(event.seq);
        }
        entry.row.status = Some(
            if failed {
                "failed"
            } else if matched.is_some() {
                "succeeded"
            } else {
                "unknown"
            }
            .into(),
        );
        entry.row.ended_at = valid_time(event.time);
        entry.matches |= !self.query.is_empty() && text(&safe).to_lowercase().contains(&self.query);
        if let Some(detail) = &mut entry.detail {
            detail.output = Some(safe);
        }
        i
    }
    pub(super) fn assistant(
        &mut self,
        event: &SessionEvent,
        step: (u64, u64),
        reasoning: Option<String>,
        body: String,
        provider: String,
        model: String,
    ) {
        let (turn, step) = step;
        for (kind, content) in [
            ("reasoning", reasoning.unwrap_or_default()),
            ("message", body),
        ] {
            if content.is_empty() {
                continue;
            }
            let i = self.add(event, (turn > 0).then_some(turn), kind, &content);
            let row = &mut self.entries[i].row;
            row.provider = Some(provider.clone());
            row.model = Some(model.clone());
            row.started_at = self.steps.get(&(turn, step)).copied().and_then(valid_time);
            row.ended_at = valid_time(event.time);
            row.estimated = true;
        }
    }
    pub(super) fn metadata(&mut self, event: &SessionEvent) {
        use crate::session::catalog::{InspectionKind, event_spec};
        let turn = event.data["turn"].as_u64().filter(|t| *t > 0).or(self.turn);
        let step = event.data["step"].as_u64();
        match event_spec(&event.event_type)
            .map(|s| s.inspection)
            .unwrap_or_default()
        {
            InspectionKind::StepStart => {
                if let (Some(t), Some(s)) = (turn, step) {
                    self.steps.insert((t, s), event.time);
                }
            }
            InspectionKind::None => {}
            InspectionKind::Request => {
                let config = &event.data["header"]["config"];
                let i = self.add(event, turn, "request", "");
                self.entries[i].row.provider = config["provider"].as_str().map(str::to_owned);
                self.entries[i].row.model = config["model"].as_str().map(str::to_owned);
            }
            InspectionKind::Approval => {
                let reason = event.data["reason"].as_str().unwrap_or_default();
                let i = self.add(event, turn, "approval", reason);
                self.entries[i].row.tool = event.data["toolName"].as_str().map(str::to_owned);
            }
        }
    }
    pub(super) fn raw(&mut self, event: &SessionEvent) {
        let Some(index) = self.detail_index else {
            return;
        };
        let entry = &mut self.entries[index];
        if !entry.row.source_seqs.contains(&event.seq) {
            return;
        }
        if let Some(detail) = &mut entry.detail {
            let safe = super::super::inspect::redact_trajectory_value(
                &serde_json::to_value(event).expect("event serializes"),
            );
            detail.raw.push(text(&safe));
        }
    }
}

fn take_call(calls: &mut Vec<(Option<u64>, usize)>, step: Option<u64>) -> Option<usize> {
    let index = calls
        .iter()
        .rposition(|(call_step, _)| *call_step == step)
        .or_else(|| match calls.as_slice() {
            [(call_step, _)] if step.is_none() || call_step.is_none() => Some(0),
            _ => None,
        })?;
    Some(calls.remove(index).1)
}
