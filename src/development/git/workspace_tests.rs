//! Read/write invariants use disposable repositories only.
use super::*;
fn nested(f: &Fixture) -> (PathBuf, String) {
    let path = f.project.join("nested");
    fs::create_dir(&path).unwrap();
    for args in [
        vec!["init", "-q", "--template="],
        vec!["config", "user.name", "Nested"],
        vec!["config", "user.email", "nested@invalid"],
        vec!["config", "commit.gpgSign", "false"],
    ] {
        assert!(
            Command::new("git")
                .current_dir(&path)
                .args(args)
                .status()
                .unwrap()
                .success()
        );
    }
    fs::write(path.join("inside.txt"), "nested\n").unwrap();
    assert!(
        Command::new("git")
            .current_dir(&path)
            .args(["add", "."])
            .status()
            .unwrap()
            .success()
    );
    assert!(
        Command::new("git")
            .current_dir(&path)
            .args(["-c", "core.hooksPath=", "commit", "-qm", "Nested root"])
            .status()
            .unwrap()
            .success()
    );
    let rows = f.call("git.repositories", json!({})).unwrap();
    let id = rows["repositories"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["path"] == "nested")
        .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    (path, id)
}
#[test]
fn development_git_repository_selection_fences_operations_and_cold_receipts() {
    let f = Fixture::new();
    f.repository();
    let (path, id) = nested(&f);
    fs::write(path.join("inside.txt"), "after\n").unwrap();
    let snapshot = f.call("git.snapshot", json!({"repositoryId":id})).unwrap();
    assert_eq!(snapshot["files"][0]["path"], "inside.txt");
    let operation = uuid::Uuid::new_v4().to_string();
    let req = json!({"repositoryId":id,"operationId":operation,"action":"stage","paths":["inside.txt"],"expected":snapshot});
    f.call("git.start", req.clone()).unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let view = f
            .call(
                "git.operation",
                json!({"repositoryId":id,"operationId":operation}),
            )
            .unwrap();
        if view["phase"] == "completed" {
            break;
        }
        assert!(Instant::now() < deadline, "{view}");
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(
        f.call("git.operation", json!({"operationId":operation}))
            .is_err()
    );
    assert!(f.git(&["diff", "--cached", "--name-only"]).is_empty());
    let reopened = Arc::new(DevelopmentService::new(
        Project::new(&f.project),
        &f.storage,
    ));
    let view = reopened.request("git.start", req, None).unwrap();
    assert_eq!(view["phase"], "completed");
    reopened.close();
    assert!(
        f.call("git.snapshot", json!({"repositoryId":"../../escape"}))
            .is_err()
    );
}
#[test]
fn development_git_historical_root_rename_and_partial_index_are_exact() {
    let f = Fixture::new();
    f.repository();
    let root = f.git(&["rev-parse", "HEAD"]);
    let files = f.call("git.commit.files", json!({"commit":root})).unwrap();
    assert_eq!(files["files"][0]["path"], "file.txt");
    assert!(files["parent"].is_null());
    let diff = f
        .call("git.diff", json!({"commit":root,"path":"file.txt"}))
        .unwrap();
    assert_eq!(diff["left"], "");
    assert_eq!(diff["right"], "before\n");
    f.git(&["mv", "file.txt", "renamed.txt"]);
    f.git(&["commit", "-qm", "Rename"]);
    let next = f.git(&["rev-parse", "HEAD"]);
    let diff = f
        .call("git.diff", json!({"commit":next,"path":"renamed.txt"}))
        .unwrap();
    assert_eq!(diff["previousPath"], "file.txt");
    assert_eq!(diff["left"], diff["right"]);
    fs::write(f.project.join("renamed.txt"), "staged\n").unwrap();
    f.git(&["add", "renamed.txt"]);
    fs::write(f.project.join("renamed.txt"), "unstaged\n").unwrap();
    let staged = f
        .call("git.diff", json!({"path":"renamed.txt","staged":true}))
        .unwrap();
    let work = f.call("git.diff", json!({"path":"renamed.txt"})).unwrap();
    assert_eq!(staged["right"], "staged\n");
    assert_eq!(work["left"], "staged\n");
    assert_eq!(work["right"], "unstaged\n");
    assert!(
        f.call(
            "git.diff",
            json!({"commit":next,"parent":"0".repeat(40),"path":"renamed.txt"})
        )
        .is_err()
    );
}
#[test]
fn development_git_graph_pages_freeze_the_revision_walk() {
    let f = Fixture::new();
    f.repository();
    for i in 0..54 {
        f.git(&["commit", "--allow-empty", "-qm", &format!("Commit {i}")]);
    }
    let first = f.call("git.history", json!({"scope":"all"})).unwrap();
    assert_eq!(first["commits"].as_array().unwrap().len(), 50);
    f.git(&["branch", "graph-label"]);
    f.git(&["commit", "--allow-empty", "-qm", "Late"]);
    let second = f
        .call("git.history", json!({"cursor":first["nextCursor"]}))
        .unwrap();
    assert_eq!(second["commits"].as_array().unwrap().len(), 5);
    let ids = first["commits"]
        .as_array()
        .unwrap()
        .iter()
        .chain(second["commits"].as_array().unwrap())
        .map(|r| r["id"].as_str().unwrap())
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(ids.len(), 55);
    let current = f.call("git.history", json!({"scope":"current"})).unwrap();
    assert_eq!(current["commits"][0]["subject"], "Late");
    assert_eq!(
        current["commits"][0]["parents"][0],
        first["commits"][0]["id"]
    );
    assert!(
        current["commits"][1]["refs"]
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r.as_str().unwrap().contains("graph-label"))
    );
}
#[cfg(unix)]
#[test]
fn development_git_repository_cache_rejects_replaced_directory_links() {
    let f = Fixture::new();
    f.repository();
    let (path, id) = nested(&f);
    f.call("git.snapshot", json!({"repositoryId":id})).unwrap();
    let outside = f.base.join("outside");
    fs::rename(&path, &outside).unwrap();
    std::os::unix::fs::symlink(&outside, &path).unwrap();
    assert!(f.call("git.snapshot", json!({"repositoryId":id})).is_err());
    let rows = f.call("git.repositories", json!({})).unwrap();
    assert!(
        !rows["repositories"]
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r["id"] == id)
    );
}

