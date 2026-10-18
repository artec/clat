use super::*;
#[derive(Deserialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
pub(super) enum Check {
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
    pub(super) fn identity(&self) -> (&str, &str) {
        match self {
            Self::IsTrue { id, pointer }
            | Self::Equals { id, pointer, .. }
            | Self::AllBefore { id, pointer, .. } => (id, pointer),
        }
    }
    pub(super) fn validate(&self) -> Result<(), ToolError> {
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
    pub(super) fn evaluate(&self, source: &Value) -> Value {
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
