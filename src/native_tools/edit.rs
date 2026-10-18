//! Exact edits are planned against one authorized snapshot, then committed once.
use super::*;

struct Edit<'a> {
    old: &'a str,
    new: &'a str,
}
struct Replacement<'a> {
    start: usize,
    end: usize,
    new: &'a str,
}

impl Tool for EditFileTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "edit_file".into(),
            description: "Replace exact unique text in an existing file. Use old_str/new_str for one edit, OR edits:[{old_str,new_str},...] for up to 64 atomic edits of the SAME original file. Each old_str must match byte-for-byte exactly once; ranges must not overlap. Failure preserves the file and may return bounded read_file line hints; re-read before retrying, never guess or fuzzy-replace. Project-relative and absolute scratch paths work in Project Write; other outside paths require Full Access.".into(),
            input_schema: json!({
                "type":"object", "properties":{
                    "path":{"type":"string","description":"Existing project-relative or authorized absolute file path"},
                    "old_str":{"type":"string","description":"Exact unique text (single edit only)"},
                    "new_str":{"type":"string","description":"Replacement text (single edit only)"},
                    "edits":{"type":"array","minItems":1,"maxItems":64,"description":"Atomic replacements matched against the same original snapshot, not sequential substitutions", "items":{
                        "type":"object","properties":{"old_str":{"type":"string"},"new_str":{"type":"string"}},
                        "required":["old_str","new_str"],"additionalProperties":false
                    }}
                },"required":["path"],"additionalProperties":false
            }),
            effect: ToolEffect::Write,
            strict: false,
        }
    }

    fn error_output(&self, error: &ToolError) -> Value {
        super::argument_recovery::output(error)
    }

    fn invoke(
        &self,
        args: &Value,
        project: &Project,
        cancel: &CancelToken,
    ) -> Result<Value, ToolError> {
        check_cancelled(cancel)?;
        let edits = parse_edits(args)?;
        let requested = required_string_arg(args, "path", "edit_file")?;
        let target = project
            .writable_target(requested, false, self.scope.resolve())
            .map_err(|e| tool_io_error("edit_file", requested, e))?;
        if !target
            .is_file()
            .map_err(|e| tool_io_error("edit_file", requested, e))?
        {
            return Err(ToolError::new("edit_file: target is not a file"));
        }
        let content = target
            .read_to_string_limited(MAX_WRITE_BYTES)
            .map_err(|e| tool_io_error("edit_file", requested, e))?;
        let updated = plan(&content, &edits)?;
        check_cancelled(cancel)?;
        // Reuse the shared parent lock, no-follow capability, snapshot recheck,
        // atomic publish and file-review capture; no second write path.
        target
            .atomic_write(&updated, Some(&content))
            .map_err(|e| tool_io_error("edit_file", requested, e))?;
        Ok(json!({"path":requested,"replacements":edits.len(),"bytes":updated.len()}))
    }
}

fn check_keys(value: &Value, allowed: &[&str]) -> Result<(), ToolError> {
    let object = value
        .as_object()
        .ok_or_else(|| ToolError::new("edit_file: expected object"))?;
    if object.keys().any(|key| !allowed.contains(&key.as_str())) {
        return Err(ToolError::new("edit_file: unknown argument"));
    }
    Ok(())
}

fn parse_edits(args: &Value) -> Result<Vec<Edit<'_>>, ToolError> {
    super::argument_recovery::validate_edit(args)?;
    check_keys(args, &["path", "old_str", "new_str", "edits"])?;
    let values = if let Some(batch) = args.get("edits") {
        if args.get("old_str").is_some() || args.get("new_str").is_some() {
            return Err(ToolError::new(
                "edit_file: use edits OR old_str/new_str, not both",
            ));
        }
        let list = batch
            .as_array()
            .filter(|v| (1..=64).contains(&v.len()))
            .ok_or_else(|| ToolError::new("edit_file: edits requires 1..=64 entries"))?;
        list.iter().collect::<Vec<_>>()
    } else {
        vec![args]
    };
    let mut bytes = 0usize;
    values
        .into_iter()
        .map(|value| {
            if args.get("edits").is_some() {
                check_keys(value, &["old_str", "new_str"])?;
            }
            let old = required_string_arg(value, "old_str", "edit_file")?;
            let new = required_string_arg(value, "new_str", "edit_file")?;
            if old.is_empty() {
                return Err(ToolError::new("edit_file: `old_str` must not be empty"));
            }
            if old == new {
                return Err(ToolError::new(
                    "edit_file: `old_str` and `new_str` are identical",
                ));
            }
            bytes = bytes.saturating_add(old.len()).saturating_add(new.len());
            if bytes > 2 * MAX_WRITE_BYTES {
                return Err(ToolError::new(
                    "edit_file: combined edit text exceeds 2 MiB",
                ));
            }
            Ok(Edit { old, new })
        })
        .collect()
}

fn plan(content: &str, edits: &[Edit<'_>]) -> Result<String, ToolError> {
    let mut ranges = Vec::with_capacity(edits.len());
    for (index, edit) in edits.iter().enumerate() {
        let mut matches = super::edit_recovery::offsets(content, edit.old);
        let first = matches.next();
        if first.is_none() || matches.next().is_some() {
            return Err(super::edit_recovery::mismatch(content, edit.old, index));
        }
        let start = first.expect("unique match");
        ranges.push(Replacement {
            start,
            end: start + edit.old.len(),
            new: edit.new,
        });
    }
    ranges.sort_by_key(|r| r.start);
    if ranges.windows(2).any(|pair| pair[0].end > pair[1].start) {
        return Err(ToolError::new(
            "edit_file: edit ranges overlap; file preserved",
        ));
    }
    let removed: usize = ranges.iter().map(|r| r.end - r.start).sum();
    let added: usize = ranges.iter().map(|r| r.new.len()).sum();
    let size = content.len() - removed + added;
    if size > MAX_WRITE_BYTES {
        return Err(ToolError::new(format!(
            "edit_file: result would exceed {MAX_WRITE_BYTES} bytes"
        )));
    }
    let mut updated = content.to_owned();
    for range in ranges.into_iter().rev() {
        updated.replace_range(range.start..range.end, range.new);
    }
    Ok(updated)
}
