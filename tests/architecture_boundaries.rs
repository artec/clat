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
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name == "tui.rs" || name.starts_with("tui_"))
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
