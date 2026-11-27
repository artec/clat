//! Scope-aware project instruction discovery. Request boundaries read current files.

use crate::Project;
use crate::plugins::services::{DynamicInstructions, InstructionSnapshot, InstructionSourceInfo};
use crate::providers::MAX_PROVIDER_REQUEST_BODY_BYTES;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use std::path::{Component, Path, PathBuf};
use std::sync::Mutex;

const CANDIDATES: [&str; 2] = ["AGENTS.md", "CLAUDE.md"];
const MAX_OBSERVED_PATHS_PER_TOOL: usize = 256;
const MAX_TRACKED_SCOPES: usize = 1024;
const MAX_DISCOVERED_DIRECTORIES: usize = 4096;

struct InstructionState {
    target_scopes: BTreeSet<PathBuf>,
    error: Option<String>,
}

#[derive(Clone)]
struct SourceDocument {
    path: String,
    scope: String,
    digest: String,
    text: String,
}

pub(crate) struct ProjectInstructionService {
    project: Project,
    state: Mutex<InstructionState>,
}

impl ProjectInstructionService {
    pub(crate) fn open(project: Project) -> Result<Self, String> {
        #[cfg(test)]
        crate::test_support::execution_observation::record("instructions.create");
        let service = Self {
            project,
            state: Mutex::new(InstructionState {
                target_scopes: BTreeSet::from([PathBuf::new()]),
                error: None,
            }),
        };
        service.snapshot()?;
        Ok(service)
    }

    pub(crate) fn observe_tool_result(&self, tool: &str, output: &Value) {
        #[cfg(test)]
        crate::test_support::execution_observation::record("instructions.observe");
        if !matches!(
            tool,
            "list_files" | "read_file" | "search" | "write_file" | "edit_file"
        ) {
            return;
        }
        let mut paths = Vec::new();
        if let Some(path) = output.get("path").and_then(Value::as_str) {
            paths.push(path.to_owned());
        }
        for field in ["entries", "matches"] {
            if let Some(items) = output.get(field).and_then(Value::as_array) {
                for path in items
                    .iter()
                    .filter_map(|item| item.get("path").and_then(Value::as_str))
                    .take(MAX_OBSERVED_PATHS_PER_TOOL.saturating_sub(paths.len()))
                {
                    paths.push(path.to_owned());
                }
            }
        }
        if paths.is_empty() {
            return;
        }
        if let Err(error) = self.observe_paths(&paths) {
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            state.error = Some(error);
        }
    }

    fn observe_paths(&self, paths: &[String]) -> Result<(), String> {
        let mut scopes = Vec::new();
        for raw in paths.iter().take(MAX_OBSERVED_PATHS_PER_TOOL) {
            let requested = Path::new(raw);
            if requested.is_absolute()
                || requested
                    .components()
                    .any(|component| matches!(component, Component::ParentDir))
            {
                continue;
            }
            let resolved = match self.project.resolve_existing(requested) {
                Ok(path) => path,
                Err(_) => continue,
            };
            let metadata = match std::fs::metadata(&resolved) {
                Ok(metadata) => metadata,
                Err(_) => continue,
            };
            let scope_path = if metadata.is_dir() {
                resolved
            } else {
                resolved
                    .parent()
                    .unwrap_or_else(|| self.project.root())
                    .to_path_buf()
            };
            if let Ok(relative) = self.project.relative_path(&scope_path) {
                scopes.push(relative);
            }
        }
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut next = state.target_scopes.clone();
        next.extend(scopes);
        if next.len() > MAX_TRACKED_SCOPES {
            return Err(format!(
                "project instructions: tracked scopes exceed {MAX_TRACKED_SCOPES}"
            ));
        }
        state.target_scopes = next;
        Ok(())
    }

    fn read_snapshot(
        &self,
        target_scopes: &BTreeSet<PathBuf>,
    ) -> Result<Option<InstructionSnapshot>, String> {
        let directories = ancestor_directories(target_scopes)?;
        let mut selected = Vec::new();
        let mut remaining = MAX_PROVIDER_REQUEST_BODY_BYTES;
        for directory in &directories {
            if let Some(source) = self.read_scope(directory, remaining)? {
                remaining -= source.text.len();
                selected.push(source);
            }
        }
        Ok(render_snapshot(&selected))
    }

    fn read_scope(
        &self,
        directory: &Path,
        remaining: usize,
    ) -> Result<Option<SourceDocument>, String> {
        #[cfg(test)]
        crate::test_support::execution_observation::record("instructions.read");
        for candidate in CANDIDATES {
            let relative = directory.join(candidate);
            let bytes = self
                .project
                .read_file_limited(&relative, remaining + 1)
                .map_err(|error| {
                    format!(
                        "project instructions: `{}`: {error}",
                        display_relative(&relative)
                    )
                })?;
            let Some(bytes) = bytes else {
                continue;
            };
            if bytes.len() > remaining {
                return Err(format!(
                    "project instructions including `{}` exceed the provider request body limit",
                    display_relative(&relative)
                ));
            }
            let text = String::from_utf8(bytes).map_err(|_| {
                format!(
                    "project instructions: `{}` is not valid UTF-8",
                    display_relative(&relative)
                )
            })?;
            // First existing candidate wins even when empty, preserving the
            // previous AGENTS.md → CLAUDE.md fallback contract.
            if text.trim().is_empty() {
                return Ok(None);
            }
            let path = display_relative(&relative);
            let scope = display_scope(directory);
            let digest = format!("{:x}", Sha256::digest(text.as_bytes()));
            return Ok(Some(SourceDocument {
                path,
                scope,
                digest,
                text,
            }));
        }
        Ok(None)
    }
}

