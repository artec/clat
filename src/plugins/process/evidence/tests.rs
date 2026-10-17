use super::*;

#[test]
fn ag7_evidence_create_only_publication_never_replaces_an_external_entry() {
    let root = super::super::sbx4_tests::root();
    let project = Project::new(&root);
    let capture = Capture::prepare(
        &json!({"evidence":{"directory":"records"}}),
        &project,
        root.to_str().unwrap(),
        &CancelToken::new(),
    )
    .unwrap()
    .unwrap();
    let path = root.join(&capture.path);
    std::fs::write(&path, "external owner").unwrap();
    assert_eq!(capture.publish("synthetic record")["status"], "failed");
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "external owner");
    crate::test_support::cleanup_tree(&root);
}
