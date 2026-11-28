//! Normalize one `im.message.receive_v1` event into a private-chat text message.
//!
//! Group chats, bots, and non-text messages are dropped here so the remote
//! core never sees them.

use serde_json::Value;

const MAX_ID_BYTES: usize = 1024;
const MAX_TEXT_BYTES: usize = 64 * 1024;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct InboundText {
    pub(crate) message_id: String,
    pub(crate) open_id: String,
    pub(crate) chat_id: String,
    pub(crate) text: String,
}

pub(crate) fn p2p_user_text(payload: &Value) -> Option<InboundText> {
    if payload.pointer("/header/event_type")?.as_str()? != "im.message.receive_v1" {
        return None;
    }
    let event = payload.get("event")?;
    if event.pointer("/sender/sender_type")?.as_str()? != "user" {
        return None;
    }
    let message = event.get("message")?;
    if message.get("chat_type")?.as_str()? != "p2p" {
        return None;
    }
    if message.get("message_type")?.as_str()? != "text" {
        return None;
    }
    let open_id = event.pointer("/sender/sender_id/open_id")?.as_str()?;
    let chat_id = message.get("chat_id")?.as_str()?;
    let message_id = message.get("message_id")?.as_str()?;
    let content = message.get("content")?.as_str()?;
    let text = serde_json::from_str::<Value>(content)
        .ok()?
        .get("text")?
        .as_str()?
        .trim()
        .to_owned();
    inbound(message_id, open_id, chat_id, text)
}

fn inbound(message_id: &str, open_id: &str, chat_id: &str, text: String) -> Option<InboundText> {
    if text.is_empty() || text.len() > MAX_TEXT_BYTES || !safe_id(message_id) {
        return None;
    }
    if !safe_id(open_id) || !safe_id(chat_id) {
        return None;
    }
    Some(InboundText {
        message_id: message_id.to_owned(),
        open_id: open_id.to_owned(),
        chat_id: chat_id.to_owned(),
        text,
    })
}

fn safe_id(value: &str) -> bool {
    !value.is_empty() && value.len() <= MAX_ID_BYTES && !value.chars().any(char::is_control)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn event(chat_type: &str, sender: &str, message_type: &str, content: &str) -> Value {
        json!({
            "header": {"event_type": "im.message.receive_v1"},
            "event": {
                "sender": {"sender_type": sender, "sender_id": {"open_id": "ou_user"}},
                "message": {
                    "message_id": "om_1",
                    "chat_id": "oc_1",
                    "chat_type": chat_type,
                    "message_type": message_type,
                    "content": content
                }
            }
        })
    }

    #[test]
    fn only_private_user_text_is_admitted() {
        let text = event("p2p", "user", "text", r#"{"text":" hello "}"#);
        let inbound = p2p_user_text(&text).unwrap();
        assert_eq!(inbound.text, "hello");
        assert_eq!(inbound.open_id, "ou_user");
        assert_eq!(inbound.chat_id, "oc_1");
        assert!(p2p_user_text(&event("group", "user", "text", r#"{"text":"hi"}"#)).is_none());
        assert!(p2p_user_text(&event("p2p", "bot", "text", r#"{"text":"hi"}"#)).is_none());
        assert!(p2p_user_text(&event("p2p", "user", "image", r#"{"image_key":"img"}"#)).is_none());
    }
}
