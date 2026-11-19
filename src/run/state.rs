//! Owned progress shared by steps; terminal construction keeps history and accounting together.
use super::*;
pub(super) struct StepState {
    pub items: Vec<ModelItem>,
    pub usage: Usage,
    pub turn: usize,
    pub repeat: Option<RepeatedCall>,
}

const UNPRODUCTIVE_LIMIT: u8 = 12;
const BARREN: u64 = 0;

pub(super) enum RepeatStop {
    Identical(u8),
    Discovery(u8),
    Unproductive(u8),
}

pub(super) struct RepeatedCall {
    name: String,
    arguments: serde_json::Value,
    output: serde_json::Value,
    count: u8,
    barren: u8,
    seen: Vec<u64>,
}
impl StepState {
    pub fn fail(
        &mut self,
        events: &mut dyn EventSink,
        steering: &SteeringQueue,
        message: impl Into<String>,
    ) -> RunError {
        fail(
            events,
            steering,
            message,
            self.turn,
            self.usage.clone(),
            std::mem::take(&mut self.items),
        )
    }
    pub fn cancel(
        &mut self,
        events: &mut dyn EventSink,
        steering: &SteeringQueue,
        text: String,
    ) -> RunOutput {
        cancelled(
            events,
            steering,
            self.turn,
            &self.usage,
            text,
            std::mem::take(&mut self.items),
        )
    }
    /// Count a consecutive identical call. Returns the stop that was reached.
    ///
    /// Discovery calls stop at three identical arguments. Their replies carry a
    /// fresh observation time, so the catalog can be unchanged while the bytes differ.
    /// Other identical results stop at 41, above the suite's 40-step `list_files`
    /// loop that locks the absence of a turn budget. A changed argument whose
    /// result is empty, an error, or a result this tool already returned stops
    /// after 12 in a row.
    pub fn repeated_result(
        &mut self,
        name: &str,
        arguments: &serde_json::Value,
        output: &serde_json::Value,
    ) -> Option<RepeatStop> {
        let discovery = crate::tool::capabilities::is_discovery(name);
        let same = self.repeat.as_ref().is_some_and(|prior| {
            prior.name == name
                && prior.arguments == *arguments
                && (discovery || prior.output == *output)
        });
        if !same {
            let (barren, seen) = self.carry_barren(name, output);
            self.repeat = Some(RepeatedCall {
                name: name.to_owned(),
                arguments: arguments.clone(),
                output: output.clone(),
                count: 1,
                barren,
                seen,
            });
            return (barren >= UNPRODUCTIVE_LIMIT)
                .then_some(RepeatStop::Unproductive(UNPRODUCTIVE_LIMIT));
        }
        let prior = self.repeat.as_mut().expect("matched repeat");
        prior.count = prior.count.saturating_add(1);
        let limit = if discovery { 3 } else { 41 };
        (prior.count >= limit).then_some(if discovery {
            RepeatStop::Discovery(limit)
        } else {
            RepeatStop::Identical(limit)
        })
    }

    fn carry_barren(&self, name: &str, output: &serde_json::Value) -> (u8, Vec<u64>) {
        let fp = fingerprint(output);
        let Some(prior) = self.repeat.as_ref().filter(|prior| prior.name == name) else {
            return (u8::from(fp == BARREN), vec![fp]);
        };
        if prior.seen.contains(&fp) {
            (prior.barren.saturating_add(1), prior.seen.clone())
        } else {
            let mut seen = prior.seen.clone();
            seen.push(fp);
            (0, seen)
        }
    }

    pub fn complete(&mut self, events: &mut dyn EventSink, text: String) -> RunOutput {
        events.emit(RunEvent::RunCompleted {
            output: text.clone(),
            turns: self.turn,
            usage: self.usage.clone(),
        });
        RunOutput {
            text,
            turns: self.turn,
            items: std::mem::take(&mut self.items),
            usage: self.usage.clone(),
            cancelled: false,
        }
    }
}

fn is_barren(output: &serde_json::Value) -> bool {
    output.get("error").is_some()
        || output
            .get("total_matches")
            .and_then(serde_json::Value::as_u64)
            == Some(0)
        || output
            .get("matches")
            .and_then(serde_json::Value::as_array)
            .is_some_and(Vec::is_empty)
        || output
            .get("entries")
            .and_then(serde_json::Value::as_array)
            .is_some_and(Vec::is_empty)
}

fn fingerprint(output: &serde_json::Value) -> u64 {
    use std::hash::Hasher;
    if is_barren(output) {
        return BARREN;
    }
    let text = serde_json::to_string(output).unwrap_or_default();
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    std::hash::Hash::hash(&text, &mut hasher);
    match hasher.finish() {
        BARREN => 1,
        hash => hash,
    }
}
