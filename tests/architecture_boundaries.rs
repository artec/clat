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

fn is_frontend(path: &Path) -> bool {
    let name = path.file_name().and_then(|name| name.to_str());
    let in_frontend_dir = path
        .parent()
        .and_then(|parent| parent.file_name())
        .is_some_and(|dir| dir == "tui");
    name.is_some_and(|name| name == "tui.rs" || name.starts_with("tui_")) || in_frontend_dir
}

fn relative<'a>(root: &'a Path, path: &'a Path) -> &'a Path {
    path.strip_prefix(root)
        .expect("source under repository root")
}

/// Every Rust source below src is classified automatically. lib.rs and
/// main.rs are composition roots; tui*.rs are terminal frontend files; every
/// other source is core, including newly added nested plugin/provider files.
#[test]
fn core_modules_do_not_depend_on_terminal_frontend_code() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let sources = rust_sources(root);
    let mut checked = 0;
    for path in sources {
        let name = path.file_name().and_then(|name| name.to_str());
        if is_frontend(&path) || matches!(name, Some("lib.rs" | "main.rs")) {
            continue;
        }
        checked += 1;
        let source = fs::read_to_string(&path).expect("read core source");
        let relative = relative(root, &path).display();
        for forbidden in ["tui", "ratatui", "crossterm"] {
            assert!(
                !source.contains(forbidden),
                "core module {relative} must not reference frontend token `{forbidden}`"
            );
        }
    }
    assert!(checked > 0, "architecture guard discovered no core sources");
}

/// The composition root may expose the frontend module itself, but it must not
/// re-export frontend types at crate root: that would let core code hide a TUI
/// dependency behind `crate::SomeType`.
#[test]
fn crate_root_does_not_reexport_terminal_frontend_types() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let source = fs::read_to_string(root.join("src/lib.rs")).expect("read crate root");
    for statement in source.split(';') {
        let compact = statement.split_whitespace().collect::<String>();
        let reexports = compact.contains("pubuse") || compact.contains("pub(crate)use");
        assert!(
            !(reexports && compact.contains("tui")),
            "src/lib.rs must not hide a frontend dependency behind `{compact}`"
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
        "src/tui_markdown.rs",
        "src/tui_model.rs",
        "src/tui_sessions.rs",
    ] {
        let source = fs::read_to_string(root.join(name)).expect("read frontend source");
        let production = source.split("\n#[cfg(test)]").next().unwrap_or("");
        assert!(
            !production.contains("Color::"),
            "{name} production code must use tui_theme roles, not raw Color::"
        );
    }
}

/// Frontends may hold public DTOs and implement ports, but must not reach into
/// private core modules or construct, persist, or spawn business capabilities.
#[test]
fn terminal_frontend_has_no_core_assembly_or_persistence_entrypoints() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let frontends = rust_sources(root)
        .into_iter()
        .filter(|path| is_frontend(path))
        .collect::<Vec<_>>();
    assert!(
        frontends.len() >= 6,
        "architecture guard failed to discover the complete terminal frontend"
    );
    for path in frontends {
        let source = fs::read_to_string(&path).expect("read frontend source");
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
            "mcp_client",
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
    let frontends = rust_sources(root)
        .into_iter()
        .filter(|path| is_frontend(path))
        .collect::<Vec<_>>();
    assert!(
        frontends.len() >= 6,
        "architecture guard failed to discover the complete terminal frontend"
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
        let source = fs::read_to_string(&path).expect("read frontend source");
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
