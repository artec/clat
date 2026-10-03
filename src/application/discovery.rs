//! Read-only interaction discovery. Registration and skill discovery stay core-owned.
use super::{ApplicationError, SkillsOverviewDto, TrustedProjectApplication};
use serde::Serialize;

#[derive(Clone, Debug, Serialize)]
pub struct InteractionCatalog {
    pub commands: Vec<InteractionCommand>,
    pub skills: SkillsOverviewDto,
}

#[derive(Clone, Debug, Serialize)]
pub struct InteractionCommand {
    pub name: String,
    pub aliases: Vec<String>,
    pub description: String,
    pub group: String,
    pub usage: String,
    pub unavailable_reason: Option<String>,
}

impl TrustedProjectApplication {
    pub fn interaction_catalog(&self) -> Result<InteractionCatalog, ApplicationError> {
        let commands = self
            .command_catalog()
            .into_iter()
            .map(|entry| InteractionCommand {
                unavailable_reason: if entry.name == "rename" && self.current_session_id().is_none()
                {
                    Some("No active conversation to rename".into())
                } else {
                    None
                },
                usage: command_usage(&entry.name),
                name: entry.name,
                aliases: entry.aliases,
                description: entry.description,
                group: entry.group.as_str().to_owned(),
            })
            .collect();
        Ok(InteractionCatalog {
            commands,
            skills: self.skills_overview()?,
        })
    }
}

fn command_usage(name: &str) -> String {
    let arguments = match name {
        "skill" => " [name]",
        "goal" => {
            " [show | create <objective> | run | pause | resume | complete [summary] | cancel]"
        }
        "mem" => {
            " [list [all|project|user] | show <id> | add <project|user> <content> | edit <id> <revision> <content> | delete <id> <revision>]"
        }
        "sub" => " [status | on | off]",
        _ => "",
    };
    format!("/{name}{arguments}")
}

#[cfg(test)]
mod tests {
    #[test]
    fn discovery_usage_does_not_turn_arguments_into_an_invocation() {
        assert_eq!(super::command_usage("skill"), "/skill [name]");
        assert_eq!(super::command_usage("custom"), "/custom");
        assert!(super::command_usage("goal").contains("<objective>"));
    }
}
