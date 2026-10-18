use super::*;
fn test(expected: &str, status: &str, actual: &str) -> Value {
    json!({"expectedStatus":expected,"status":status,"results":[{"status":actual}]})
}
fn capture(tests: Vec<Value>) -> Capture {
    let bytes = json!({"errors":[],"suites":[{"specs":[{"tests":tests}],"suites":[]}]}).to_string();
    let mut c = Capture::default();
    for chunk in bytes.as_bytes().chunks(7) {
        c.append(chunk);
    }
    c
}
#[test]
fn ag9_json_receipt_expected_failures_are_not_ordinary_passes() {
    let c = capture(vec![
        test("passed", "expected", "passed"),
        test("failed", "expected", "failed"),
    ]);
    let n = c.counts(true);
    assert!(n.complete);
    assert_eq!(n.passed, 1);
    assert_eq!(n.expected_failures, 1);
    assert_eq!(n.failed, 0);
    let c = capture(vec![
        test("failed", "unexpected", "passed"),
        test("passed", "unexpected", "failed"),
        test("passed", "flaky", "passed"),
        test("skipped", "skipped", "skipped"),
    ]);
    let n = c.counts(true);
    assert!(n.complete);
    assert_eq!(
        (n.failed, n.unexpected_passes, n.flaky, n.ignored),
        (2, 1, 1, 1)
    );
}
#[test]
fn ag9_json_receipt_missing_partial_malformed_and_overflow_never_complete() {
    assert!(
        !capture(vec![test("passed", "expected", "passed")])
            .counts(false)
            .complete
    );
    for bytes in [
        b"{}".as_slice(),
        b"{\"suites\":[]}",
        b"prefix {}",
        b"{\"errors\":[],\"suites\":[{}]}",
    ] {
        let mut c = Capture::default();
        c.append(bytes);
        assert!(!c.counts(true).complete);
    }
    let mut c = capture(vec![test("passed", "expected", "passed")]);
    c.append(&vec![b' '; MAX_BYTES + 1]);
    assert!(!c.counts(true).complete);
    assert!(c.overflow);
    assert_eq!(c.bytes.len(), MAX_BYTES);
    let n = capture(vec![test("unknown", "expected", "passed")]).counts(true);
    assert!(!n.complete);
}
