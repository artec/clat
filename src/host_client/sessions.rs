use super::HostClient;
use crate::{SessionId, SessionSummary};
use serde_json::{Value, json};

impl HostClient {
    pub fn sessions(&self) -> Result<Vec<SessionSummary>, String> {
        decode_sessions(self.call("session.list", &json!({}))?)
    }
}

fn decode_sessions(value: Value) -> Result<Vec<SessionSummary>, String> {
    value["sessions"]
        .as_array()
        .ok_or("host omitted session list")?
        .iter()
        .map(|row| {
            let integer = |key| {
                row[key]
                    .as_u64()
                    .ok_or_else(|| format!("session lacks {key}"))
            };
            let timestamp = |key| {
                row[key]
                    .as_i64()
                    .ok_or_else(|| format!("session lacks {key}"))
            };
            let id = row["id"]
                .as_str()
                .filter(|id| !id.is_empty())
                .ok_or("session lacks id")?;
            Ok(SessionSummary {
                id: SessionId::new(id.to_owned()),
                title: row["title"].as_str().map(str::to_owned),
                created_at_ms: timestamp("created_at_ms")?,
                last_activity_ms: timestamp("last_activity_ms")?,
                message_count: integer("message_count")?,
                turns: integer("turns")?,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn host_session_rows_are_typed_and_malformed_lists_fail_closed() {
        let value = json!({"sessions":[{"id":"one","title":"First", "created_at_ms":1,
            "last_activity_ms":2,"message_count":3,"turns":1}]});
        let rows = decode_sessions(value.clone()).unwrap();
        assert_eq!(rows[0].id.as_str(), "one");
        assert_eq!(rows[0].message_count, 3);
        let mut malformed = value;
        malformed["sessions"][0]["turns"] = json!("1");
        assert!(decode_sessions(malformed).is_err());
        assert!(decode_sessions(json!({})).is_err());
    }
}
