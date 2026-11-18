//! Owned progress shared by steps; terminal construction keeps history and accounting together.
use super::*;
pub(super) struct StepState {
    pub items: Vec<ModelItem>,
    pub usage: Usage,
    pub turn: usize,
    pub repeat: Option<RepeatedCall>,
}

pub(super) struct RepeatedCall {
    name: String,
    arguments: serde_json::Value,
    output: serde_json::Value,
    count: u8,
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
    /// Count a consecutive identical call. Three unchanged results stop the run.
    pub fn repeated_result(
        &mut self,
        name: &str,
        arguments: &serde_json::Value,
        output: &serde_json::Value,
    ) -> bool {
        let same = self.repeat.as_ref().is_some_and(|prior| {
            prior.name == name && prior.arguments == *arguments && prior.output == *output
        });
        if !same {
            self.repeat = Some(RepeatedCall {
                name: name.to_owned(),
                arguments: arguments.clone(),
                output: output.clone(),
                count: 1,
            });
            return false;
        }
        let prior = self.repeat.as_mut().expect("matched repeat");
        prior.count = prior.count.saturating_add(1);
        prior.count >= 3
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
