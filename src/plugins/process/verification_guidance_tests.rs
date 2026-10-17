use super::*;

#[test]
fn ag6_verification_guidance_is_passive_bounded_and_never_overrides_receipts() {
    for (status, expected) in [
        ("failed", "failure_observed"),
        ("compile_failed", "failure_observed"),
        ("zero_matches", "inconclusive"),
    ] {
        let value = json!({"exit_code":0,"verification_witness":{"observations":[{"status":status}]},
            "command":"x".repeat(20000),"stdout":"中\"\\".repeat(10000),"command_outcome":{"kind":"success"},"execution":"shell","exit_scope":"shell"});
        let out = with_guidance(value.clone());
        assert_eq!(out["verification_guidance"]["status"], expected);
        assert_eq!(out["exit_code"], 0);
        assert_eq!(out["exit_scope"], "shell");
        assert!(out.to_string().chars().count() <= crate::plugins::THRESHOLD_CHARS);
        for patch in [
            json!({"exit_code":101}),
            json!({"running":true}),
            json!({"cancelled":true}),
            json!({"timed_out":true}),
            json!({"terminated":true}),
            json!({"signal":"9"}),
        ] {
            let mut changed = value.clone();
            changed
                .as_object_mut()
                .unwrap()
                .extend(patch.as_object().unwrap().clone());
            assert!(
                with_guidance(changed)
                    .get("verification_guidance")
                    .is_none()
            );
        }
        let mut existing = value;
        existing["verification_receipt"] = json!({"status":"failed","exit_code":0});
        let out = with_guidance(existing);
        assert!(out.get("verification_guidance").is_none());
        assert_eq!(out["verification_receipt"]["status"], "failed");
    }
}
