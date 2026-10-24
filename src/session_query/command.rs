use super::*;
use crate::{
    application::TrustedProjectApplication,
    command::{CommandError, CommandGroup, CommandHandler, CommandOutcome, CommandSpec},
};
pub(crate) fn spec() -> CommandSpec {
    CommandSpec {
        names: vec!["history".into()],
        description: UiMessage::new("history.description").english(),
        takes_args: true,
        group: CommandGroup::Conversation,
        order: 19,
        listed: true,
        handler: Arc::new(Handler),
    }
}
struct Handler;
impl CommandHandler for Handler {
    fn run(
        &self,
        app: &mut TrustedProjectApplication,
        args: &str,
    ) -> Result<CommandOutcome, CommandError> {
        let request = parse_history_command(args).map_err(CommandError::presented)?;
        let _ = app;
        Ok(CommandOutcome::QueryHistory(request))
    }
}
#[derive(Clone, Debug)]
pub enum HistoryCommandRequest {
    Search(HistorySearch),
    Events(HistoryEventsRequest),
    Read(HistoryReadRequest),
}
impl HistoryCommandRequest {
    pub fn method(&self) -> &'static str {
        match self {
            Self::Search(_) => "history.search",
            Self::Events(_) => "history.events",
            Self::Read(_) => "history.event.read",
        }
    }
    pub fn params(&self) -> Value {
        match self {
            Self::Search(v) => json!(v),
            Self::Events(v) => json!(v),
            Self::Read(v) => json!(v),
        }
    }
}
pub fn parse_history_command(args: &str) -> Result<HistoryCommandRequest, UiMessage> {
    match args.trim().split_once(' ') {
        Some((kind @ ("search" | "all"), query)) if !query.trim().is_empty() => {
            Ok(HistoryCommandRequest::Search(HistorySearch {
                query: query.into(),
                trusted_projects: kind == "all",
                ..Default::default()
            }))
        }
        Some(("events", reference)) => Ok(HistoryCommandRequest::Events(HistoryEventsRequest {
            reference: decode_reference(reference)?,
            trusted_projects: true,
            after_seq: None,
            event_type: None,
        })),
        Some(("read", reference)) => Ok(HistoryCommandRequest::Read(HistoryReadRequest {
            reference: decode_reference(reference)?,
            trusted_projects: true,
            offset: 0,
            version: None,
        })),
        _ => Err(UiMessage::new("history.command_help")),
    }
}
fn decode_reference(text: &str) -> Result<HistoryReference, UiMessage> {
    serde_json::from_str(text)
        .map_err(|_| UiMessage::new("history.invalid_reference").arg("detail", ""))
}
