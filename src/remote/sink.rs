use super::projection::{RemoteProjection, project_run_event};
use crate::event::{EventSink, RunEvent};
use crate::i18n::UiMessage;

/// Projects runtime events while the host and channel own their I/O.
pub(crate) struct RemoteRunSink {
    pub(crate) fanout: Box<dyn Fn(&RunEvent) + Send>,
    pub(crate) text: Box<dyn Fn(String) + Send>,
    pub(crate) notice: Box<dyn Fn(UiMessage) + Send>,
    pub(crate) typing: Box<dyn Fn(bool) + Send>,
}

impl EventSink for RemoteRunSink {
    fn emit(&mut self, event: RunEvent) {
        (self.fanout)(&event);
        for projection in project_run_event(&event) {
            match projection {
                RemoteProjection::Typing(active) => (self.typing)(active),
                RemoteProjection::Text(text) => (self.text)(text),
                RemoteProjection::Notice(message) => (self.notice)(message),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::i18n::{Locale, LocaleContext};
    use std::sync::{Arc, Mutex};

    #[test]
    fn event_fanout_and_remote_projection_share_the_same_input() {
        let events = Arc::new(Mutex::new(Vec::new()));
        let messages = Arc::new(Mutex::new(Vec::new()));
        let observed_events = Arc::clone(&events);
        let observed_messages = Arc::clone(&messages);
        let observed_notices = Arc::clone(&messages);
        let mut sink = RemoteRunSink {
            fanout: Box::new(move |event| {
                observed_events
                    .lock()
                    .unwrap()
                    .push(matches!(event, RunEvent::ToolStarted { .. }))
            }),
            text: Box::new(move |message| observed_messages.lock().unwrap().push(message)),
            notice: Box::new(move |message| {
                observed_notices
                    .lock()
                    .unwrap()
                    .push(LocaleContext::new(Locale::Simplified).render(&message))
            }),
            typing: Box::new(|_| {}),
        };
        sink.emit(RunEvent::ToolStarted {
            call_id: "call".into(),
            tool: "run_command".into(),
        });
        assert_eq!(*events.lock().unwrap(), vec![true]);
        let messages = messages.lock().unwrap();
        assert_eq!(messages.len(), 1);
        assert!(!messages[0].contains("run_command"));
    }
}
