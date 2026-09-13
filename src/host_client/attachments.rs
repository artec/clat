//! Client files become host-owned opaque uploads before admission. No client
//! path is sent as attachment authority and no failed operation is retried.
use super::{HostCallError, HostClient, error, transport};
use serde_json::{Value, json};
use std::io::Read;
use std::path::PathBuf;

#[cfg(test)]
mod tests;

impl HostClient {
    pub fn submit_message(
        &self,
        text: &str,
        images: &[PathBuf],
        running: bool,
        selection: u64,
    ) -> Result<Value, HostCallError> {
        if images.is_empty() {
            return self.submit_text(text, running, selection);
        }
        if text.trim_start().starts_with('/') {
            return Err("attachments belong to messages, not slash commands".into());
        }
        let sources = read_sources(images)?;
        let scope = self.call_with_receipt(
            "draft.open",
            &json!({
                "clientDraftId": uuid::Uuid::new_v4().to_string(),
                "expected_selection_generation": selection,
            }),
        )?;
        if scope["selectionGeneration"].as_u64() != Some(selection) {
            return Err("session changed; image draft was not submitted".into());
        }
        let scope = opaque_id(&scope, "draftScopeId")?;
        let mut uploads = Vec::with_capacity(sources.len());
        for (bytes, media_type) in sources {
            let mut response = transport::request_with_type(
                self,
                "POST",
                &format!("/api/drafts/{scope}/images"),
                &bytes,
                media_type,
            )?;
            let body = transport::read_body(&mut response)?;
            let value = serde_json::from_slice(&body).map_err(|_| "invalid upload response")?;
            uploads.push(opaque_id(&error::decode_result(value)?, "uploadId")?);
        }
        let mut events = if running {
            Some(claim_subscription(self)?)
        } else {
            None
        };
        let client_message_id = uuid::Uuid::new_v4().to_string();
        let accepted = self.call_with_receipt(
            if running { "steer.send" } else { "prompt.send" },
            &json!({
                "text": text, "attachments": uploads, "draftScopeId": scope,
                "clientMessageId": client_message_id,
                "expected_selection_generation": selection,
            }),
        )?;
        if let Some(events) = &mut events {
            if accepted["outcome"] != "queued" {
                return Err(HostCallError {
                    message: "run is no longer accepting steering; draft retained".into(),
                    code: Some("not-running".into()),
                    receipt: accepted
                        .get("receipt")
                        .cloned()
                        .and_then(|value| serde_json::from_value(value).ok()),
                });
            }
            return wait_for_image_claim(events, &client_message_id);
        }
        Ok(accepted)
    }
}

// An in-run reservation is not admission. Keep the client's image draft until
// the recorder publishes its committed receipt, or explicitly report failure.
fn wait_for_image_claim(events: &mut super::HostEvents, id: &str) -> Result<Value, HostCallError> {
    loop {
        let (kind, value) = events.next_frame()?;
        match super::decode_host_event(&kind, value)? {
            super::HostEvent::Run(crate::RunEvent::SteeringApplied {
                client_message_id: Some(applied),
                receipt: Some(receipt),
                ..
            }) if applied == id => return Ok(json!({"receipt": receipt})),
            super::HostEvent::Control { kind, .. } if kind == "prompt.settled" => {
                return Err("run ended before image steering committed; draft retained".into());
            }
            _ => {}
        }
    }
}

fn claim_subscription(client: &HostClient) -> Result<super::HostEvents, HostCallError> {
    let mut events = client.events()?;
    loop {
        let (kind, _) = events.next_frame()?;
        if kind == "subscribed" {
            return Ok(events);
        }
    }
}

fn opaque_id(value: &Value, key: &str) -> Result<String, HostCallError> {
    let id = value[key].as_str().ok_or("host omitted upload identity")?;
    if id.is_empty()
        || id.len() > 128
        || !id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    {
        return Err("invalid host upload identity".into());
    }
    Ok(id.to_owned())
}

fn read_sources(paths: &[PathBuf]) -> Result<Vec<(Vec<u8>, &'static str)>, HostCallError> {
    use crate::session::attachments::{
        MAX_IMAGES_PER_MESSAGE, MAX_RAW_BATCH_BYTES, open_private_regular_file_no_follow,
    };
    if paths.len() > MAX_IMAGES_PER_MESSAGE {
        return Err("too many images in this message".into());
    }
    let mut total = 0;
    let mut sources = Vec::with_capacity(paths.len());
    for path in paths {
        let (file, metadata) = open_private_regular_file_no_follow(path)
            .map_err(|_| "cannot open image as a regular no-follow file")?;
        if metadata.len() == 0 || metadata.len() > crate::media::MAX_ATTACHMENT_BYTES {
            return Err("image exceeds the upload size limit".into());
        }
        total += metadata.len();
        if total > MAX_RAW_BATCH_BYTES {
            return Err("image batch exceeds the upload size limit".into());
        }
        let mut bytes = Vec::new();
        file.take(crate::media::MAX_ATTACHMENT_BYTES + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| "cannot read image")?;
        if bytes.len() as u64 != metadata.len() {
            return Err(
                "image changed while being read; submit again after editing finishes".into(),
            );
        }
        let media_type = if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
            "image/png"
        } else if bytes.starts_with(b"\xff\xd8\xff") {
            "image/jpeg"
        } else {
            return Err("host attachments require PNG or JPEG images".into());
        };
        sources.push((bytes, media_type));
    }
    Ok(sources)
}