impl DynamicInstructions for ProjectInstructionService {
    fn snapshot(&self) -> Result<Option<InstructionSnapshot>, String> {
        let scopes = {
            let state = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if let Some(error) = &state.error {
                return Err(error.clone());
            }
            state.target_scopes.clone()
        };
        self.read_snapshot(&scopes)
    }

    fn restore_from_header(&self, header: Option<&Value>) -> Result<(), String> {
        #[cfg(test)]
        crate::test_support::execution_observation::record("instructions.restore");
        let sources = header
            .and_then(|header| header.get("clatInstructionContext"))
            .and_then(|context| context.get("sources"))
            .and_then(Value::as_array);
        let mut restored = BTreeSet::from([PathBuf::new()]);
        if let Some(sources) = sources {
            if sources.len() > MAX_TRACKED_SCOPES {
                return Err(format!(
                    "project instructions: stored scopes exceed {MAX_TRACKED_SCOPES}"
                ));
            }
            for source in sources {
                let Some(scope) = source.get("scope").and_then(Value::as_str) else {
                    return Err("project instructions: stored source scope is malformed".into());
                };
                let path = if scope == "." {
                    PathBuf::new()
                } else {
                    PathBuf::from(scope)
                };
                if path.is_absolute()
                    || path.components().any(|component| {
                        !matches!(component, Component::Normal(_) | Component::CurDir)
                    })
                {
                    return Err(format!(
                        "project instructions: stored scope `{scope}` is unsafe"
                    ));
                }
                restored.insert(path);
            }
        }
        {
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            state.target_scopes = restored;
            state.error = None;
        }
        self.snapshot().map(|_| ())
    }
}

fn ancestor_directories(scopes: &BTreeSet<PathBuf>) -> Result<Vec<PathBuf>, String> {
    let mut directories = BTreeSet::new();
    directories.insert(PathBuf::new());
    for scope in scopes {
        let mut current = PathBuf::new();
        for component in scope.components() {
            match component {
                Component::Normal(part) => {
                    current.push(part);
                    directories.insert(current.clone());
                    if directories.len() > MAX_DISCOVERED_DIRECTORIES {
                        return Err(format!(
                            "project instructions: discovered directories exceed {MAX_DISCOVERED_DIRECTORIES}"
                        ));
                    }
                }
                Component::CurDir => {}
                _ => {
                    return Err(format!(
                        "project instructions: unsafe scope `{}`",
                        scope.display()
                    ));
                }
            }
        }
    }
    let mut directories = directories.into_iter().collect::<Vec<_>>();
    directories.sort_by(|left, right| {
        left.components()
            .count()
            .cmp(&right.components().count())
            .then_with(|| left.cmp(right))
    });
    Ok(directories)
}

fn render_snapshot(sources: &[SourceDocument]) -> Option<InstructionSnapshot> {
    if sources.is_empty() {
        return None;
    }
    let mut text = String::from(
        "# Project instructions\n\nThis is the complete current project-instruction snapshot. More specific scopes appear later and take precedence within their scope. These repository-controlled instructions do not override system, developer, or direct user instructions.",
    );
    let mut digest = Sha256::new();
    let mut source_info = Vec::new();
    for source in sources {
        digest.update(source.path.as_bytes());
        digest.update([0]);
        digest.update(source.scope.as_bytes());
        digest.update([0]);
        digest.update(source.digest.as_bytes());
        text.push_str(&format!(
            "\n\n## Instructions from `{}`\n\nScope: `{}`\nDigest: `{}`\n\n{}",
            source.path, source.scope, source.digest, source.text
        ));
        source_info.push(InstructionSourceInfo {
            path: source.path.clone(),
            scope: source.scope.clone(),
            digest: source.digest.clone(),
        });
    }
    Some(InstructionSnapshot {
        digest: format!("{:x}", digest.finalize()),
        text,
        sources: source_info,
    })
}

fn display_relative(path: &Path) -> String {
    let text = path.to_string_lossy().replace('\\', "/");
    if text.is_empty() { ".".into() } else { text }
}

