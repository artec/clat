use super::*;
use serde::Serialize;
#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum Check {
    IsTrue {
        id: String,
        pointer: String,
    },
    Equals {
        id: String,
        pointer: String,
        expected: Value,
    },
    AllBefore {
        id: String,
        pointer: String,
        before: String,
        after: String,
    },
}
impl Check {
    pub(crate) fn identity(&self) -> (&str, &str) {
        match self {
            Self::IsTrue { id, pointer }
            | Self::Equals { id, pointer, .. }
            | Self::AllBefore { id, pointer, .. } => (id, pointer),
        }
    }
    pub(crate) fn validate(&self) -> Result<(), ToolError> {
        if let Self::AllBefore { before, after, .. } = self
            && (before.is_empty()
                || after.is_empty()
                || before == after
                || before.len() > 256
                || after.len() > 256)
        {
            return Err(ToolError::new(
                "all_before needs distinct nonempty bounded labels",
            ));
        }
        Ok(())
    }
    pub(crate) fn evaluate(&self, source: &Value) -> Value {
        let (id, pointer) = self.identity();
        let Some(actual) = source.pointer(pointer) else {
            return result(id, "inconclusive", "missing_pointer");
        };
        let (status, reason) = match self {
            Self::IsTrue { .. } => match actual.as_bool() {
                Some(true) => ("passed", "true"),
                Some(false) => ("failed", "false"),
                None => ("inconclusive", "expected_boolean"),
            },
            Self::Equals { expected, .. } => {
                if actual == expected {
                    ("passed", "equal")
                } else {
                    ("failed", "different")
                }
            }
            Self::AllBefore { before, after, .. } => ordered(actual, before, after),
        };
        result(id, status, reason)
    }
}
fn result(id: &str, status: &str, reason: &str) -> Value {
    json!({"id":id,"status":status,"reason":reason})
}
fn ordered(actual: &Value, before: &str, after: &str) -> (&'static str, &'static str) {
    let Some(rows) = actual.as_array() else {
        return ("inconclusive", "expected_trace_array");
    };
    let mut last_before = None;
    let mut first_after = None;
    for (i, row) in rows.iter().enumerate() {
        let Some(label) = row.as_str() else {
            return ("inconclusive", "expected_string_labels");
        };
        if label == before {
            last_before = Some(i);
        }
        if label == after && first_after.is_none() {
            first_after = Some(i);
        }
    }
    match (last_before, first_after) {
        (Some(a), Some(b)) if a < b => ("passed", "all_before"),
        (Some(_), Some(_)) => ("failed", "order_violated"),
        _ => ("inconclusive", "missing_order_group"),
    }
}

pub(crate) fn validate_checks(checks: &[Check]) -> Result<(), ToolError> {
    if checks.is_empty() || checks.len() > 16 {
        return Err(ToolError::new("observation needs 1..16 checks"));
    }
    let mut ids = std::collections::HashSet::new();
    for check in checks {
        let (id, pointer) = check.identity();
        if id.is_empty()
            || id.len() > 64
            || !id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b))
            || !ids.insert(id)
            || pointer.len() > 1024
            || (!pointer.is_empty() && !pointer.starts_with('/'))
        {
            return Err(ToolError::new(
                "verify_observation: unique ASCII check ids and bounded JSON pointers required",
            ));
        }
        check.validate()?;
    }
    Ok(())
}
