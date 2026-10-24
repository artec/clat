use super::*;
use crate::{
    application::TrustedProjectApplication,
    command::{CommandError, CommandGroup, CommandHandler, CommandOutcome, CommandSpec},
};
pub(crate) fn spec() -> CommandSpec {
    CommandSpec {
        names: vec!["web".into()],
        description: UiMessage::new("command.web.description").english(),
        takes_args: true,
        group: CommandGroup::Extensions,
        order: 30,
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
        let args: Vec<_> = args.split_whitespace().collect();
        let result = app
            .web_settings()
            .map_err(|e| CommandError::presented(e.presentation()))?;
        if args.is_empty() {
            let key = if result.search_available {
                "webtool.available"
            } else {
                "webtool.not_configured"
            };
            return Ok(CommandOutcome::Status(UiMessage::new(key).into()));
        }
        let mut edit = WebSettingsEdit {
            fetch_provider: result.fetch_provider,
            use_model_key: result.use_model_key,
            api_key: None,
            clear_key: false,
        };
        match args.as_slice() {
            ["fetch", "http"] => edit.fetch_provider = FetchProvider::Http,
            ["fetch", "deepseek"] => edit.fetch_provider = FetchProvider::Deepseek,
            ["credential", "model"] => edit.use_model_key = true,
            ["credential", "dedicated"] => edit.use_model_key = false,
            _ => {
                return Err(CommandError::presented(UiMessage::new(
                    "webtool.command_help",
                )));
            }
        }
        app.save_web_settings(edit)
            .map_err(|e| CommandError::presented(e.presentation()))?;
        Ok(CommandOutcome::Status(
            UiMessage::new("webtool.saved").into(),
        ))
    }
}
