use super::*;
use crate::process::{ManagedStdioLease, ProcessService};
use std::{collections::BTreeMap, ffi::OsString, sync::Mutex, time::Duration};

pub(in crate::acp) struct Terminals {
    process: Arc<ProcessService>,
    entries: Mutex<BTreeMap<String, Terminal>>,
}

#[derive(Clone)]
struct Terminal {
    lease: ManagedStdioLease,
    limit: usize,
}

impl Terminals {
    pub(in crate::acp) fn new(process: Arc<ProcessService>) -> Self {
        Self {
            process,
            entries: Default::default(),
        }
    }

    pub(in crate::acp) fn supported(&self) -> bool {
        cfg!(target_os = "macos")
    }
}

impl Drop for Terminals {
    fn drop(&mut self) {
        for terminal in self.entries.get_mut().expect("ACP terminals").values() {
            let _ = self.process.close_managed_stdio(&terminal.lease);
        }
    }
}

impl CallbackScope {
    pub(super) fn terminal(&self, method: &str, params: &Value) -> Reply {
        if !self.current() {
            return Err(-32800);
        }
        if method == "terminal/create" {
            return self.start_terminal(params);
        }
        let id = params["terminalId"].as_str().ok_or(-32602)?;
        let terminal = self
            .terminals
            .entries
            .lock()
            .expect("ACP terminals")
            .get(id)
            .cloned()
            .ok_or(-32602)?;
        match method {
            "terminal/output" => {
                let (output, truncated, status) = terminal.lease.terminal_output(terminal.limit);
                Ok(
                    json!({"output":crate::redact::redact_secrets(&output),"truncated":truncated,"exitStatus":status}),
                )
            }
            "terminal/wait_for_exit" => loop {
                if !self.current() {
                    return Err(-32800);
                }
                if let Some(status) = terminal.lease.terminal_output(0).2 {
                    return Ok(status);
                }
                std::thread::sleep(Duration::from_millis(50));
            },
            "terminal/kill" => {
                terminal.lease.terminate();
                Ok(json!({}))
            }
            "terminal/release" => {
                self.terminals
                    .entries
                    .lock()
                    .expect("ACP terminals")
                    .remove(id);
                self.terminals
                    .process
                    .close_managed_stdio(&terminal.lease)
                    .map_err(|_| -32603)?;
                Ok(json!({}))
            }
            _ => Err(-32601),
        }
    }

    fn start_terminal(&self, params: &Value) -> Reply {
        let spec = TerminalSpec::parse(params, self.root.as_path())?;
        let choice = self.interaction.approver.decide(PermissionRequest {
            tool:"acp.terminal".into(),effect:ToolEffect::Execute,
            reason:crate::i18n::UiMessage::new("acp.terminal_approval").english(),
            arguments:json!({"agent":self.agent_label,"command":spec.program,"args":spec.args,"cwd":spec.cwd,"env":spec.env.iter().map(|(key,_)|key).collect::<Vec<_>>()}),
            call_id:uuid::Uuid::new_v4().to_string(),
        },&self.cancel);
        if choice != PermissionDecision::Allow || !self.current() {
            return Err(-32603);
        }
        if super::super::state::fingerprint(&spec.program).as_deref()
            != Ok(spec.fingerprint.as_str())
            || spec.cwd.canonicalize().ok().as_deref() != Some(&spec.cwd)
        {
            return Err(-32603);
        }
        let mut entries = self.terminals.entries.lock().expect("ACP terminals");
        if entries.len() >= 4 || !self.current() {
            return Err(-32603);
        }
        let id = format!("acp-term-{}", uuid::Uuid::new_v4());
        let lease = self
            .terminals
            .process
            .acquire_acp_terminal(
                &id,
                spec.program.into_os_string(),
                spec.args,
                spec.env,
                spec.cwd,
            )
            .map_err(|_| -32603)?;
        entries.insert(
            id.clone(),
            Terminal {
                lease,
                limit: spec.limit,
            },
        );
        Ok(json!({"terminalId":id}))
    }
}

struct TerminalSpec {
    program: PathBuf,
    fingerprint: String,
    cwd: PathBuf,
    args: Vec<OsString>,
    env: Vec<(OsString, OsString)>,
    limit: usize,
}

impl TerminalSpec {
    fn parse(params: &Value, root: &Path) -> Result<Self, i64> {
        let command = Path::new(params["command"].as_str().ok_or(-32602)?);
        let program = terminal_program(command)?;
        let fingerprint = super::super::state::fingerprint(&program).map_err(|_| -32602)?;
        let cwd = params["cwd"]
            .as_str()
            .map(Path::new)
            .unwrap_or(root)
            .canonicalize()
            .map_err(|_| -32602)?;
        if !cwd.starts_with(root) || !cwd.is_dir() {
            return Err(-32602);
        }
        let args = params["args"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|arg| arg.as_str().map(OsString::from).ok_or(-32602))
            .collect::<Result<Vec<_>, _>>()?;
        let mut env = ["PATH", "LANG", "LC_ALL", "HOME", "TMPDIR"]
            .into_iter()
            .filter_map(|name| std::env::var_os(name).map(|value| (name.into(), value)))
            .collect::<Vec<_>>();
        for item in params["env"].as_array().into_iter().flatten() {
            let key = item["name"]
                .as_str()
                .filter(|name| {
                    !name.is_empty() && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
                })
                .ok_or(-32602)?;
            let value = item["value"].as_str().ok_or(-32602)?;
            env.push((key.into(), value.into()));
        }
        let limit = params["outputByteLimit"]
            .as_u64()
            .unwrap_or(256 * 1024)
            .min(256 * 1024) as usize;
        Ok(Self {
            program,
            fingerprint,
            cwd,
            args,
            env,
            limit,
        })
    }
}

fn terminal_program(command: &Path) -> Result<PathBuf, i64> {
    if command.is_absolute() {
        return command.canonicalize().map_err(|_| -32602);
    }
    if command.components().count() != 1 {
        return Err(-32602);
    }
    std::env::var_os("PATH")
        .map(|path| {
            std::env::split_paths(&path)
                .filter(|dir| dir.is_absolute())
                .map(|dir| dir.join(command))
                .find(|path| path.is_file())
        })
        .flatten()
        .and_then(|path| path.canonicalize().ok())
        .ok_or(-32602)
}
