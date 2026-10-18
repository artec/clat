use super::*;
fn report(tests: Value) -> Value {
    json!({"errors":[],"suites":[{"specs":[{"tests":tests}]}]})
}
fn passed() -> Value {
    json!({"expectedStatus":"passed","status":"expected","results":[{"status":"passed"}]})
}
#[test]
fn ag9_json_receipt_controls_real_exit_poll_and_cancel_take_precedence() {
    let f = Fixture::new();
    for (source, exit, minimum, status) in [
        (report(json!([passed()])), "0", 1, "passed"),
        (report(json!([passed()])), "0", 2, "inconclusive"),
        (report(json!([passed()])), "7", 1, "failed"),
        (report(json!([])), "0", 1, "inconclusive"),
        (json!({}), "0", 1, "inconclusive"),
        (
            report(
                json!([{"expectedStatus":"failed","status":"unexpected","results":[{"status":"passed"}]}]),
            ),
            "0",
            1,
            "failed",
        ),
        (
            report(json!([{"expectedStatus":"skipped","status":"skipped","results":[]}])),
            "0",
            1,
            "inconclusive",
        ),
        (
            report(
                json!([{"expectedStatus":"passed","status":"expected","results":[{"status":"failed"},{"status":"passed"}]}]),
            ),
            "0",
            1,
            "inconclusive",
        ),
    ] {
        std::fs::write(f.root.join("report.json"), source.to_string()).unwrap();
        let o = f.finish(
            f.exec(
                &json!({"argv":[f.program,"ag9-json","report.json",exit],"yield_time_ms":1000,
            "verification":{"format":"playwright-json","minimum_executed":minimum}}),
            )
            .unwrap(),
        );
        assert_eq!(o["verification_receipt"]["status"], status, "{o}");
        assert_eq!(o["exit_code"], exit.parse::<i32>().unwrap());
        assert!(o.to_string().chars().count() <= crate::plugins::THRESHOLD_CHARS);
    }
    std::fs::write(
        f.root.join("report.json"),
        report(json!([passed()])).to_string(),
    )
    .unwrap();
    let start = f.exec(&json!({"argv":[f.program,"ag9-json-wait","report.json"],"stdin":"pipe","yield_time_ms":250,
        "verification":{"format":"playwright-json","minimum_executed":1}})).unwrap();
    assert_eq!(start["verification_receipt"]["status"], "running");
    f.cancel.cancel();
    let done = f.finish(start);
    assert_eq!(done["verification_receipt"]["status"], "inconclusive");
    assert_eq!(done["cancelled"], true);
}
