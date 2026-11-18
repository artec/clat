//! Native file creation requires explicit whole-file replacement intent.
use super::*;

/// 写工具携带写入围栏来源（SR2）：每次 invoke 解析当前档位——FA 开放
/// 绝对路径，PW（与 exec）使用共享 roots；RO 保持项目根相对路径。
#[derive(Clone, Default)]
pub struct WriteFileTool {
    pub(super) scope: crate::permission::WriteScopeSource,
}

impl Tool for WriteFileTool {
    fn capability_metadata(&self) -> crate::ToolCapability {
        crate::ToolCapability::native(
            &["write", "file", "text", "json", "写入", "报表"],
            &["utf8-text"],
            &["file"],
            "Writes supplied text bytes, including JSON or reports; does not compute aggregates or validate formats. Write scope and approval checked at invocation.",
        )
    }

    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "write_file".into(),
            description: "Create a UTF-8 file, or replace it entirely when overwrite is true. Use edit_file for a partial change. Outside paths require Full Access.".into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string" },
                    "content": { "type": "string" },
                    "overwrite": { "type": "boolean" }
                },
                "required": ["path", "content"],
                "additionalProperties": false
            }),
            effect: ToolEffect::Write,
            strict: false,
        }
    }

    fn invoke(
        &self,
        arguments: &Value,
        project: &Project,
        cancel: &CancelToken,
    ) -> Result<Value, ToolError> {
        check_cancelled(cancel)?;
        let requested = required_string_arg(arguments, "path", "write_file")?;
        let content = required_string_arg(arguments, "content", "write_file")?;
        if content.len() > MAX_WRITE_BYTES {
            // FIX-5/CA-06：不再指向 edit_file——它的结果与目标同受
            // MAX_WRITE_BYTES 约束，指路只会把模型引向另一条失败路径。
            return Err(ToolError::new(format!(
                "write_file: content exceeds {MAX_WRITE_BYTES} bytes — split the write into smaller pieces"
            )));
        }
        let overwrite = match arguments.get("overwrite") {
            None => false,
            Some(Value::Bool(overwrite)) => *overwrite,
            Some(_) => return Err(ToolError::new("write_file: `overwrite` must be a boolean")),
        };
        // W-INV1：父目录创建、临时文件与 rename 全部绑定到同一个
        // 目标根 capability（项目根，或 FA 绝对路径的目标父目录），
        // 路径竞态不能把后续 I/O 引到目标根之外。
        let target = project
            .writable_target(requested, true, self.scope.resolve())
            .map_err(|error| tool_io_error("write_file", requested, error))?;
        let existed = if overwrite {
            target.atomic_write(content, None)
        } else {
            target.atomic_create(content).map(|()| false)
        }
        .map_err(|error| {
            if !overwrite && error.kind() == std::io::ErrorKind::AlreadyExists {
                ToolError::new("write_file: target already exists; existing contents were preserved. Use edit_file to append or change part of the file, or set overwrite: true with the complete replacement content.")
            } else {
                tool_io_error("write_file", requested, error)
            }
        })?;
        Ok(json!({
            "path": requested,
            "bytes": content.len(),
            "created": !existed
        }))
    }
}
