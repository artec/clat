use super::*;
use crate::task::acceptance::{AcceptanceContract, CommandFact};
use sha2::{Digest, Sha256};
#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct CommandRule {
    argv: Vec<String>,
    cwd: String,
    format: String,
    minimum_executed: u64,
}
pub(crate) struct CommandPlan {
    pub contract_version: u64,
    pub clauses: Vec<String>,
    pub invocation_digest: String,
    pub files: Vec<String>,
}
pub(crate) fn command_plan(
    contract: &AcceptanceContract,
    name: &str,
    args: &Value,
    identity: &str,
    project: &Project,
) -> Option<CommandPlan> {
    if !matches!(name, "run_command" | "exec_command")
        || args.get("argv").is_none()
        || args.get("cmd").is_some()
        || args.get("command").is_some()
        || args["tty"] == true
    {
        return None;
    }
    let mut ids = Vec::new();
    let mut files = std::collections::BTreeSet::new();
    for c in &contract.clauses {
        if c.checker != "command" || c.checker_version != 1 || c.source != "command_operation" {
            continue;
        }
        let Ok(r) = parse::<CommandRule>(&c.parameters) else {
            continue;
        };
        if args["argv"] == json!(r.argv)
            && crate::process::invocation_digest(&r.argv, &declared_cwd(project.root(), &r.cwd))
                == identity
            && args["verification"]["format"] == r.format
            && args["verification"]["minimum_executed"]
                .as_u64()
                .is_some_and(|n| n >= r.minimum_executed)
        {
            ids.push(c.id.clone());
            files.extend(c.files.iter().cloned());
        }
    }
    if ids.is_empty() {
        return None;
    }
    let invocation_digest = identity.to_owned();
    Some(CommandPlan {
        contract_version: contract.version,
        clauses: ids,
        invocation_digest,
        files: files.into_iter().collect(),
    })
}
fn signature(argv: &Value, cwd: &str) -> String {
    format!(
        "{:x}",
        Sha256::digest(serde_json::to_vec(&(argv, cwd)).expect("command identity"))
    )
}
/// Earlier GA-2 facts had only a contract-relative command digest. Retain that
/// fence after upgrade; an unavailable old rule cannot silently grant replay.
pub(crate) fn prior_command_matches(
    op: &crate::task::state::Operation,
    s: &TaskState,
    identity: &str,
) -> bool {
    if let Some(saved) = &op.execution_identity {
        return saved == identity;
    }
    let Some(fact) = &op.command_fact else {
        return false;
    };
    if fact.invocation_digest == identity {
        return true;
    }
    let Some(c) = s
        .acceptance_state
        .as_ref()
        .and_then(|a| a.contract.as_ref())
        .filter(|c| c.version == fact.contract_version)
    else {
        return true;
    };
    for clause in &c.clauses {
        if !fact.clauses.contains(&clause.id) {
            continue;
        }
        let Ok(rule) = parse::<CommandRule>(&clause.parameters) else {
            continue;
        };
        if signature(&json!(rule.argv), &rule.cwd) == fact.invocation_digest {
            return crate::process::invocation_digest(
                &rule.argv,
                &declared_cwd(std::path::Path::new(&s.project), &rule.cwd),
            ) == identity;
        }
    }
    true
}
pub(crate) fn command_observation(fact: &mut CommandFact, result: &Value) {
    let mut lifecycle = serde_json::Map::new();
    for key in [
        "session_id",
        "execution",
        "exit_scope",
        "exit_code",
        "signal",
        "timed_out",
        "cancelled",
        "terminated",
        "running",
        "sandbox",
        "cwd",
    ] {
        if let Some(value) = result.get(key) {
            lifecycle.insert(key.into(), value.clone());
        }
    }
    fact.lifecycle = Value::Object(lifecycle);
    if let Some(receipt) = result.get("verification_receipt")
        && let Ok(bytes) = serde_json::to_vec(receipt)
        && bytes.len() <= 8192
    {
        fact.receipt_digest = Some(format!("{:x}", Sha256::digest(&bytes)));
        fact.receipt = Some(receipt.clone());
    }
}
pub(super) struct CommandChecker;
impl AcceptanceChecker for CommandChecker {
    fn identity(&self) -> (&'static str, u32) {
        ("command", 1)
    }
    fn validate(&self, c: &Clause) -> Result<(), String> {
        let r: CommandRule = parse(&c.parameters)?;
        if c.source != "command_operation"
            || c.files.is_empty()
            || r.argv.is_empty()
            || r.argv.len() > 64
            || r.argv.iter().any(|a| a.len() > 1024 || a.contains('\0'))
            || r.minimum_executed == 0
            || r.minimum_executed > 1_000_000
            || !matches!(
                r.format.as_str(),
                "rust-test-summary" | "playwright-summary" | "playwright-json"
            )
        {
            return Err("command needs explicit argv, nonempty input scope, supported receipt and nonzero execution minimum".into());
        }
        if r.cwd != "." {
            crate::task::acceptance::validate_relative(&r.cwd)?;
        }
        Ok(())
    }
    fn evaluate(
        &self,
        c: &Clause,
        s: &TaskState,
        files: &CheckSnapshot,
        operation: Option<&str>,
    ) -> CheckerOutcome {
        let Some(op) = operation.and_then(|id| s.operations.iter().find(|o| o.id == id)) else {
            return CheckerOutcome::new(
                "inconclusive",
                "explicit current-task runtime operation identity required",
            );
        };
        let mut result = match check(c, s, files, op) {
            Ok(status) => CheckerOutcome::new(
                status,
                "runtime-linked declared command/output observation; coverage and build freshness are not authenticated",
            ),
            Err(e) => CheckerOutcome::new("inconclusive", e),
        };
        if op.outcome == "confirmed" && op.command_fact.is_some() {
            result.operation = Some(op.id.clone());
            result.evidence_digest = op.result_digest.clone();
        }
        result
    }
}
fn check(
    c: &Clause,
    s: &TaskState,
    files: &CheckSnapshot,
    op: &crate::task::state::Operation,
) -> Result<&'static str, String> {
    let r: CommandRule = parse(&c.parameters)?;
    let f = op
        .command_fact
        .as_ref()
        .ok_or("operation has no runtime command provenance")?;
    let version = s
        .acceptance_state
        .as_ref()
        .and_then(|a| a.contract.as_ref())
        .ok_or("no contract")?
        .version;
    if op.outcome != "confirmed"
        || op.result_kind.as_deref() != Some("ok")
        || !matches!(op.tool.as_str(), "run_command" | "exec_command")
        || f.contract_version != version
        || !f.clauses.contains(&c.id)
        || (f.invocation_digest != signature(&json!(r.argv), &r.cwd)
            && f.invocation_digest
                != crate::process::invocation_digest(
                    &r.argv,
                    &declared_cwd(std::path::Path::new(&s.project), &r.cwd),
                ))
    {
        return Err("command producer, contract, execution or invocation mismatch".into());
    }
    let receipt = f.receipt.as_ref().ok_or("no complete bounded receipt")?;
    let expected_cwd = declared_cwd(std::path::Path::new(&s.project), &r.cwd);
    if f.lifecycle
        .get("cwd")
        .is_some_and(|cwd| cwd.as_str() != expected_cwd.to_str())
        || receipt["invocation_sha256"] != crate::process::invocation_digest(&r.argv, &expected_cwd)
        || receipt["session_id"].as_u64().is_none_or(|n| n == 0)
        || receipt["owner_generation"].as_u64().is_none_or(|n| n == 0)
    {
        return Err("actual command/cwd/process identity does not match declared scope".into());
    }
    for path in &c.files {
        let hash = files
            .files
            .get(path)
            .and_then(|f| f.sha256.as_ref())
            .ok_or("input unavailable")?;
        if !f
            .before
            .iter()
            .any(|x| &x.path == path && &x.sha256 == hash)
            || !f.after.iter().any(|x| &x.path == path && &x.sha256 == hash)
        {
            return Err("declared input changed before/during/after command".into());
        }
    }
    receipt_verdict(f, receipt, &r)
}
fn receipt_verdict(
    f: &CommandFact,
    receipt: &Value,
    r: &CommandRule,
) -> Result<&'static str, String> {
    for key in ["cancelled", "timed_out", "terminated", "running"] {
        if f.lifecycle[key] == true || receipt[key] == true {
            return Err("command interrupted, running, cancelled or timed out".into());
        }
    }
    if f.lifecycle["execution"] != "argv"
        || f.lifecycle["exit_scope"] != "direct_child"
        || receipt["format"] != r.format
        || receipt["session_id"] != f.lifecycle["session_id"]
    {
        return Err("receipt identity/format/EOF does not match actual command".into());
    }
    if receipt["exit_code"] != f.lifecycle["exit_code"] {
        return Err("receipt and actual process exit mismatch".into());
    }
    if f.lifecycle["exit_code"] != 0
        || receipt["status"] == "failed"
        || receipt["failed"].as_u64().unwrap_or(0) > 0
    {
        return Ok("failed");
    }
    if receipt["scan_complete"] != true
        || receipt["expected_failures"].as_u64().unwrap_or(0) > 0
        || receipt["status"] != "passed"
        || receipt["flaky"].as_u64().unwrap_or(0) > 0
        || receipt["executed"]
            .as_u64()
            .is_none_or(|n| n < r.minimum_executed)
        || receipt["minimum_executed"]
            .as_u64()
            .is_none_or(|n| n < r.minimum_executed)
    {
        return Err("receipt has no stable nonempty passing observation".into());
    }
    Ok("passed")
}

