use super::*;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::io::Read;
use std::path::{Component, Path, PathBuf};
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Request {
    candidate_dir: String,
    counterexample_dir: String,
    package: String,
    test_filter: String,
    test_files: Vec<String>,
    production_files: Vec<String>,
    #[serde(default)]
    expected_failure: Option<String>,
    #[serde(default = "timeout")]
    timeout_seconds: u64,
    #[serde(default)]
    sandbox: Option<String>,
}
fn timeout() -> u64 {
    120
}
struct Arm {
    request: ProcessStart,
    plan: crate::process::PreparedProcess,
    root: PathBuf,
    hashes: BTreeMap<String, String>,
}
pub(super) struct Pair {
    candidate: Arm,
    counterexample: Arm,
    timeout: Duration,
    expected_failure: Option<String>,
}
impl Pair {
    pub(super) fn prepare(service: &ProcessService, value: &Value) -> Result<Self, String> {
        let req: Request = serde_json::from_value(value.clone()).map_err(|e| e.to_string())?;
        req.validate()?;
        let build = format!("target/clat-discrimination-{}", uuid::Uuid::new_v4());
        let command = format!(
            "cargo test --offline --locked --lib --package {} --target-dir {build} {} -- --exact --test-threads=1",
            req.package, req.test_filter
        );
        let candidate = arm(service, &req.candidate_dir, &command, &req)?;
        let counterexample = arm(service, &req.counterexample_dir, &command, &req)?;
        if candidate.root.starts_with(&counterexample.root)
            || counterexample.root.starts_with(&candidate.root)
        {
            return Err("experiment directories must be disjoint disposable copies".into());
        }
        let project_root = service
            .project_root()
            .canonicalize()
            .map_err(|e| e.to_string())?;
        if candidate.root == project_root || counterexample.root == project_root {
            return Err("use disposable copies, not the live project root".into());
        }
        compare_inputs(&candidate, &counterexample, &req)?;
        Ok(Self {
            candidate,
            counterexample,
            timeout: Duration::from_secs(req.timeout_seconds),
            expected_failure: req.expected_failure,
        })
    }
    pub(super) fn workspace_enforced(&self) -> bool {
        [&self.candidate, &self.counterexample]
            .iter()
            .all(|a| super::super::prepared::enforced_workspace(&a.plan.planned.facts))
    }
    pub(super) fn status(&self) -> String {
        format!(
            "paired offline cargo tests; candidate={}, counterexample={}; two executions; sandbox providers={}/{}",
            self.candidate.root.display(),
            self.counterexample.root.display(),
            self.candidate.plan.planned.facts.provider,
            self.counterexample.plan.planned.facts.provider
        )
    }
    pub(super) fn execute(
        self,
        service: &ProcessService,
        cancel: &CancelToken,
    ) -> Result<Value, String> {
        self.candidate.check_inputs()?;
        self.counterexample.check_inputs()?;
        let inputs = json!({"candidate_sha256":fingerprint(&self.candidate.hashes),
            "counterexample_sha256":fingerprint(&self.counterexample.hashes),
            "declared_files":self.candidate.hashes.len()});
        let candidate = run_arm(self.candidate, service, cancel, self.timeout, None)?;
        if !result::candidate_passed(&candidate) {
            return Ok(result::finish(candidate, None, inputs));
        }
        let counterexample = run_arm(
            self.counterexample,
            service,
            cancel,
            self.timeout,
            self.expected_failure.as_deref(),
        )?;
        Ok(result::finish(candidate, Some(counterexample), inputs))
    }
}
impl Request {
    fn validate(&self) -> Result<(), String> {
        for token in [&self.package, &self.test_filter] {
            if token.is_empty()
                || token.len() > 256
                || !token
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"_:.-".contains(&b))
            {
                return Err(
                    "package/test_filter must be a bounded Rust identifier (no shell syntax)"
                        .into(),
                );
            }
        }
        if !(1..=300).contains(&self.timeout_seconds) {
            return Err("timeout_seconds outside 1..=300".into());
        }
        if self
            .expected_failure
            .as_ref()
            .is_some_and(|text| text.trim().is_empty() || text.len() > 512)
        {
            return Err("expected_failure must be nonblank and at most 512 UTF-8 bytes".into());
        }
        for list in [&self.test_files, &self.production_files] {
            if list.is_empty() || list.len() > 32 {
                return Err("file lists need 1..=32 entries".into());
            }
            for p in list {
                relative_file(p)?;
            }
        }
        if self
            .test_files
            .iter()
            .any(|p| self.production_files.contains(p))
        {
            return Err("test and production file lists must not overlap".into());
        }
        if !matches!(self.sandbox.as_deref(), None | Some("auto" | "required")) {
            return Err("sandbox off is unavailable for experiments".into());
        }
        Ok(())
    }
}
fn arm(service: &ProcessService, dir: &str, command: &str, req: &Request) -> Result<Arm, String> {
    let request = ProcessStart {
        command: command.into(),
        workdir: Some(dir.into()),
        tty: false,
        network: false,
        sandbox: SandboxRequest::parse(Some(req.sandbox.as_deref().unwrap_or("required")))?,
    };
    let plan = service.prepare_start(&request)?;
    let root = plan.working_directory().to_owned();
    let mut files = req.test_files.clone();
    files.extend(req.production_files.clone());
    files.push("Cargo.toml".into());
    files.push("Cargo.lock".into());
    let hashes = hash_files(&root, files.iter().map(String::as_str))?;
    Ok(Arm {
        request,
        plan,
        root,
        hashes,
    })
}
fn relative_file(path: &str) -> Result<(), String> {
    let p = Path::new(path);
    if path.is_empty() || !p.components().all(|c| matches!(c, Component::Normal(_))) {
        return Err("experiment file paths must be relative, without dot/parent components".into());
    }
    Ok(())
}
fn hash_files<'a>(
    root: &Path,
    files: impl Iterator<Item = &'a str>,
) -> Result<BTreeMap<String, String>, String> {
    let mut hashes = BTreeMap::new();
    for path in files {
        relative_file(path)?;
        let full = root.join(path).canonicalize().map_err(|e| e.to_string())?;
        if !full.starts_with(root) {
            return Err("experiment file escapes copy root".into());
        }
        let mut bytes = Vec::new();
        std::fs::File::open(full)
            .map_err(|e| e.to_string())?
            .take(1024 * 1024 + 1)
            .read_to_end(&mut bytes)
            .map_err(|e| e.to_string())?;
        if bytes.len() > 1024 * 1024 {
            return Err("experiment file exceeds 1MiB".into());
        }
        hashes.insert(path.to_owned(), format!("{:x}", Sha256::digest(&bytes)));
    }
    Ok(hashes)
}
fn compare_inputs(a: &Arm, b: &Arm, req: &Request) -> Result<(), String> {
    for p in req
        .test_files
        .iter()
        .map(String::as_str)
        .chain(["Cargo.toml", "Cargo.lock"])
    {
        if a.hashes.get(p) != b.hashes.get(p) {
            return Err(format!("test/build input differs: {p}"));
        }
    }
    if !req
        .production_files
        .iter()
        .any(|p| a.hashes.get(p) != b.hashes.get(p))
    {
        return Err("no declared production counterexample difference".into());
    }
    Ok(())
}
fn run_arm(
    arm: Arm,
    service: &ProcessService,
    cancel: &CancelToken,
    timeout: Duration,
    expected_failure: Option<&str>,
) -> Result<Value, String> {
    if cancel.is_cancelled() {
        return Err("experiment cancelled before execution".into());
    }
    arm.check_inputs()?;
    let output = service.run_compat_prepared(arm.request, timeout, arm.plan)?;
    if hash_files(&arm.root, arm.hashes.keys().map(String::as_str))? != arm.hashes {
        return Err("experiment changed declared source/test inputs".into());
    }
    Ok(result::observation(output, expected_failure))
}

impl Arm {
    fn check_inputs(&self) -> Result<(), String> {
        if hash_files(&self.root, self.hashes.keys().map(String::as_str))? != self.hashes {
            return Err("experiment inputs changed after preparation".into());
        }
        Ok(())
    }
}

fn fingerprint(hashes: &BTreeMap<String, String>) -> String {
    format!(
        "{:x}",
        Sha256::digest(serde_json::to_vec(hashes).expect("hash map serializes"))
    )
}
