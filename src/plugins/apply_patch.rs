//! Single-file multi-hunk atomic patch plugin (Agent phase 1-A).

use super::services::{TOOL_SERVICE, TOOL_SERVICE_ID};
use crate::permission::WriteScopeSource;
use crate::plugin::{
    DisposeError, Plugin, PluginContext, PluginDescriptor, PluginError, PluginId, ScopeKind,
    ServiceId,
};
use crate::{CancelToken, Project, Tool, ToolDefinition, ToolEffect, ToolError};
use serde_json::{Value, json};
use std::sync::Arc;

const ID: PluginId = PluginId::new("builtin.apply_patch");
const REQUIRES: &[ServiceId] = &[TOOL_SERVICE_ID];
const DESCRIPTOR: PluginDescriptor = PluginDescriptor {
    id: ID,
    scope: ScopeKind::TrustedProject,
    provides: &[],
    requires: REQUIRES,
    optional: &[],
};
const MAX_PATCH_BYTES: usize = 1024 * 1024;
const MAX_FILE_BYTES: usize = 1024 * 1024;

pub(crate) struct ApplyPatchPlugin {
    pub(crate) scope: WriteScopeSource,
}

impl Plugin for ApplyPatchPlugin {
    fn descriptor(&self) -> &'static PluginDescriptor {
        &DESCRIPTOR
    }

    fn mount(&self, context: &mut PluginContext<'_>) -> Result<(), PluginError> {
        let tools = context
            .require(TOOL_SERVICE)
            .map_err(|error| PluginError::new(error.to_string()))?;
        let lease = tools
            .register(
                context.owner(),
                Arc::new(ApplyPatchTool {
                    scope: self.scope.clone(),
                }),
            )
            .map_err(|error| PluginError::new(error.to_string()))?;
        context.defer(move || {
            lease
                .revoke()
                .map_err(|error| DisposeError::new(error.to_string()))
        });
        Ok(())
    }
}

#[derive(Clone, Default)]
pub(crate) struct ApplyPatchTool {
    scope: WriteScopeSource,
}

