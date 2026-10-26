//! Public-message reading without selecting, repairing or opening a journal writer.
use super::*;
use crate::session::catalog::search;
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConversationRequest {
    pub reference: HistoryReference,
    #[serde(default)]
    pub trusted_projects: bool,
    #[serde(default)]
    pub after_seq: Option<u64>,
    #[serde(default)]
    pub version: Option<String>,
    #[serde(default)]
    pub attachments_only: bool,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ConversationMessage {
    pub reference: HistoryReference,
    pub role: String,
    pub client_message_id: Option<String>,
    pub time: i64,
    pub text: String,
    pub truncated: bool,
    pub attachments: Vec<crate::message::AttachmentDescriptor>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ConversationPage {
    pub reference: HistoryReference,
    pub messages: Vec<ConversationMessage>,
    pub next_seq: Option<u64>,
    pub version: String,
}
impl SessionQueryService {
    pub(crate) fn conversation(
        &self,
        request: ConversationRequest,
        cancel: &CancelToken,
    ) -> Result<ConversationPage, QueryError> {
        let key = self.resolve(&request.reference, request.trusted_projects, false)?;
        let mut messages = Vec::new();
        let mut more = false;
        let mut bytes = 0;
        let version = self.scan(&key, cancel, false, |event| {
            if request.after_seq.is_some_and(|seq| event.seq <= seq) {
                return;
            }
            let attachments = event_attachments(event);
            if request.attachments_only && attachments.is_empty() {
                return;
            }
            let doc = search::document(event);
            if doc.is_none() && attachments.is_empty() {
                return;
            }
            let role = doc.as_ref().map(|d| d.kind).unwrap_or("user");
            let source_text = doc.map(|d| d.text).unwrap_or_default();
            if messages.len() >= 40 || bytes >= 256 * 1024 {
                more = true;
                return;
            }
            let text: String = source_text.chars().take(65536).collect();
            bytes += text.len();
            messages.push(ConversationMessage {
                reference: HistoryReference {
                    seq: Some(event.seq),
                    ..request.reference.clone()
                },
                role: role.into(),
                client_message_id: event.data["clientMessageId"].as_str().map(str::to_owned),
                time: event.time,
                truncated: text.len() < source_text.len(),
                text,
                attachments,
            });
        })?;
        if request
            .version
            .as_ref()
            .is_some_and(|expected| expected != &version)
        {
            return Err(QueryError::new("source_changed"));
        }
        Ok(ConversationPage {
            reference: request.reference,
            next_seq: more
                .then(|| messages.last().and_then(|m| m.reference.seq))
                .flatten(),
            messages,
            version,
        })
    }
}

impl SessionQueryService {
    pub(crate) fn public_message(
        &self,
        reference: &HistoryReference,
        cancel: &CancelToken,
    ) -> Result<String, QueryError> {
        let key = self.resolve(reference, false, false)?;
        let mut text = None;
        self.scan(&key, cancel, false, |event| {
            if Some(event.seq) == reference.seq {
                text = search::document(event)
                    .map(|d| d.text)
                    .or_else(|| (!event_attachments(event).is_empty()).then(String::new));
            }
        })?;
        text.ok_or_else(|| QueryError::new("not_found"))
    }
}

fn event_attachments(
    event: &crate::session::event::SessionEvent,
) -> Vec<crate::message::AttachmentDescriptor> {
    use crate::session::replay::{ReplayAdapter, ReplayEvent};
    let mut rows = Vec::new();
    ReplayAdapter::new().push(event, &mut rows);
    rows.into_iter()
        .flat_map(|row| match row {
            ReplayEvent::UserMessage { content_blocks, .. }
            | ReplayEvent::ToolFinished { content_blocks, .. } => content_blocks,
            _ => Vec::new(),
        })
        .filter_map(|block| match block {
            crate::message::ContentBlock::Image { attachment } => Some(attachment),
            _ => None,
        })
        .collect()
}
impl SessionQueryService {
    pub(crate) fn attachment(
        &self,
        reference: &HistoryReference,
        id: &str,
        all: bool,
        cancel: &CancelToken,
    ) -> Result<serde_json::Value, QueryError> {
        let key = self.resolve(reference, all, false)?;
        let mut descriptor = None;
        self.scan(&key, cancel, false, |event| {
            if reference.seq == Some(event.seq) {
                descriptor = event_attachments(event)
                    .into_iter()
                    .find(|a| a.attachment_id == id);
            }
        })?;
        let descriptor = descriptor.ok_or_else(|| QueryError::new("not_found"))?;
        let bytes = self
            .reader
            .attachment(&key, &descriptor)
            .map_err(|e| QueryError::new("source_unavailable").detail(e))?;
        // Recheck trust after I/O, before releasing any bytes.
        self.resolve(reference, all, false)?;
        use base64::Engine;
        Ok(
            json!({"media_type":descriptor.media_type,"base64":base64::engine::general_purpose::STANDARD.encode(bytes)}),
        )
    }
}
