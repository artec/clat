//! Evaluate explicitly declared contracts over bounded observation artifact bytes.
use super::*;
use serde::Deserialize;
use sha2::{Digest, Sha256};
pub(super) mod rules;
#[cfg(test)]
mod tests;
const LIMIT: usize = 256 * 1024;
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Request {
    path: String,
    checks: Vec<rules::Check>,
}
pub(super) struct VerifyObservationTool;
impl Tool for VerifyObservationTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name:"verify_observation".into(),
            description:"Evaluate a declared contract against a bounded project-relative JSON observation artifact. is_true checks independent truth, equals compares an explicit expected JSON value (including a complete step sequence), and all_before requires EVERY occurrence of before to precede EVERY occurrence of after in a string trace. Both order groups must be nonempty; missing pointers/types/anchors are inconclusive, never vacuous passes. Use actual behavioral observations, a normal control and one meaningful counterexample per claim; equality between two wrong runs or a final value alone cannot prove intermediate steps. Returns bounded per-claim results and source SHA, not raw artifact data. It does not authenticate artifact provenance, choose the correct contract, certify coverage, execute code or perform retries.".into(),
            input_schema:json!({"type":"object","properties":{
                "path":{"type":"string","maxLength":1024},
                "checks":{"type":"array","minItems":1,"maxItems":16,"items":{"oneOf":[
                    {"type":"object","properties":{"id":{"type":"string"},"op":{"const":"is_true"},"pointer":{"type":"string"}},"required":["id","op","pointer"],"additionalProperties":false},
                    {"type":"object","properties":{"id":{"type":"string"},"op":{"const":"equals"},"pointer":{"type":"string"},"expected":{}},"required":["id","op","pointer","expected"],"additionalProperties":false},
                    {"type":"object","properties":{"id":{"type":"string"},"op":{"const":"all_before"},"pointer":{"type":"string"},"before":{"type":"string"},"after":{"type":"string"}},"required":["id","op","pointer","before","after"],"additionalProperties":false}
                ]}}
            },"required":["path","checks"],"additionalProperties":false}),
            effect:ToolEffect::Read, strict:false,
        }
    }
    fn invoke(&self, a: &Value, p: &Project, c: &CancelToken) -> Result<Value, ToolError> {
        if c.is_cancelled() {
            return Err(ToolError::new("verify_observation: run cancelled"));
        }
        if a.to_string().len() > 64 * 1024 {
            return Err(ToolError::new(
                "verify_observation: arguments exceed 64 KiB",
            ));
        }
        let req: Request = serde_json::from_value(a.clone())
            .map_err(|_| ToolError::new("verify_observation: invalid contract schema"))?;
        validate(&req)?;
        let bytes = p
            .read_file_limited(&req.path, LIMIT)
            .map_err(|_| ToolError::new("verify_observation: bounded project file read failed"))?
            .ok_or_else(|| ToolError::new("verify_observation: observation file missing"))?;
        if c.is_cancelled() {
            return Err(ToolError::new("verify_observation: run cancelled"));
        }
        let source: Value = serde_json::from_slice(&bytes)
            .map_err(|_| ToolError::new("verify_observation: invalid JSON artifact"))?;
        let checks = req
            .checks
            .iter()
            .map(|check| check.evaluate(&source))
            .collect::<Vec<_>>();
        let status = if checks.iter().any(|r| r["status"] == "failed") {
            "failed"
        } else if checks.iter().any(|r| r["status"] != "passed") {
            "inconclusive"
        } else {
            "passed"
        };
        Ok(
            json!({"status":status,"evidence":"declared_observation_contract","authenticated":false,
            "source_sha256":format!("{:x}",Sha256::digest(&bytes)),"source_bytes":bytes.len(),"checks":checks,
            "scope":"Only these artifact bytes and explicit checks. Source freshness, observer validity, contract completeness and meaningful counterexamples are not authenticated."}),
        )
    }
    fn journal_arguments(&self, a: &Value) -> Value {
        json!({"path":a["path"].as_str().filter(|p| p.len() <= 1024),"check_count":a["checks"].as_array().map(Vec::len),"contract_sha256":format!("{:x}",Sha256::digest(a.to_string().as_bytes())),"contract_omitted":true})
    }
}
fn validate(req: &Request) -> Result<(), ToolError> {
    if req.path.is_empty()
        || req.path.len() > 1024
        || req.checks.is_empty()
        || req.checks.len() > 16
    {
        return Err(ToolError::new(
            "verify_observation: path/check count exceeds bounds",
        ));
    }
    rules::validate_checks(&req.checks)
}
