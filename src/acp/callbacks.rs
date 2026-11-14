mod forms;
mod terminals;
use super::{
    Service,
    wire::{Callback, Client, Reply},
};
use crate::{
    CancelToken, Project,
    interaction::{AskAnswer, AskOption, AskQuestion, AskQuestionPresentation, UserAsker},
    permission::{PermissionApprover, PermissionDecision, PermissionRequest, WriteScope},
    tool::ToolEffect,
};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::Arc,
    sync::Mutex,
};
pub(super) use terminals::Terminals;

#[derive(Clone)]
pub struct Interaction {
    pub approver: Arc<dyn PermissionApprover>,
    pub asker: Option<Arc<dyn UserAsker>>,
}

pub(super) struct Bridge {
    pub service: Arc<Service>,
    pub project: Project,
    pub root: PathBuf,
    pub control: Arc<crate::control_storage::ControlStorage>,
    pub id: String,
    pub agent_label: String,
    pub session: Option<String>,
    pub interaction: Interaction,
    pub cancel: CancelToken,
    pub writable: bool,
    pub replay: Option<Vec<Value>>,
    pub option_updates: serde_json::Map<String, Value>,
    pub terminals: Arc<Terminals>,
    pub permissions: Arc<Mutex<BTreeMap<String, String>>>,
}

impl Client for Bridge {
    fn update(&mut self, mut params: Value) -> Result<(), String> {
        if params["sessionId"].as_str() != self.session.as_deref() || self.session.is_none() {
            return Err("ACP update belongs to another session".into());
        }
        let update = params
            .get_mut("update")
            .filter(|update| update["sessionUpdate"].as_str().is_some())
            .ok_or("invalid ACP update")?
            .take();
        super::updates::remember_options(&mut self.option_updates, &update);
        if let Some(replay) = &mut self.replay {
            return super::updates::append(replay, update);
        }
        self.service.add_update(&self.id, update)
    }

    fn callback(
        &mut self,
        method: &str,
        params: Value,
        request_id: u64,
        cancel: &CancelToken,
    ) -> Result<Callback, i64> {
        if !matches!(
            method,
            "session/request_permission"
                | "fs/read_text_file"
                | "fs/write_text_file"
                | "elicitation/create"
                | "terminal/create"
                | "terminal/output"
                | "terminal/wait_for_exit"
                | "terminal/kill"
                | "terminal/release"
        ) {
            return Err(-32601);
        }
        let session_matches =
            self.session.is_some() && params["sessionId"].as_str() == self.session.as_deref();
        let scope_matches = if method == "elicitation/create" {
            match (params.get("sessionId"), params.get("requestId")) {
                (Some(_), None) => session_matches,
                (None, Some(id)) => id.as_u64() == Some(request_id),
                _ => false,
            }
        } else {
            session_matches
        };
        if !scope_matches
            || !self.control.is_project_trusted(self.root.as_path())
            || self.replay.is_some()
        {
            return Err(-32602);
        }
        let scope = CallbackScope {
            agent_label: self.agent_label.clone(),
            project: self.project.clone(),
            root: self.root.clone(),
            control: self.control.clone(),
            interaction: self.interaction.clone(),
            cancel: cancel.clone(),
            writable: self.writable,
            terminals: self.terminals.clone(),
            permissions: self.permissions.clone(),
        };
        let method = method.to_owned();
        Ok(Box::new(move || scope.run(&method, params)))
    }
}

struct CallbackScope {
    agent_label: String,
    project: Project,
    root: PathBuf,
    control: Arc<crate::control_storage::ControlStorage>,
    interaction: Interaction,
    cancel: CancelToken,
    writable: bool,
    terminals: Arc<Terminals>,
    permissions: Arc<Mutex<BTreeMap<String, String>>>,
}

impl CallbackScope {
    fn run(&self, method: &str, params: Value) -> Reply {
        match method {
            "session/request_permission" => self.permission(&params),
            "fs/read_text_file" => self.read(&params),
            "fs/write_text_file" => self.write(&params),
            "elicitation/create" => self.elicitation(&params),
            method if method.starts_with("terminal/") && self.terminals.supported() => {
                self.terminal(method, &params)
            }
            _ => Err(-32601),
        }
    }

    fn permission(&self, params: &Value) -> Reply {
        let choices = params["options"]
            .as_array()
            .filter(|opts| opts.len() <= 16)
            .ok_or(-32602)?;
        let options = permission_options(choices)?;
        if !self.current() {
            return Ok(json!({"outcome":{"outcome":"cancelled"}}));
        }
        let key = permission_key(&params["toolCall"]);
        if let Some(key) = &key
            && let Some(kind) = self.permissions.lock().expect("ACP permissions").get(key)
            && let Some(option) = options.iter().find(|option| &option.kind == kind)
        {
            return Ok(json!({"outcome":{"outcome":"selected","optionId":option.id}}));
        }
        let outcome = self.choose(
            &params["toolCall"]["title"],
            options
                .iter()
                .map(|option| AskOption {
                    label: option.label.clone(),
                    description: None,
                })
                .collect(),
            false,
            Vec::new(),
        );
        if !self.current() {
            return Ok(json!({"outcome":{"outcome":"cancelled"}}));
        }
        if let AskAnswer::Selected(label) = outcome
            && let Some(option) = options.iter().find(|option| option.label == label)
        {
            if let Some(key) = key
                && matches!(option.kind.as_str(), "allow_always" | "reject_always")
            {
                let mut permissions = self.permissions.lock().expect("ACP permissions");
                if permissions.len() < 128 {
                    permissions.insert(key, option.kind.clone());
                }
            }
            return Ok(json!({"outcome":{"outcome":"selected","optionId":option.id}}));
        }
        Ok(json!({"outcome":{"outcome":"cancelled"}}))
    }

