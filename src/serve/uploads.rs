//! New-user image ingress, authenticated before this adapter and gated before body reads.
use super::{
    http,
    protocol::{self, RpcError},
    state::ServeShared,
    write_json,
};
use std::{net::TcpStream, sync::Arc};
pub(super) fn accept(
    stream: &mut TcpStream,
    request: &mut http::HttpRequestHead,
    scope: &str,
    shared: &Arc<ServeShared>,
) {
    let result = prepare(stream, request, scope, shared);
    match result {
        Ok(value) => {
            let _ = write_json(stream, 200, protocol::rpc_result_json(&Ok(value)));
        }
        Err(Some((status, error))) => {
            let _ = write_json(stream, status, protocol::rpc_result_json(&Err(error)));
        }
        Err(None) => {}
    }
}
fn prepare(
    stream: &mut TcpStream,
    request: &mut http::HttpRequestHead,
    scope: &str,
    shared: &Arc<ServeShared>,
) -> Result<serde_json::Value, Option<(u16, RpcError)>> {
    let store = shared
        .app
        .lock()
        .expect("application")
        .draft_image_store()
        .map_err(|e| Some((400, e.into())))?;
    let media = request
        .content_type
        .as_deref()
        .map(str::trim)
        .filter(|m| matches!(*m, "image/png" | "image/jpeg"))
        .ok_or_else(|| {
            Some((
                400,
                RpcError::bad_request(
                    "image upload requires Content-Type: image/png or image/jpeg",
                ),
            ))
        })?;
    if request.content_length == 0
        || request.content_length as u64 > crate::media::MAX_ATTACHMENT_BYTES
    {
        return Err(Some((
            413,
            RpcError::bad_request(format!(
                "image upload must be 1..={} bytes",
                crate::media::MAX_ATTACHMENT_BYTES
            )),
        )));
    }
    let _permit = shared
        .try_upload_permit()
        .ok_or_else(|| Some((429, RpcError::busy("too many image uploads are active"))))?;
    let mut writer = store
        .begin_upload(
            scope,
            shared.selection_generation(),
            shared.token_generation(),
            request.content_length as u64,
            media,
            request.display_name.as_deref(),
        )
        .map_err(|e| Some((400, RpcError::bad_request(e))))?;
    match http::read_body_into(
        stream,
        request,
        crate::media::MAX_ATTACHMENT_BYTES as usize,
        &mut writer,
    ) {
        Ok(()) => {}
        Err(http::HttpReadError::TooLarge(_)) => {
            return Err(Some((
                413,
                RpcError::bad_request("image upload exceeds the size limit"),
            )));
        }
        Err(http::HttpReadError::BadRequest(reason)) => {
            return Err(Some((400, RpcError::bad_request(reason))));
        }
        Err(
            http::HttpReadError::TimedOut | http::HttpReadError::Closed | http::HttpReadError::Io,
        ) => return Err(None),
    }
    let uploaded = writer
        .finish()
        .map_err(|e| Some((400, RpcError::bad_request(e))))?;
    Ok(serde_json::json!({"uploadId":uploaded.upload_id,"bytes":uploaded.bytes}))
}
