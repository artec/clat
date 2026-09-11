use std::fs;
use std::path::{Path, PathBuf};

fn rust_sources(root: &Path) -> Vec<PathBuf> {
    fn visit(directory: &Path, files: &mut Vec<PathBuf>) {
        let mut entries = fs::read_dir(directory)
            .unwrap_or_else(|error| panic!("read {}: {error}", directory.display()))
            .map(|entry| entry.expect("read source entry").path())
            .collect::<Vec<_>>();
        entries.sort();
        for path in entries {
            if path.is_dir() {
                visit(&path, files);
            } else if path.extension().and_then(|extension| extension.to_str()) == Some("rs") {
                files.push(path);
            }
        }
    }

    let mut files = Vec::new();
    visit(&root.join("src"), &mut files);
    files
}

/// Whether a source is nested under one specific first-level `src/` tree.
/// This keeps discovery automatic for future nested modules without relying
/// on a hand-maintained file list.
fn is_under_src_dir(path: &Path, directory: &str) -> bool {
    path.ancestors().any(|ancestor| {
        ancestor.file_name().and_then(|name| name.to_str()) == Some(directory)
            && ancestor
                .parent()
                .and_then(|parent| parent.file_name())
                .and_then(|name| name.to_str())
                == Some("src")
    })
}

fn is_src_root_file(path: &Path, file: &str) -> bool {
    path.file_name().and_then(|name| name.to_str()) == Some(file)
        && path
            .parent()
            .and_then(|parent| parent.file_name())
            .and_then(|name| name.to_str())
            == Some("src")
}

/// The CLAT TUI shell family (`tui*.rs` / `src/tui/`).
fn is_clat_tui_frontend(path: &Path) -> bool {
    let name = path.file_name().and_then(|name| name.to_str());
    is_src_root_file(path, "tui.rs")
        || (name.is_some_and(|name| name.starts_with("tui_"))
            && path
                .parent()
                .and_then(|parent| parent.file_name())
                .and_then(|name| name.to_str())
                == Some("src"))
        || is_under_src_dir(path, "tui")
}

/// Terminal clients are the CLAT TUI shell plus the DSH protocol/backend
/// tree that feeds that shell.
fn is_terminal_frontend(path: &Path) -> bool {
    is_clat_tui_frontend(path) || is_under_src_dir(path, "dsh")
}

/// Every local client of the Application boundary. `main.rs` stays a
/// composition root; these modules own presentation/transport only.
fn is_local_frontend(path: &Path) -> bool {
    is_terminal_frontend(path)
        || is_src_root_file(path, "exec.rs")
        || is_under_src_dir(path, "exec")
        || is_src_root_file(path, "serve.rs")
        || is_under_src_dir(path, "serve")
}

fn relative<'a>(root: &'a Path, path: &'a Path) -> &'a Path {
    path.strip_prefix(root)
        .expect("source under repository root")
}

fn without_line_comments(source: &str) -> String {
    source
        .lines()
        .map(|line| line.split_once("//").map_or(line, |(code, _)| code))
        .collect::<Vec<_>>()
        .join("\n")
}

fn internal_owner_violation(source: &str) -> Option<&'static str> {
    let code = without_line_comments(source);
    let identifiers = code
        .split(|character: char| !(character.is_ascii_alphanumeric() || character == '_'))
        .filter(|identifier| !identifier.is_empty())
        .collect::<Vec<_>>();
    [
        "ControlStorage",
        "SessionService",
        "TrustedProjectComposition",
        "ProjectPorts",
        "RunContextSnapshot",
        "RunExecutionEngine",
        "WaitingRunExecution",
        "composition",
        "run_context",
        "run_execution",
        "use_cases",
        "control_storage",
    ]
    .into_iter()
    .find(|forbidden| identifiers.contains(forbidden))
}

/// Every Rust source below src is classified automatically. lib.rs and
/// main.rs are composition roots; TUI/DSH/exec/serve are local clients; every
/// other source is core, including newly added nested plugin/provider files.
#[test]
fn core_modules_do_not_depend_on_local_frontend_code() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let sources = rust_sources(root);
    let mut checked = 0;
    for path in sources {
        let name = path.file_name().and_then(|name| name.to_str());
        if is_local_frontend(&path) || matches!(name, Some("lib.rs" | "core.rs" | "main.rs")) {
            continue;
        }
        checked += 1;
        let source = fs::read_to_string(&path)
            .map(|s| s.replace("\r\n", "\n"))
            .expect("read core source");
        let relative = relative(root, &path).display();
        for forbidden in [
            "crate::tui",
            "crate::dsh",
            "crate::exec",
            "crate::serve",
            "ratatui",
            "crossterm",
        ] {
            assert!(
                !source.contains(forbidden),
                "core module {relative} must not reference frontend token `{forbidden}`"
            );
        }
    }
    assert!(checked > 0, "architecture guard discovered no core sources");
}

