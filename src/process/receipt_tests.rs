use super::StreamKind;
use super::receipt::*;

fn fresh() -> Receipt {
    Receipt::new(Context::new(
        &["fixture".into()],
        std::path::Path::new("."),
        Some(VerificationSpec {
            format: Format::RustTestSummary,
            minimum_executed: 1,
        }),
    ))
    .unwrap()
}

#[test]
fn receipt_rust_summary_is_independent_of_read_boundaries() {
    let line = b"test result: ok. 2 passed; 0 failed; 1 ignored; 0 measured; 3 filtered out; finished in 0.01s\r\n";
    for split in 0..=line.len() {
        let mut r = fresh();
        r.append(StreamKind::Stdout, &line[..split]);
        r.append(StreamKind::Stdout, &line[split..]);
        assert_eq!(r.test_counts(), (1, 2, 0, 1, false), "split={split}");
    }
}

#[test]
fn receipt_colored_rows_keep_counts_and_malformed_summary_is_incomplete() {
    let line = b"\x1b[32mtest result: ok\x1b[0m. 2 passed; 0 failed; 1 ignored; 0 measured; 3 filtered out; finished in 0.01s\r\n";
    for split in 0..=line.len() {
        let mut r = fresh();
        r.append(StreamKind::Stdout, &line[..split]);
        r.append(StreamKind::Stdout, &line[split..]);
        assert_eq!(r.test_counts(), (1, 2, 0, 1, false), "split={split}");
        r.append(
            StreamKind::Stderr,
            b"\x1b[31mtest result: malformed\x1b[0m\n",
        );
        assert_eq!(r.test_counts(), (1, 2, 0, 1, true));
    }
}

#[test]
fn receipt_bounds_lines_summaries_and_count_overflow_without_losing_later_failure() {
    let mut r = fresh();
    r.append(StreamKind::Stdout, &vec![b'x'; 100_000]);
    r.append(StreamKind::Stdout, b"\n");
    let failure = b"test result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.01s\n";
    r.append(StreamKind::Stdout, failure);
    assert_eq!(r.test_counts(), (1, 0, 1, 0, true));
    for _ in 0..100 {
        r.append(StreamKind::Stdout, failure);
    }
    assert_eq!(r.test_counts(), (64, 0, 64, 0, true));
    let mut r = fresh();
    r.append(StreamKind::Stdout, b"test result: ok. 18446744073709551615 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.01s\n");
    r.append(StreamKind::Stdout, failure);
    assert!(
        r.test_counts().4,
        "executed count overflow must be incomplete"
    );
}

#[test]
fn ag5_playwright_summary_is_chunk_safe_format_specific_and_fail_closed() {
    let text = b"\x1b[32m  13 passed (2.5s)\x1b[0m\r\n  2 skipped\n";
    for split in 0..=text.len() {
        let mut r = Receipt::new(Context::new(
            &["fixture".into()],
            std::path::Path::new("."),
            Some(VerificationSpec {
                format: Format::PlaywrightSummary,
                minimum_executed: 1,
            }),
        ))
        .unwrap();
        r.append(StreamKind::Stdout, &text[..split]);
        r.append(StreamKind::Stdout, &text[split..]);
        assert_eq!(r.test_counts(), (2, 13, 0, 2, false), "split={split}");
        r.append(StreamKind::Stdout,b"test result: ok. 999 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.01s\n");
        assert_eq!(r.test_counts(), (2, 13, 0, 2, false));
        r.append(StreamKind::Stdout, b"  18446744073709551616 failed\n");
        assert!(
            r.test_counts().4,
            "malformed browser count must make receipt incomplete"
        );
    }
}
