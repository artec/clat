use super::*;
use crate::message::AttachmentDescriptor;
use serde_json::{Value, json};

pub(crate) fn export_attachments(
    event: &mut SessionEvent,
    archive: &mut Archive,
    missing: &mut BTreeMap<String, &'static str>,
    mut read: impl FnMut(&AttachmentDescriptor) -> Option<Vec<u8>>,
) -> Result<(), String> {
    visit_blocks(event, |block| {
        if block["type"] == "file" {
            if let Some(path) = file_path(&block["attachment"]) {
                missing
                    .entry(path)
                    .or_insert("File bytes are unavailable from the source session");
            }
            return Ok(());
        }
        if block["type"] != "image" {
            return Ok(());
        }
        let flat = block.get("attachment").is_none();
        let carrier = if flat { &*block } else { &block["attachment"] };
        let Some(descriptor) = descriptor(carrier) else {
            return Ok(());
        };
        let digest = descriptor
            .attachment_id
            .strip_prefix("sha256:")
            .unwrap_or(&descriptor.attachment_id);
        let (Some(extension), true) = (image_extension(&descriptor.media_type), is_digest(digest))
        else {
            return Err("Image has no exportable content-addressed identity".into());
        };
        let id = format!("sha256:{digest}");
        let path = format!("media/{id}.{extension}");
        let source = AttachmentDescriptor {
            attachment_id: digest.to_owned(),
            ..descriptor
        };
        if flat {
            let descriptor = &source;
            let mut attachment = json!({"attachmentId":id,"mediaType":descriptor.media_type,"bytes":descriptor.bytes,"width":descriptor.width,"height":descriptor.height});
            if let Some(name) = &descriptor.display_name {
                attachment["name"] = json!(name);
            }
            if let Some((width, height)) = descriptor.original_width.zip(descriptor.original_height)
            {
                attachment["originalDimensions"] = json!({"width":width,"height":height});
            }
            *block = json!({"type":"image","attachment":attachment});
        }
        if archive.entries.contains_key(&path) {
            return Ok(());
        }
        if let Some(bytes) = read(&source) {
            missing.remove(&path);
            archive.insert(path, bytes)?;
        } else {
            missing
                .entry(path)
                .or_insert("Image bytes are unavailable from the source session");
        }
        Ok(())
    })
}

fn descriptor(value: &Value) -> Option<AttachmentDescriptor> {
    Some(AttachmentDescriptor {
        attachment_id: value["attachmentId"].as_str()?.to_owned(),
        media_type: value["mediaType"].as_str()?.to_owned(),
        display_name: value["name"]
            .as_str()
            .or_else(|| value["displayName"].as_str())
            .map(str::to_owned),
        width: value["width"].as_u64()?,
        height: value["height"].as_u64()?,
        bytes: value["bytes"].as_u64()?,
        original_width: value["originalWidth"].as_u64(),
        original_height: value["originalHeight"].as_u64(),
    })
}

fn visit_blocks(
    event: &mut SessionEvent,
    mut block: impl FnMut(&mut Value) -> Result<(), String>,
) -> Result<(), String> {
    let mut content = |value: Option<&mut Value>| {
        if let Some(items) = value.and_then(Value::as_array_mut) {
            for item in items {
                block(item)?;
            }
        }
        Ok::<_, String>(())
    };
    match event.event_type.as_str() {
        "user/message" | "tool/ptc-dispatch" => content(event.data.get_mut("content"))?,
        "system/message"
        | "developer/message"
        | "tool/result"
        | "team/message/queued"
        | "assistant/message" => content(event.data.pointer_mut("/message/content"))?,
        "agent/inbox/spliced" => {
            if let Some(messages) = event.data.get_mut("inserted").and_then(Value::as_array_mut) {
                for message in messages {
                    content(message.get_mut("content"))?;
                }
            }
        }
        "compaction/summary" => {
            content(event.data.get_mut("summary"))?;
            content(event.data.get_mut("rawOutput"))?;
        }
        _ => {}
    }
    if matches!(
        event.event_type.as_str(),
        "assistant/message" | "assistant/attempt"
    ) && let Some(stream) = event.data.get_mut("stream").and_then(Value::as_array_mut)
    {
        for row in stream {
            if row["type"] == "chunk"
                && row["chunk"]["type"] == "block-end"
                && let Some(value) = row.pointer_mut("/chunk/block")
            {
                block(value)?;
            }
        }
    }
    Ok(())
}

pub(super) fn is_digest(digest: &str) -> bool {
    digest.len() == 64
        && digest
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

pub(crate) fn import_images(event: &mut SessionEvent) {
    let _ = visit_blocks(event, |block| {
        if block["type"] == "image"
            && let Some(reference) = descriptor(&block["attachment"])
        {
            *block = json!({
                "type": "image",
                "attachmentId": reference.attachment_id.strip_prefix("sha256:").unwrap_or(&reference.attachment_id),
                "mediaType": reference.media_type,
                "bytes": reference.bytes,
                "width": reference.width,
                "height": reference.height,
                "displayName": reference.display_name,
            });
        }
        Ok(())
    });
}

fn image_extension(media: &str) -> Option<&'static str> {
    match media {
        "image/png" => Some("png"),
        "image/jpeg" => Some("jpg"),
        "image/webp" => Some("webp"),
        "image/gif" => Some("gif"),
        _ => None,
    }
}

fn file_path(attachment: &Value) -> Option<String> {
    let digest = attachment["attachmentId"]
        .as_str()?
        .strip_prefix("sha256:")?;
    if !is_digest(digest) {
        return None;
    }
    let mut name = attachment["name"].as_str()?.replace(
        |c: char| c.is_ascii_control() || matches!(c, '/' | '\\'),
        "_",
    );
    if matches!(name.as_str(), "" | "." | "..") {
        name = "file".into();
    }
    Some(format!("files/{}/{digest}/{name}", &digest[..2]))
}
