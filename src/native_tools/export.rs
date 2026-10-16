use super::*;

pub(super) struct ExportPatchTool;
impl Tool for ExportPatchTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name:"export_patch".into(),
            description:"Freeze current workspace changes against Git HEAD into a standard binary-safe patch plus SHA256 manifest, including non-ignored untracked files. This includes pre-existing edits, not Agent attribution. Use a fresh Git-ignored project-relative path; manifest is published last. No staging or commits. Refuses unsupported types, drift and limits instead of exporting a partial patch.".into(),
            input_schema:json!({"type":"object","properties":{"path":{"type":"string","description":"New patch path in a Git-ignored directory, e.g. .tmp/delivery.patch; adjacent .manifest.json is the completion marker. Requires repository root; at most 128 changed files, 1 MiB per file/patch, 8 MiB aggregate sources."}},"required":["path"],"additionalProperties":false}),
            effect:ToolEffect::Write,
            strict:false,
        }
    }
    fn invoke(
        &self,
        arguments: &Value,
        project: &Project,
        cancel: &CancelToken,
    ) -> Result<Value, ToolError> {
        check_cancelled(cancel)?;
        let path = required_string_arg(arguments, "path", "export_patch")?;
        project.export_patch(path, cancel).map_err(ToolError::new)
    }
}
