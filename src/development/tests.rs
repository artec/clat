//! Invariant-driven core tests. All mutations use disposable projects/remotes.
use super::*;
use serde_json::json;
use std::{
    fs,
    process::Command,
    time::{Duration, Instant},
};
struct Fixture {
    base: PathBuf,
    project: PathBuf,
    storage: PathBuf,
    service: Arc<DevelopmentService>,
}
impl Fixture {
    fn new() -> Self {
        let base = std::env::temp_dir().join(format!("clat-human-{}", uuid::Uuid::new_v4()));
        let project = base.join("project");
        let storage = base.join("private");
        fs::create_dir_all(&project).unwrap();
        let service = Arc::new(DevelopmentService::new(Project::new(&project), &storage));
        Self {
            base,
            project,
            storage,
            service,
        }
    }
    fn call(&self, method: &str, value: Value) -> Result<Value, String> {
        self.service.request(method, value, None)
    }
    fn git(&self, args: &[&str]) -> String {
        let mut command = Command::new("git");
        command
            .current_dir(&self.project)
            .args(args)
            .env("GIT_CONFIG_NOSYSTEM", "1");
        let out =
            crate::process::capture_command(command, 16 * 1024 * 1024, Duration::from_secs(10))
                .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).trim().into()
    }
    fn repository(&self) {
        self.git(&["init", "-q", "--template="]);
        self.git(&["config", "core.autocrlf", "false"]);
        self.git(&["config", "commit.gpgSign", "false"]);
        self.git(&["config", "core.hooksPath", ".git/hooks"]);
        self.git(&["config", "user.name", "Core Test"]);
        self.git(&["config", "user.email", "test@invalid"]);
        fs::write(self.project.join("file.txt"), "before\n").unwrap();
        self.git(&["add", "--", "file.txt"]);
        self.git(&["-c", "commit.gpgSign=false", "commit", "-qm", "Initial"]);
    }
    fn snapshot(&self) -> Value {
        self.call("git.snapshot", json!({})).unwrap()
    }
    fn operation(&self, action: &str, extra: Value) -> Value {
        let mut req = json!({"operationId":uuid::Uuid::new_v4().to_string(),"action":action,"expected":self.snapshot()});
        for (k, v) in extra.as_object().unwrap() {
            req[k] = v.clone();
        }
        self.run(req, None)
    }
    fn run(&self, req: Value, model: Option<GitModel>) -> Value {
        let mut row = self
            .service
            .request("git.start", req.clone(), model)
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(15);
        while matches!(
            row["phase"].as_str(),
            Some("pending" | "running" | "generating" | "committing")
        ) {
            assert!(Instant::now() < deadline, "{row}");
            std::thread::sleep(Duration::from_millis(10));
            row = self
                .call("git.operation", json!({"operationId":req["operationId"]}))
                .unwrap();
        }
        row
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.service.close();
        let _ = fs::remove_dir_all(&self.base);
    }
}
#[test]
fn development_editor_preserves_bom_crlf_and_fences_external_writer() {
    let f = Fixture::new();
    fs::write(f.project.join("file.txt"), b"\xef\xbb\xbfone\r\ntwo\r\n").unwrap();
    let doc = f.call("editor.open", json!({"path":"file.txt"})).unwrap();
    let req = json!({"operationId":"save-one","path":"file.txt","version":doc["version"],"text":"changed\nline\n"});
    let saved = f.call("editor.save", req.clone()).unwrap();
    assert_eq!(
        fs::read(f.project.join("file.txt")).unwrap(),
        b"\xef\xbb\xbfchanged\r\nline\r\n"
    );
    fs::write(f.project.join("file.txt"), "external").unwrap();
    assert_eq!(f.call("editor.save", req).unwrap(), saved);
    let stale = json!({"operationId":"save-two","path":"file.txt","version":saved["version"],"text":"lost"});
    assert_eq!(f.call("editor.save", stale).unwrap_err(), "file_conflict");
    assert_eq!(
        fs::read_to_string(f.project.join("file.txt")).unwrap(),
        "external"
    );
}
#[test]
fn development_editor_encoding_and_size_are_readonly() {
    let f = Fixture::new();
    for (name, bytes, reason) in [
        ("mixed", b"a\r\nb\n".to_vec(), "file_newlines"),
        ("binary", vec![0, 255], "file_encoding"),
        ("large", vec![b'a'; 2 * 1024 * 1024 + 1], "file_large"),
    ] {
        fs::write(f.project.join(name), bytes).unwrap();
        let doc = f.call("editor.open", json!({"path":name})).unwrap();
        assert_eq!(doc["editable"], false);
        assert_eq!(doc["reason"], reason);
    }
}
#[test]
fn development_drafts_cold_restore_and_client_project_isolation() {
    let f = Fixture::new();
    let req = json!({"clientId":"first","expectedVersion":0,"path":"a.rs","version":"old","text":"unsaved","bom":false,"newline":"lf"});
    f.call("editor.draft.save", req.clone()).unwrap();
    assert_eq!(
        f.call("editor.draft.save", req).unwrap_err(),
        "draft_conflict"
    );
    assert_eq!(
        f.call("editor.drafts", json!({"clientId":"second"}))
            .unwrap()["documents"],
        json!([])
    );
    let reopened = Arc::new(DevelopmentService::new(
        Project::new(&f.project),
        &f.storage,
    ));
    let docs = reopened
        .request("editor.drafts", json!({"clientId":"first"}), None)
        .unwrap();
    assert_eq!(docs["documents"][0]["text"], "unsaved");
    assert!(!f.project.join("a.rs").exists());
    let other = Fixture::new();
    assert_eq!(
        other
            .call("editor.drafts", json!({"clientId":"first"}))
            .unwrap()["documents"],
        json!([])
    );
}
#[test]
fn development_file_rename_trash_restore_are_no_clobber() {
    let f = Fixture::new();
    fs::create_dir(f.project.join("folder")).unwrap();
    fs::write(f.project.join("folder/a"), "kept").unwrap();
    fs::write(f.project.join("exists"), "foreign").unwrap();
    let stat = f.call("editor.inspect", json!({"path":"folder"})).unwrap();
    assert!(f.call("editor.rename", json!({"operationId":"rename-conflict","path":"folder","target":"exists","version":stat["version"]})).is_err());
    assert_eq!(
        fs::read_to_string(f.project.join("exists")).unwrap(),
        "foreign"
    );
    assert_eq!(
        fs::read_to_string(f.project.join("folder/a")).unwrap(),
        "kept"
    );
    let moved=f.call("editor.rename",json!({"operationId":"rename","path":"folder","target":"renamed","version":stat["version"]})).unwrap();
    assert_eq!(moved["path"], "renamed");
    let stat = f.call("editor.inspect", json!({"path":"renamed"})).unwrap();
    f.call(
        "editor.delete",
        json!({"operationId":"trash","path":"renamed","version":stat["version"],"confirmed":true}),
    )
    .unwrap();
    assert!(!f.project.join("renamed").exists());
    fs::write(f.project.join("renamed"), "new").unwrap();
    assert!(
        f.call(
            "editor.restore",
            json!({"operationId":"restore-conflict","path":"renamed","backupId":"trash"})
        )
        .is_err()
    );
    assert_eq!(
        fs::read_to_string(f.project.join("renamed")).unwrap(),
        "new"
    );
    fs::remove_file(f.project.join("renamed")).unwrap();
    f.call(
        "editor.restore",
        json!({"operationId":"restore","path":"renamed","backupId":"trash"}),
    )
    .unwrap();
    assert_eq!(
        fs::read_to_string(f.project.join("renamed/a")).unwrap(),
        "kept"
    );
}
#[cfg(unix)]
#[test]
fn development_editor_rejects_escape_private_and_symlink_targets() {
    let f = Fixture::new();
    let outside = f.base.join("outside");
    fs::write(&outside, "secret").unwrap();
    std::os::unix::fs::symlink(&outside, f.project.join("escape")).unwrap();
    for path in ["../outside", "escape", ".git/config"] {
        assert!(f.call("editor.open", json!({"path":path})).is_err());
    }
    fs::create_dir_all(&f.storage).unwrap();
    std::os::unix::fs::symlink(&f.storage, f.project.join("private")).unwrap();
    assert!(
        f.call(
            "editor.create",
            json!({"operationId":"private","path":"private/secret","directory":false})
        )
        .is_err()
    );
    assert!(!f.storage.join("secret").exists());
}
#[test]
fn development_git_stage_commit_exact_index_and_duplicate_reopen() {
    let f = Fixture::new();
    f.repository();
    fs::write(f.project.join("file.txt"), "staged\n").unwrap();
    assert_eq!(
        f.operation("stage", json!({"paths":["file.txt"]}))["phase"],
        "completed"
    );
    fs::write(f.project.join("file.txt"), "unstaged\n").unwrap();
    let req = json!({"operationId":"commit-once","action":"commit","expected":f.snapshot(),"message":"Keep staged content"});
    let row = f.run(req.clone(), None);
    assert_eq!(row["phase"], "completed", "{row}");
    assert_eq!(f.git(&["show", "HEAD:file.txt"]), "staged");
    assert_eq!(
        fs::read_to_string(f.project.join("file.txt")).unwrap(),
        "unstaged\n"
    );
    let head = f.git(&["rev-parse", "HEAD"]);
    assert_eq!(f.run(req.clone(), None)["phase"], "completed");
    let reopened = Arc::new(DevelopmentService::new(
        Project::new(&f.project),
        &f.storage,
    ));
    assert_eq!(
        reopened.request("git.start", req, None).unwrap()["phase"],
        "completed"
    );
    assert_eq!(head, f.git(&["rev-parse", "HEAD"]));
}
#[test]
fn development_git_rejects_changed_worktree_even_if_porcelain_status_same() {
    let f = Fixture::new();
    f.repository();
    fs::write(f.project.join("file.txt"), "first edit\n").unwrap();
    let expected = f.snapshot();
    fs::write(f.project.join("file.txt"), "other edit\n").unwrap();
    let row=f.run(json!({"operationId":"stale-stage","action":"stage","expected":expected,"paths":["file.txt"]}),None);
    assert_eq!(row["phase"], "failed", "{row}");
    assert_eq!(f.git(&["diff", "--cached"]), "");
}
#[test]
fn development_git_discard_restores_binary_and_preserves_later_edits() {
    for autocrlf in [false, true] {
        let f = Fixture::new();
        f.repository();
        f.git(&[
            "config",
            "core.autocrlf",
            if autocrlf { "true" } else { "false" },
        ]);
        fs::write(f.project.join("file.txt"), b"binary\0bytes").unwrap();
        let row = f.operation("discard", json!({"paths":["file.txt"],"confirmed":true}));
        assert_eq!(row["phase"], "completed", "{row}");
        let id = row["id"].clone();
        let restored = fs::read(f.project.join("file.txt")).unwrap();
        fs::write(f.project.join("file.txt"), "later").unwrap();
        let failed = f.operation("restore", json!({"backupId":id,"confirmed":true}));
        assert_eq!(failed["phase"], "failed");
        assert_eq!(
            fs::read_to_string(f.project.join("file.txt")).unwrap(),
            "later"
        );
        fs::write(f.project.join("file.txt"), restored).unwrap();
        let row = f.operation("restore", json!({"backupId":id,"confirmed":true}));
        assert_eq!(row["phase"], "completed", "{row}");
        assert_eq!(
            fs::read(f.project.join("file.txt")).unwrap(),
            b"binary\0bytes"
        );
    }
}
#[test]
fn development_git_branch_history_and_local_remote_roundtrip() {
    let f = Fixture::new();
    f.repository();
    assert_eq!(
        f.operation("branch.create", json!({"branch":"feature"}))["phase"],
        "completed"
    );
    assert_eq!(
        f.operation("branch.switch", json!({"branch":"feature"}))["phase"],
        "completed"
    );
    assert_eq!(f.snapshot()["branch"], "feature");
    assert_eq!(
        f.call("git.history", json!({})).unwrap()["commits"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    let remote = f.base.join("remote.git");
    let status = Command::new("git")
        .args(["init", "--bare", "-q"])
        .arg(&remote)
        .status()
        .unwrap();
    assert!(status.success());
    f.git(&["remote", "add", "origin", remote.to_str().unwrap()]);
    f.git(&["config", "push.default", "current"]);
    assert_eq!(f.operation("push", json!({}))["phase"], "completed");
    assert_eq!(f.operation("fetch", json!({}))["phase"], "completed");
}
type Script = Arc<dyn Fn(crate::ModelRequest<'_>) -> String + Send + Sync>;
struct Factory(Script);
struct Scripted(Script);
impl crate::ModelFactory for Factory {
    fn protocol(&self) -> crate::ModelProtocol {
        crate::ModelProtocol::OpenAiCompatible
    }
    fn describe(&self, _: &crate::ProviderCredentials) -> crate::ProviderDescriptor {
        crate::ProviderDescriptor {
            protocol: self.protocol(),
            display_name: "test".into(),
            fields: vec![],
        }
    }
    fn build(
        &self,
        _: &crate::ModelConfig,
        _: &crate::ProviderCredentials,
    ) -> Result<Box<dyn crate::Model>, crate::ModelError> {
        Ok(Box::new(Scripted(self.0.clone())))
    }
}
impl crate::Model for Scripted {
    fn provider(&self) -> &str {
        "test"
    }
    fn model_id(&self) -> &str {
        "test"
    }
    fn stream(
        &mut self,
        request: crate::ModelRequest<'_>,
        _: &mut dyn crate::ModelEventSink,
    ) -> Result<crate::ModelResponse, crate::ModelError> {
        assert!(request.tools.is_empty());
        Ok(crate::test_support::response(
            &(self.0)(request),
            crate::FinishReason::Completed,
        ))
    }
}
fn model(script: Script) -> (GitModel, Box<dyn Send>) {
    let providers = Arc::new(crate::plugins::services::ProviderRegistry::new());
    let lease = providers
        .register(
            crate::plugin::PluginOwner::for_test(crate::plugin::PluginId::new("test.human-git")),
            Arc::new(Factory(script)),
        )
        .unwrap();
    (
        GitModel {
            providers,
            config: crate::ModelConfig::default(),
            credentials: crate::ProviderCredentials::default(),
        },
        Box::new(lease),
    )
}
#[test]
fn development_generated_commit_uses_staged_data_and_requested_languages() {
    for (language, system, expected) in [
        ("en", "zh-CN", "English"),
        ("zh-Hans", "en", "Simplified Chinese"),
        ("zh-Hant", "en", "Traditional Chinese"),
        ("system", "zh-TW", "Traditional Chinese"),
    ] {
        let f = Fixture::new();
        f.repository();
        // Force stat-cache reuse deterministically: same byte length and
        // timestamp, with ctime ignored. The model must still receive blobs
        // from the staged index rather than the live worktree optimization.
        f.git(&["config", "core.trustctime", "false"]);
        f.git(&["config", "core.checkstat", "minimal"]);
        fs::write(f.project.join("file.txt"), "STAGED_EVIDENCE\n").unwrap();
        let modified = std::time::SystemTime::now() - Duration::from_secs(60);
        let times = std::fs::FileTimes::new().set_modified(modified);
        fs::File::options()
            .write(true)
            .open(f.project.join("file.txt"))
            .unwrap()
            .set_times(times)
            .unwrap();
        f.git(&["add", "file.txt"]);
        fs::write(f.project.join("file.txt"), "UNSTAGED_SECRET\n").unwrap();
        fs::File::options()
            .write(true)
            .open(f.project.join("file.txt"))
            .unwrap()
            .set_times(times)
            .unwrap();
        let script: Script = Arc::new(move |request| {
            assert!(request.instructions.unwrap().contains(expected));
            let input = format!("{:?}", request.items);
            assert!(input.contains("STAGED_EVIDENCE"), "{input}");
            assert!(!input.contains("UNSTAGED_SECRET"));
            "Describe approved changes".into()
        });
        let (model, _lease) = model(script);
        let row=f.run(json!({"operationId":dev_id(),"action":"commit","expected":f.snapshot(),"message":"  ","language":language,"systemLanguage":system}),Some(model));
        assert_eq!(row["phase"], "completed", "{row}");
        assert_eq!(
            f.git(&["log", "-1", "--format=%s"]),
            "Describe approved changes"
        );
    }
}
fn dev_id() -> String {
    uuid::Uuid::new_v4().to_string()
}
#[test]
fn development_generation_failure_and_index_change_cannot_commit() {
    for change in [false, true] {
        let f = Fixture::new();
        f.repository();
        fs::write(f.project.join("file.txt"), "approved\n").unwrap();
        f.git(&["add", "file.txt"]);
        let head = f.git(&["rev-parse", "HEAD"]);
        let root = f.project.clone();
        let script: Script = Arc::new(move |_| {
            if change {
                fs::write(root.join("file.txt"), "different\n").unwrap();
                assert!(
                    Command::new("git")
                        .current_dir(&root)
                        .args(["add", "file.txt"])
                        .status()
                        .unwrap()
                        .success()
                );
                "Old description".into()
            } else {
                String::new()
            }
        });
        let (model, _lease) = model(script);
        let row = f.run(
            json!({"operationId":dev_id(),"action":"commit","expected":f.snapshot(),"message":""}),
            Some(model),
        );
        assert_eq!(row["phase"], "failed", "{row}");
        assert_eq!(f.git(&["rev-parse", "HEAD"]), head);
        assert!(!f.git(&["diff", "--cached"]).is_empty());
        if change {
            assert_eq!(row["message"], "Old description");
        }
    }
}
#[test]
fn development_generation_only_and_cancel_preserve_history() {
    let f = Fixture::new();
    f.repository();
    fs::write(f.project.join("file.txt"), "approved\n").unwrap();
    f.git(&["add", "file.txt"]);
    let head = f.git(&["rev-parse", "HEAD"]);
    let (model, _lease) = model(Arc::new(|request| {
        request.cancel.cancel();
        "Should not commit".into()
    }));
    let row = f.run(
        json!({"operationId":dev_id(),"action":"commit","expected":f.snapshot(),"message":""}),
        Some(model),
    );
    assert_eq!(row["phase"], "cancelled");
    assert_eq!(f.git(&["rev-parse", "HEAD"]), head);
    let (model, _lease) = self::model(Arc::new(|_| "Preview only".into()));
    let row = f.run(
        json!({"operationId":dev_id(),"action":"generate","expected":f.snapshot()}),
        Some(model),
    );
    assert_eq!(row["phase"], "completed");
    assert_eq!(row["message"], "Preview only");
    assert_eq!(f.git(&["rev-parse", "HEAD"]), head);
}
#[cfg(unix)]
#[test]
fn development_git_honors_hooks_and_filters_on_human_writes() {
    use std::os::unix::fs::PermissionsExt;
    let f = Fixture::new();
    f.repository();
    fs::write(
        f.project.join(".gitattributes"),
        "file.txt filter=example\n",
    )
    .unwrap();
    f.git(&["config", "filter.example.clean", "tr a-z A-Z"]);
    fs::write(f.project.join("file.txt"), "lowercase\n").unwrap();
    assert_eq!(
        f.operation("stage", json!({"paths":["file.txt"]}))["phase"],
        "completed"
    );
    assert_eq!(f.git(&["show", ":file.txt"]), "LOWERCASE");
    let hook = f.project.join(".git/hooks/pre-commit");
    fs::create_dir_all(hook.parent().unwrap()).unwrap();
    fs::write(&hook, "#!/bin/sh\nprintf 'called' > hook-marker\n").unwrap();
    fs::set_permissions(&hook, fs::Permissions::from_mode(0o700)).unwrap();
    let row = f.operation("commit", json!({"message":"Human configured workflow"}));
    assert_eq!(row["phase"], "completed", "{row}");
    assert_eq!(
        fs::read_to_string(f.project.join("hook-marker")).unwrap(),
        "called"
    );
}
#[cfg(unix)]
#[test]
fn development_pty_single_writer_resize_replay_and_close() {
    use base64::Engine;
    let f = Fixture::new();
    let row = f
        .call(
            "terminal.create",
            json!({"operationId":"terminal-one","clientId":"first","shell":"/bin/sh"}),
        )
        .unwrap();
    assert_eq!(row["id"], "terminal-one");
    assert_eq!(
        f.call(
            "terminal.create",
            json!({"operationId":"terminal-one","clientId":"first","shell":"/bin/sh"})
        )
        .unwrap()["id"],
        "terminal-one"
    );
    assert_eq!(
        f.call(
            "terminal.create",
            json!({"operationId":"terminal-one","clientId":"second","shell":"/bin/sh"})
        )
        .unwrap_err(),
        "operation_conflict"
    );
    assert_eq!(
        f.call(
            "terminal.claim",
            json!({"id":"terminal-one","clientId":"second"})
        )
        .unwrap()["controlled"],
        false
    );
    let input = json!({"id":"terminal-one","clientId":"first","lease":1,"sequence":0,"data":"printf 'ONE_MARKER\\n'\n"});
    f.call("terminal.input", input.clone()).unwrap();
    f.call("terminal.input", input).unwrap();
    f.call(
        "terminal.resize",
        json!({"id":"terminal-one","clientId":"first","lease":1,"rows":33,"cols":111}),
    )
    .unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    let captured = loop {
        let row = f.service.terminal_output("terminal-one", 0).unwrap();
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(row["data"].as_str().unwrap())
            .unwrap();
        if String::from_utf8_lossy(&bytes).contains("ONE_MARKER\r\n") {
            break bytes;
        }
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(20));
    };
    assert!(!captured.is_empty());
    let claim = f
        .call(
            "terminal.claim",
            json!({"id":"terminal-one","clientId":"second","takeover":true}),
        )
        .unwrap();
    assert_eq!(claim["lease"], 2);
    assert!(
        f.call(
            "terminal.input",
            json!({"id":"terminal-one","clientId":"first","lease":1,"sequence":1,"data":"bad"})
        )
        .is_err()
    );
    let other = Fixture::new();
    assert!(other.service.terminal_output("terminal-one", 0).is_err());
    f.call("terminal.close", json!({"id":"terminal-one"}))
        .unwrap();
    assert_eq!(
        f.call("terminal.list", json!({})).unwrap()["terminals"][0]["phase"],
        "ended"
    );
    let reopened = Arc::new(DevelopmentService::new(
        Project::new(&f.project),
        &f.storage,
    ));
    assert_eq!(
        reopened.request("terminal.list", json!({}), None).unwrap()["terminals"][0]["phase"],
        "ended"
    );
    assert!(reopened.terminal_output("terminal-one", 0).is_err());
}

#[test]
fn development_unborn_index_unstage_preserves_both_disk_and_no_history() {
    let f = Fixture::new();
    f.git(&["init", "--template=", "-q"]);
    fs::write(f.project.join("first"), "staged").unwrap();
    f.git(&["add", "first"]);
    fs::write(f.project.join("first"), "newer").unwrap();
    let row = f.operation("unstage", json!({"paths":["first"]}));
    assert_eq!(row["phase"], "completed", "{row}");
    assert_eq!(f.git(&["ls-files"]), "");
    assert_eq!(
        fs::read_to_string(f.project.join("first")).unwrap(),
        "newer"
    );
}
#[cfg(unix)]
#[test]
fn development_terminal_close_reaps_shell_background_jobs() {
    let f = Fixture::new();
    f.call(
        "terminal.create",
        json!({"operationId":"children","clientId":"owner","shell":"/bin/sh"}),
    )
    .unwrap();
    f.call("terminal.input",json!({"id":"children","clientId":"owner","lease":1,"sequence":0,"data":"(sleep 1; touch orphan-marker) & echo ready > child-ready\n"})).unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    while !f.project.join("child-ready").exists() {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(10));
    }
    f.call("terminal.close", json!({"id":"children"})).unwrap();
    std::thread::sleep(Duration::from_millis(1100));
    assert!(!f.project.join("orphan-marker").exists());
}
#[test]
fn development_large_staged_diff_includes_every_segment() {
    let f = Fixture::new();
    f.repository();
    let content = (0..3000)
        .map(|i| format!("evidence_line_{i}\n"))
        .collect::<String>();
    fs::write(f.project.join("file.txt"), content).unwrap();
    f.git(&["add", "file.txt"]);
    let seen = Arc::new(Mutex::new(String::new()));
    let captured = seen.clone();
    let (model, _lease) = model(Arc::new(move |req| {
        captured
            .lock()
            .unwrap()
            .push_str(&format!("{:?}", req.items));
        "Summarize evidenced code changes".into()
    }));
    let row = f.run(
        json!({"operationId":dev_id(),"action":"generate","expected":f.snapshot(),"language":"en"}),
        Some(model),
    );
    assert_eq!(row["phase"], "completed", "{row}");
    let seen = seen.lock().unwrap();
    assert!(seen.contains("evidence_line_0"));
    assert!(seen.contains("evidence_line_2999"));
}
#[test]
fn development_draft_recovery_does_not_overwrite_another_client() {
    let f = Fixture::new();
    f.call("editor.draft.save",json!({"clientId":"old-window","expectedVersion":0,"path":"a","text":"recover me","version":"old"})).unwrap();
    let recovered = f
        .call("editor.drafts.recover", json!({"clientId":"new-window"}))
        .unwrap();
    assert_eq!(recovered["documents"][0]["text"], "recover me");
    assert_eq!(
        f.call("editor.drafts", json!({"clientId":"old-window"}))
            .unwrap()["documents"][0]["text"],
        "recover me"
    );
    f.service.close();
    assert_eq!(
        f.call("editor.drafts", json!({"clientId":"new-window"}))
            .unwrap_err(),
        "host_closed"
    );
}

#[cfg(unix)]
#[test]
fn development_git_private_index_excludes_late_external_staging() {
    use std::os::unix::fs::PermissionsExt;
    let f = Fixture::new();
    f.repository();
    fs::write(f.project.join("file.txt"), "approved staged\n").unwrap();
    f.git(&["add", "file.txt"]);
    fs::create_dir_all(f.project.join(".git/hooks")).unwrap();
    let hook = f.project.join(".git/hooks/pre-commit");
    fs::write(&hook,"#!/bin/sh\nprintf 'late external\\n' > file.txt\nGIT_INDEX_FILE=.git/index git add -- file.txt\n").unwrap();
    fs::set_permissions(&hook, fs::Permissions::from_mode(0o700)).unwrap();
    let row = f.operation("commit", json!({"message":"Approved snapshot"}));
    assert_eq!(row["phase"], "completed", "{row}");
    assert_eq!(f.git(&["show", "HEAD:file.txt"]), "approved staged");
    assert_eq!(f.git(&["show", ":file.txt"]), "late external");
}
#[test]
fn development_uncertain_commit_receipt_reconciles_parent_tree_message() {
    let f = Fixture::new();
    f.repository();
    fs::write(f.project.join("file.txt"), "approved staged\n").unwrap();
    f.git(&["add", "file.txt"]);
    let req = json!({"operationId":"recover-commit","action":"commit","message":"Approved snapshot","expected":f.snapshot()});
    assert_eq!(f.run(req.clone(), None)["phase"], "completed");
    f.service.close();
    let mut saved: Value = f.service.store.load("git-operations.json").unwrap();
    saved["rows"]["recover-commit"]["view"]["phase"] = json!("committing");
    f.service.store.save("git-operations.json", &saved).unwrap();
    fs::write(f.project.join("file.txt"), "later staged\n").unwrap();
    f.git(&["add", "file.txt"]);
    let head = f.git(&["rev-parse", "HEAD"]);
    let service = Arc::new(DevelopmentService::new(
        Project::new(&f.project),
        &f.storage,
    ));
    assert_eq!(
        service.request("git.start", req, None).unwrap()["phase"],
        "completed"
    );
    assert_eq!(f.git(&["rev-parse", "HEAD"]), head);
}
#[test]
fn development_file_identity_detects_same_size_same_mtime_replacement() {
    let f = Fixture::new();
    let path = f.project.join("file.txt");
    fs::write(&path, "old").unwrap();
    let modified = fs::metadata(&path).unwrap().modified().unwrap();
    let snapshot = f
        .call("editor.inspect", json!({"path":"file.txt"}))
        .unwrap();
    fs::write(&path, "new").unwrap();
    fs::OpenOptions::new()
        .write(true)
        .open(&path)
        .unwrap()
        .set_times(fs::FileTimes::new().set_modified(modified))
        .unwrap();
    assert_eq!(f.call("editor.delete",json!({"operationId":"stale-delete","path":"file.txt","version":snapshot["version"],"confirmed":true})).unwrap_err(),"file_conflict");
    assert_eq!(fs::read_to_string(&path).unwrap(), "new");
}

#[test]
fn development_missing_draft_requires_explicit_recreate_and_never_clobbers() {
    let f = Fixture::new();
    assert!(
        f.call(
            "editor.save",
            json!({"operationId":"old-missing","path":"restored","version":"old","text":"draft"})
        )
        .is_err()
    );
    f.call("editor.save",json!({"operationId":"recreate","path":"restored","version":"missing","text":"draft\n","bom":true,"newline":"crlf"})).unwrap();
    assert_eq!(
        fs::read(f.project.join("restored")).unwrap(),
        b"\xef\xbb\xbfdraft\r\n"
    );
    assert!(f.call("editor.save",json!({"operationId":"newly-exists","path":"restored","version":"missing","text":"clobber"})).is_err());
    assert_eq!(
        fs::read(f.project.join("restored")).unwrap(),
        b"\xef\xbb\xbfdraft\r\n"
    );
}
#[cfg(windows)]
#[test]
fn development_windows_conpty_input_lease_resize_and_job_close() {
    if !windows_terminal_redirected_child() {
        return;
    }
    let f = Fixture::new();
    // Exercise the PTY transport with the system command interpreter. User
    // PowerShell profiles/PSReadLine prompts are not part of this invariant.
    let choices = f.call("terminal.shells", json!({})).unwrap();
    let shell = choices["shells"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(Value::as_str)
        .find(|shell| {
            Path::new(shell)
                .file_name()
                .is_some_and(|name| name.eq_ignore_ascii_case("cmd.exe"))
        })
        .expect("Windows system command interpreter");
    f.call(
        "terminal.create",
        json!({"operationId":"win-native","clientId":"owner","shell":shell}),
    )
    .unwrap();
    assert!(f.service.has_live_work());
    f.call(
        "terminal.resize",
        json!({"id":"win-native","clientId":"owner","lease":1,"rows":30,"cols":100}),
    )
    .unwrap();
    windows_terminal_probe(&f);
    let lease = f
        .call(
            "terminal.claim",
            json!({"id":"win-native","clientId":"other","takeover":true}),
        )
        .unwrap();
    assert_eq!(lease["lease"], 2);
    f.call("terminal.close", json!({"id":"win-native"}))
        .unwrap();
    assert!(!f.service.has_live_work());
    assert_eq!(
        f.call("terminal.list", json!({})).unwrap()["terminals"][0]["phase"],
        "ended"
    );
}

#[cfg(windows)]
fn windows_terminal_redirected_child() -> bool {
    const CHILD: &str = "CLAT_CONPTY_REDIRECTED_CHILD";
    if std::env::var_os(CHILD).is_some() {
        return true;
    }
    // Re-enter only this test with all parent streams redirected. The shell
    // must use ConPTY, not duplicate the test process's EOF stdin/captured stdout.
    let output = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "development::tests::development_windows_conpty_input_lease_resize_and_job_close",
            "--nocapture",
        ])
        .env(CHILD, "1")
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "redirected ConPTY child failed: stdout={:?}, stderr={:?}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    false
}

