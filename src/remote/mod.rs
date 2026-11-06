//! Channel-independent remote interaction semantics.

mod approval;
mod conversations;
mod identity;
mod outbox;
mod pairing;
mod projection;
mod sink;
pub(crate) use approval::{
    ApprovalRegistry, ApprovalScope, ApprovalWait, RemoteChannel, RemotePrincipal,
    wait_for_approval,
};
pub(crate) use conversations::{RemoteConversations, RemoteRoute};
pub(crate) use identity::{RemoteChatId, RemoteDeliveryId, RemoteUserId};
pub(crate) use outbox::{RemoteOutbox, RemoteOutput, RemotePayload, unicode_chunks};
pub(crate) use pairing::{PairingAttempt, PairingChallenge};
pub(crate) use projection::approval_prompt;
pub(crate) use sink::RemoteRunSink;

/// A chat route after the remote principal has been authorized.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct RemoteChatBinding {
    pub(crate) channel: RemoteChannel,
    pub(crate) user_id: String,
    pub(crate) session_id: crate::SessionId,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct RemotePendingMapping {
    pub(crate) request_digest: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ChatMappingIntent {
    Existing,
    Fresh,
    Recovery,
}

/// Decide mapping recovery before admitting a remote message.
pub(crate) fn mapping_intent(
    pending_digest: Option<&str>,
    incoming_digest: &str,
    has_binding: bool,
    allow_new: bool,
) -> Result<Option<ChatMappingIntent>, ()> {
    if let Some(pending) = pending_digest {
        return if pending == incoming_digest {
            Ok(Some(ChatMappingIntent::Recovery))
        } else {
            Err(())
        };
    }
    if has_binding {
        Ok(Some(ChatMappingIntent::Existing))
    } else if allow_new {
        Ok(Some(ChatMappingIntent::Fresh))
    } else {
        Ok(None)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ApprovalAnswer {
    Allow,
    Always,
    Deny,
}

#[derive(Debug, Eq, PartialEq)]
pub(crate) enum Command<'a> {
    Help,
    Status,
    New,
    Stop,
    Approval(ApprovalAnswer, &'a str),
    Unknown,
}

/// Only call for a standalone text item; mixed content is ordinary input.
pub(crate) fn command(text: &str) -> Option<Command<'_>> {
    let command = match text {
        "/help" => Command::Help,
        "/status" => Command::Status,
        "/new" => Command::New,
        "/stop" => Command::Stop,
        _ if text.starts_with('/') => approval_command(text)
            .map(|(answer, id)| Command::Approval(answer, id))
            .unwrap_or(Command::Unknown),
        _ => return None,
    };
    Some(command)
}

pub(crate) fn approval_command(text: &str) -> Option<(ApprovalAnswer, &str)> {
    let mut fields = text.split_whitespace();
    let answer = match fields.next()? {
        "/allow" => ApprovalAnswer::Allow,
        "/always" => ApprovalAnswer::Always,
        "/deny" => ApprovalAnswer::Deny,
        _ => return None,
    };
    let id = fields.next()?;
    if fields.next().is_some()
        || id.len() > 128
        || !id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
    {
        return None;
    }
    Some((answer, id))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remote_commands_are_exact_and_unknown_slash_commands_are_not_prompts() {
        assert_eq!(command("/new"), Some(Command::New));
        assert_eq!(
            command("/allow 123-a"),
            Some(Command::Approval(ApprovalAnswer::Allow, "123-a"))
        );
        assert_eq!(command("/allow 123 extra"), Some(Command::Unknown));
        assert_eq!(command("/allow 你好"), Some(Command::Unknown));
        assert_eq!(command("please /allow 123"), None);
    }

    #[test]
    fn recovery_intent_cannot_be_reused_for_a_different_message() {
        assert_eq!(mapping_intent(Some("old"), "new", true, true), Err(()));
        assert_eq!(
            mapping_intent(Some("same"), "same", false, false),
            Ok(Some(ChatMappingIntent::Recovery))
        );
        assert_eq!(
            mapping_intent(None, "new", true, false),
            Ok(Some(ChatMappingIntent::Existing))
        );
        assert_eq!(
            mapping_intent(None, "new", false, true),
            Ok(Some(ChatMappingIntent::Fresh))
        );
        assert_eq!(mapping_intent(None, "new", false, false), Ok(None));
    }
}