fn declared_cwd(root: &std::path::Path, cwd: &str) -> std::path::PathBuf {
    if cwd == "." {
        root.to_path_buf()
    } else {
        root.join(cwd)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn task_receipt_gate_preserves_process_failure_and_refuses_partial_empty_flaky_expected_failure()
     {
        let r = CommandRule {
            argv: vec!["test".into()],
            cwd: ".".into(),
            format: "playwright-json".into(),
            minimum_executed: 1,
        };
        let receipt = json!({"format":"playwright-json","session_id":3,"exit_code":0,"status":"passed","failed":0,"executed":2,"minimum_executed":1,"scan_complete":true});
        let f = CommandFact {
            contract_version: 1,
            clauses: vec!["tests".into()],
            invocation_digest: "0".repeat(64),
            before: vec![],
            after: vec![],
            receipt: None,
            receipt_digest: None,
            lifecycle: json!({"session_id":3,"execution":"argv","exit_scope":"direct_child","exit_code":0}),
        };
        assert_eq!(receipt_verdict(&f, &receipt, &r).unwrap(), "passed");
        for (key, value) in [
            ("scan_complete", json!(false)),
            ("executed", json!(0)),
            ("flaky", json!(1)),
            ("expected_failures", json!(1)),
            ("status", json!("inconclusive")),
            ("session_id", json!(4)),
            ("format", json!("rust-test-summary")),
        ] {
            let mut partial = receipt.clone();
            partial[key] = value;
            assert!(receipt_verdict(&f, &partial, &r).is_err(), "{key}");
        }
        for key in ["cancelled", "timed_out", "running", "terminated"] {
            let mut interrupted = f.clone();
            interrupted.lifecycle[key] = json!(true);
            assert!(
                receipt_verdict(&interrupted, &receipt, &r).is_err(),
                "{key}"
            );
        }
        let mut failed = receipt.clone();
        failed["status"] = json!("failed");
        failed["failed"] = json!(1);
        assert_eq!(receipt_verdict(&f, &failed, &r).unwrap(), "failed");
        let mut nonzero = f;
        nonzero.lifecycle["exit_code"] = json!(7);
        failed["exit_code"] = json!(7);
        assert_eq!(receipt_verdict(&nonzero, &failed, &r).unwrap(), "failed");
    }
}
