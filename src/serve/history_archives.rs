use super::{
    http::{self, HttpReadError, HttpRequestHead},
    protocol::{self, RpcError, app_error},
    state::ServeShared,
    write_json,
};
use crate::{CancelToken, application::HistoryReader, session::portable::MAX_ARCHIVE_BYTES};
use std::{net::TcpStream, sync::Arc, time::Duration};

const EXPORT: &str = "/api/history.archive.export";
const IMPORT: &str = "/api/history.archive.import";
const IMAGE: &str = "/api/history.archive.image";

pub(super) fn handles(path: &str) -> bool {
    matches!(path, EXPORT | IMPORT | IMAGE)
}

pub(super) fn handle(
    stream: &mut TcpStream,
    request: &mut HttpRequestHead,
    shared: &Arc<ServeShared>,
) {
    let Some(_permit) = shared.try_attachment_download_permit() else {
        failure(
            stream,
            503,
            RpcError::busy("archive transfer limit reached"),
        );
        return;
    };
    let importing = request.path == IMPORT;
    let expected_type = if importing {
        "application/zip"
    } else {
        "application/json"
    };
    if request
        .content_type
        .as_deref()
        .and_then(|value| value.split(';').next())
        .map(str::trim)
        != Some(expected_type)
    {
        failure(
            stream,
            400,
            RpcError::bad_request(format!("expected Content-Type: {expected_type}")),
        );
        return;
    }
    let limit = if importing {
        MAX_ARCHIVE_BYTES
    } else {
        16 * 1024
    };
    let body = match http::read_body(stream, request, limit) {
        Ok(body) => body,
        Err(HttpReadError::TooLarge(_)) => {
            failure(
                stream,
                413,
                RpcError::bad_request("request exceeds archive transport limit"),
            );
            return;
        }
        Err(HttpReadError::BadRequest(reason)) => {
            failure(stream, 400, RpcError::bad_request(reason));
            return;
        }
        Err(_) => return,
    };
    let reader = shared
        .app
        .lock()
        .expect("application lock")
        .history_reader();
    let cancel = CancelToken::new();
    let _ = stream.set_write_timeout(Some(Duration::from_secs(15)));
    if let Err(error) = transfer(stream, &request.path, &body, &reader, &cancel) {
        failure(stream, 400, error);
    }
}

fn transfer(
    stream: &mut TcpStream,
    path: &str,
    body: &[u8],
    reader: &HistoryReader,
    cancel: &CancelToken,
) -> Result<(), RpcError> {
    if path == IMPORT {
        let rows = reader.import_archive(body, cancel).map_err(app_error)?;
        let value = serde_json::to_value(rows).map_err(|e| RpcError::internal(e.to_string()))?;
        let _ = write_json(stream, 200, protocol::rpc_result_json(&Ok(value)));
    } else if path == EXPORT {
        let request =
            serde_json::from_slice(body).map_err(|e| RpcError::bad_request(e.to_string()))?;
        let archive = reader.export_archive(request, cancel).map_err(app_error)?;
        let disposition = format!("attachment; filename=\"{}\"", archive.filename);
        binary(stream, "application/zip", &archive.bytes, &disposition);
    } else {
        #[derive(serde::Deserialize)]
        #[serde(deny_unknown_fields)]
        struct ImageRequest {
            reference: crate::application::HistoryReference,
            trusted_projects: bool,
            attachment_id: String,
        }
        let request: ImageRequest =
            serde_json::from_slice(body).map_err(|e| RpcError::bad_request(e.to_string()))?;
        let (bytes, media) = reader
            .archive_image(
                &request.reference,
                request.trusted_projects,
                &request.attachment_id,
                cancel,
            )
            .map_err(app_error)?;
        binary(stream, media, &bytes, "inline");
    }
    Ok(())
}

fn binary(stream: &mut TcpStream, media: &str, bytes: &[u8], disposition: &str) {
    let _ = http::write_response_with_headers(
        stream,
        200,
        media,
        bytes,
        &[
            ("Content-Disposition", disposition),
            ("Cache-Control", "no-store"),
            ("X-Content-Type-Options", "nosniff"),
        ],
    );
}

fn failure(stream: &mut TcpStream, status: u16, error: RpcError) {
    let _ = write_json(stream, status, protocol::rpc_result_json(&Err(error)));
}