    fn path(&self, value: &Value) -> Result<PathBuf, i64> {
        let path = Path::new(value["path"].as_str().ok_or(-32602)?);
        let relative = path.strip_prefix(self.root.as_path()).map_err(|_| -32602)?;
        if relative.as_os_str().is_empty()
            || !relative
                .components()
                .all(|c| matches!(c, std::path::Component::Normal(_)))
        {
            return Err(-32602);
        }
        Ok(relative.to_owned())
    }

    fn read(&self, params: &Value) -> Reply {
        let path = self.path(params)?;
        if !self.current() {
            return Err(-32800);
        }
        let bytes = self
            .project
            .read_file_limited(path, 512 * 1024)
            .map_err(|_| -32603)?
            .ok_or(-32603)?;
        let content = String::from_utf8(bytes).map_err(|_| -32602)?;
        let line = params["line"].as_u64().unwrap_or(1);
        let limit = params["limit"].as_u64().unwrap_or(u64::MAX);
        if line == 0 {
            return Err(-32602);
        }
        let content = if line == 1 && limit == u64::MAX {
            content
        } else {
            content
                .split_inclusive('\n')
                .skip(usize::try_from(line - 1).unwrap_or(usize::MAX))
                .take(usize::try_from(limit).unwrap_or(usize::MAX))
                .collect::<String>()
        };
        Ok(json!({"content": content}))
    }

    fn write(&self, params: &Value) -> Reply {
        if !self.writable || !self.current() {
            return Err(-32603);
        }
        let path = self.path(params)?;
        let text = params["content"]
            .as_str()
            .filter(|s| s.len() <= 512 * 1024)
            .ok_or(-32602)?;
        let target = self
            .project
            .writable_target(&path, false, WriteScope::ProjectRoot)
            .map_err(|_| -32603)?;
        let previous = match target.read_to_string_limited(512 * 1024) {
            Ok(data) => Some(data),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(_) => return Err(-32603),
        };
        let choice = self.interaction.approver.decide(
            PermissionRequest {
                tool: "acp.write_text_file".into(),
                effect: ToolEffect::Write,
                reason: crate::i18n::UiMessage::new("acp.write_approval").english(),
                arguments: json!({"agent":self.agent_label,"path":path,"bytes":text.len()}),
                call_id: uuid::Uuid::new_v4().to_string(),
            },
            &self.cancel,
        );
        if choice != PermissionDecision::Allow || !self.current() {
            return Err(-32603);
        }
        // Reopen via the project capability and check the pre-approval bytes.
        let current = self
            .project
            .writable_target(&path, false, WriteScope::ProjectRoot)
            .map_err(|_| -32603)?;
        match previous.as_deref() {
            Some(expected) => {
                current
                    .atomic_write(text, Some(expected))
                    .map_err(|_| -32603)?;
            }
            None => {
                current.atomic_create(text).map_err(|_| -32603)?;
            }
        }
        Ok(json!({}))
    }

    fn current(&self) -> bool {
        !self.cancel.is_cancelled() && self.control.is_project_trusted(self.root.as_path())
    }

    fn choose(
        &self,
        text: &Value,
        options: Vec<AskOption>,
        allow_custom: bool,
        labels: Vec<crate::i18n::UiMessage>,
    ) -> AskAnswer {
        let Some(asker) = &self.interaction.asker else {
            return AskAnswer::Declined;
        };
        let question = crate::i18n::UiMessage::new("acp.question_from")
            .arg("agent", display_text(&self.agent_label, 256))
            .arg("detail", display_text(text.as_str().unwrap_or(""), 2000));
        asker.ask(
            AskQuestion {
                question: question.english(),
                options,
                allow_custom,
                presentation: Some(AskQuestionPresentation {
                    question,
                    options: labels,
                }),
            },
            &self.cancel,
        )
    }
}

struct Choice {
    label: String,
    id: String,
    kind: String,
}

fn permission_options(choices: &[Value]) -> Result<Vec<Choice>, i64> {
    let mut ids = std::collections::HashSet::new();
    choices
        .iter()
        .enumerate()
        .map(|(index, option)| {
            let id = option["optionId"]
                .as_str()
                .filter(|id| !id.is_empty() && id.len() <= 256 && ids.insert(*id))
                .ok_or(-32602)?;
            let name = option["name"].as_str().ok_or(-32602)?;
            let kind = option["kind"]
                .as_str()
                .filter(|kind| {
                    matches!(
                        *kind,
                        "allow_once" | "allow_always" | "reject_once" | "reject_always"
                    )
                })
                .ok_or(-32602)?;
            Ok(Choice {
                label: format!("{}. {}", index + 1, display_text(name, 100)),
                id: id.to_owned(),
                kind: kind.to_owned(),
            })
        })
        .collect()
}

fn permission_key(tool: &Value) -> Option<String> {
    use sha2::{Digest, Sha256};
    let title = tool["title"].as_str()?;
    let input = tool.get("rawInput").filter(|input| !input.is_null())?;
    let bytes = serde_json::to_vec(&(title, &tool["kind"], input, &tool["locations"])).ok()?;
    (bytes.len() <= 64 * 1024).then(|| format!("{:x}", Sha256::digest(bytes)))
}

pub(super) fn display_text(s: &str, max: usize) -> String {
    s.chars().filter(|c| (*c == '\n' || *c == '\t' || !c.is_control()) && !matches!(*c,
        '\u{061c}' | '\u{200b}'..='\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2060}'..='\u{206f}' | '\u{e0000}'..='\u{e007f}'
    )).take(max).collect()
}
