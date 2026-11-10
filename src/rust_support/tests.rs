use super::*;
use crate::tool::{ToolInvocation, ToolMiddleware, ToolNext};
use std::sync::atomic::{AtomicUsize, Ordering};

fn fixture() -> (PathBuf, Project) {
    let (_, root) = crate::test_support::roots("rust-support");
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(root.join("src/lib.rs"), "").unwrap();
    std::fs::write(
        root.join("Cargo.toml"),
        "[package]\nname=\"fixture\"\nversion=\"0.1.0\"\nedition=\"2024\"\n",
    )
    .unwrap();
    (root.clone(), Project::new(root))
}

fn fake(project: &Project, failure: bool) -> (RustSupport, Arc<AtomicUsize>) {
    let mut support = RustSupport::new(
        project.clone(),
        Some(Arc::new(RwLock::new(
            crate::permission::PermissionMode::ProjectWrite,
        ))),
    );
    let calls = Arc::new(AtomicUsize::new(0));
    let count = calls.clone();
    support.runner = Arc::new(move |root, args, discard, _| {
        if args[0] == "locate-project" {
            return Ok(root.join("Cargo.toml").to_string_lossy().into_owned());
        }
        assert_eq!(&args[..3], &["update", "--workspace", "--offline"]);
        assert!(discard);
        assert!(!args.contains(&"--locked"));
        count.fetch_add(1, Ordering::SeqCst);
        if failure {
            Err("missing offline dependency".into())
        } else {
            Ok(String::new())
        }
    });
    (support, calls)
}

struct Invoke;
impl ToolNext for Invoke {
    fn execute(&self, invocation: &ToolInvocation<'_>) -> Result<Value, crate::ToolError> {
        invocation
            .tool
            .invoke(invocation.arguments, invocation.project, invocation.cancel)
    }
}

#[test]
fn rust_support_synchronizes_actual_agent_manifest_changes_and_preserves_write_success() {
    let (root, project) = fixture();
    let (support, calls) = fake(&project, true);
    let middleware = changes::Maintenance(Arc::new(support));
    let tool = crate::native_tools::EditFileTool::default();
    let args = json!({"path":"Cargo.toml","old_str":"0.1.0","new_str":"0.1.1"});
    let cancel = CancelToken::new();
    let invocation = ToolInvocation {
        call_id: None,
        tool: &tool,
        arguments: &args,
        project: &project,
        cancel: &cancel,
    };
    let result = middleware.execute(&invocation, &Invoke).unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(result["rust_support"][0]["status"], "failed");
    assert!(
        std::fs::read_to_string(root.join("Cargo.toml"))
            .unwrap()
            .contains("0.1.1")
    );
    assert!(middleware.execute(&invocation, &Invoke).is_err());
    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "failed exact edit performs no maintenance"
    );
    crate::test_support::cleanup_tree(&root);
}

#[test]
fn rust_support_human_save_returns_sync_result_and_operation_retry_does_not_repeat_it() {
    let (root, project) = fixture();
    let (support, calls) = fake(&project, false);
    let support = Arc::new(support);
    let private = root.with_extension("private");
    let editor = Arc::new(crate::development::DevelopmentService::new(
        project, &private,
    ));
    editor.set_rust_support(Arc::downgrade(&support));
    let document = editor
        .request("editor.open", json!({"path":"Cargo.toml"}), None)
        .unwrap();
    let request = json!({"operationId":"save-manifest","path":"Cargo.toml","version":document["version"],"text":document["text"].as_str().unwrap().replace("0.1.0","0.1.2")});
    let first = editor
        .request("editor.save", request.clone(), None)
        .unwrap();
    let retry = editor.request("editor.save", request, None).unwrap();
    assert_eq!(first["rust_support"]["status"], "synchronized");
    assert_eq!(retry["rust_support"], first["rust_support"]);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    support.close().unwrap();
    editor.close().unwrap();
    crate::test_support::cleanup_tree(&root);
    crate::test_support::cleanup_tree(&private);
}

