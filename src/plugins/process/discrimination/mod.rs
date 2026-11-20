//! Opt-in paired test experiment; does not certify source coverage.
use super::*;
use std::sync::Mutex;
mod plan;
mod playwright;
mod result;
#[cfg(all(test, unix, feature = "runtime-tests"))]
mod tests;
use experiment::Pair;
mod experiment;

#[derive(Clone)]
pub(super) struct TestDiscriminationTool {
    pub(super) service: Arc<ProcessService>,
}
impl Tool for TestDiscriminationTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "test_discrimination".into(),
            description: "Check whether a test distinguishes one deliberate production defect. Run the same tests in two isolated copies: candidate (intended implementation) and counterexample (exactly one defect); keep test files and Cargo manifests identical. Default: offline Cargo test filter; optional Playwright literal argv with JSON report (no package/test_filter) and declared identical build_files. expected_failure identifies the intended assertion, required for Playwright. Mismatched test sets, unrelated failures, changed inputs, skips or retries are inconclusive. No source mutation or automatic retries. Execution permission and sandbox apply. Neither exit status, hashes nor reports authenticate coverage or build freshness.".into(),
            input_schema: json!({"type":"object","properties":{
                "candidate_dir":{"type":"string"},"counterexample_dir":{"type":"string"},
                "package":{"type":"string"},"test_filter":{"type":"string"},
                "test_files":{"type":"array","items":{"type":"string"},"minItems":1,"maxItems":32},
                "production_files":{"type":"array","items":{"type":"string"},"minItems":1,"maxItems":32},
                "playwright":{"type":"object","properties":{
                    "argv":{"type":"array","minItems":1,"maxItems":128,"items":{"type":"string"}},
                    "build_files":{"type":"array","minItems":1,"maxItems":16,"items":{"type":"string"}},
                    "artifact_files":{"type":"array","maxItems":4,"items":{"type":"string","maxLength":160}}
                },"required":["argv","build_files"],"additionalProperties":false},
                "expected_failure":{"type":"string","minLength":1,"maxLength":512,"description":"Literal marker in counterexample failure diagnostics; optional for Cargo panic evidence, required for Playwright assertion messages; max 512 UTF-8 bytes. Missing or different failure evidence is inconclusive."},
                "timeout_seconds":{"type":"integer","minimum":1,"maximum":300,"default":120},
                "sandbox":{"type":"string","enum":["auto","required"],"default":"required"}
            },"required":["candidate_dir","counterexample_dir","test_files","production_files"],"additionalProperties":false}),
            effect: ToolEffect::Execute,
            strict: true,
        }
    }
    fn prepare_execution(&self, arguments: &Value) -> Option<crate::tool::PreparedTool> {
        let pair = Pair::prepare(&self.service, arguments);
        let workspace_enforced = pair.as_ref().is_ok_and(Pair::workspace_enforced);
        Some(crate::tool::PreparedTool {
            approval: None,
            workspace_enforced,
            status: pair.as_ref().map_or_else(Clone::clone, |p| p.status()),
            tool: Arc::new(FrozenPair {
                source: self.clone(),
                arguments: arguments.clone(),
                pair: Mutex::new(Some(pair)),
            }),
        })
    }
    fn invoke(&self, a: &Value, p: &Project, c: &CancelToken) -> Result<Value, ToolError> {
        self.prepare_execution(a).unwrap().tool.invoke(a, p, c)
    }
    fn journal_arguments(&self, value: &Value) -> Value {
        playwright::journal_arguments(value)
    }
    fn journal_output(&self, value: &Value) -> Value {
        result::journal(value)
    }
}
struct FrozenPair {
    source: TestDiscriminationTool,
    arguments: Value,
    pair: Mutex<Option<Result<Pair, String>>>,
}
impl Tool for FrozenPair {
    fn definition(&self) -> ToolDefinition {
        self.source.definition()
    }
    fn journal_arguments(&self, value: &Value) -> Value {
        self.source.journal_arguments(value)
    }
    fn journal_output(&self, value: &Value) -> Value {
        self.source.journal_output(value)
    }
    fn invoke(&self, a: &Value, _: &Project, c: &CancelToken) -> Result<Value, ToolError> {
        if a != &self.arguments {
            return Err(ToolError::new("prepared arguments changed"));
        }
        let pair = self
            .pair
            .lock()
            .expect("pair plan")
            .take()
            .ok_or_else(|| ToolError::new("prepared experiment already consumed"))?
            .map_err(ToolError::new)?;
        pair.execute(&self.source.service, c)
            .map_err(ToolError::new)
    }
}
