//! Human commands have explicit stop/retry tokens and never enter the model.
use super::*;
use crate::command::{CommandError, CommandGroup, CommandHandler, CommandOutcome, CommandSpec};
#[derive(Clone, Debug)]
pub enum JobUserCommand {
    List,
    Output { id: String, offset: u64 },
    Stop(String),
    Retry(String),
}
impl JobUserCommand {
    pub fn method(&self) -> &'static str {
        match self {
            Self::List => "jobs.list",
            Self::Output { .. } => "jobs.output",
            Self::Stop(_) => "jobs.kill",
            Self::Retry(_) => "jobs.retry",
        }
    }
    pub fn params(&self) -> Value {
        match self {
            Self::List => json!({}),
            Self::Output { id, offset } => json!({"job_id":id,"offset":offset}),
            Self::Stop(id) | Self::Retry(id) => json!({"job_id":id,"confirm":id}),
        }
    }
    pub(crate) fn execute(
        &self,
        reader: &crate::application::JobsReader,
    ) -> Result<Value, crate::ApplicationError> {
        Ok(match self {
            Self::List => json!({"jobs":reader.list()?}),
            Self::Output { id, offset } => json!(reader.read_output(id, *offset)?),
            Self::Stop(id) => json!({"job":reader.kill(id)?}),
            Self::Retry(id) => json!({"job":reader.retry_settlement(id)?}),
        })
    }
}
pub fn parse_job_command(args: &str) -> Result<JobUserCommand, UiMessage> {
    let parts: Vec<_> = args.split_whitespace().collect();
    let valid = |id: &str| uuid::Uuid::parse_str(id).is_ok();
    match parts.as_slice() {
        [] | ["list"] => Ok(JobUserCommand::List),
        ["output", id] if valid(id) => Ok(JobUserCommand::Output {
            id: (*id).into(),
            offset: 0,
        }),
        ["output", id, offset] if valid(id) => Ok(JobUserCommand::Output {
            id: (*id).into(),
            offset: offset
                .parse()
                .map_err(|_| UiMessage::new("job.command_help"))?,
        }),
        ["stop", id, "--confirm"] if valid(id) => Ok(JobUserCommand::Stop((*id).into())),
        ["retry", id, "--confirm"] if valid(id) => Ok(JobUserCommand::Retry((*id).into())),
        _ => Err(UiMessage::new("job.command_help")),
    }
}
pub(crate) fn spec() -> CommandSpec {
    CommandSpec {
        names: vec!["jobs".into()],
        description: UiMessage::new("job.description").english(),
        takes_args: true,
        group: CommandGroup::Conversation,
        order: 20,
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
        let request = parse_job_command(args).map_err(CommandError::presented)?;
        let value = request
            .execute(&app.jobs_reader())
            .map_err(|e| CommandError::presented(e.presentation()))?;
        Ok(CommandOutcome::ShowJobs(value))
    }
}
pub(crate) fn display(value: &Value, locale: crate::i18n::LocaleContext) -> String {
    let mut lines = vec![locale.text("job.title").to_owned()];
    let rows = value["jobs"]
        .as_array()
        .cloned()
        .unwrap_or_else(|| value.get("job").cloned().into_iter().collect());
    for row in rows {
        let state = locale
            .text(&format!(
                "job.{}",
                row["state"].as_str().unwrap_or("interrupted")
            ))
            .to_owned();
        lines.push(format!(
            "{} · {} · {}",
            row["id"].as_str().unwrap_or(""),
            state,
            row["label"].as_str().unwrap_or("")
        ));
        if let Some(observation) = row.get("observation").filter(|v| !v.is_null()) {
            lines.push(observation.to_string());
        }
        for key in ["persistence_error", "cleanup_error"] {
            if let Some(detail) = row[key].as_str() {
                let message = if key == "persistence_error" {
                    "job.persistence_failed"
                } else {
                    "job.cleanup_failed"
                };
                lines.push(locale.render(&UiMessage::new(message).arg("detail", detail)));
            }
        }
    }
    if let Some(text) = value["text"].as_str() {
        lines.push(text.into());
    }
    if value["lost"] == true {
        lines.push(locale.text("job.lost").into());
    }
    if let Some(offset) = value["next_offset"].as_u64() {
        lines.push(
            locale.render(&UiMessage::new("job.next_offset").arg("offset", offset.to_string())),
        );
    }
    lines.join("\n\n")
}