/// Local clients consume use cases, DTOs, and events. Internal storage,
/// session, composition, context, and execution owners are not client ports;
/// naming any of them from a frontend is a semantic boundary violation even
/// if someone later loosens Rust visibility.
#[test]
fn local_frontends_do_not_reach_internal_core_owners() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let frontends = rust_sources(root)
        .into_iter()
        .filter(|path| is_local_frontend(path))
        .collect::<Vec<_>>();
    for (family, discovered) in [
        (
            "TUI",
            frontends.iter().any(|path| is_clat_tui_frontend(path)),
        ),
        (
            "DSH",
            frontends.iter().any(|path| is_under_src_dir(path, "dsh")),
        ),
        (
            "exec",
            frontends
                .iter()
                .any(|path| is_src_root_file(path, "exec.rs") || is_under_src_dir(path, "exec")),
        ),
        (
            "serve",
            frontends
                .iter()
                .any(|path| is_src_root_file(path, "serve.rs") || is_under_src_dir(path, "serve")),
        ),
    ] {
        assert!(
            discovered,
            "architecture guard failed to discover the {family} frontend family"
        );
    }
    for path in frontends {
        let source = fs::read_to_string(&path)
            .map(|s| s.replace("\r\n", "\n"))
            .expect("read frontend source");
        let code = without_line_comments(&source);
        let relative = relative(root, &path).display();
        assert!(
            internal_owner_violation(&code).is_none(),
            "local frontend {relative} must use the Application facade, not internal owner `{}`",
            internal_owner_violation(&code).expect("checked violation")
        );
    }
}

#[test]
fn internal_owner_guard_rejects_aliases_and_glob_imports() {
    assert_eq!(
        internal_owner_violation("use crate::application::run_execution as execution;"),
        Some("run_execution")
    );
    assert_eq!(
        internal_owner_violation("use crate::control_storage::*;"),
        Some("control_storage")
    );
    assert_eq!(
        internal_owner_violation("use crate::session::use_cases::*;"),
        Some("use_cases")
    );
    assert_eq!(
        internal_owner_violation("use crate::control_storage::control_error as err;"),
        Some("control_storage")
    );
    assert_eq!(
        internal_owner_violation("use crate::{control_storage::*};"),
        Some("control_storage")
    );
    assert_eq!(
        internal_owner_violation("use crate::{application::run_context as context};"),
        Some("run_context")
    );
    assert_eq!(
        internal_owner_violation("use crate::application::{run_context::*};"),
        Some("run_context")
    );
    assert_eq!(
        internal_owner_violation("use crate::{application::{run_execution as execution}};"),
        Some("run_execution")
    );
    assert_eq!(
        internal_owner_violation("use crate::application::DshWorkspaceFile;"),
        None
    );
}

/// The crate root may re-export supported facade/domain contracts, but
/// internal owners must remain impossible for downstream callers to name.
#[test]
fn crate_root_does_not_export_internal_core_owners() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let source = ["src/lib.rs", "src/core.rs", "src/client_ports.rs"]
        .map(|path| {
            fs::read_to_string(root.join(path))
                .map(|s| s.replace("\r\n", "\n"))
                .expect("read crate boundary")
        })
        .join("\n");
    for statement in source.split(';') {
        let compact = statement.split_whitespace().collect::<String>();
        let reexports = compact.contains("pubuse") || compact.contains("pub(crate)use");
        for owner in [
            "ControlStorage",
            "SessionService",
            "TrustedProjectComposition",
            "ProjectPorts",
            "RunContextSnapshot",
            "RunExecutionEngine",
            "WaitingRunExecution",
        ] {
            assert!(
                !(reexports && compact.contains(owner)),
                "src/lib.rs must not expose internal owner `{owner}` through `{compact}`"
            );
        }
        assert!(
            compact != "pubmoddsh" && compact != "pub(crate)moddsh",
            "the DSH client implementation is frontend-internal, not a library API"
        );
        assert!(
            compact != "pubmodcontrol_storage" && compact != "pub(crate)modcontrol_storage",
            "the control-storage implementation must stay behind Application/domain DTOs"
        );
    }
}

/// The composition root may expose the frontend module itself, but it must not
/// re-export frontend types at crate root: that would let core code hide a TUI
/// dependency behind `crate::SomeType`.
#[test]
fn crate_root_does_not_reexport_terminal_frontend_types() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let source = fs::read_to_string(root.join("src/lib.rs"))
        .map(|s| s.replace("\r\n", "\n"))
        .expect("read crate root");
    for statement in source.split(';') {
        let compact = statement.split_whitespace().collect::<String>();
        let reexports = compact.contains("pubuse") || compact.contains("pub(crate)use");
        assert!(
            !(reexports && compact.contains("tui")),
            "src/lib.rs must not hide a frontend dependency behind `{compact}`"
        );
    }
}