#[test]
fn development_git_merge_history_preserves_both_parents_and_deletions() {
    let f = Fixture::new();
    f.repository();
    let base = f.git(&["symbolic-ref", "--short", "HEAD"]);
    f.git(&["checkout", "-qb", "side"]);
    fs::write(f.project.join("side.txt"), "side\n").unwrap();
    f.git(&["add", "."]);
    f.git(&["commit", "-qm", "Side"]);
    f.git(&["checkout", "-q", &base]);
    f.git(&["rm", "file.txt"]);
    f.git(&["commit", "-qm", "Delete"]);
    f.git(&["merge", "--no-ff", "-qm", "Merge", "side"]);
    let history = f.call("git.history", json!({"scope":"all"})).unwrap();
    let merge = &history["commits"][0];
    assert_eq!(merge["parents"].as_array().unwrap().len(), 2);
    let second = &merge["parents"][1];
    let files = f
        .call(
            "git.commit.files",
            json!({"commit":merge["id"],"parent":second}),
        )
        .unwrap();
    assert!(
        files["files"]
            .as_array()
            .unwrap()
            .iter()
            .any(|f| f["path"] == "file.txt" && f["status"] == "D")
    );
    let diff = f
        .call(
            "git.diff",
            json!({"commit":merge["id"],"parent":second,"path":"file.txt"}),
        )
        .unwrap();
    assert_eq!(diff["left"], "before\n");
    assert_eq!(diff["right"], "");
}
#[test]
fn development_git_linked_worktree_and_uninitialized_submodule_are_discovered() {
    let f = Fixture::new();
    f.repository();
    let head = f.git(&["rev-parse", "HEAD"]);
    f.git(&["worktree", "add", "-qb", "linked", "linked"]);
    f.git(&[
        "update-index",
        "--add",
        "--cacheinfo",
        &format!("160000,{head},missing-submodule"),
    ]);
    let source = Fixture::new();
    source.repository();
    f.git(&[
        "-c",
        "protocol.file.allow=always",
        "submodule",
        "add",
        "-q",
        source.project.to_str().unwrap(),
        "module",
    ]);
    fs::write(f.project.join(".gitignore"), "linked/\nmodule/\n").unwrap();
    let list = f.call("git.repositories", json!({})).unwrap();
    let rows = list["repositories"].as_array().unwrap();
    let linked = rows.iter().find(|r| r["path"] == "linked").unwrap();
    assert_eq!(linked["available"], true);
    assert_eq!(linked["branch"], "linked");
    let snapshot = f
        .call("git.snapshot", json!({"repositoryId":linked["id"]}))
        .unwrap();
    assert_eq!(snapshot["head"], head);
    let module = rows.iter().find(|r| r["path"] == "module").unwrap();
    assert_eq!(module["kind"], "submodule");
    assert_eq!(module["available"], true);
    let missing = rows
        .iter()
        .find(|r| r["path"] == "missing-submodule")
        .unwrap();
    assert_eq!(missing["available"], false);
    let diff = f
        .call(
            "git.diff",
            json!({"path":"missing-submodule","staged":true}),
        )
        .unwrap();
    assert_eq!(diff["unsupported"], "submodule");
    assert!(Arc::ptr_eq(
        &super::super::git::test_repository_lock(&Project::new(&f.project)).unwrap(),
        &super::super::git::test_repository_lock(&Project::new(f.project.join("linked"))).unwrap()
    ));
}

