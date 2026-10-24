//! Shared terminal parser. JSON carries complete sentences and explicit budgets.
use super::*;
use crate::command::{CommandError, CommandGroup, CommandHandler, CommandOutcome, CommandSpec};
use serde_json::{Value, json};
#[derive(Clone, Debug)]
pub struct ScheduleCommand {
    method: &'static str,
    params: Value,
}
impl ScheduleCommand {
    pub fn method(&self) -> &'static str {
        self.method
    }
    pub fn params(&self) -> Value {
        self.params.clone()
    }
    pub(crate) fn execute(
        &self,
        app: &crate::TrustedProjectApplication,
    ) -> Result<Value, crate::ApplicationError> {
        let p = &self.params;
        let id = p["id"].as_str().unwrap_or("");
        let rev = p["revision"].as_u64().unwrap_or(0);
        Ok(match self.method {
            "schedule.list" => json!({"rules":app.schedules()?}),
            "schedule.deliveries" => json!({"deliveries":app.schedule_deliveries()?}),
            "schedule.create" => {
                json!({"rule":app.schedule_create(serde_json::from_value(p.clone()).map_err(|_|invalid())?)?})
            }
            "schedule.update" => {
                json!({"rule":app.schedule_update(id,rev,serde_json::from_value(p["draft"].clone()).map_err(|_|invalid())?)?})
            }
            "schedule.review" => json!({"review":app.schedule_review(id,rev)?}),
            "schedule.confirm" => {
                json!({"rule":app.schedule_confirm(id,rev,p["nonce"].as_str().unwrap(),p["digest"].as_str().unwrap())?})
            }
            "schedule.pause" => json!({"rule":app.schedule_pause(id,rev)?}),
            "schedule.resume" => json!({"rule":app.schedule_resume(id,rev)?}),
            "schedule.delete" => {
                app.schedule_delete(id, rev)?;
                json!({"deleted":true})
            }
            "schedule.reload" => {
                app.schedule_reload()?;
                json!({"reloaded":true})
            }
            _ => return Err(invalid()),
        })
    }
}
fn invalid() -> crate::ApplicationError {
    crate::ApplicationError::presented(UiMessage::new("schedule.command_help"))
}
pub fn parse_schedule_command(args: &str) -> Result<ScheduleCommand, UiMessage> {
    let args = args.trim();
    let (verb, rest) = args.split_once(char::is_whitespace).unwrap_or((args, ""));
    let parts: Vec<_> = rest.split_whitespace().collect();
    let help = || UiMessage::new("schedule.command_help");
    let (method, params) = match (verb, parts.as_slice()) {
        ("" | "list", []) => ("schedule.list", json!({})),
        ("deliveries", []) => ("schedule.deliveries", json!({})),
        ("reload", ["--confirm"]) => ("schedule.reload", json!({"confirm":"reload"})),
        ("create", _) => {
            let draft: crate::application::ScheduleDraft =
                serde_json::from_str(rest).map_err(|_| help())?;
            ("schedule.create", json!(draft))
        }
        ("update", _) => {
            let mut split = rest
                .splitn(3, char::is_whitespace)
                .filter(|p| !p.is_empty());
            let id = split.next().ok_or_else(help)?;
            let revision: u64 = split.next().ok_or_else(help)?.parse().map_err(|_| help())?;
            let draft: crate::application::ScheduleDraft =
                serde_json::from_str(split.next().ok_or_else(help)?).map_err(|_| help())?;
            (
                "schedule.update",
                json!({"id":id,"revision":revision,"draft":draft}),
            )
        }
        ("review" | "pause" | "resume" | "delete", [id, revision]) => {
            let rev: u64 = revision.parse().map_err(|_| help())?;
            let method = match verb {
                "review" => "schedule.review",
                "pause" => "schedule.pause",
                "resume" => "schedule.resume",
                _ => "schedule.delete",
            };
            (method, json!({"id":id,"revision":rev}))
        }
        ("confirm", [id, revision, nonce, digest]) => {
            let rev: u64 = revision.parse().map_err(|_| help())?;
            (
                "schedule.confirm",
                json!({"id":id,"revision":rev,"nonce":nonce,"digest":digest}),
            )
        }
        _ => return Err(help()),
    };
    Ok(ScheduleCommand { method, params })
}
pub(crate) fn spec() -> CommandSpec {
    CommandSpec {
        names: vec!["schedule".into()],
        description: UiMessage::new("schedule.description").english(),
        takes_args: true,
        group: CommandGroup::Conversation,
        order: 21,
        listed: true,
        handler: Arc::new(Handler),
    }
}
struct Handler;
impl CommandHandler for Handler {
    fn run(
        &self,
        app: &mut crate::TrustedProjectApplication,
        args: &str,
    ) -> Result<CommandOutcome, CommandError> {
        let command = parse_schedule_command(args).map_err(CommandError::presented)?;
        let value = command
            .execute(app)
            .map_err(|e| CommandError::presented(e.presentation()))?;
        Ok(CommandOutcome::ShowSchedules(value))
    }
}
pub(crate) fn display(value: &Value, locale: crate::i18n::LocaleContext) -> String {
    let mut lines = vec![locale.text("schedule.title").to_owned()];
    if !value["review"].is_null() {
        lines.push(locale.text("schedule.review_note").into());
    }
    lines.push(serde_json::to_string_pretty(value).unwrap_or_default());
    lines.push(locale.text("schedule.command_help").into());
    lines.join("\n\n")
}