#[cfg(windows)]
fn windows_terminal_probe(f: &Fixture) {
    use base64::Engine;
    // Queue input without guessing prompt text; keep responding to terminal
    // queries while consuming output. The caret prevents input echo from
    // containing the marker, so only executed output satisfies the assertion.
    f.call("terminal.input",json!({"id":"win-native","clientId":"owner","lease":1,"sequence":0,"data":"echo CLAT_NA^TIVE_PTY\r"})).unwrap();
    let (mut offset, mut sequence, mut answered) = (0, 1, 0);
    let mut output = String::new();
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let row = f.service.terminal_output("win-native", offset).unwrap();
        offset = row["offset"].as_u64().unwrap();
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(row["data"].as_str().unwrap())
            .unwrap();
        output.push_str(&String::from_utf8_lossy(&bytes));
        let queries = output.matches("\u{1b}[6n").count();
        while answered < queries {
            f.call("terminal.input",json!({"id":"win-native","clientId":"owner","lease":1,"sequence":sequence,"data":"\u{1b}[1;1R"})).unwrap();
            sequence += 1;
            answered += 1;
        }
        if output.contains("CLAT_NATIVE_PTY") {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "ConPTY did not execute the probe: {output:?}"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn development_unstage_rename_restores_both_index_paths() {
    let f = Fixture::new();
    f.repository();
    fs::rename(f.project.join("file.txt"), f.project.join("renamed.txt")).unwrap();
    f.git(&["add", "-A"]);
    fs::write(f.project.join("renamed.txt"), "after rename\n").unwrap();
    let diff = f
        .call("git.diff", json!({"path":"renamed.txt","staged":false}))
        .unwrap();
    assert_eq!(diff["left"], "before\n");
    assert_eq!(diff["right"], "after rename\n");
    let files = f.snapshot()["files"].as_array().unwrap().clone();
    assert!(
        files
            .iter()
            .any(|file| file["path"] == "renamed.txt" && file["previousPath"] == "file.txt")
    );
    let row = f.operation("unstage", json!({"paths":["renamed.txt"]}));
    assert_eq!(row["phase"], "completed", "{row}");
    assert_eq!(f.git(&["ls-files"]), "file.txt");
    assert!(f.project.join("renamed.txt").is_file());
    assert!(!f.project.join("file.txt").exists());
}

#[test]
fn development_failed_receipt_admission_never_leaves_a_live_operation() {
    let f = Fixture::new();
    let request = json!({"operationId":"cannot-record","action":"fetch","message":"x".repeat(64 * 1024 * 1024)});
    assert_eq!(f.call("git.start", request).unwrap_err(), "state_limit");
    assert!(!f.service.has_live_work());
    assert_eq!(
        f.call("git.operation", json!({"operationId":"cannot-record"}))
            .unwrap_err(),
        "operation_missing"
    );
    assert_eq!(fs::read_dir(&f.project).unwrap().count(), 0);
}

#[cfg(unix)]
#[test]
fn development_private_state_rejects_directory_aliases_between_projects() {
    let f = Fixture::new();
    f.call("git.settings", json!({})).unwrap();
    let developer = f.storage.join("development");
    let owner = fs::read_dir(&developer)
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    let other = developer.join("other-project");
    fs::rename(&owner, &other).unwrap();
    std::os::unix::fs::symlink("other-project", &owner).unwrap();
    assert!(f.call("git.settings", json!({})).is_err());
    assert!(!other.join("git-settings.json").exists());
}

#[cfg(unix)]
#[test]
fn development_terminal_initial_size_remove_and_retry_never_respawns() {
    use base64::Engine;
    let f = Fixture::new();
    let request =
        json!({"operationId":"sized","clientId":"owner","shell":"/bin/sh","rows":31,"cols":123});
    f.call("terminal.create", request.clone()).unwrap();
    f.call(
        "terminal.input",
        json!({"id":"sized","clientId":"owner","lease":1,"sequence":0,"data":"stty size\n"}),
    )
    .unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let output = f.service.terminal_output("sized", 0).unwrap();
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(output["data"].as_str().unwrap())
            .unwrap();
        if String::from_utf8_lossy(&bytes).contains("31 123") {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "initial PTY geometry was not applied"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(
        f.call(
            "terminal.remove",
            json!({"operationId":"unconfirmed","id":"sized"})
        )
        .unwrap_err(),
        "confirmation_required"
    );
    let remove = json!({"operationId":"remove-sized","id":"sized","confirmed":true});
    let result = f.call("terminal.remove", remove.clone()).unwrap();
    assert_eq!(f.call("terminal.remove", remove).unwrap(), result);
    assert!(
        f.call("terminal.list", json!({})).unwrap()["terminals"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        f.call("terminal.create", request.clone()).unwrap_err(),
        "terminal_ended"
    );
    assert!(!f.service.has_live_work());
    assert!(f.service.terminal_output("sized", 0).is_err());
    let reopened = Arc::new(DevelopmentService::new(
        Project::new(&f.project),
        &f.storage,
    ));
    assert_eq!(
        reopened
            .request("terminal.create", request, None)
            .unwrap_err(),
        "terminal_ended"
    );
}
#[cfg(unix)]
#[test]
fn development_terminal_ended_cleanup_preserves_live_and_other_project() {
    let f = Fixture::new();
    let other = Fixture::new();
    for fixture in [&f, &other] {
        for id in ["live", "ended"] {
            fixture
                .call(
                    "terminal.create",
                    json!({"operationId":id,"clientId":"owner","shell":"/bin/sh"}),
                )
                .unwrap();
        }
        fixture
            .call("terminal.close", json!({"id":"ended"}))
            .unwrap();
    }
    f.call("terminal.removeEnded", json!({"operationId":"cleanup"}))
        .unwrap();
    let rows = f.call("terminal.list", json!({})).unwrap();
    assert_eq!(rows["terminals"].as_array().unwrap().len(), 1);
    assert_eq!(rows["terminals"][0]["id"], "live");
    assert!(f.service.has_live_work());
    assert_eq!(
        other.call("terminal.list", json!({})).unwrap()["terminals"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
}
#[cfg(unix)]
#[test]
fn development_terminal_cold_records_can_rename_remove_and_not_resurrect() {
    let f = Fixture::new();
    f.call(
        "terminal.create",
        json!({"operationId":"old","clientId":"owner","shell":"/bin/sh"}),
    )
    .unwrap();
    f.service.close();
    let reopened = Arc::new(DevelopmentService::new(
        Project::new(&f.project),
        &f.storage,
    ));
    reopened
        .request(
            "terminal.rename",
            json!({"id":"old","name":"retained"}),
            None,
        )
        .unwrap();
    assert_eq!(
        reopened.request("terminal.list", json!({}), None).unwrap()["terminals"][0]["name"],
        "retained"
    );
    reopened
        .request(
            "terminal.remove",
            json!({"operationId":"remove-old","id":"old"}),
            None,
        )
        .unwrap();
    assert!(
        reopened.request("terminal.list", json!({}), None).unwrap()["terminals"]
            .as_array()
            .unwrap()
            .is_empty()
    );
}

#[cfg(unix)]
#[test]
fn development_terminal_remove_rename_compete_without_resurrection() {
    let f = Fixture::new();
    f.call(
        "terminal.create",
        json!({"operationId":"race","clientId":"owner","shell":"/bin/sh"}),
    )
    .unwrap();
    f.call("terminal.close", json!({"id":"race"})).unwrap();
    std::thread::scope(|scope| {
        let service = &f.service;
        let rename = scope.spawn(move || {
            service.request(
                "terminal.rename",
                json!({"id":"race","name":"changed"}),
                None,
            )
        });
        let service = &f.service;
        let remove = scope.spawn(move || {
            service.request(
                "terminal.remove",
                json!({"operationId":"forget","id":"race"}),
                None,
            )
        });
        let _ = rename.join().unwrap();
        remove.join().unwrap().unwrap();
    });
    f.call(
        "terminal.create",
        json!({"operationId":"new","clientId":"owner","shell":"/bin/sh"}),
    )
    .unwrap();
    let rows = f.call("terminal.list", json!({})).unwrap();
    assert_eq!(rows["terminals"].as_array().unwrap().len(), 1);
    assert_eq!(rows["terminals"][0]["id"], "new");
}

#[path = "git/workspace_tests.rs"]
mod git_workspace;
