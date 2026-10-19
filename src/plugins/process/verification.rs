//! Optional direct execution and model-visible verification envelope.
use super::*;

pub(super) struct Invocation {
    pub command: String,
    pub argv: Option<Vec<std::ffi::OsString>>,
    pub verification: Option<crate::process::VerificationSpec>,
}

pub(super) fn journal_arguments(args: &Value) -> Value {
    let mut copy = bounded_command_journal(args, "cmd");
    if let Some(argv) = args.get("argv") {
        let bytes = argv.as_array().filter(|a| a.len() <= 128).and_then(|a| {
            a.iter()
                .try_fold(0usize, |sum, v| sum.checked_add(v.as_str()?.len()))
        });
        if bytes.is_none_or(|n| n > MAX_COMMAND_BYTES)
            && let Some(copy) = copy.as_object_mut()
        {
            copy.remove("argv");
            copy.insert("argv_bytes".into(), json!(bytes));
            copy.insert("argv_omitted_from_journal".into(), json!(true));
        }
    }
    copy
}

impl Invocation {
    pub fn compat(args: &Value) -> Result<Self, ToolError> {
        let mut translated = args.clone();
        let object = translated
            .as_object_mut()
            .ok_or_else(|| ToolError::new("run_command: expected object"))?;
        if let Some(command) = object.remove("command") {
            object.insert("cmd".into(), command);
        }
        Self::parse(&translated)
    }

    pub fn parse(args: &Value) -> Result<Self, ToolError> {
        let cmd = optional_string(args, "cmd", "exec_command")?;
        let argv = args.get("argv");
        if cmd.is_some() == argv.is_some() {
            return Err(
                ToolError::new("exec_command: specify exactly one of cmd or argv")
                    .with_recovery(json!({"code":"ambiguous_command_input"})),
            );
        }
        let verification = args
            .get("verification")
            .map(|v| {
                serde_json::from_value::<crate::process::VerificationSpec>(v.clone())
                    .map_err(|e| ToolError::new(e.to_string()))
            })
            .transpose()?;
        if let Some(spec) = verification {
            if argv.is_none() || optional_bool(args, "tty", "exec_command")?.unwrap_or(false) {
                return Err(ToolError::new("verification requires argv and tty=false")
                    .with_recovery(json!({"code":"verification_requires_argv"})));
            }
            if !(1..=1_000_000).contains(&spec.minimum_executed) {
                return Err(ToolError::new("minimum_executed must be in 1..=1000000"));
            }
        }
        if let Some(cmd) = cmd {
            if cmd.trim().is_empty() {
                return Err(ToolError::new("exec_command: cmd must not be empty"));
            }
            ensure_command_bound(cmd, "exec_command")?;
            return Ok(Self {
                command: cmd.into(),
                argv: None,
                verification,
            });
        }
        let list = argv
            .and_then(Value::as_array)
            .ok_or_else(|| ToolError::new("argv must be an array"))?;
        if list.is_empty() || list.len() > 128 {
            return Err(ToolError::new("argv needs 1..=128 strings"));
        }
        let strings: Vec<&str> = list
            .iter()
            .map(|v| {
                v.as_str()
                    .ok_or_else(|| ToolError::new("argv entries must be strings"))
            })
            .collect::<Result<_, _>>()?;
        if strings[0].is_empty()
            || strings.iter().any(|s| s.contains('\0'))
            || strings.iter().map(|s| s.len()).sum::<usize>() > MAX_COMMAND_BYTES
        {
            return Err(ToolError::new(
                "argv needs a nonempty program, no NUL and at most 65536 UTF-8 bytes",
            ));
        }
        Ok(Self {
            command: serde_json::to_string(&strings).expect("argv JSON"),
            argv: Some(strings.into_iter().map(Into::into).collect()),
            verification,
        })
    }
}
