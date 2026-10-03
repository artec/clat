use super::*;
use crate::process::capture_command;
use std::process::Command;
use std::time::Duration;

fn fixture(name: &str) -> (std::path::PathBuf, WorkspaceReview) {
    let (_, root) = crate::test_support::roots(name);
    std::fs::create_dir_all(&root).unwrap();
    let reader = WorkspaceReview {
        project: Project::new(&root),
    };
    (root, reader)
}

fn git(root: &Path, args: &[&str]) {
    let mut command = Command::new("git");
    command
        .current_dir(root)
        .args(["-c", "commit.gpgSign=false"])
        .env("GIT_CONFIG_GLOBAL", root.join("no-global-config"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .args(args);
    let result = capture_command(command, 128 * 1024, Duration::from_secs(5)).unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
}

#[test]
fn available_review_labels_existing_edits_as_workspace_not_turn_attribution() {
    let (root, reader) = fixture("workspace-review-scope");
    git(&root, &["init", "-q"]);
    std::fs::write(root.join("existing.txt"), "existing user edit\n").unwrap();
    let changes = reader.changes();
    assert_eq!(changes.state, "available");
    assert_eq!(changes.scope, "workspace");
    assert!(changes.note.contains("including existing edits"));
    assert!(changes.note.contains("not agent or run attribution"));
    assert!(changes.files.iter().any(|file| file.path == "existing.txt"));
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn status_parser_preserves_rename_and_literal_special_names() {
    let files =
        parse::parse_status("R  new name\0old name\0?? -[x]\n中文\0 M modified\0".as_bytes())
            .unwrap();
    assert_eq!(files.len(), 3);
    assert_eq!(files[0].previous_path.as_deref(), Some("old name"));
    assert!(files[0].staged);
    assert_eq!(files[1].path, "-[x]\n中文");
    assert!(files[1].untracked);
    assert!(files[2].unstaged);
    for path in [
        "/etc/passwd",
        "../secret",
        "a/../../b",
        "a\0b",
        "C:\\secret",
    ] {
        assert!(validate_path(path).is_err());
    }
}

#[test]
fn review_is_read_only_and_includes_existing_staged_unstaged_and_untracked_edits() {
    let (root, reader) = fixture("workspace-review-dirty");
    git(&root, &["init", "-q"]);
    std::fs::write(root.join("tracked.txt"), "original\n").unwrap();
    git(&root, &["add", "tracked.txt"]);
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
    std::fs::write(root.join("tracked.txt"), "staged\n").unwrap();
    git(&root, &["add", "tracked.txt"]);
    std::fs::write(root.join("tracked.txt"), "unstaged\n").unwrap();
    std::fs::write(root.join("-[x] 中文.txt"), "user or shell edit\n").unwrap();
    std::fs::write(root.join("binary"), b"\0\x01").unwrap();
    let before = std::fs::read(root.join(".git/index")).unwrap();
    let files = git::status(&reader.project).unwrap();
    let tracked = files.iter().find(|f| f.path == "tracked.txt").unwrap();
    assert!(tracked.staged && tracked.unstaged);
    let patch = git::diff(&reader.project, tracked).unwrap();
    assert!(patch.staged.contains("+staged"));
    assert!(patch.unstaged.contains("+unstaged"));
    let text = git::diff(
        &reader.project,
        files.iter().find(|f| f.path == "-[x] 中文.txt").unwrap(),
    )
    .unwrap();
    assert_eq!(text.unstaged, "user or shell edit\n");
    assert_eq!(
        git::diff(
            &reader.project,
            files.iter().find(|f| f.path == "binary").unwrap()
        )
        .unwrap()
        .state,
        "unsupported"
    );
    assert_eq!(std::fs::read(root.join(".git/index")).unwrap(), before);
    assert!(reader.diff("../secret").is_err());
    assert!(reader.diff("absent").is_err());
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn non_repository_is_not_reported_as_a_clean_worktree() {
    let (root, reader) = fixture("workspace-review-non-git");
    assert_eq!(
        git::status(&reader.project).unwrap_err(),
        "Not a Git working tree"
    );
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn nested_project_and_cross_boundary_renames_do_not_read_siblings() {
    let (root, _) = fixture("workspace-review-subdir");
    git(&root, &["init", "-q"]);
    std::fs::create_dir(root.join("sub")).unwrap();
    std::fs::write(root.join("outside"), "outside private\n").unwrap();
    std::fs::write(root.join("sub/tracked"), "base\n").unwrap();
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
    std::fs::write(root.join("sub/tracked"), "inside modified\n").unwrap();
    std::fs::write(root.join("sub/untracked"), "inside new\n").unwrap();
    std::fs::write(root.join("outside"), "outside modified\n").unwrap();
    let project = Project::new(root.join("sub"));
    let files = git::status(&project).unwrap();
    assert_eq!(files.len(), 2);
    assert!(
        files
            .iter()
            .all(|file| !file.path.contains("outside") && !file.path.starts_with("sub/"))
    );
    assert!(
        git::diff(
            &project,
            files.iter().find(|f| f.path == "tracked").unwrap()
        )
        .unwrap()
        .unstaged
        .contains("+inside modified")
    );
    assert_eq!(
        git::diff(
            &project,
            files.iter().find(|f| f.path == "untracked").unwrap()
        )
        .unwrap()
        .unstaged,
        "inside new\n"
    );
    git(&root, &["mv", "outside", "sub/moved"]);
    let files = git::status(&project).unwrap();
    let moved = files.iter().find(|f| f.path == "moved").unwrap();
    assert!(moved.previous_path.is_none());
    let patch = git::diff(&project, moved).unwrap();
    assert!(!patch.staged.contains("a/outside"));
    #[cfg(unix)]
    {
        std::fs::write(root.join("outside-secret"), "NOT TO BE READ").unwrap();
        std::os::unix::fs::symlink("../outside-secret", root.join("sub/link")).unwrap();
        let files = git::status(&project).unwrap();
        let link = files.iter().find(|f| f.path == "link").unwrap();
        assert!(git::diff(&project, link).is_err());
    }
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn external_diff_textconv_and_fsmonitor_are_not_executed() {
    let (root, reader) = fixture("workspace-review-no-external");
    git(&root, &["init", "-q"]);
    std::fs::write(root.join("tracked"), "base\n").unwrap();
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
    // These invalid drivers make any accidental execution fail loudly, on every platform.
    git(
        &root,
        &[
            "config",
            "core.fsmonitor",
            "clat-forbidden-fsmonitor-driver",
        ],
    );
    git(
        &root,
        &[
            "config",
            "diff.external",
            "clat-forbidden-external-diff-driver",
        ],
    );
    git(
        &root,
        &[
            "config",
            "diff.evil.textconv",
            "clat-forbidden-textconv-driver",
        ],
    );
    for (key, value) in [
        ("filter.evil.clean", "clat-forbidden-clean-driver"),
        ("filter.evil.process", "clat-forbidden-process-driver"),
        ("filter.evil.required", "true"),
    ] {
        git(&root, &["config", key, value]);
    }
    std::fs::write(
        root.join(".gitattributes"),
        "tracked diff=evil filter=evil\n",
    )
    .unwrap();
    std::fs::write(root.join("tracked"), "actual edit\n").unwrap();
    let files = git::status(&reader.project).unwrap();
    assert!(
        git::diff(
            &reader.project,
            files.iter().find(|f| f.path == "tracked").unwrap()
        )
        .unwrap()
        .unstaged
        .contains("+actual edit")
    );
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn deleted_and_renamed_files_and_byte_caps_remain_explicit() {
    let (root, reader) = fixture("workspace-review-rename-delete-cap");
    git(&root, &["init", "-q"]);
    std::fs::write(root.join("old"), "rename source\n").unwrap();
    std::fs::write(root.join("deleted"), "deleted source\n").unwrap();
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
    git(&root, &["mv", "old", "new"]);
    std::fs::remove_file(root.join("deleted")).unwrap();
    std::fs::write(root.join("too-big"), vec![b'a'; DIFF_CAP + 1]).unwrap();
    let files = git::status(&reader.project).unwrap();
    let renamed = files.iter().find(|f| f.path == "new").unwrap();
    assert_eq!(renamed.previous_path.as_deref(), Some("old"));
    assert!(
        git::diff(&reader.project, renamed)
            .unwrap()
            .staged
            .contains("rename from old")
    );
    assert!(
        git::diff(
            &reader.project,
            files.iter().find(|f| f.path == "deleted").unwrap()
        )
        .unwrap()
        .unstaged
        .contains("-deleted source")
    );
    assert!(
        git::diff(
            &reader.project,
            files.iter().find(|f| f.path == "too-big").unwrap()
        )
        .is_err()
    );
    std::fs::remove_dir_all(root).unwrap();
}
