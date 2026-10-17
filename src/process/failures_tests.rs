use super::*;

#[test]
fn ag7_failure_accumulator_handles_chunks_colors_duplicates_and_incomplete_streams() {
    let mut failures = Failures::default();
    failures.append(StreamKind::Stdout, b"test case::split ... FAI");
    assert!(failures.json(true).is_none());
    failures.append(
        StreamKind::Stdout,
        b"LED\n\x1b[31mtest case::split ... FAILED\x1b[0m\r\n",
    );
    failures.append(StreamKind::Stderr, b"---- case::quiet stdout ----\n");
    assert_eq!(
        failures.json(true).unwrap()["tests"],
        json!(["case::split", "case::quiet"])
    );
    failures.finish(StreamKind::Stderr, false);
    assert_eq!(failures.json(false).unwrap()["incomplete"], true);
    assert!(
        failures.json(true).unwrap()["next_action"]
            .as_str()
            .unwrap()
            .contains("Wait")
    );
}

#[test]
fn ag7_failure_accumulator_bounds_untrusted_names_diagnostics_and_long_lines() {
    let mut failures = Failures::default();
    for n in 0..12 {
        failures.append(
            StreamKind::Stdout,
            format!("test case::item{n} ... FAILED\n").as_bytes(),
        );
    }
    for _ in 0..12 {
        failures.append(
            StreamKind::Stderr,
            b"thread 'x' panicked at synthetic:1:\nSYNTHETIC\n",
        );
    }
    failures.append(StreamKind::Stdout, &vec![b'x'; 100000]);
    failures.append(StreamKind::Stdout, b"\n");
    let out = failures.json(false).unwrap();
    assert_eq!(out["tests"].as_array().unwrap().len(), 8);
    assert_eq!(out["diagnostics"].as_array().unwrap().len(), 8);
    assert_eq!(out["incomplete"], true);
    assert!(out.to_string().len() < 4096);
    let mut control = Failures::default();
    for line in [
        "prefix test case::x ... FAILED\n",
        "test x;touch_marker ... FAILED\n",
        "\x1b[2Ktest case::x ... FAILED\n",
        "test case::pass ... ok\n",
    ] {
        control.append(StreamKind::Stdout, line.as_bytes());
    }
    control.append(StreamKind::Stdout, b"test case::unfinished ... FAILED");
    control.finish(StreamKind::Stdout, true);
    assert!(
        control.json(false).is_none(),
        "partial/control/forged names are not retry instructions"
    );
}
