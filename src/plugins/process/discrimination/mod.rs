//! Opt-in paired test experiment; does not certify source coverage.
use super::*;
use std::sync::Mutex;
mod plan;
mod result;
#[cfg(all(test, unix, feature = "runtime-tests"))]
mod tests;
use plan::Pair;

#[derive(Clone)]
pub(super) struct TestDiscriminationTool {
    pub(super) service: Arc<ProcessService>,
}
impl Tool for TestDiscriminationTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "test_discrimination".into(),
            description: "Check whether a Rust unit test actually distinguishes a declared production defect. Prepare two disposable project-relative or active-scratch copies: candidate with the intended implementation, counterexample with ONE deliberate production defect; keep listed test files and Cargo manifests identical. Runs the same exact cargo unit-test filter offline in both, with fresh separate build directories. Supply expected_failure with a short assertion marker from the intended invariant to reject unrelated panic failures; merely printing that marker does not qualify. Both green means the test failed to discriminate that defect: inspect the real production entry/call path and strengthen the test, rather than treating green as coverage. No automatic source mutation, retries, or model calls. Execute permission and normal sandbox apply; cargo build scripts/tests execute code. Output is local experimental evidence, not authenticated coverage or proof that the defect is meaningful.".into(),
            input_schema: json!({"type":"object","properties":{
                "candidate_dir":{"type":"string"},"counterexample_dir":{"type":"string"},
                "package":{"type":"string"},"test_filter":{"type":"string"},
                "test_files":{"type":"array","items":{"type":"string"},"minItems":1,"maxItems":32},
                "production_files":{"type":"array","items":{"type":"string"},"minItems":1,"maxItems":32},
                "expected_failure":{"type":"string","minLength":1,"maxLength":512,"description":"Optional literal marker expected in the counterexample's Rust panic diagnostic; max 512 UTF-8 bytes. Missing or different panic evidence is inconclusive."},
                "timeout_seconds":{"type":"integer","minimum":1,"maximum":300,"default":120},
                "sandbox":{"type":"string","enum":["auto","required"],"default":"required"}
            },"required":["candidate_dir","counterexample_dir","package","test_filter","test_files","production_files"],"additionalProperties":false}),
            effect: ToolEffect::Execute,
            strict: true,
        }
    }
    fn prepare_execution(&self, arguments: &Value) -> Option<crate::tool::PreparedTool> {
        let pair = Pair::prepare(&self.service, arguments);
        let workspace_enforced = pair.as_ref().is_ok_and(Pair::workspace_enforced);
        Some(crate::tool::PreparedTool {
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
