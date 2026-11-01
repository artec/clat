//! Generic pipeline contract formerly covered incidentally by the destructive pruner tests.
use super::*;
struct Append(u64);
impl ToolResultTransformer for Append {
    fn transform_result(&self, result: &mut ToolResult) {
        result
            .output
            .as_array_mut()
            .unwrap()
            .push(serde_json::json!(self.0));
    }
}
#[test]
fn phase8_transformer_order_freeze_and_revocation_remain_independent_of_pruning() {
    let pipeline = ToolExecutionPipeline::new();
    let owner = crate::plugin::PluginOwner::for_test(PluginId::new("test.pipeline"));
    let first = pipeline
        .register_result_transformer(owner, Arc::new(Append(1)))
        .unwrap();
    let second = pipeline
        .register_result_transformer(owner, Arc::new(Append(2)))
        .unwrap();
    pipeline.freeze().unwrap();
    assert!(matches!(
        pipeline.register_result_transformer(owner, Arc::new(Append(3))),
        Err(ToolRegistryError::Frozen)
    ));
    let mut result = ToolResult {
        call_id: "id".into(),
        tool_name: "tool".into(),
        output: serde_json::json!([]),
        is_error: false,
        blocks: vec![],
        image_parts: vec![],
    };
    pipeline.transform_result(&mut result);
    assert_eq!(result.output, serde_json::json!([1, 2]));
    first.revoke().unwrap();
    result.output = serde_json::json!([]);
    pipeline.transform_result(&mut result);
    assert_eq!(result.output, serde_json::json!([2]));
    second.revoke().unwrap();
}