impl Tool for ApplyPatchTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "apply_patch".into(),
            description: "Atomically apply one multi-hunk patch to one existing UTF-8 file. The patch must use `*** Begin Patch`, one `*** Update File: path`, one or more `@@` hunks, and `*** End Patch`. Every hunk is validated before the file is changed; add/delete/rename and multi-file patches are unsupported in v1.".into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "patch": {
                        "type": "string",
                        "description": "Single-file CLAT patch text (max 1 MiB)"
                    }
                },
                "required": ["patch"],
                "additionalProperties": false
            }),
            effect: ToolEffect::Write,
            strict: true,
        }
    }

    fn invoke(
        &self,
        arguments: &Value,
        project: &Project,
        cancel: &CancelToken,
    ) -> Result<Value, ToolError> {
        if cancel.is_cancelled() {
            return Err(ToolError::new("apply_patch: cancelled"));
        }
        let patch_text = arguments
            .get("patch")
            .and_then(Value::as_str)
            .ok_or_else(|| ToolError::new("apply_patch: `patch` must be a string"))?;
        if patch_text.len() > MAX_PATCH_BYTES {
            return Err(ToolError::new(format!(
                "apply_patch: patch exceeds {MAX_PATCH_BYTES} bytes"
            )));
        }
        let patch = crate::apply_patch::parse(patch_text).map_err(ToolError::new)?;
        let target = project
            .writable_target(&patch.path, false, self.scope.resolve())
            .map_err(|error| ToolError::new(format!("apply_patch: `{}`: {error}", patch.path)))?;
        if !target
            .is_file()
            .map_err(|error| ToolError::new(format!("apply_patch: `{}`: {error}", patch.path)))?
        {
            return Err(ToolError::new(format!(
                "apply_patch: `{}` is not an existing regular file",
                patch.path
            )));
        }
        let original = target
            .read_to_string_limited(MAX_FILE_BYTES)
            .map_err(|error| ToolError::new(format!("apply_patch: `{}`: {error}", patch.path)))?;
        if cancel.is_cancelled() {
            return Err(ToolError::new("apply_patch: cancelled"));
        }
        let updated = crate::apply_patch::apply(&original, &patch).map_err(ToolError::new)?;
        if updated.len() > MAX_FILE_BYTES {
            return Err(ToolError::new(format!(
                "apply_patch: result would exceed {MAX_FILE_BYTES} bytes"
            )));
        }
        if cancel.is_cancelled() {
            return Err(ToolError::new("apply_patch: cancelled"));
        }
        target
            .atomic_write(&updated, Some(&original))
            .map_err(|error| ToolError::new(format!("apply_patch: `{}`: {error}", patch.path)))?;
        Ok(json!({
            "path": patch.path,
            "bytes": updated.len(),
            "hunks": patch_text.lines().filter(|line| line.starts_with("@@")).count()
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ToolCall;
    use crate::permission::{PermissionDecision, PermissionPolicy, SafeByDefault};
    use crate::plugin::PluginManager;
    use crate::plugins::ToolRegistryPlugin;
    use crate::plugins::services::TOOL_SERVICE;
    use std::path::PathBuf;

    fn fixture(tag: &str) -> (PathBuf, Project) {
        let root = std::env::temp_dir().join(format!(
            "clat-apply-patch-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).expect("root");
        (root.clone(), Project::new(root))
    }

    fn patch(path: &str, body: &str) -> Value {
        json!({
            "patch": format!(
                "*** Begin Patch\n*** Update File: {path}\n{body}\n*** End Patch"
            )
        })
    }

    #[test]
    fn later_hunk_conflict_leaves_the_file_byte_exact() {
        let (root, project) = fixture("conflict");
        let path = root.join("demo.txt");
        let original = "alpha\nold-a\nmid\nold-b\nomega\n";
        std::fs::write(&path, original).expect("file");
        let error = ApplyPatchTool::default()
            .invoke(
                &patch(
                    "demo.txt",
                    "@@ first\n-old-a\n+new-a\n@@ stale\n-missing\n+new-b",
                ),
                &project,
                &CancelToken::new(),
            )
            .expect_err("second hunk conflicts");
        assert!(error.to_string().contains("hunk 2"), "{error}");
        assert_eq!(std::fs::read(&path).unwrap(), original.as_bytes());
        crate::test_support::cleanup_tree(&root);
    }

    #[test]
    fn preserves_permissions_and_rejects_symlink_targets() {
        let (root, project) = fixture("fence");
        let file = root.join("script.sh");
        std::fs::write(&file, "old\n").expect("file");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o755)).expect("mode");
        }
        ApplyPatchTool::default()
            .invoke(
                &patch("script.sh", "@@\n-old\n+new"),
                &project,
                &CancelToken::new(),
            )
            .expect("patch");
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "new\n");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            assert_eq!(
                std::fs::metadata(&file).unwrap().permissions().mode() & 0o777,
                0o755
            );
            std::os::unix::fs::symlink(&file, root.join("link.sh")).expect("symlink");
            assert!(
                ApplyPatchTool::default()
                    .invoke(
                        &patch("link.sh", "@@\n-new\n+bad"),
                        &project,
                        &CancelToken::new(),
                    )
                    .is_err()
            );
            assert_eq!(std::fs::read_to_string(&file).unwrap(), "new\n");
        }
        crate::test_support::cleanup_tree(&root);
    }

    #[test]
    fn patch_and_file_size_limits_fail_before_commit() {
        let (root, project) = fixture("limits");
        let file = root.join("demo.txt");
        std::fs::write(&file, "old\n").expect("file");
        let error = ApplyPatchTool::default()
            .invoke(
                &json!({"patch": "x".repeat(MAX_PATCH_BYTES + 1)}),
                &project,
                &CancelToken::new(),
            )
            .expect_err("patch cap");
        assert!(error.to_string().contains("patch exceeds"));
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "old\n");

        std::fs::write(&file, "x".repeat(MAX_FILE_BYTES + 1)).expect("large file");
        let error = ApplyPatchTool::default()
            .invoke(
                &patch("demo.txt", "@@\n-x\n+y"),
                &project,
                &CancelToken::new(),
            )
            .expect_err("file cap");
        assert!(error.to_string().contains("file cap"), "{error}");
        assert_eq!(
            std::fs::metadata(&file).unwrap().len(),
            (MAX_FILE_BYTES + 1) as u64
        );
        crate::test_support::cleanup_tree(&root);
    }

    #[test]
    fn plugin_registers_revokes_and_permission_classifies_write() {
        let (root, project) = fixture("plugin");
        let mut manager = PluginManager::root(ScopeKind::TrustedProject);
        manager
            .mount_all(vec![
                Arc::new(ToolRegistryPlugin),
                Arc::new(ApplyPatchPlugin {
                    scope: WriteScopeSource::ProjectRoot,
                }),
            ])
            .expect("mount");
        let tools = manager.require(TOOL_SERVICE).expect("tools");
        let definition = tools.get("apply_patch").expect("registered").definition();
        let call = ToolCall {
            id: "p".into(),
            name: "apply_patch".into(),
            arguments: json!({"patch": "x"}),
        };
        assert!(matches!(
            SafeByDefault.check(&project, &definition, &call),
            PermissionDecision::Ask { .. }
        ));
        manager.close().expect("close");
        assert!(tools.get("apply_patch").is_none(), "lease revoked");
        crate::test_support::cleanup_tree(&root);
    }
}