#[test]
fn workspace_enforces_terminal_dependency_direction_and_single_binary() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let output = std::process::Command::new("cargo")
        .args([
            "metadata",
            "--no-deps",
            "--format-version",
            "1",
            "--offline",
        ])
        .current_dir(root)
        .output()
        .expect("cargo metadata");
    assert!(output.status.success(), "metadata failed");
    let metadata: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let packages = metadata["packages"].as_array().unwrap();
    let core = packages.iter().find(|p| p["name"] == "clat-core").unwrap();
    let app = packages.iter().find(|p| p["name"] == "clat").unwrap();
    assert_eq!(
        core["version"], app["version"],
        "core/client version must be bumped together"
    );
    for forbidden in ["clat", "ratatui", "crossterm", "arboard"] {
        assert!(
            !core["dependencies"]
                .as_array()
                .unwrap()
                .iter()
                .any(|d| d["name"] == forbidden),
            "core must not depend on {forbidden}"
        );
    }
    assert!(
        app["dependencies"]
            .as_array()
            .unwrap()
            .iter()
            .any(|d| d["name"] == "clat-core")
    );
    for package in [core, app] {
        assert!(
            metadata["workspace_default_members"]
                .as_array()
                .unwrap()
                .contains(&package["id"]),
            "default cargo gates must test every product crate"
        );
    }
    let bins = [core, app]
        .into_iter()
        .flat_map(|p| p["targets"].as_array().unwrap())
        .filter(|t| {
            t["kind"]
                .as_array()
                .unwrap()
                .iter()
                .any(|kind| kind == "bin")
        })
        .collect::<Vec<_>>();
    assert_eq!(bins.len(), 1);
    assert_eq!(bins[0]["name"], "clat");
}

#[test]
fn runtime_projection_vocabulary_has_one_catalog_home() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let catalog = fs::read_to_string(root.join("src/session/catalog/run_events.rs"))
        .map(|s| s.replace("\r\n", "\n"))
        .unwrap();
    for tag in [
        "run_started",
        "model_requested",
        "steering_applied",
        "run_failed",
    ] {
        assert!(catalog.contains(&format!("=> \"{tag}\";")));
    }
    for path in ["src/wire/mod.rs", "src/session/recorder.rs"] {
        let source = fs::read_to_string(root.join(path))
            .map(|s| s.replace("\r\n", "\n"))
            .unwrap();
        let source = source.split("\n#[cfg(test)]").next().unwrap();
        assert!(
            source.contains("catalog::run_event_seats!"),
            "{path} bypasses catalog"
        );
        assert!(
            !source.contains("RunEvent::ModelRequested"),
            "{path} reintroduced projection mirror"
        );
    }
}

/// Frontend styles come from the theme module (phase-1 P0-2): the
/// production prefix of the terminal frontend files must not mention
/// `Color::` — every visual style routes through `tui_theme::style(Role)`.
/// Test modules after the first `#[cfg(test)]` are exempt (they assert on
/// concrete colors); the truecolor whitelist lives in `tui_theme.rs`.
#[test]
fn frontend_styles_come_from_the_theme_module() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    for name in [
        "src/tui.rs",
        "src/tui/markdown.rs",
        "src/tui/model_editor.rs",
        "src/tui/model_editor/picker.rs",
        "src/tui/session_picker.rs",
        // D-2（INV-U1 撤豁免）：dsh 协议文件不渲染（天然绿），入清单
        // 锁未来——任何新的渲染代码必须走 theme 角色。
        "src/dsh/backend.rs",
        "src/dsh/client.rs",
        "src/dsh/connect.rs",
        "src/dsh/files.rs",
        "src/dsh/frames.rs",
        "src/dsh/transcript.rs",
        "src/dsh/ws.rs",
    ] {
        let source = fs::read_to_string(root.join(name))
            .map(|s| s.replace("\r\n", "\n"))
            .expect("read frontend source");
        let production = source
            .split("\n#[cfg(test)]\nmod ")
            .next()
            .unwrap_or("")
            .split("\n#[cfg(all(test, feature = \"runtime-tests\"))]\nmod ")
            .next()
            .unwrap_or("");
        assert!(
            !production.contains("Color::"),
            "{name} production code must use tui_theme roles, not raw Color::"
        );
    }
}

/// INV-U1 锚：D-1 的独立 dsh 壳必须保持拆除——dsh 渲染只经 CLAT App
/// 本体，任何"再起一个 dsh 专属 UI"都是回归。
#[test]
fn the_dsh_standalone_shell_stays_dismantled() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    assert!(
        !root.join("src/dsh/app.rs").exists(),
        "INV-U1: src/dsh/app.rs must stay deleted — dsh renders through the CLAT App"
    );
}

