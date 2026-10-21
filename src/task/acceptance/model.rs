use serde::{Deserialize, Serialize};
use serde_json::Value;
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Clause {
    pub id: String,
    pub required: bool,
    pub checker: String,
    pub checker_version: u32,
    pub source: String,
    pub files: Vec<String>,
    pub parameters: Value,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AcceptanceContract {
    pub version: u64,
    pub allow_model_completion: bool,
    pub clauses: Vec<Clause>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Attestation {
    pub clause: String,
    pub contract_version: u64,
    pub note: String,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileIdentity {
    pub path: String,
    pub sha256: String,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClauseResult {
    pub clause: String,
    pub checker: String,
    pub checker_version: u32,
    pub status: String,
    pub source: String,
    pub reason: String,
    pub files: Vec<FileIdentity>,
    pub operation: Option<String>,
    pub evidence_digest: Option<String>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VerificationReport {
    pub id: String,
    pub task_id: String,
    pub project_anchor: String,
    pub session: String,
    pub contract_version: u64,
    pub contract_digest: String,
    pub attestation_digest: String,
    pub status: String,
    pub results: Vec<ClauseResult>,
    pub limitations: String,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Completion {
    pub kind: String,
    pub authority: String,
    pub report: Option<String>,
    pub note: String,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CompletionCandidate {
    pub report: String,
    pub run: String,
    pub state: String,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VerificationGrant {
    pub id: String,
    pub clause: String,
    pub contract_version: u64,
    pub consumed_by: Option<String>,
}
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AcceptanceState {
    pub contract: Option<AcceptanceContract>,
    pub attestations: Vec<Attestation>,
    pub reports: Vec<VerificationReport>,
    pub candidate: Option<CompletionCandidate>,
    pub completion: Option<Completion>,
    pub grants: Vec<VerificationGrant>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommandFact {
    pub contract_version: u64,
    pub clauses: Vec<String>,
    pub invocation_digest: String,
    pub before: Vec<FileIdentity>,
    pub after: Vec<FileIdentity>,
    pub receipt: Option<Value>,
    pub receipt_digest: Option<String>,
    pub lifecycle: Value,
}

impl AcceptanceContract {
    pub(crate) fn completion_kind(&self) -> &'static str {
        let human = self
            .clauses
            .iter()
            .any(|p| p.required && p.checker == "human");
        let machine = self
            .clauses
            .iter()
            .any(|p| p.required && p.checker != "human");
        match (human, machine) {
            (true, true) => "mixed_verified",
            (true, false) => "human_attested",
            _ => "verified",
        }
    }
}