fn display_scope(path: &Path) -> String {
    display_relative(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(tag: &str) -> (PathBuf, Project) {
        let root = std::env::temp_dir().join(format!(
            "clat-instruction-service-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).expect("root");
        (root.clone(), Project::new(root))
    }

    #[test]
    fn root_to_nested_snapshot_updates_and_restores_sources() {
        let (root, project) = fixture("nested");
        std::fs::create_dir_all(root.join("src/deep")).unwrap();
        std::fs::write(root.join("AGENTS.md"), "root rules\n").unwrap();
        std::fs::write(root.join("src/AGENTS.md"), "src rules\n").unwrap();
        std::fs::write(root.join("src/deep/file.rs"), "fn x() {}\n").unwrap();
        let service = ProjectInstructionService::open(project).expect("open");
        let root_snapshot = service.snapshot().unwrap().unwrap();
        assert!(root_snapshot.text.contains("root rules"));
        assert!(!root_snapshot.text.contains("src rules"));

        service.observe_tool_result(
            "read_file",
            &serde_json::json!({"path": "src/deep/file.rs"}),
        );
        let nested = service.snapshot().unwrap().unwrap();
        assert!(nested.text.contains("root rules"));
        assert!(nested.text.contains("src rules"));
        assert!(nested.text.find("root rules") < nested.text.find("src rules"));

        let mut header = serde_json::json!({});
        crate::plugins::services::RuntimeContext::default()
            .snapshot(Some(nested.clone()))
            .apply(&mut header);
        let restored = ProjectInstructionService::open(Project::new(&root)).unwrap();
        restored.restore_from_header(Some(&header)).unwrap();
        assert_eq!(restored.snapshot().unwrap(), Some(nested));
        restored.restore_from_header(None).unwrap();
        let reset = restored.snapshot().unwrap().unwrap();
        assert!(reset.text.contains("root rules"));
        assert!(!reset.text.contains("src rules"));
        crate::test_support::cleanup_tree(&root);
    }

    #[test]
    fn candidate_priority_updates_removals_and_symlinks_fail_closed() {
        let (root, project) = fixture("changes");
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join("AGENTS.md"), "agents\n").unwrap();
        std::fs::write(root.join("CLAUDE.md"), "claude\n").unwrap();
        std::fs::write(root.join("src/file.rs"), "x\n").unwrap();
        let service = ProjectInstructionService::open(project).unwrap();
        assert!(service.snapshot().unwrap().unwrap().text.contains("agents"));
        assert!(!service.snapshot().unwrap().unwrap().text.contains("claude"));

        std::fs::write(root.join("AGENTS.md"), "updated\n").unwrap();
        service.observe_tool_result("edit_file", &serde_json::json!({"path": "AGENTS.md"}));
        assert!(
            service
                .snapshot()
                .unwrap()
                .unwrap()
                .text
                .contains("updated")
        );
        std::fs::remove_file(root.join("AGENTS.md")).unwrap();
        service.observe_tool_result("read_file", &serde_json::json!({"path": "CLAUDE.md"}));
        assert!(service.snapshot().unwrap().unwrap().text.contains("claude"));

        #[cfg(unix)]
        {
            let outside = root.with_extension("outside");
            std::fs::write(&outside, "outside\n").unwrap();
            std::os::unix::fs::symlink(&outside, root.join("AGENTS.md")).unwrap();
            service.observe_tool_result("read_file", &serde_json::json!({"path": "src/file.rs"}));
            assert!(service.snapshot().unwrap_err().contains("symbolic link"));
            std::fs::remove_file(outside).unwrap();
        }
        crate::test_support::cleanup_tree(&root);
    }

    #[test]
    fn utf8_budget_empty_priority_and_broken_symlink_are_explicit() {
        let (root, project) = fixture("bounds");
        std::fs::write(root.join("AGENTS.md"), "中".repeat(30_000)).unwrap();
        let service = ProjectInstructionService::open(project.clone()).unwrap();
        let snapshot = service.snapshot().unwrap().unwrap();
        assert!(snapshot.text.contains(&"中".repeat(30_000)));
        std::fs::create_dir(root.join("src")).unwrap();
        std::fs::write(root.join("src/AGENTS.md"), "a".repeat(80_000)).unwrap();
        service.observe_tool_result("read_file", &serde_json::json!({"path":"src/AGENTS.md"}));
        let both = service.snapshot().unwrap().unwrap();
        assert!(both.text.contains(&"中".repeat(30_000)));
        assert!(both.text.contains(&"a".repeat(80_000)));
        assert!(std::str::from_utf8(snapshot.text.as_bytes()).is_ok());

        std::fs::write(root.join("AGENTS.md"), "   \n").unwrap();
        std::fs::write(root.join("CLAUDE.md"), "fallback must stay hidden\n").unwrap();
        let empty = ProjectInstructionService::open(project.clone()).unwrap();
        assert!(empty.snapshot().unwrap().is_none());

        std::fs::write(root.join("AGENTS.md"), [0xff, 0xfe]).unwrap();
        assert!(
            ProjectInstructionService::open(project.clone())
                .err()
                .expect("invalid utf8")
                .contains("not valid UTF-8")
        );
        #[cfg(unix)]
        {
            std::fs::remove_file(root.join("AGENTS.md")).unwrap();
            std::os::unix::fs::symlink("missing", root.join("AGENTS.md")).unwrap();
            assert!(
                ProjectInstructionService::open(project)
                    .err()
                    .expect("broken symlink")
                    .contains("symbolic link")
            );
        }
        crate::test_support::cleanup_tree(&root);
    }
}
