use super::*;
use std::{fs, path::PathBuf};

struct Fixture {
    root: PathBuf,
    storage: PathBuf,
    project: PathBuf,
    bin: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let (storage, project) = crate::test_support::roots("acp-presets");
        fs::create_dir_all(&project).unwrap();
        fs::create_dir_all(&storage).unwrap();
        let root = fs::canonicalize(project.parent().unwrap()).unwrap();
        let bin = root.join("bin");
        fs::create_dir(&bin).unwrap();
        Self {
            storage: root.join("storage"),
            project: root.join("project"),
            root,
            bin,
        }
    }

    fn search(&self, windows: bool) -> Search<'_> {
        Search {
            dirs: vec![self.bin.clone()],
            project: &self.project,
            windows,
            launch: crate::acp::launch_path::LaunchPath::fixture(
                Some(std::env::join_paths([&self.bin]).unwrap()),
                vec![],
            ),
        }
    }

    fn control(&self) -> ControlStorage {
        crate::control_storage::sentinel::initialize(&self.storage).unwrap();
        ControlStorage::open_ready(&self.storage).unwrap()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        crate::test_support::cleanup_tree(&self.root);
    }
}

fn preset(id: &str) -> &'static Preset {
    PRESETS.iter().find(|preset| preset.id == id).unwrap()
}

fn launcher(path: &Path, bytes: &[u8]) {
    fs::write(path, bytes).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
    }
}

#[cfg(unix)]
#[test]
fn acp_preset_reports_missing_runtime_and_rechecks_it_before_saving() {
    let fixture = Fixture::new();
    let search = fixture.search(false);
    let opencode = preset("opencode");
    let runtime = fixture.bin.join("fixture-runtime");
    launcher(
        &fixture.bin.join("opencode"),
        b"#!/usr/bin/env fixture-runtime\n",
    );
    let row = opencode.view(&Catalog::default(), search.detect(opencode), false);
    assert_eq!(row["status"], "missing_runtime");
    assert_eq!(row["statusDetail"]["key"], "acp.runtime_unavailable");
    assert!(row["detection"].is_null());

    launcher(&runtime, b"fixture");
    let Detection::Ready(candidate) = search.detect(opencode) else {
        panic!("expected a runnable launcher");
    };
    fs::remove_file(runtime).unwrap();
    let control = fixture.control();
    let service = Service::default();
    let error = service
        .add_preset_from(
            &control,
            &search,
            AddPreset {
                id: opencode.id.into(),
                detection: candidate.token,
            },
        )
        .unwrap_err();
    assert_eq!(error.key, "acp.runtime_unavailable");
    assert_eq!(
        service
            .agents(&control, fixture.project.to_str().unwrap())
            .unwrap(),
        json!([])
    );
}

#[test]
fn acp_preset_windows_distinguishes_main_cli_adapter_and_script() {
    let fixture = Fixture::new();
    let search = fixture.search(true);
    let codex = preset("codex");
    assert!(matches!(search.detect(codex), Detection::Missing));
    launcher(&fixture.bin.join("codex.exe"), b"unused");
    assert!(matches!(search.detect(codex), Detection::MissingAdapter));
    launcher(&fixture.bin.join("codex-acp.cmd"), b"unused");
    assert!(matches!(search.detect(codex), Detection::Incompatible));
    launcher(&fixture.bin.join("codex-acp.exe"), b"unused");
    assert!(matches!(search.detect(codex), Detection::Ready(_)));
}

#[test]
fn acp_preset_setup_uses_the_host_platform_and_the_actual_acp_entrypoint() {
    let catalog = Catalog::default();
    let agy = preset("antigravity-cli");
    assert_eq!(agy.args, ["acp"]);
    let grok = preset("grok-build");
    assert_eq!(grok.args, ["--no-auto-update", "agent", "stdio"]);
    let local = agy.view(&catalog, Detection::Missing, false);
    let windows = agy.view(&catalog, Detection::Missing, true);
    assert_eq!(local["requiresAdapter"], false);
    assert_eq!(
        local["install"]["command"],
        "curl -fsSL https://antigravity.google/cli/install.sh | bash"
    );
    assert_eq!(
        windows["install"]["command"],
        "irm https://antigravity.google/cli/install.ps1 | iex"
    );

    let codex = preset("codex");
    let local = codex.view(&catalog, Detection::MissingAdapter, false);
    let windows = codex.view(&catalog, Detection::MissingAdapter, true);
    assert_eq!(local["requiresAdapter"], true);
    assert_eq!(
        local["install"]["command"],
        "npm install -g @agentclientprotocol/codex-acp"
    );
    assert_eq!(windows["install"]["method"], "wsl");
    let claude_windows = preset("claude-code").view(&catalog, Detection::MissingAdapter, true);
    assert_eq!(claude_windows["install"]["method"], "download");
    assert_eq!(claude_windows["install"]["launcher"], "claude-agent-acp");
    assert!(claude_windows["install"]["command"].is_null());
}

