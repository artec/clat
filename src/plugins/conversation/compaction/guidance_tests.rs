use super::*;

#[test]
fn harness2_guidance_is_instance_local_and_charged_before_summary_capacity() {
    let config = ModelConfig {
        max_context_tokens: Some(24000),
        ..Default::default()
    };
    let baseline = Policy::new(&config).unwrap();
    let custom = Arc::from(format!(
        "{}\n{}",
        SUMMARY_INSTRUCTIONS,
        "BOUND_EVIDENCE ".repeat(200)
    ));
    let candidate = Policy::with_guidance(&config, custom).unwrap();
    assert!(candidate.instructions().contains("BOUND_EVIDENCE"));
    assert!(!baseline.instructions().contains("BOUND_EVIDENCE"));
    assert!(
        candidate.input < baseline.input,
        "candidate guidance must reduce input capacity"
    );
    assert_eq!(
        candidate.output, baseline.output,
        "candidate cannot change generation policy"
    );
    assert!(Policy::with_guidance(&config, Arc::from("x".repeat(100000))).is_err());
}
