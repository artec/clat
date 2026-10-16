//! A frozen delivery must reconstruct bytes, including non-ignored new files.
use super::write_guard::response;
use super::*;
use crate::application::changes::tests::git;

struct ExportScript(Arc<Mutex<serde_json::Value>>);
impl TestModelScript for ExportScript {
    fn stream(
        &self,
        request: crate::model::ModelRequest<'_>,
        _events: &mut dyn crate::model::ModelEventSink,
    ) -> Result<crate::model::ModelResponse, crate::model::ModelError> {
        if let Some(result) = request.items.iter().rev().find_map(|item| match item {
            crate::model::ModelItem::ToolResult(result) => Some(result),
            _ => None,
        }) {
            *self.0.lock().unwrap() = result.output.clone();
            Ok(response(None, "export attempted"))
        } else {
            Ok(response(
                Some(("export_patch", json!({"path":".tmp/delivery.patch"}))),
                "",
            ))
        }
    }
}

fn fixture() -> (std::path::PathBuf, std::path::PathBuf) {
    let (storage, root) = roots("ag4-batch-export");
    std::fs::create_dir_all(&root).unwrap();
    git(&root, &["init", "-q"]);
    std::fs::write(root.join(".gitignore"), ".tmp/\nprivate/\n").unwrap();
    std::fs::write(root.join("tracked"), "before\n").unwrap();
    std::fs::write(root.join("removed"), "to delete\n").unwrap();
    std::fs::write(root.join("tracked.bin"), b"\0old").unwrap();
    git(&root, &["add", "."]);
    git(
        &root,
        &[
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@example.invalid",
            "commit",
            "-qm",
            "base",
        ],
    );
    std::fs::write(root.join("tracked"), "staged\n").unwrap();
    git(&root, &["add", "tracked"]);
    std::fs::write(root.join("tracked"), "final workspace\n").unwrap();
    std::fs::remove_file(root.join("removed")).unwrap();
    std::fs::write(root.join("新 建 note.txt"), "new file survives\n").unwrap();
    std::fs::write(root.join("new.bin"), b"\0\x01\xff").unwrap();
    std::fs::write(root.join("raw.dat"), b"\xffno NUL\n").unwrap();
    std::fs::write(root.join("tracked.bin"), b"\0new").unwrap();
    std::fs::create_dir(root.join("private")).unwrap();
    std::fs::write(root.join("private/local"), "private marker").unwrap();
    (storage, root)
}