#[test]
fn rust_support_does_not_cross_project_boundary_or_run_after_close_or_in_readonly() {
    let (root, project) = fixture();
    let (mut support, calls) = fake(&project, false);
    let mode = Arc::new(RwLock::new(crate::permission::PermissionMode::ReadOnly));
    support.mode = Some(mode.clone());
    assert_eq!(
        support
            .saved("Cargo.toml", false, &CancelToken::new())
            .unwrap()["status"],
        "failed"
    );
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    support.mode = None;
    assert_eq!(
        support
            .saved("Cargo.toml", false, &CancelToken::new())
            .unwrap()["status"],
        "failed"
    );
    assert_eq!(
        calls.load(Ordering::SeqCst),
        0,
        "classic file-write approval is not command approval"
    );
    support.mode = Some(mode.clone());
    *mode.write().unwrap() = crate::permission::PermissionMode::ProjectWrite;
    let external = root.with_extension("outside.toml");
    std::fs::write(&external, "[workspace]").unwrap();
    let outside = external.clone();
    support.runner = Arc::new(move |_, args, _, _| {
        assert_eq!(
            args[0], "locate-project",
            "Cargo must not write an external workspace lockfile"
        );
        Ok(outside.to_string_lossy().into_owned())
    });
    let result = support
        .saved("Cargo.toml", false, &CancelToken::new())
        .unwrap();
    assert!(
        result["error"]
            .as_str()
            .unwrap()
            .contains("outside the project")
    );
    support.close().unwrap();
    assert_eq!(
        support
            .saved("Cargo.toml", true, &CancelToken::new())
            .unwrap()["status"],
        "failed"
    );
    assert!(
        support
            .saved("unrelated.toml", true, &CancelToken::new())
            .is_none()
    );
    std::fs::remove_file(external).unwrap();
    crate::test_support::cleanup_tree(&root);
}

#[test]
fn rust_support_is_independently_disableable_and_does_not_require_the_human_editor() {
    use crate::plugin::Plugin;
    let plugin = plugin::RustSupportPlugin::new(Project::new("unused"), None);
    let descriptor = plugin.descriptor();
    assert_eq!(descriptor.id.as_str(), "builtin.rust_support");
    assert!(
        !descriptor
            .requires
            .contains(&crate::development::plugin::DEVELOPMENT_SERVICE.id())
    );
    let catalog: Vec<Arc<dyn Plugin>> = vec![
        Arc::new(plugin),
        Arc::new(crate::plugins::ToolPipelinePlugin),
    ];
    let selection = crate::plugin::selection::Selection::resolve(
        &catalog,
        &[],
        &crate::plugin::selection::Configuration {
            disabled: ["builtin.rust_support".into()].into_iter().collect(),
        },
    )
    .unwrap();
    assert!(
        !selection
            .entries
            .iter()
            .find(|row| row.descriptor.id.as_str() == "builtin.rust_support")
            .unwrap()
            .enabled()
    );
}

#[test]
fn rust_support_disabled_composition_preserves_agent_edit_without_maintenance() {
    use crate::plugin::{Plugin, PluginManager, ScopeKind};
    let (root, project) = fixture();
    // A sentinel lockfile must survive: disabling the plugin removes its effects,
    // not the native writer itself, even if Cargo is installed.
    std::fs::write(root.join("Cargo.lock"), "user lockfile sentinel").unwrap();
    let mut catalog: Vec<Arc<dyn Plugin>> = vec![
        Arc::new(plugin::RustSupportPlugin::new(project.clone(), None)),
        Arc::new(crate::plugins::ToolPipelinePlugin),
    ];
    let selection = crate::plugin::selection::Selection::resolve(
        &catalog,
        &[],
        &crate::plugin::selection::Configuration {
            disabled: ["builtin.rust_support".into()].into_iter().collect(),
        },
    )
    .unwrap();
    selection.apply(&mut catalog);
    let mut manager = PluginManager::root(ScopeKind::TrustedProject);
    manager.mount_all(catalog).unwrap();
    let pipeline = manager
        .require(crate::plugins::services::TOOL_PIPELINE_SERVICE)
        .unwrap();
    let tool = crate::native_tools::EditFileTool::default();
    let result = pipeline
        .execute(&ToolInvocation {
            call_id: None,
            tool: &tool,
            arguments: &json!({"path":"Cargo.toml","old_str":"0.1.0","new_str":"0.1.1"}),
            project: &project,
            cancel: &CancelToken::new(),
        })
        .unwrap();
    assert!(result.get("rust_support").is_none());
    assert!(
        std::fs::read_to_string(root.join("Cargo.toml"))
            .unwrap()
            .contains("0.1.1")
    );
    assert_eq!(
        std::fs::read_to_string(root.join("Cargo.lock")).unwrap(),
        "user lockfile sentinel"
    );
    manager.close().unwrap();
    crate::test_support::cleanup_tree(&root);
}