#[test]
fn development_git_nested_generation_cancels_only_its_repository() {
    use std::sync::atomic::{AtomicBool, Ordering};
    let f = Fixture::new();
    f.repository();
    let (path, id) = nested(&f);
    fs::write(path.join("inside.txt"), "generate this\n").unwrap();
    assert!(
        Command::new("git")
            .current_dir(&path)
            .args(["add", "inside.txt"])
            .status()
            .unwrap()
            .success()
    );
    let snapshot = f.call("git.snapshot", json!({"repositoryId":id})).unwrap();
    let original = snapshot["head"].clone();
    let entered = Arc::new(AtomicBool::new(false));
    let release = Arc::new(AtomicBool::new(false));
    let (provider, _guard) = model(Arc::new({
        let entered = entered.clone();
        let release = release.clone();
        move |_| {
            entered.store(true, Ordering::SeqCst);
            let end = Instant::now() + Duration::from_secs(15);
            while !release.load(Ordering::SeqCst) && Instant::now() < end {
                std::thread::sleep(Duration::from_millis(5));
            }
            "Nested change".into()
        }
    }));
    let operation = uuid::Uuid::new_v4().to_string();
    let request = json!({"repositoryId":id,"operationId":operation,"action":"commit","message":"","language":"en","systemLanguage":"en","expected":snapshot});
    f.service
        .request("git.start", request.clone(), Some(provider))
        .unwrap();
    let end = Instant::now() + Duration::from_secs(15);
    while !entered.load(Ordering::SeqCst) {
        assert!(Instant::now() < end);
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(
        f.call("git.cancel", json!({"operationId":operation}))
            .is_err()
    );
    fs::write(f.project.join("file.txt"), "independent\n").unwrap();
    assert_eq!(
        f.operation("stage", json!({"paths":["file.txt"]}))["phase"],
        "completed"
    );
    f.call(
        "git.cancel",
        json!({"repositoryId":id,"operationId":operation}),
    )
    .unwrap();
    release.store(true, Ordering::SeqCst);
    loop {
        let row = f
            .call(
                "git.operation",
                json!({"repositoryId":id,"operationId":operation}),
            )
            .unwrap();
        if row["phase"] == "cancelled" {
            break;
        }
        assert!(Instant::now() < end, "{row}");
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(
        f.call("git.snapshot", json!({"repositoryId":id})).unwrap()["head"],
        original
    );
    assert_eq!(f.call("git.start", request).unwrap()["phase"], "cancelled");
}

#[test]
fn development_git_repository_scan_ignores_build_trees_without_losing_changes() {
    let f = Fixture::new();
    f.repository();
    let (_, id) = nested(&f);
    fs::write(f.project.join(".gitignore"), ".build-cache/\n").unwrap();
    let cache = f.project.join(".build-cache");
    fs::create_dir(&cache).unwrap();
    for n in 0..5100 {
        fs::create_dir(cache.join(n.to_string())).unwrap();
    }
    for n in 0..42 {
        fs::write(f.project.join(format!("change-{n}.txt")), "work\n").unwrap();
    }
    let directory = f.call("git.repositories", json!({})).unwrap();
    assert_eq!(
        directory["truncated"], false,
        "ignored build trees must not exhaust repository discovery"
    );
    assert!(
        directory["repositories"]
            .as_array()
            .unwrap()
            .iter()
            .any(|row| row["id"] == id)
    );
    let files = f.snapshot()["files"].as_array().unwrap().clone();
    assert_eq!(
        files
            .iter()
            .filter(|row| row["path"].as_str().unwrap().starts_with("change-"))
            .count(),
        42
    );
    assert!(
        !files
            .iter()
            .any(|row| row["path"].as_str().unwrap().starts_with(".build-cache"))
    );
}
