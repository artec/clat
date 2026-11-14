use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    ffi::OsString,
    io::Read,
    path::{Path, PathBuf},
};

pub(super) const MAX_TEXT: usize = 64 * 1024;
pub(super) const MAX_RECORDS: usize = 512;
pub(super) const MAX_HISTORY: usize = 8 * 1024 * 1024;

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct AgentEdit {
    pub label: String,
    pub program: PathBuf,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub env: Vec<String>,
}

#[derive(Clone, Deserialize, Serialize)]
pub(super) struct Agent {
    pub id: String,
    pub revision: String,
    pub executable_fingerprint: String,
    pub edit: AgentEdit,
    pub trusted: BTreeMap<String, String>,
}

impl AgentEdit {
    pub(super) fn checked(mut self) -> Result<Self, String> {
        if self.label.trim().is_empty()
            || self.label.len() > 256
            || self.label.chars().any(char::is_control)
            || !self.program.is_absolute()
            || self.args.len() > 128
            || self.args.iter().any(|s| s.len() > 4096 || s.contains('\0'))
            || self.env.len() > 64
            || self.env.iter().any(|s| !env_name(s))
        {
            return Err("invalid ACP Agent configuration".into());
        }
        self.program = self
            .program
            .canonicalize()
            .map_err(|_| "ACP executable unavailable")?;
        Ok(self)
    }

    pub(super) fn environment(&self) -> Vec<(OsString, OsString)> {
        const BASE: &[&str] = &[
            "HOME",
            "USERPROFILE",
            "PATH",
            "SystemRoot",
            "WINDIR",
            "TEMP",
            "TMP",
            "TMPDIR",
            "LANG",
            "LC_ALL",
            "XDG_CONFIG_HOME",
            "XDG_DATA_HOME",
            "APPDATA",
            "LOCALAPPDATA",
        ];
        BASE.iter()
            .copied()
            .chain(self.env.iter().map(String::as_str))
            .filter_map(|name| std::env::var_os(name).map(|val| (name.into(), val)))
            .collect()
    }
}

fn env_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 128
        && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
}

pub(super) fn fingerprint(path: &Path) -> Result<String, String> {
    if path.canonicalize().ok().as_deref() != Some(path)
        || path.extension().is_some_and(|e| {
            matches!(
                e.to_string_lossy().to_ascii_lowercase().as_str(),
                "cmd" | "bat" | "ps1"
            )
        })
    {
        return Err("ACP executable has changed or needs a direct launcher".into());
    }
    let mut file = std::fs::File::open(path).map_err(|_| "ACP executable unavailable")?;
    let info = file
        .metadata()
        .map_err(|_| "cannot inspect ACP executable")?;
    if !info.is_file() || info.len() > 1024 * 1024 * 1024 {
        return Err("ACP executable is not a supported regular file".into());
    }
    let mut digest = Sha256::new();
    let mut buffer = [0; 64 * 1024];
    let mut reader = (&mut file).take(1024 * 1024 * 1024 + 1);
    let mut read = 0;
    loop {
        let count = reader
            .read(&mut buffer)
            .map_err(|_| "cannot fingerprint ACP executable")?;
        if count == 0 {
            break;
        }
        read += count as u64;
        digest.update(&buffer[..count]);
    }
    if read != info.len() {
        return Err("ACP executable changed during inspection".into());
    }
    Ok(format!("{:x}", digest.finalize()))
}

impl Agent {
    pub(super) fn view(&self, project: &str) -> Value {
        let args = self
            .edit
            .args
            .iter()
            .map(|arg| crate::redact::redact_secrets(arg))
            .collect::<Vec<_>>();
        let arguments_hidden = args != self.edit.args;
        json!({
            "id":self.id, "label":self.edit.label, "program":self.edit.program,
            "args":if arguments_hidden {Vec::new()} else {args}, "argumentsHidden":arguments_hidden,
            "env":self.edit.env, "revision":self.revision, "trusted":self.trusted.contains_key(project),
        })
    }

    pub(super) fn trusted(&self, project: &str) -> Result<(), String> {
        let current = fingerprint(&self.edit.program)?;
        if self.executable_fingerprint != current || self.trusted.get(project) != Some(&current) {
            return Err("ACP executable requires confirmation for this project".into());
        }
        Ok(())
    }
}

#[derive(Clone, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct Start {
    pub agent: String,
    pub request_id: String,
    #[serde(default)]
    pub text: String,
    #[serde(default)]
    pub continue_with_clat: bool,
    #[serde(default)]
    pub origin: Option<String>,
    #[serde(default)]
    pub delegation: bool,
    #[serde(default)]
    pub remote: Option<String>,
    #[serde(default)]
    pub additional_directories: Vec<PathBuf>,
    #[serde(default)]
    pub system_prompt: Option<String>,
    #[serde(default)]
    pub mcp_servers: Vec<Value>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum Phase {
    Starting,
    Idle,
    Running,
    Completed,
    Cancelled,
    Failed,
    Interrupted,
    Closed,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum Handoff {
    None,
    Pending,
    Admitted,
    Cancelled,
    Unavailable,
}

#[derive(Clone, Deserialize, Serialize)]
pub(super) struct Record {
    pub id: String,
    pub project: String,
    pub agent: String,
    pub agent_label: String,
    pub revision: String,
    pub request: Start,
    pub remote: Option<String>,
    pub phase: Phase,
    pub handoff: Handoff,
    pub updates: Vec<Value>,
    pub result: String,
    pub stop: Option<String>,
    pub error: Option<String>,
    pub capabilities: Value,
    pub session_options: Value,
    #[serde(default)]
    pub actions: BTreeMap<String, String>,
}

impl Record {
    pub(super) fn view(&self, history: bool) -> Value {
        json!({
            "id": self.id, "agent": self.agent, "label":self.agent_label,
            "origin":self.request.origin, "task":self.request.text, "phase":self.phase,
            "handoff":self.handoff, "result":history.then_some(&self.result),
            "stop":self.stop, "error":self.error, "capabilities":self.capabilities,
            "options":self.session_options,"updates":history.then_some(&self.updates),
        })
    }
}

#[derive(Clone, Deserialize, Serialize)]
pub(super) struct Catalog {
    pub version: u8,
    pub agents: BTreeMap<String, Agent>,
    pub records: BTreeMap<String, Record>,
}

impl Default for Catalog {
    fn default() -> Self {
        Self {
            version: 1,
            agents: BTreeMap::new(),
            records: BTreeMap::new(),
        }
    }
}
