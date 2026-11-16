//! Catalog of locally installed ACP launchers. Detection never starts a process.
mod batch;
mod discovery;

pub(crate) use batch::AddPresets;

use super::{Agent, AgentEdit, Catalog, ControlStorage, Service, state};
use crate::i18n::UiMessage;
use discovery::{Candidate, Detection, Search};
use serde::Deserialize;
use serde_json::{Value, json};
use std::path::Path;

struct Preset {
    id: &'static str,
    label: &'static str,
    command: &'static str,
    args: &'static [&'static str],
    base_command: Option<&'static str>,
    install: Install,
    windows_install: Install,
}

#[derive(Clone, Copy)]
enum Install {
    Npm(&'static str, &'static str),
    Terminal(&'static str, &'static str),
    Download(&'static str),
    Wsl(&'static str, &'static str),
    Unavailable(&'static str),
}

impl Install {
    fn view(self, launcher: &str, windows: bool) -> Value {
        let (method, command, url) = match self {
            Self::Npm(package, url) => ("npm", Some(format!("npm install -g {package}")), url),
            Self::Terminal(command, url) => ("terminal", Some(command.to_owned()), url),
            Self::Download(url) => ("download", None, url),
            Self::Wsl(package, url) => ("wsl", Some(format!("npm install -g {package}")), url),
            Self::Unavailable(url) => ("unavailable", None, url),
        };
        json!({
            "method": method,
            "command": command,
            "url": url,
            "launcher": launcher,
            "windows": windows,
        })
    }
}

const CODEX_INSTALL: &str = "https://github.com/agentclientprotocol/codex-acp#installation";
const CLAUDE_INSTALL: &str =
    "https://github.com/agentclientprotocol/claude-agent-acp#standalone-executables";
const OPENCODE_INSTALL: &str = "https://opencode.ai/docs/";
const DSH_INSTALL: &str = "https://github.com/deepseek-ai/deepseek-harness";
const COPILOT_INSTALL: &str =
    "https://docs.github.com/en/copilot/how-tos/copilot-cli/set-up-copilot-cli/install-copilot-cli";
const GEMINI_INSTALL: &str = "https://geminicli.com/docs/get-started/installation/";
const QODER_INSTALL: &str = "https://docs.qoder.com/cli/installation";
const KIMI_INSTALL: &str = "https://moonshotai.github.io/kimi-code/en/guides/getting-started.html";
const GROK_INSTALL: &str = "https://docs.x.ai/build/overview";
const AGY_INSTALL: &str = "https://antigravity.google/docs/cli/tutorial";

const PRESETS: &[Preset] = &[
    Preset {
        id: "codex",
        label: "Codex",
        command: "codex-acp",
        args: &[],
        base_command: Some("codex"),
        install: Install::Npm("@agentclientprotocol/codex-acp", CODEX_INSTALL),
        windows_install: Install::Wsl("@agentclientprotocol/codex-acp", CODEX_INSTALL),
    },
    Preset {
        id: "claude-code",
        label: "Claude Code",
        command: "claude-agent-acp",
        args: &[],
        base_command: Some("claude"),
        install: Install::Npm("@agentclientprotocol/claude-agent-acp", CLAUDE_INSTALL),
        windows_install: Install::Download(
            "https://github.com/agentclientprotocol/claude-agent-acp/releases/latest",
        ),
    },
    // https://opencode.ai/docs/cli/acp/
    Preset {
        id: "opencode",
        label: "OpenCode",
        command: "opencode",
        args: &["acp"],
        base_command: None,
        install: Install::Terminal(
            "curl -fsSL https://opencode.ai/v2/install | bash",
            OPENCODE_INSTALL,
        ),
        windows_install: Install::Download(OPENCODE_INSTALL),
    },
    // https://github.com/deepseek-ai/deepseek-harness/blob/master/docs/cli-help.md#dsh-acp
    Preset {
        id: "deepseek-harness",
        label: "DeepSeek Harness",
        command: "dsh",
        args: &["acp"],
        base_command: None,
        install: Install::Npm("@deepseek-ai/dsh", DSH_INSTALL),
        windows_install: Install::Wsl("@deepseek-ai/dsh", DSH_INSTALL),
    },
    // https://docs.x.ai/build/cli/headless-scripting
    Preset {
        id: "grok-build",
        label: "Grok Build",
        command: "grok",
        args: &["--no-auto-update", "agent", "stdio"],
        base_command: None,
        install: Install::Terminal(
            "curl -fsSL https://x.ai/cli/install.sh | bash",
            GROK_INSTALL,
        ),
        windows_install: Install::Terminal("irm https://x.ai/cli/install.ps1 | iex", GROK_INSTALL),
    },
    // https://docs.github.com/en/copilot/reference/copilot-cli-reference/acp-server
    Preset {
        id: "github-copilot",
        label: "GitHub Copilot CLI",
        command: "copilot",
        args: &["--acp", "--stdio"],
        base_command: None,
        install: Install::Npm("@github/copilot", COPILOT_INSTALL),
        windows_install: Install::Terminal("winget install GitHub.Copilot", COPILOT_INSTALL),
    },
    // https://docs.qoder.com/cli/acp
    Preset {
        id: "qoder-cli",
        label: "Qoder CLI",
        command: "qodercli",
        args: &["--acp"],
        base_command: None,
        install: Install::Terminal("curl -fsSL https://qoder.com/install | bash", QODER_INSTALL),
        windows_install: if cfg!(target_arch = "aarch64") {
            Install::Unavailable(QODER_INSTALL)
        } else {
            Install::Terminal("irm https://qoder.com/install.ps1 | iex", QODER_INSTALL)
        },
    },
    // https://antigravity.google/docs/ide/extensions/acp
    Preset {
        id: "antigravity-cli",
        label: "Antigravity CLI",
        command: "agy",
        args: &["acp"],
        base_command: None,
        install: Install::Terminal(
            "curl -fsSL https://antigravity.google/cli/install.sh | bash",
            AGY_INSTALL,
        ),
        windows_install: Install::Terminal(
            "irm https://antigravity.google/cli/install.ps1 | iex",
            AGY_INSTALL,
        ),
    },
    // https://geminicli.com/docs/cli/acp-mode/
    Preset {
        id: "gemini-cli",
        label: "Gemini CLI",
        command: "gemini",
        args: &["--acp"],
        base_command: None,
        install: Install::Npm("@google/gemini-cli", GEMINI_INSTALL),
        windows_install: Install::Wsl("@google/gemini-cli", GEMINI_INSTALL),
    },
    // https://moonshotai.github.io/kimi-code/en/reference/kimi-acp.html
    Preset {
        id: "kimi-code",
        label: "Kimi Code",
        command: "kimi",
        args: &["acp"],
        base_command: None,
        install: Install::Npm("@moonshot-ai/kimi-code", KIMI_INSTALL),
        windows_install: Install::Terminal(
            "irm https://code.kimi.com/kimi-code/install.ps1 | iex",
            KIMI_INSTALL,
        ),
    },
];

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct AddPreset {
    id: String,
    detection: String,
}

impl Service {
    pub(crate) fn agent_presets(
        &self,
        control: &ControlStorage,
        project: &Path,
    ) -> Result<Value, UiMessage> {
        let search = Search::host(project);
        let rows = PRESETS
            .iter()
            .map(|preset| (preset, search.detect(preset)))
            .collect::<Vec<_>>();
        self.inspect(control, |catalog| {
            Value::Array(
                rows.into_iter()
                    .map(|(preset, detection)| preset.view(catalog, detection, search.windows))
                    .collect(),
            )
        })
        .map_err(operation_error)
    }

    pub(crate) fn add_agent_preset(
        &self,
        control: &ControlStorage,
        project: &Path,
        request: AddPreset,
    ) -> Result<Value, UiMessage> {
        self.add_preset_from(control, &Search::host(project), request)
    }

    fn add_preset_from(
        &self,
        control: &ControlStorage,
        search: &Search<'_>,
        request: AddPreset,
    ) -> Result<Value, UiMessage> {
        self.add_presets_from(control, search, std::slice::from_ref(&request))?
            .pop()
            .expect("one ACP preset result")
    }
}

impl Preset {
    fn edit(&self, candidate: &Candidate) -> AgentEdit {
        AgentEdit {
            label: self.label.into(),
            program: candidate.path.clone(),
            args: self.args.iter().map(|value| (*value).into()).collect(),
            env: Vec::new(),
        }
    }

    fn view(&self, catalog: &Catalog, detection: Detection, windows: bool) -> Value {
        let install = if windows {
            self.windows_install
        } else {
            self.install
        };
        let (status, token, detail) = match detection {
            Detection::Ready(candidate) if existing(catalog, &self.edit(&candidate)).is_some() => {
                ("added", None, None)
            }
            Detection::Ready(candidate) => ("ready", Some(candidate.token), None),
            Detection::MissingRuntime(_, error) => ("missing_runtime", None, Some(error)),
            Detection::MissingAdapter => ("missing_adapter", None, None),
            Detection::Incompatible => ("incompatible", None, None),
            Detection::Missing => ("missing", None, None),
        };
        json!({
            "id": self.id,
            "label": self.label,
            "status": status,
            "detection": token,
            "statusDetail": detail,
            "install": install.view(self.command, windows),
            "requiresAdapter": self.base_command.is_some(),
        })
    }
}

fn existing<'a>(catalog: &'a Catalog, edit: &AgentEdit) -> Option<&'a Agent> {
    catalog.agents.values().find(|agent| {
        agent.edit.program == edit.program
            && agent.edit.args == edit.args
            && agent.edit.env == edit.env
    })
}

fn operation_error(detail: String) -> UiMessage {
    UiMessage::new("acp.operation_error").arg("detail", crate::redact::redact_secrets(&detail))
}

#[cfg(test)]
mod tests;
