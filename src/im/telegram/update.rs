//! One Telegram update, reduced to a private-chat text message.
//!
//! Groups, channels, photos, and every other update stay out of the queue.
//! The offset still moves past them so a crash retry does not pull them again.

use serde_json::Value;
use std::sync::mpsc::{SyncSender, TrySendError};

#[derive(Debug)]
pub(crate) struct InboundText {
    pub(crate) user_id: String,
    pub(crate) chat_id: String,
    pub(crate) update_id: String,
    pub(crate) text: String,
}

#[derive(Debug)]
pub(crate) struct PolledUpdate {
    pub(crate) id: i64,
    pub(crate) text: Option<InboundText>,
}

#[derive(Debug)]
pub(crate) enum QueuePause {
    Full,
    Closed,
}

pub(crate) fn read_update(value: &Value) -> Option<PolledUpdate> {
    let id = value.get("update_id")?.as_i64()?;
    if id < 0 {
        return None;
    }
    Some(PolledUpdate {
        id,
        text: private_text(value, id),
    })
}

pub(crate) fn queue_update(
    update: PolledUpdate,
    sender: &SyncSender<InboundText>,
) -> Result<i64, QueuePause> {
    if let Some(text) = update.text {
        match sender.try_send(text) {
            Ok(()) => {}
            Err(TrySendError::Full(_)) => return Err(QueuePause::Full),
            Err(TrySendError::Disconnected(_)) => return Err(QueuePause::Closed),
        }
    }
    Ok(update.id.saturating_add(1))
}

fn private_text(value: &Value, id: i64) -> Option<InboundText> {
    let message = value.get("message")?;
    let chat = message.get("chat")?;
    if chat.get("type")?.as_str()? != "private" {
        return None;
    }
    if message
        .get("from")
        .and_then(|from| from.get("is_bot"))
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        return None;
    }
    let text = message.get("text")?.as_str()?;
    if text.is_empty() {
        return None;
    }
    let user_id = message.get("from")?.get("id")?.as_i64()?;
    let chat_id = chat.get("id")?.as_i64()?;
    Some(InboundText {
        user_id: user_id.to_string(),
        chat_id: chat_id.to_string(),
        update_id: id.to_string(),
        text: text.to_owned(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn message(kind: &str, text: Option<&str>, extra: Value) -> Value {
        let mut message = json!({
            "message_id": 8,
            "from": {"id": 42, "is_bot": false},
            "chat": {"id": 42, "type": kind},
        });
        if let Some(text) = text {
            message["text"] = json!(text);
        }
        if let Some(object) = extra.as_object() {
            for (key, value) in object {
                message[key] = value.clone();
            }
        }
        json!({"update_id": 7, "message": message})
    }

    #[test]
    fn only_private_text_is_queued() {
        let private = read_update(&message("private", Some("hello"), json!({}))).unwrap();
        let text = private.text.as_ref().unwrap();
        assert_eq!(text.user_id, "42");
        assert_eq!(text.chat_id, "42");
        assert_eq!(text.update_id, "7");
        assert_eq!(text.text, "hello");
        assert!(
            read_update(&message("group", Some("hello"), json!({})))
                .unwrap()
                .text
                .is_none()
        );
        assert!(
            read_update(&message("supergroup", Some("hello"), json!({})))
                .unwrap()
                .text
                .is_none()
        );
        assert!(
            read_update(&message("channel", Some("hello"), json!({})))
                .unwrap()
                .text
                .is_none()
        );
        assert!(
            read_update(&message(
                "private",
                None,
                json!({"photo": [{"file_id": "x"}]})
            ))
            .unwrap()
            .text
            .is_none()
        );
        let bot = json!({
            "update_id": 9,
            "message": {
                "text": "hello",
                "from": {"id": 1, "is_bot": true},
                "chat": {"id": 1, "type": "private"}
            }
        });
        assert!(read_update(&bot).unwrap().text.is_none());
    }

    #[test]
    fn a_full_queue_does_not_advance_and_a_dropped_update_does() {
        let (sender, receiver) = std::sync::mpsc::sync_channel(1);
        let dropped = PolledUpdate { id: 4, text: None };
        assert_eq!(queue_update(dropped, &sender).unwrap(), 5);
        assert!(receiver.try_recv().is_err());
        let first = PolledUpdate {
            id: 5,
            text: Some(InboundText {
                user_id: "42".into(),
                chat_id: "42".into(),
                update_id: "5".into(),
                text: "one".into(),
            }),
        };
        assert_eq!(queue_update(first, &sender).unwrap(), 6);
        let blocked = PolledUpdate {
            id: 6,
            text: Some(InboundText {
                user_id: "42".into(),
                chat_id: "42".into(),
                update_id: "6".into(),
                text: "two".into(),
            }),
        };
        assert!(matches!(
            queue_update(blocked, &sender),
            Err(QueuePause::Full)
        ));
        assert_eq!(receiver.try_recv().unwrap().text, "one");
    }
}
