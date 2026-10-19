//! Explicit paired Playwright JSON experiments through the existing process owner.
use super::*;
use serde::Deserialize;
use std::path::PathBuf;
mod inputs;
mod report;
use inputs::{Hashes, hash};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Runner {
    argv: Vec<String>,
    build_files: Vec<String>,
    #[serde(default)]
    artifact_files: Vec<String>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Request {
    candidate_dir: String,
    counterexample_dir: String,
    test_files: Vec<String>,
    production_files: Vec<String>,
    expected_failure: String,
    playwright: Runner,
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
    hashes: Hashes,
}
pub(super) struct Pair {
    candidate: Arm,
    counterexample: Arm,
    req: Request,
}
impl Request {
    fn files(&self) -> Vec<String> {
        self.test_files
            .iter()
            .chain(&self.production_files)
            .chain(&self.playwright.build_files)
            .chain(&self.playwright.artifact_files)
            .cloned()
            .collect()
    }
    fn validate(&self) -> Result<(), String> {
        if !(1..=300).contains(&self.timeout_seconds)
            || self.expected_failure.trim().is_empty()
            || self.expected_failure.len() > 512
        {
            return Err(
                "bounded timeout and nonblank expected_failure (at most 512 bytes) required".into(),
            );
        }
        if !matches!(self.sandbox.as_deref(), None | Some("auto" | "required")) {
            return Err("sandbox off is unavailable for experiments".into());
        }
        let lists = [
            (&self.test_files, 32),
            (&self.production_files, 32),
            (&self.playwright.build_files, 16),
        ];
        if lists.iter().any(|(v, max)| v.is_empty() || v.len() > *max)
            || self.playwright.artifact_files.len() > 4
        {
            return Err("experiment file lists exceed bounds".into());
        }
        let files = self.files();
        let mut seen = std::collections::HashSet::new();
        for path in &files {
            inputs::relative(path)?;
            if !seen.insert(path) {
                return Err("experiment input lists must be disjoint without duplicates".into());
            }
        }
        if !self.playwright.build_files.iter().any(|p| {
            std::path::Path::new(p)
                .file_name()
                .is_some_and(|n| n == "package.json")
        }) {
            return Err(
                "playwright build_files must include package.json and relevant lock/config files"
                    .into(),
            );
        }
        super::super::verification::Invocation::parse(&json!({"argv":self.playwright.argv}))
            .map_err(|e| e.to_string())?;
        Ok(())
    }
}
impl Pair {
    pub(super) fn prepare(service: &ProcessService, value: &Value) -> Result<Self, String> {
        if value.to_string().len() > 64 * 1024 {
            return Err("experiment arguments exceed 64 KiB".into());
        }
        let req: Request = serde_json::from_value(value.clone()).map_err(|e| e.to_string())?;
        req.validate()?;
        let candidate = arm(service, &req.candidate_dir, &req)?;
        let counterexample = arm(service, &req.counterexample_dir, &req)?;
        let live = service
            .project_root()
            .canonicalize()
            .map_err(|e| e.to_string())?;
        if candidate.root.starts_with(&counterexample.root)
            || counterexample.root.starts_with(&candidate.root)
            || candidate.root == live
            || counterexample.root == live
        {
            return Err("experiment directories must be disjoint disposable copies, not the live project root".into());
        }
        for path in req.test_files.iter().chain(&req.playwright.build_files) {
            if candidate.hashes[path] != counterexample.hashes[path] {
                return Err(format!("test/build input differs: {path}"));
            }
        }
        if !req
            .production_files
            .iter()
            .any(|p| candidate.hashes[p] != counterexample.hashes[p])
        {
            return Err("no declared production counterexample difference".into());
        }
        Ok(Self {
            candidate,
            counterexample,
            req,
        })
    }
    pub(super) fn workspace_enforced(&self) -> bool {
        [&self.candidate, &self.counterexample]
            .iter()
            .all(|a| super::super::prepared::enforced_workspace(&a.plan.planned.facts))
    }
    pub(super) fn status(&self) -> String {
        "paired literal argv Playwright JSON tests; two executions; declared source/test/artifact digests; normal Execute permission".into()
    }
    pub(super) fn execute(
        self,
        service: &ProcessService,
        cancel: &CancelToken,
    ) -> Result<Value, String> {
        self.candidate.check(&self.req)?;
        self.counterexample.check(&self.req)?;
        let frozen = [&self.candidate, &self.counterexample]
            .map(|arm| (arm.root.clone(), arm.hashes.clone()));
        let inputs = inputs::identity(
            &self.candidate.hashes,
            &self.counterexample.hashes,
            &self.req.playwright.artifact_files,
        );
        let candidate = run(self.candidate, service, cancel, &self.req)?;
        check_pair(&frozen, &self.req)?;
        if !report::candidate_passed(&candidate) {
            return Ok(report::finish(candidate, None, inputs));
        }
        let counterexample = run(self.counterexample, service, cancel, &self.req)?;
        check_pair(&frozen, &self.req)?;
        Ok(report::finish(candidate, Some(counterexample), inputs))
    }
}
fn check_pair(frozen: &[(PathBuf, Hashes); 2], req: &Request) -> Result<(), String> {
    for (root, expected) in frozen {
        if hash(root, &req.files(), &req.playwright.artifact_files)? != *expected {
            return Err("experiment inputs changed during paired execution".into());
        }
    }
    Ok(())
}
fn arm(service: &ProcessService, dir: &str, req: &Request) -> Result<Arm, String> {
    let request = ProcessStart {
        command: serde_json::to_string(&req.playwright.argv).unwrap(),
        workdir: Some(dir.into()),
        tty: false,
        network: false,
        sandbox: SandboxRequest::parse(Some(req.sandbox.as_deref().unwrap_or("required")))?,
    };
    let plan = service.prepare_direct(
        &request,
        req.playwright.argv.iter().map(Into::into).collect(),
        Some(crate::process::VerificationSpec {
            format: crate::process::Format::PlaywrightJson,
            minimum_executed: 1,
        }),
    )?;
    let root = plan.working_directory().to_owned();
    let hashes = hash(&root, &req.files(), &req.playwright.artifact_files)?;
    Ok(Arm {
        request,
        plan,
        root,
        hashes,
    })
}
impl Arm {
    fn check(&self, req: &Request) -> Result<(), String> {
        if hash(&self.root, &req.files(), &req.playwright.artifact_files)? != self.hashes {
            return Err("experiment inputs changed after preparation or execution".into());
        }
        Ok(())
    }
}
fn run(
    arm: Arm,
    service: &ProcessService,
    cancel: &CancelToken,
    req: &Request,
) -> Result<Value, String> {
    if cancel.is_cancelled() {
        return Err("experiment cancelled before execution".into());
    }
    arm.check(req)?;
    let output = service.run_compat_prepared(
        arm.request,
        Duration::from_secs(req.timeout_seconds),
        arm.plan,
    )?;
    if hash(&arm.root, &req.files(), &req.playwright.artifact_files)? != arm.hashes {
        return Err("experiment changed declared source/test/artifact inputs".into());
    }
    Ok(report::observation(
        output,
        &arm.root,
        &req.expected_failure,
    ))
}

/// Custom commands/assertion markers never become durable native call data.
pub(super) fn journal_arguments(value: &Value) -> Value {
    use sha2::{Digest, Sha256};
    if value.get("playwright").is_none() {
        return value.clone();
    }
    json!({"runner":"playwright-json","arguments_omitted":true,
        "arguments_sha256":format!("{:x}",Sha256::digest(value.to_string().as_bytes())),
        "test_file_count":value["test_files"].as_array().map(Vec::len),
        "artifact_file_count":value["playwright"]["artifact_files"].as_array().map(Vec::len)})
}
