use super::*;

#[test]
fn phase9_summary_rejects_completed_but_unstructured_model_output() {
    struct Unused;
    impl Model for Unused {
        fn provider(&self) -> &str {
            "test"
        }
        fn model_id(&self) -> &str {
            "test"
        }
        fn stream(
            &mut self,
            _: ModelRequest<'_>,
            _: &mut dyn crate::model::ModelEventSink,
        ) -> Result<crate::ModelResponse, crate::ModelError> {
            unreachable!()
        }
    }
    let cancel = CancelToken::new();
    let mut model = Unused;
    let policy = summary::Policy::new(&ModelConfig::default()).unwrap();
    let run = summary::SummaryRun::new(&mut model, &cancel, policy);
    let response = crate::test_support::response(
        r#"{"answer":"a complete JSON reply is not a checkpoint"}"#,
        FinishReason::Completed,
    );
    assert!(
        run.accept(response, 0, &mut String::new(), &mut 8192)
            .is_err(),
        "complete text must still satisfy the checkpoint structure"
    );
}

#[test]
fn phase8_pressure_prices_input_without_reserving_output_twice() {
    let config = ModelConfig {
        output_limit: Some(8192),
        ..Default::default()
    };
    let items = [ModelItem::user_text("preserve this instruction")];
    let refs = items.iter().collect::<Vec<_>>();
    assert_eq!(
        view_budget_tokens(&refs, "system", &[], &config) as u64,
        crate::model::estimate_request_tokens(Some("system"), &items, &[])
    );
}

#[test]
fn phase8_summary_directive_requires_all_eight_checkpoint_sections() {
    for section in [
        "Primary Request and Intent",
        "Key Technical Concepts",
        "Files and Code",
        "Errors and Fixes",
        "Pending Jobs",
        "Current Work",
        "Next Step",
        "Critical Context",
    ] {
        assert!(
            SUMMARY_INSTRUCTIONS.contains(section),
            "missing checkpoint section {section}"
        );
    }
}

#[test]
fn phase8_replaced_system_head_sequence_is_not_a_chronological_cutoff() {
    let nodes = (0..4)
        .map(|seq| CompactionNode {
            seq,
            item: ModelItem::user_text(format!("evidence {seq}")),
        })
        .collect::<Vec<_>>();
    // System heads are omitted from model items; a replacement at surface position 0 has a newer seq.
    let range = selection::historical_range(&nodes, 0, Some(99))
        .expect("ordinary history before a replaced head remains compactable");
    assert_eq!(range, 0..3);
}

#[test]
fn phase8_checkpoint_shrink_includes_the_complete_frame() {
    let nodes = vec![CompactionNode {
        seq: 1,
        item: ModelItem::user_text("source ".repeat(10)),
    }];
    let plan = plan::CompactionPlan::new(&nodes, 0..1).unwrap();
    assert!(
        plan.validate_summary("short").is_err(),
        "the framing alone must not grow the selected span"
    );
    let nodes = vec![CompactionNode {
        seq: 1,
        item: ModelItem::user_text("source ".repeat(1000)),
    }];
    let plan = plan::CompactionPlan::new(&nodes, 0..1).unwrap();
    assert!(plan.validate_summary("short").is_ok());
    assert!(plan.validate_summary(&"long ".repeat(3000)).is_err());
}
