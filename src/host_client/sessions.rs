use super::HostClient;
use crate::{SessionId, SessionSummary};
use serde_json::{Value, json};

#[derive(Clone, Debug)]
pub struct HostSessionSharePreview {
    pub selection: crate::application::SessionShareSelection,
    pub title: String,
    pub turns: usize,
    pub characters: usize,
    pub last_time: i64,
    pub snapshot_time: u64,
}

#[derive(Clone, Debug)]
pub struct HostSharedInput {
    pub text: String,
    pub client_message_id: String,
    pub operation_id: String,
    pub selection_generation: u64,
    pub shares: Vec<crate::application::SessionShareSelection>,
}

impl HostClient {
    pub fn submit_shared_input(
        &self,
        input: &HostSharedInput,
    ) -> Result<Value, super::HostCallError> {
        self.call_with_receipt("input.submit", &shared_input_params(input))
    }
    pub fn sessions(&self) -> Result<Vec<SessionSummary>, String> {
        decode_sessions(self.call("session.list", &json!({}))?)
    }

    pub fn session_share_preview(
        &self,
        reference: &crate::application::HistoryReference,
        recent_turns: Option<usize>,
        selection: u64,
    ) -> Result<HostSessionSharePreview, String> {
        let value = self.call(
            "history.share.preview",
            &json!({
                "reference": reference,
                "recent_turns": recent_turns,
                "expected_selection_generation": selection,
            }),
        )?;
        validate_share_preview(decode_share_preview(value)?, reference, recent_turns)
    }
}

fn shared_input_params(input: &HostSharedInput) -> Value {
    json!({
        "text": input.text,
        "clientMessageId": input.client_message_id,
        "operationId": input.operation_id,
        "expected_selection_generation": input.selection_generation,
        "sessionShares": input.shares,
    })
}

fn validate_share_preview(
    preview: HostSessionSharePreview,
    reference: &crate::application::HistoryReference,
    recent_turns: Option<usize>,
) -> Result<HostSessionSharePreview, String> {
    if preview.selection.reference != *reference
        || preview.selection.recent_turns != recent_turns
        || preview.selection.version.is_empty()
    {
        return Err("session share preview does not match the request".into());
    }
    Ok(preview)
}

fn decode_share_preview(value: Value) -> Result<HostSessionSharePreview, String> {
    #[derive(serde::Deserialize)]
    struct Preview {
        reference: crate::application::HistoryReference,
        recent_turns: Option<usize>,
        version: String,
        title: String,
        turns: usize,
        characters: usize,
        last_time: i64,
        snapshot_time: u64,
    }
    let preview: Preview =
        serde_json::from_value(value).map_err(|_| "invalid session share preview")?;
    Ok(HostSessionSharePreview {
        selection: crate::application::SessionShareSelection {
            reference: preview.reference,
            recent_turns: preview.recent_turns,
            version: preview.version,
        },
        title: preview.title,
        turns: preview.turns,
        characters: preview.characters,
        last_time: preview.last_time,
        snapshot_time: preview.snapshot_time,
    })
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
                pinned: row["pinned"].as_bool().unwrap_or(false),
                archived: row["archived"].as_bool().unwrap_or(false),
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn share_preview_requires_complete_metadata_and_preserves_version() {
        let value = json!({"reference":{"project":"/project","session":"source","seq":null},"recent_turns":null,"characters":42,"turns":2,"version":"v1","title":"Title","last_time":12,"snapshot_time":13});
        let preview = decode_share_preview(value.clone()).unwrap();
        assert_eq!(preview.selection.version, "v1");
        assert_eq!(preview.characters, 42);
        let mut malformed = value;
        malformed["version"] = json!(null);
        assert!(decode_share_preview(malformed).is_err());
    }
    #[test]
    fn share_preview_rejects_wrong_source_and_range() {
        let reference = crate::application::HistoryReference {
            project: Some("/project".into()),
            session: "source".into(),
            seq: None,
        };
        let value = json!({"reference":reference,"recent_turns":2,"characters":42,"turns":2,"version":"v1","title":"Title","last_time":12,"snapshot_time":13});
        assert!(
            validate_share_preview(
                decode_share_preview(value.clone()).unwrap(),
                &reference,
                Some(2)
            )
            .is_ok()
        );
        assert!(
            validate_share_preview(
                decode_share_preview(value.clone()).unwrap(),
                &reference,
                None
            )
            .is_err()
        );
        let mut wrong = value;
        wrong["reference"]["session"] = json!("other");
        assert!(
            validate_share_preview(decode_share_preview(wrong).unwrap(), &reference, Some(2))
                .is_err()
        );
    }
    #[test]
    fn shared_input_request_preserves_user_and_operation_identity() {
        let selection = crate::application::SessionShareSelection {
            reference: crate::application::HistoryReference {
                project: Some("/project".into()),
                session: "source".into(),
                seq: None,
            },
            recent_turns: Some(3),
            version: "v1".into(),
        };
        let request = HostSharedInput {
            text: "ask".into(),
            client_message_id: "message".into(),
            operation_id: "operation".into(),
            selection_generation: 8,
            shares: vec![selection],
        };
        let value = shared_input_params(&request);
        assert_eq!(value["clientMessageId"], "message");
        assert_eq!(value["operationId"], "operation");
        assert_eq!(value["expected_selection_generation"], 8);
        assert_eq!(value["sessionShares"][0]["reference"]["session"], "source");
        assert_eq!(value["sessionShares"][0]["recent_turns"], 3);
    }
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
