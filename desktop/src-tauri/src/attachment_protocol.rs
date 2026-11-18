//! Tauri URI-scheme handler for `soyo-attachment://` previews.
//! Resolution lives in the helper (staged drafts and session media are
//! host-side); this module proxies the request over IPC and maps the payload
//! onto an HTTP response with strict content headers.

use crate::DesktopState;
use base64::{Engine, engine::general_purpose::STANDARD};
use soyo_ipc::types::AttachmentPayloadDto;
use tauri::Manager;
use tauri::http::{Response, StatusCode};

fn error_response(status: StatusCode, msg: &str) -> Response<Vec<u8>> {
    Response::builder()
        .status(status)
        .header("Content-Type", "text/plain; charset=utf-8")
        .header("X-Content-Type-Options", "nosniff")
        .header("Cache-Control", "private, no-store")
        .body(msg.as_bytes().to_vec())
        .unwrap_or_else(|_| Response::new(Vec::new()))
}

fn into_http_response(payload: AttachmentPayloadDto) -> Response<Vec<u8>> {
    let status = StatusCode::from_u16(payload.status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
    let body = STANDARD
        .decode(payload.body_b64.as_bytes())
        .unwrap_or_default();
    Response::builder()
        .status(status)
        .header("Content-Type", payload.content_type)
        .header("X-Content-Type-Options", "nosniff")
        .header("Cache-Control", "private, no-store")
        .body(body)
        .unwrap_or_else(|_| Response::new(Vec::new()))
}

pub fn register_attachment_protocols(
    builder: tauri::Builder<tauri::Wry>,
) -> tauri::Builder<tauri::Wry> {
    let make_handler = || {
        move |ctx: tauri::UriSchemeContext<'_, tauri::Wry>,
              request: tauri::http::Request<Vec<u8>>| {
            let state = ctx.app_handle().state::<DesktopState>();
            let method = request.method().as_str().to_owned();
            let uri = request.uri().to_string();
            // The scheme handler is synchronous; block on the IPC call.
            match tauri::async_runtime::block_on(state.fetch_attachment(&method, &uri)) {
                Ok(payload) => into_http_response(payload),
                Err(err) => error_response(StatusCode::BAD_GATEWAY, &err.message),
            }
        }
    };
    builder
        .register_uri_scheme_protocol("soyo-attachment", make_handler())
        .register_uri_scheme_protocol("saya-attachment", make_handler())
}