#[test]
fn acp_preset_qoder_uses_the_native_launcher_and_installer() {
    let fixture = Fixture::new();
    let path = fixture.bin.join("qodercli.exe");
    launcher(&path, b"windows launcher");
    let qoder = preset("qoder-cli");
    let detection = fixture.search(true).detect(qoder);
    let Detection::Ready(candidate) = detection else {
        panic!("Qoder's native executable must be detected");
    };
    let edit = qoder.edit(&candidate);
    assert_eq!(edit.program, path);
    assert_eq!(edit.args, ["--acp"]);

    let catalog = Catalog::default();
    let unix = qoder.view(&catalog, Detection::Missing, false);
    assert_eq!(unix["requiresAdapter"], false);
    assert_eq!(
        unix["install"]["command"],
        "curl -fsSL https://qoder.com/install | bash"
    );
    let windows = qoder.view(&catalog, Detection::Missing, true);
    if cfg!(target_arch = "aarch64") {
        assert_eq!(windows["install"]["method"], "unavailable");
        assert!(windows["install"]["command"].is_null());
    } else {
        assert_eq!(
            windows["install"]["command"],
            "irm https://qoder.com/install.ps1 | iex"
        );
    }
}

#[test]
fn acp_preset_ignores_relative_paths_and_project_launchers() {
    let fixture = Fixture::new();
    let mut search = fixture.search(true);
    search.dirs = vec![
        PathBuf::from("."),
        fixture.project.clone(),
        fixture.bin.clone(),
    ];
    launcher(&fixture.project.join("codex-acp.exe"), b"project");
    assert!(matches!(search.detect(preset("codex")), Detection::Missing));
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(
            fixture.project.join("codex-acp.exe"),
            fixture.bin.join("codex-acp.exe"),
        )
        .unwrap();
        assert!(matches!(search.detect(preset("codex")), Detection::Missing));
    }
}

#[test]
fn acp_preset_batch_keeps_valid_rows_when_a_launcher_changes() {
    let fixture = Fixture::new();
    let control = fixture.control();
    let search = fixture.search(true);
    let capture = |id, name| {
        let path = fixture.bin.join(name);
        launcher(&path, b"detected executable");
        let Detection::Ready(candidate) = search.detect(preset(id)) else {
            panic!("expected a known launcher");
        };
        AddPreset {
            id: id.into(),
            detection: candidate.token,
        }
    };
    let requests = [
        capture("opencode", "opencode.exe"),
        capture("deepseek-harness", "dsh.exe"),
        capture("grok-build", "grok.exe"),
    ];
    launcher(&fixture.bin.join("dsh.exe"), b"changed before submit");
    let results = Service::default()
        .add_presets_from(&control, &search, &requests)
        .unwrap();
    assert_eq!(results[0].as_ref().unwrap()["label"], "OpenCode");
    assert_eq!(results[1].as_ref().unwrap_err().key, "acp.preset_changed");
    assert_eq!(results[2].as_ref().unwrap()["label"], "Grok Build");

    let stored = Service::default()
        .agents(&control, fixture.project.to_str().unwrap())
        .unwrap();
    assert_eq!(stored.as_array().unwrap().len(), 2);
    assert!(
        stored
            .as_array()
            .unwrap()
            .iter()
            .all(|agent| agent["trusted"] == false)
    );
}

#[test]
fn acp_preset_rejects_changed_launcher_and_keeps_existing_project_authorization() {
    let fixture = Fixture::new();
    let control = fixture.control();
    let service = Service::default();
    let preset = preset("opencode");
    let name = if cfg!(windows) {
        "opencode.exe"
    } else {
        "opencode"
    };
    let path = fixture.bin.join(name);
    let search = fixture.search(cfg!(windows));
    let capture = || match search.detect(preset) {
        Detection::Ready(candidate) => candidate.token,
        _ => panic!("expected a known launcher"),
    };
    let request = |detection| AddPreset {
        id: preset.id.to_owned(),
        detection,
    };
    launcher(&path, b"previous launcher");
    let old = capture();
    launcher(&path, b"new executable contents");
    assert_eq!(
        service
            .add_preset_from(&control, &search, request(old))
            .unwrap_err()
            .key,
        "acp.preset_changed"
    );
    assert_eq!(service.agents(&control, "project-one").unwrap(), json!([]));

    let added = service
        .add_preset_from(&control, &search, request(capture()))
        .unwrap();
    let id = added["id"].as_str().unwrap();
    let revision = added["revision"].as_str().unwrap();
    assert_eq!(added["args"], json!(["acp"]));
    assert_eq!(added["trusted"], false);
    let project = fixture.project.to_str().unwrap();
    service
        .trust_agent(&control, id, project, revision, true)
        .unwrap();
    let repeated = service
        .add_preset_from(&control, &search, request(capture()))
        .unwrap();
    assert_eq!(repeated["id"], id);
    assert_eq!(repeated["revision"], revision);
    assert_eq!(repeated["trusted"], true);
    assert_eq!(
        service.agents(&control, "another-project").unwrap()[0]["trusted"],
        false
    );
}