#[cfg(target_os = "macos")]
#[test]
fn rust_support_gui_path_finds_cargo_and_syncs_lockfile() {
    const CHILD: &str = "CLAT_TEST_RUST_SUPPORT_GUI_PATH_CHILD";
    if std::env::var_os(CHILD).is_some() {
        rust_support_real_offline_cargo_updates_lock_version_without_compiling();
        return;
    }
    let cargo =
        std::path::PathBuf::from(std::env::var_os("HOME").unwrap()).join(".cargo/bin/cargo");
    assert!(
        cargo.is_file(),
        "test requires a rustup Cargo at {}",
        cargo.display()
    );
    let mut command = std::process::Command::new(std::env::current_exe().unwrap());
    command
        .args([
            "rust_support_gui_path_finds_cargo_and_syncs_lockfile",
            "--nocapture",
        ])
        .env(CHILD, "1")
        .env("PATH", "/usr/bin:/bin:/usr/sbin:/sbin")
        .env_remove("CARGO_HOME");
    let output = crate::process::capture_command_cancellable(
        command,
        32 * 1024,
        std::time::Duration::from_secs(60),
        &CancelToken::new(),
        false,
    )
    .unwrap();
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[cfg(target_os = "macos")]
#[test]
fn rust_support_real_offline_cargo_updates_lock_version_without_compiling() {
    let (root, project) = fixture();
    let support = RustSupport::new(
        project,
        Some(Arc::new(RwLock::new(
            crate::permission::PermissionMode::ProjectWrite,
        ))),
    );
    let first = support
        .saved("Cargo.toml", false, &CancelToken::new())
        .unwrap();
    assert_eq!(first["status"], "synchronized", "{first}");
    let manifest = root.join("Cargo.toml");
    std::fs::write(
        &manifest,
        std::fs::read_to_string(&manifest)
            .unwrap()
            .replace("0.1.0", "0.1.1"),
    )
    .unwrap();
    let second = support
        .saved("Cargo.toml", false, &CancelToken::new())
        .unwrap();
    assert_eq!(second["status"], "synchronized", "{second}");
    assert!(
        std::fs::read_to_string(root.join("Cargo.lock"))
            .unwrap()
            .contains("version = \"0.1.1\"")
    );
    assert!(!root.join("target").exists(), "no cargo check/build/test");
    let lock = std::fs::read(root.join("Cargo.lock")).unwrap();
    std::fs::write(&manifest, "[package\ninvalid").unwrap();
    let invalid = support
        .saved("Cargo.toml", false, &CancelToken::new())
        .unwrap();
    assert_eq!(invalid["status"], "failed");
    assert_eq!(std::fs::read(root.join("Cargo.lock")).unwrap(), lock);
    assert_eq!(
        std::fs::read_to_string(manifest).unwrap(),
        "[package\ninvalid"
    );
    support.close().unwrap();
    crate::test_support::cleanup_tree(&root);
}

#[cfg(target_os = "macos")]
#[test]
fn rust_support_real_workspace_inherited_version_updates_the_workspace_lock() {
    let (root, project) = fixture();
    std::fs::create_dir_all(root.join("member/src")).unwrap();
    std::fs::write(root.join("member/src/lib.rs"), "").unwrap();
    std::fs::write(
        root.join("member/Cargo.toml"),
        "[package]\nname=\"member\"\nversion.workspace=true\nedition=\"2024\"\n",
    )
    .unwrap();
    let manifest = root.join("Cargo.toml");
    let workspace = "[workspace]\nmembers=[\"member\"]\nresolver=\"3\"\n[workspace.package]\nversion=\"0.1.0\"\n";
    std::fs::write(&manifest, workspace).unwrap();
    let support = RustSupport::new(
        project,
        Some(Arc::new(RwLock::new(
            crate::permission::PermissionMode::ProjectWrite,
        ))),
    );
    let first = support
        .saved("member/Cargo.toml", false, &CancelToken::new())
        .unwrap();
    assert_eq!(first["status"], "synchronized", "{first}");
    assert_eq!(first["lockfile"], "Cargo.lock");
    std::fs::write(manifest, workspace.replace("0.1.0", "0.1.1")).unwrap();
    let second = support
        .saved("Cargo.toml", false, &CancelToken::new())
        .unwrap();
    assert_eq!(second["status"], "synchronized", "{second}");
    let lock = std::fs::read_to_string(root.join("Cargo.lock")).unwrap();
    assert!(lock.contains("name = \"member\"\nversion = \"0.1.1\""));
    assert!(!root.join("member/Cargo.lock").exists());
    assert!(!root.join("target").exists());
    support.close().unwrap();
    crate::test_support::cleanup_tree(&root);
}

#[test]
fn rust_support_reactions_report_artifacts_without_running_commands() {
    let (root, project) = fixture();
    std::fs::create_dir_all(root.join(".cargo")).unwrap();
    let (support, calls) = fake(&project, false);
    let cancel = CancelToken::new();
    let toolchain = root.join("rust-toolchain.toml");
    std::fs::write(&toolchain, "[toolchain]\nchannel='stable'\n").unwrap();
    let report = support.saved("rust-toolchain.toml", true, &cancel).unwrap();
    assert_eq!(report["status"], "reported");
    assert_eq!(report["channel"], "stable");
    std::fs::write(&toolchain, "[toolchain\n").unwrap();
    assert_eq!(
        support.saved("rust-toolchain.toml", true, &cancel).unwrap()["status"],
        "failed"
    );
    let config = root.join(".cargo/config.toml");
    let old = b"[build]\njobs=2\n";
    std::fs::write(&config, "[build]\njobs=3\n").unwrap();
    let report = support
        .react(".cargo/config.toml", true, &cancel, Some(old))
        .unwrap();
    assert_eq!(report["changed_keys"], json!(["build.jobs"]));
    std::fs::write(&config, "[build\n").unwrap();
    assert_eq!(
        support.saved(".cargo/config.toml", true, &cancel).unwrap()["status"],
        "failed"
    );
    assert!(support.saved("src/lib.rs", true, &cancel).is_none());
    assert!(support.saved("config.toml", true, &cancel).is_none());
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    crate::test_support::cleanup_tree(&root);
}

#[test]
fn rust_support_agent_and_editor_report_configuration_changes() {
    let (root, project) = fixture();
    std::fs::create_dir_all(root.join(".cargo")).unwrap();
    std::fs::write(root.join(".gitignore"), ".cargo/\n").unwrap();
    std::fs::write(root.join(".cargo/config.toml"), "[build]\njobs=2\n").unwrap();
    let (support, calls) = fake(&project, false);
    let support = Arc::new(support);
    let middleware = changes::Maintenance(support.clone());
    let tool = crate::native_tools::EditFileTool::default();
    let args = json!({"path":".cargo/config.toml","old_str":"jobs=2","new_str":"jobs=3"});
    let cancel = CancelToken::new();
    let invocation = ToolInvocation {
        call_id: None,
        tool: &tool,
        arguments: &args,
        project: &project,
        cancel: &cancel,
    };
    let result = middleware.execute(&invocation, &Invoke).unwrap();
    assert_eq!(
        result["rust_support"][0]["changed_keys"],
        json!(["build.jobs"])
    );
    let private = root.with_extension("private-config");
    let editor = Arc::new(crate::development::DevelopmentService::new(
        project, &private,
    ));
    editor.set_rust_support(Arc::downgrade(&support));
    let doc = editor
        .request("editor.open", json!({"path":".cargo/config.toml"}), None)
        .unwrap();
    let request = json!({"operationId":"save-cargo-config","path":".cargo/config.toml","version":doc["version"],"text":"[build]\njobs=4\n"});
    let result = editor.request("editor.save", request, None).unwrap();
    assert_eq!(
        result["rust_support"]["changed_keys"],
        json!(["build.jobs"])
    );
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    editor.close().unwrap();
    support.close().unwrap();
    crate::test_support::cleanup_tree(&root);
    crate::test_support::cleanup_tree(&private);
}
