//! Check the existing native client frame boundary before admitting a shared message.
use super::host::{
    MAX_HOST_BYTES, encode_sse_frame, realtime_data, replay_envelope, replay_user_message_json,
};
use crate::message::{AdmissionReceipt, MessageContent, PendingMessage};
use crate::{ApplicationError, Project, RunEvent};

pub(crate) fn validate(
    project: &Project,
    message: &PendingMessage,
    content: &MessageContent,
    initial: bool,
) -> Result<(), ApplicationError> {
    if !message.contains_session_share {
        return Ok(());
    }
    let id = &message.client_message_id;
    // Journal and transport sequence numbers are assigned after admission.
    // Use their largest serialized width, with the normal receipt and envelope.
    let receipt = id.as_ref().map(|id| {
        Box::new(AdmissionReceipt::committed(
            id.clone(),
            "00000000-0000-0000-0000-000000000000".to_owned(),
            content.attachment_ids(),
        ))
    });
    let replay = replay_user_message_json(
        u64::MAX,
        u64::MAX,
        i64::MIN,
        &content.plain_text(),
        &content.blocks,
        id.as_deref(),
        receipt.as_deref(),
    );
    check("replay", &replay_envelope(replay))?;
    let live = if initial {
        RunEvent::RunStarted {
            project: project.root().to_owned(),
            message: content.clone(),
            client_message_id: id.clone(),
        }
    } else {
        RunEvent::SteeringApplied {
            message: content.clone(),
            client_message_id: id.clone(),
            request_digest: id.as_ref().map(|_| message.request_digest()),
            receipt,
        }
    };
    check("event", &realtime_data(&live))
}

fn check(event: &str, data: &str) -> Result<(), ApplicationError> {
    let frame = encode_sse_frame(event, u64::MAX, data);
    if frame.len() > MAX_HOST_BYTES {
        return Err(ApplicationError::presented(crate::i18n::UiMessage::new(
            "input.share_host_frame_too_large",
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_share_frame_limit_counts_sse_envelope() {
        let overhead = encode_sse_frame("replay", u64::MAX, "{}").len();
        let mut data = "x".repeat(MAX_HOST_BYTES - overhead);
        data.push_str("{}");
        assert!(check("replay", &data).is_ok());
        data.push('x');
        let error = check("replay", &data).unwrap_err();
        assert_eq!(error.presentation().key, "input.share_host_frame_too_large");
    }
}
