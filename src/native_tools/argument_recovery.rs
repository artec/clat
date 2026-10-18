//! Bounded, value-free diagnostics for rejected arguments. No repair or retry.
use super::*;
const MAX_ISSUES: usize = 8;
const REQUEST_KEYS: &[&str] = &["path", "old_str", "new_str", "edits"];
const EDIT_KEYS: &[&str] = &["old_str", "new_str"];

pub(super) fn output(error: &ToolError) -> Value {
    let mut value = json!({"error":error.to_string()});
    if let Some(recovery) = error.recovery() {
        let key = if recovery["kind"] == "arguments" {
            "argument_recovery"
        } else {
            "edit_recovery"
        };
        value[key] = recovery.clone();
    }
    value
}

pub(super) fn invalid_range(start: u64, end: u64) -> ToolError {
    ToolError::new("read_file: `end_line` must be greater than or equal to `start_line`")
        .with_recovery(json!({"kind":"arguments","code":"invalid_line_range",
            "operation_started":false,"start_line":start,"end_line":end,
            "next_action":"Check the intended inclusive line range and resubmit the same path with end_line >= start_line. Do not automatically swap bounds or widen the read. For continuation use the exact prior next_cursor and expected_file_version. No file was opened by this rejected call."}))
}

#[derive(Default)]
struct Issues {
    values: Vec<Value>,
    count: usize,
    first_message: Option<&'static str>,
}
impl Issues {
    fn add(&mut self, index: Option<usize>, code: &str, message: &'static str) {
        self.count += 1;
        self.first_message.get_or_insert(message);
        if self.values.len() < MAX_ISSUES {
            self.values.push(json!({"edit_index":index,"code":code}));
        }
    }
    fn check_keys(&mut self, value: &Value, allowed: &[&str], index: Option<usize>) {
        match value.as_object() {
            None => self.add(index, "expected_object", "edit_file: expected object"),
            Some(object) if object.keys().any(|key| !allowed.contains(&key.as_str())) => {
                self.add(index, "unknown_argument", "edit_file: unknown argument");
            }
            _ => {}
        }
    }
    fn finish(self) -> Result<(), ToolError> {
        let Some(message) = self.first_message else {
            return Ok(());
        };
        let omitted = self.count - self.values.len();
        Err(ToolError::new(message).with_recovery(json!({
            "kind":"arguments","code":"invalid_edit_arguments","applied":false,
            "issues":self.values,"issues_omitted":omitted,
            "allowed_request_fields":REQUEST_KEYS,"allowed_edit_fields":EDIT_KEYS,
            "next_action":"The entire batch was rejected before reading or writing the target. Correct all listed entries against the same original snapshot, then resubmit only if still authorized. Identical strings are not a replacement: remove that entry if unintended. Use only allowed fields; do not rename unexpected source text into a key. No entries were applied and no retry was scheduled."
        })))
    }
}

fn check_entry(value: &Value, index: usize, issues: &mut Issues) {
    let index = Some(index);
    issues.check_keys(value, EDIT_KEYS, index);
    if !value.is_object() {
        return;
    }
    let old = value.get("old_str").and_then(Value::as_str);
    let new = value.get("new_str").and_then(Value::as_str);
    if old.is_none() || new.is_none() {
        issues.add(
            index,
            "missing_string",
            "edit_file: old_str and new_str must be strings",
        );
    } else if old == Some("") {
        issues.add(
            index,
            "empty_match",
            "edit_file: `old_str` must not be empty",
        );
    } else if old == new {
        issues.add(
            index,
            "identical_strings",
            "edit_file: `old_str` and `new_str` are identical",
        );
    }
}

pub(super) fn validate_edit(args: &Value) -> Result<(), ToolError> {
    let mut issues = Issues::default();
    issues.check_keys(args, REQUEST_KEYS, None);
    if let Some(batch) = args.get("edits") {
        if args.get("old_str").is_some() || args.get("new_str").is_some() {
            issues.add(
                None,
                "mixed_forms",
                "edit_file: use edits OR old_str/new_str, not both",
            );
        }
        if let Some(list) = batch
            .as_array()
            .filter(|list| (1..=64).contains(&list.len()))
        {
            for (index, entry) in list.iter().enumerate() {
                check_entry(entry, index, &mut issues);
            }
        } else {
            issues.add(
                None,
                "invalid_batch",
                "edit_file: edits requires 1..=64 entries",
            );
        }
    } else if args.is_object() {
        // Single-edit request includes path, so check its text fields separately.
        check_entry(
            &json!({"old_str":args.get("old_str"),"new_str":args.get("new_str")}),
            0,
            &mut issues,
        );
    }
    issues.finish()
}