/// Frontends may hold public DTOs and implement ports, but must not reach into
/// private core modules or construct, persist, or spawn business capabilities.
#[test]
fn terminal_frontend_has_no_core_assembly_or_persistence_entrypoints() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let frontends = rust_sources(root)
        .into_iter()
        .filter(|path| is_terminal_frontend(path))
        .collect::<Vec<_>>();
    // dsh 前端无 CLAT 运行时可装配——但同样不得引用核心装配面。
    assert!(frontends.iter().any(|path| is_clat_tui_frontend(path)));
    assert!(frontends.iter().any(|path| is_under_src_dir(path, "dsh")));
    for path in frontends {
        let source = fs::read_to_string(&path)
            .map(|s| s.replace("\r\n", "\n"))
            .expect("read frontend source");
        let relative = relative(root, &path).display();
        for forbidden in [
            "crate::storage",
            "crate::providers",
            "crate::native_tools",
            "crate::mcp",
            "crate::run",
            "crate::tool",
            "Storage::",
            "ProviderRuntime",
            "ToolRegistry",
            "mcp::client",
            "Run::new",
            "register_native_",
            "register_mcp_",
            "fetch_deepseek_balance",
            "fetch_glm_quota",
            "build_model(",
        ] {
            assert!(
                !source.contains(forbidden),
                "frontend module {relative} must not contain core token `{forbidden}`"
            );
        }
    }
}

/// Agent command execution has one core spawn seam. Tools and run/application
/// orchestration may call ProcessService but must never grow a second direct
/// Command/PTY implementation; sandbox functional probes are routed through
/// the same process module too.
#[test]
fn agent_command_spawning_is_owned_only_by_process_service() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    for name in [
        "src/native_tools.rs",
        "src/plugins/process.rs",
        "src/plugins/language_intelligence.rs",
        "src/language_intelligence.rs",
        "src/application/run_lifecycle.rs",
        "src/run/mod.rs",
        "src/sandbox.rs",
    ] {
        let source = fs::read_to_string(root.join(name))
            .map(|s| s.replace("\r\n", "\n"))
            .expect("read source");
        for forbidden in [
            "std::process::Command::new",
            ".group_spawn()",
            "native_pty_system()",
        ] {
            assert!(
                !source.contains(forbidden),
                "{name} must route agent command spawning through ProcessService, found `{forbidden}`"
            );
        }
    }
    let owner = fs::read_to_string(root.join("src/process/mod.rs"))
        .map(|s| s.replace("\r\n", "\n"))
        .expect("process service");
    assert!(owner.contains(".group_spawn()"));
    assert!(owner.contains("native_pty_system()"));
}

/// Slash command semantics live in the `core.commands` registry
/// (docs/todo/commands-core.md, INV-C1): frontends reach commands only
/// through `TrustedProjectApplication::dispatch_command` and render the
/// outcome. The production prefix of the terminal frontend files must not
/// contain command-name match arms (the dispatch shape `"/model" =>` or
/// `"/new" |`). Merely *mentioning* a command in a dialog title or status
/// text (e.g. " /model " or "run /model first") is a presentation reference
/// and stays legal — adding a command must not touch any frontend.
#[test]
fn terminal_frontend_does_not_own_slash_command_dispatch() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    // 只约束 CLAT TUI 家族：dsh 模式按设计档案 §5 自有命令映射（对
    // DSH API），不经 `core.commands`（那是 CLAT 运行时的命令语义源）。
    let frontends = rust_sources(root)
        .into_iter()
        .filter(|path| is_clat_tui_frontend(path))
        .collect::<Vec<_>>();
    assert!(
        frontends
            .iter()
            .any(|path| is_src_root_file(path, "tui.rs")),
        "architecture guard failed to discover the CLAT TUI shell"
    );
    let commands = [
        "model",
        "help",
        "mcp",
        "compact",
        "resume",
        "new",
        "clear",
        "perm",
        "permission",
        "rename",
        "quit",
        "exit",
    ];
    let mut checked = 0;
    for path in frontends {
        let source = fs::read_to_string(&path)
            .map(|s| s.replace("\r\n", "\n"))
            .expect("read frontend source");
        let production = source.split("\n#[cfg(test)]").next().unwrap_or("");
        let relative = relative(root, &path).display();
        checked += 1;
        for name in commands {
            for dispatch_shape in [format!("\"/{name}\" =>"), format!("\"/{name}\" |")] {
                assert!(
                    !production.contains(&dispatch_shape),
                    "{relative} production code must not dispatch `/{name}` itself — \
                     route it through TrustedProjectApplication::dispatch_command"
                );
            }
        }
    }
    assert!(
        checked > 0,
        "architecture guard discovered no frontend sources"
    );
}
