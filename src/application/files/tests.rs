use super::*;
fn fixture() -> (std::path::PathBuf, FileBrowser) {
    let root = std::env::temp_dir().join(format!("clat-browser-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&root).unwrap();
    std::fs::create_dir(root.join("private-db")).unwrap();
    (
        root.clone(),
        FileBrowser {
            private_storage: root.join("private-db"),
            project: Project::new(root),
        },
    )
}
#[test]
fn reference_is_a_verified_fixed_snapshot_and_deleted_or_changed_files_fail() {
    let (root, browser) = fixture();
    std::fs::write(root.join("a.rs"), "one\ntwo\nthree").unwrap();
    let p = browser.preview("a.rs", 2, 3).unwrap();
    assert_eq!(p["content"], "two\nthree");
    assert!(p["modified_at_ms"].as_u64().unwrap() <= p["read_at_ms"].as_u64().unwrap());
    let quote = browser
        .reference("a.rs", 2, 3, p["version"].as_str().unwrap())
        .unwrap();
    assert!(
        quote["reference"]
            .as_str()
            .unwrap()
            .contains("> two\n> three")
    );
    std::fs::write(root.join("a.rs"), "changed").unwrap();
    assert!(
        browser
            .reference("a.rs", 2, 3, p["version"].as_str().unwrap())
            .is_err()
    );
    assert_eq!(quote["content"], "two\nthree");
    std::fs::remove_file(root.join("a.rs")).unwrap();
    assert!(browser.preview("a.rs", 1, 1).is_err());
    crate::test_support::cleanup_tree(&root);
}
#[test]
fn paths_sensitive_files_binary_and_giant_lines_are_bounded() {
    let (root, browser) = fixture();
    for path in [
        "../outside",
        "/etc/passwd",
        ".env.local",
        "a/.git/config",
        "credentials.json",
        "x\\y",
    ] {
        assert!(browser.preview(path, 1, 1).is_err(), "{path}");
    }
    std::fs::write(root.join(".env"), "secret").unwrap();
    std::fs::write(root.join("huge"), "x".repeat(READ_CAP * 2)).unwrap();
    std::fs::write(root.join("binary"), [0, 1]).unwrap();
    assert!(browser.preview("binary", 1, 1).is_err());
    let p = browser.preview("huge", 1, 1).unwrap();
    assert_eq!(p["truncated"], true);
    assert!(p["content"].as_str().unwrap().len() <= READ_CAP);
    assert!(
        browser
            .reference("huge", 1, 1, p["version"].as_str().unwrap())
            .unwrap_err()
            .contains("32 KiB")
    );
    assert!(
        !browser.search("").unwrap()["paths"]
            .as_array()
            .unwrap()
            .contains(&json!(".env"))
    );
    assert!(browser.preview("huge", 1, 201).is_err());
    crate::test_support::cleanup_tree(&root);
}
#[cfg(unix)]
#[test]
fn file_symlink_and_cross_project_directory_symlink_are_rejected() {
    let (root, browser) = fixture();
    let outside = root.with_extension("outside");
    std::fs::create_dir(&outside).unwrap();
    std::fs::write(outside.join("secret"), "outside").unwrap();
    std::os::unix::fs::symlink(&outside, root.join("link")).unwrap();
    std::os::unix::fs::symlink(outside.join("secret"), root.join("secret")).unwrap();
    assert!(browser.preview("secret", 1, 1).is_err());
    assert!(browser.preview("link/secret", 1, 1).is_err());
    assert!(
        browser.search("secret").unwrap()["paths"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    crate::test_support::cleanup_tree(&root);
    crate::test_support::cleanup_tree(&outside);
}