#[test]
fn ag4_batch_export_loop_produces_reconstructable_patch_and_complete_manifest() {
    let (storage, root) = fixture();
    let original_index = std::fs::read(root.join(".git/index")).unwrap();
    let result = Arc::new(Mutex::new(json!(null)));
    let mut app = mount(
        &Project::new(&root),
        &storage,
        TestBehavior::Scripted(Arc::new(ExportScript(result.clone()))),
    );
    configure_test_model(&app);
    run(&mut app, "freeze all changes, including new files").unwrap();
    app.close().unwrap();
    let output = result.lock().unwrap();
    assert!(
        root.join(".tmp/delivery.patch.manifest.json").exists(),
        "successful delivery requires a manifest for a real complete patch: {output}"
    );
    let patch = std::fs::read(root.join(".tmp/delivery.patch")).unwrap();
    let manifest: serde_json::Value = serde_json::from_slice(
        &std::fs::read(root.join(".tmp/delivery.patch.manifest.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(manifest["files"].as_array().unwrap().len(), 6);
    assert_eq!(
        manifest["patch_sha256"],
        format!("{:x}", sha2::Sha256::digest(&patch))
    );
    assert_eq!(output["file_count"], 6);
    for entry in manifest["files"].as_array().unwrap() {
        let path = root.join(entry["path"].as_str().unwrap());
        if path.exists() {
            let bytes = std::fs::read(path).unwrap();
            assert_eq!(
                entry["sha256"],
                format!("{:x}", sha2::Sha256::digest(&bytes))
            );
            assert_eq!(entry["bytes"], bytes.len());
        } else {
            assert!(entry["sha256"].is_null());
        }
    }

    assert!(output.to_string().len() < 2048 && !output.to_string().contains("private marker"));
    assert!(!String::from_utf8_lossy(&patch).contains("private marker"));
    assert_eq!(
        std::fs::read(root.join(".git/index")).unwrap(),
        original_index
    );
    let replica = root.parent().unwrap().join("replica");
    git(
        &root,
        &[
            "clone",
            "-q",
            "--local",
            root.to_str().unwrap(),
            replica.to_str().unwrap(),
        ],
    );
    git(
        &replica,
        &[
            "apply",
            "--check",
            root.join(".tmp/delivery.patch").to_str().unwrap(),
        ],
    );
    git(
        &replica,
        &["apply", root.join(".tmp/delivery.patch").to_str().unwrap()],
    );
    for path in [
        "tracked",
        "新 建 note.txt",
        "new.bin",
        "raw.dat",
        "tracked.bin",
    ] {
        assert_eq!(
            std::fs::read(replica.join(path)).unwrap(),
            std::fs::read(root.join(path)).unwrap()
        );
    }
    assert!(!replica.join("removed").exists());
    assert!(!replica.join("private").exists());
    assert_eq!(
        load_events(&storage)
            .iter()
            .filter(|event| event.event_type == "tool/call")
            .count(),
        1
    );
    let results: Vec<_> = load_events(&storage)
        .into_iter()
        .filter(|e| e.event_type == "tool/result")
        .collect();
    assert_eq!(results.len(), 1);
    assert!(results[0].data.to_string().len() < 3000);
    assert!(!results[0].data.to_string().contains("new file survives"));
    crate::test_support::cleanup_tree(storage.parent().unwrap());
}

use sha2::Digest;

#[test]
fn ag4_batch_export_denied_permission_publishes_nothing() {
    let (storage, root) = fixture();
    let result = Arc::new(Mutex::new(json!(null)));
    let mut app = mount(
        &Project::new(&root),
        &storage,
        TestBehavior::Scripted(Arc::new(ExportScript(result))),
    );
    configure_test_model(&app);
    run_with_approver(
        &mut app,
        "export",
        Arc::new(
            |_request: crate::PermissionRequest, _cancel: &crate::CancelToken| {
                crate::PermissionDecision::Deny {
                    reason: "not authorized".into(),
                }
            },
        ),
    )
    .unwrap();
    app.close().unwrap();
    assert!(!root.join(".tmp/delivery.patch").exists());
    assert!(
        !load_events(&storage)
            .iter()
            .any(|e| e.event_type == "tool/call")
    );
    crate::test_support::cleanup_tree(storage.parent().unwrap());
}

#[test]
fn ag4_batch_export_unsafe_targets_cancellation_and_existing_evidence_are_preserved() {
    let (storage, root) = fixture();
    let project = Project::new(&root);
    for path in ["../escape", "published.patch", ".tmp/", ".tmp/."] {
        assert!(
            project
                .export_patch(path, &crate::CancelToken::new())
                .is_err()
        );
    }
    assert!(!root.join("published.patch").exists());
    let cancel = crate::CancelToken::new();
    cancel.cancel();
    assert!(
        project
            .export_patch(".tmp/cancelled.patch", &cancel)
            .is_err()
    );
    assert!(!root.join(".tmp/cancelled.patch").exists());
    std::fs::create_dir_all(root.join(".tmp")).unwrap();
    std::fs::write(root.join(".tmp/old.patch"), "old evidence").unwrap();
    assert!(
        project
            .export_patch(".tmp/old.patch", &crate::CancelToken::new())
            .is_err()
    );
    assert_eq!(
        std::fs::read_to_string(root.join(".tmp/old.patch")).unwrap(),
        "old evidence"
    );
    assert!(!root.join(".tmp/old.patch.manifest.json").exists());
    crate::test_support::cleanup_tree(storage.parent().unwrap());
}

#[test]
fn ag4_batch_export_conversion_oversize_and_nonregular_sources_never_claim_success() {
    let (storage, root) = fixture();
    let project = Project::new(&root);
    std::fs::write(root.join(".gitattributes"), "tracked text eol=lf\n").unwrap();
    std::fs::write(root.join("tracked"), "final workspace\r\n").unwrap();
    let error = project
        .export_patch(".tmp/refused.patch", &crate::CancelToken::new())
        .unwrap_err();
    assert!(error.contains("conversion"), "{error}");
    std::fs::write(root.join("tracked"), "final workspace\n").unwrap();
    std::fs::write(root.join("too-large"), vec![b'x'; 1024 * 1024 + 1]).unwrap();
    assert!(
        project
            .export_patch(".tmp/refused.patch", &crate::CancelToken::new())
            .unwrap_err()
            .contains("exceeds")
    );
    std::fs::remove_file(root.join("too-large")).unwrap();
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(root.join("tracked"), root.join("link")).unwrap();
        assert!(
            project
                .export_patch(".tmp/refused.patch", &crate::CancelToken::new())
                .is_err()
        );
    }
    assert!(!root.join(".tmp/refused.patch").exists());
    assert!(!root.join(".tmp/refused.patch.manifest.json").exists());
    crate::test_support::cleanup_tree(storage.parent().unwrap());
}
