use super::tests::fixture;
use super::*;

#[test]
fn write_guard_existing_file_requires_explicit_replacement() {
    let (root, project) = fixture();
    let original = fs::read(root.join("README.md")).unwrap();
    let result = WriteFileTool::default().invoke(
        &json!({"path":"README.md","content":"only the appended section\n"}),
        &project,
        &CancelToken::new(),
    );
    assert_eq!(
        fs::read(root.join("README.md")).unwrap(),
        original,
        "an ambiguous write must preserve all existing bytes before feedback"
    );
    let error = result
        .expect_err("existing path requires explicit replacement")
        .to_string();
    assert!(error.contains("overwrite") && error.contains("edit_file"));
    assert!(
        !error.contains("hello world"),
        "feedback never embeds old file contents"
    );
    crate::test_support::cleanup_tree(&root);
}

#[test]
fn write_guard_new_file_and_explicit_replacement_work() {
    let (root, project) = fixture();
    let writer = WriteFileTool::default();
    let created = writer
        .invoke(
            &json!({"path":"new/note","content":"new\n"}),
            &project,
            &CancelToken::new(),
        )
        .unwrap();
    assert_eq!(created["created"], true);
    let replaced = writer
        .invoke(
            &json!({"path":"new/note","content":"replacement\n","overwrite":true}),
            &project,
            &CancelToken::new(),
        )
        .unwrap();
    assert_eq!(replaced["created"], false);
    assert_eq!(
        fs::read_to_string(root.join("new/note")).unwrap(),
        "replacement\n"
    );
    crate::test_support::cleanup_tree(&root);
}

#[test]
fn write_guard_invalid_intent_never_creates_or_changes_files() {
    let (root, project) = fixture();
    for flag in [json!("true"), json!(1), json!(null)] {
        let result = WriteFileTool::default().invoke(
            &json!({"path":"invalid","content":"wrong","overwrite":flag}),
            &project,
            &CancelToken::new(),
        );
        assert!(
            result.is_err(),
            "an invalid overwrite flag cannot become implicit authority"
        );
        assert!(!root.join("invalid").exists());
    }
    crate::test_support::cleanup_tree(&root);
}

#[test]
fn write_guard_false_empty_and_cancelled_writes_preserve_bytes() {
    let (root, project) = fixture();
    fs::write(root.join("empty"), "").unwrap();
    for path in ["README.md", "empty"] {
        let before = fs::read(root.join(path)).unwrap();
        let result = WriteFileTool::default().invoke(
            &json!({"path":path,"content":"tail", "overwrite":false}),
            &project,
            &CancelToken::new(),
        );
        assert!(result.is_err());
        assert_eq!(fs::read(root.join(path)).unwrap(), before);
        let cancel = CancelToken::new();
        cancel.cancel();
        assert!(
            WriteFileTool::default()
                .invoke(
                    &json!({"path":path,"content":"replace", "overwrite":true}),
                    &project,
                    &cancel,
                )
                .is_err()
        );
        assert_eq!(fs::read(root.join(path)).unwrap(), before);
    }
    let cancel = CancelToken::new();
    cancel.cancel();
    assert!(
        WriteFileTool::default()
            .invoke(
                &json!({"path":"cancelled/child","content":"new"}),
                &project,
                &cancel,
            )
            .is_err()
    );
    assert!(!root.join("cancelled").exists());
    crate::test_support::cleanup_tree(&root);
}

#[test]
fn write_guard_concurrent_creators_publish_exactly_one_complete_file() {
    let (root, project) = fixture();
    let (send, receive) = std::sync::mpsc::channel();
    let contents = ["a".repeat(4096), "b".repeat(4096)];
    for content in contents.clone() {
        let project = project.clone();
        let send = send.clone();
        std::thread::spawn(move || {
            let result = WriteFileTool::default().invoke(
                &json!({"path":"race","content":content}),
                &project,
                &CancelToken::new(),
            );
            send.send(result.is_ok()).unwrap();
        });
    }
    drop(send);
    let results: Vec<_> = (0..2)
        .map(|_| {
            receive
                .recv_timeout(std::time::Duration::from_secs(10))
                .unwrap()
        })
        .collect();
    assert_eq!(results.iter().filter(|ok| **ok).count(), 1);
    let actual = fs::read_to_string(root.join("race")).unwrap();
    assert!(
        contents.contains(&actual),
        "no partial or combined publication"
    );
    assert!(!fs::read_dir(&root).unwrap().any(|entry| {
        entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with(".clat-tmp-")
    }));
    crate::test_support::cleanup_tree(&root);
}
