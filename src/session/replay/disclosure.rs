//! Read-only disclosure from existing compaction facts; never estimate missing data.
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Clone, Debug, Default, Eq, PartialEq, Deserialize, Serialize)]
pub struct CompactionDisclosure {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub range: Option<(u64, u64)>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub records: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub estimated_tokens: Option<u64>,
}

impl CompactionDisclosure {
    pub(super) fn from_data(data: &Value) -> Self {
        let range = data.get("shadowedRange").and_then(|range| {
            let start = range.get("start")?.as_u64()?;
            let end = range.get("end")?.as_u64()?;
            (start <= end).then_some((start, end))
        });
        let records = data
            .get("shadowedSeqs")
            .and_then(Value::as_array)
            .and_then(|seqs| {
                let seqs = seqs
                    .iter()
                    .map(Value::as_u64)
                    .collect::<Option<std::collections::BTreeSet<_>>>()?;
                Some(seqs.len())
            });
        Self {
            id: data
                .get("compactionId")
                .and_then(Value::as_str)
                .map(str::to_owned),
            range,
            records,
            estimated_tokens: data.get("shadowedTokenCount").and_then(Value::as_u64),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn compaction_disclosure_counts_actual_records_without_filling_gaps() {
        let value = CompactionDisclosure::from_data(&json!({
            "compactionId":"c", "shadowedRange":{"start":1,"end":100},
            "shadowedSeqs":[1,4,4,100], "shadowedTokenCount":250
        }));
        assert_eq!(value.records, Some(3));
        assert_eq!(value.range, Some((1, 100)));
        assert_eq!(value.estimated_tokens, Some(250));
        assert_eq!(value.id.as_deref(), Some("c"));
    }

    #[test]
    fn compaction_disclosure_legacy_and_invalid_counts_are_unknown() {
        assert_eq!(
            CompactionDisclosure::from_data(&json!({})),
            CompactionDisclosure::default()
        );
        let value = CompactionDisclosure::from_data(&json!({
            "shadowedRange":{"start":10,"end":1}, "shadowedSeqs":[1,"bad"],
            "shadowedTokenCount":-1
        }));
        assert_eq!(value, CompactionDisclosure::default());
    }
}
