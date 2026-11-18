//! Scoped local attachment preview protocol (`soyo-attachment://` and legacy `saya-attachment://`).

use crate::{
    redaction,
    types::{DesktopError, StagedAttachmentDto},
};
use base64::{Engine, engine::general_purpose::STANDARD};
use soyo_core::{contract::SessionId, output_store::OutputStore};
use std::{
    collections::HashMap,
    fs,
    io::Read,
    path::{Path, PathBuf},
    sync::{Arc, Mutex as StdMutex},
};
use tauri::{
    Manager,
    http::{Response, StatusCode},
};
use uuid::Uuid;

pub const ATTACHMENT_SCHEME: &str = "soyo-attachment";
pub const ATTACHMENT_SCHEME_LEGACY: &str = "saya-attachment";
pub const MAX_ATTACHMENT_BYTES: usize = 32 * 1024 * 1024;
const MAX_STAGED_DRAFTS: usize = 16;
const MAX_STAGED_TOTAL_BYTES: usize = 64 * 1024 * 1024;

#[derive(Clone)]
pub struct ActiveAttachmentScope {
    pub session_id: SessionId,
    pub media: Arc<OutputStore>,
}

#[derive(Clone)]
pub struct StagedDraft {
    pub dto: StagedAttachmentDto,
    pub bytes: Vec<u8>,
}

pub struct AttachmentHttpResponse {
    pub status: StatusCode,
    pub content_type: &'static str,
    pub body: Vec<u8>,
}

impl AttachmentHttpResponse {
    fn error(status: StatusCode, msg: &str) -> Self {
        Self {
            status,
            content_type: "text/plain; charset=utf-8",
            body: msg.as_bytes().to_vec(),
        }
    }

    pub fn into_http_response(self) -> Response<Vec<u8>> {
        Response::builder()
            .status(self.status)
            .header("Content-Type", self.content_type)
            .header("X-Content-Type-Options", "nosniff")
            .header("Cache-Control", "private, no-store")
            .body(self.body)
            .unwrap_or_else(|_| Response::new(Vec::new()))
    }
}

pub fn safe_response_mime(mime: &str) -> &'static str {
    match mime.trim().to_ascii_lowercase().as_str() {
        "image/png" => "image/png",
        "image/jpeg" | "image/jpg" => "image/jpeg",
        "image/webp" => "image/webp",
        "image/gif" => "image/gif",
        "application/pdf" => "application/pdf",
        "audio/mpeg" | "audio/mp3" => "audio/mpeg",
        "audio/wav" => "audio/wav",
        "audio/ogg" => "audio/ogg",
        "video/mp4" => "video/mp4",
        "video/webm" => "video/webm",
        "text/plain" | "text/markdown" | "text/csv" | "application/json" => {
            "text/plain; charset=utf-8"
        }
        // Prevent active script execution (SVG / HTML always downgraded).
        _ => "application/octet-stream",
    }
}

fn infer_mime_and_kind(path: &Path) -> Result<(&'static str, &'static str), DesktopError> {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|s| s.to_ascii_lowercase())
        .unwrap_or_default();
    match ext.as_str() {
        "png" => Ok(("image/png", "image")),
        "jpg" | "jpeg" => Ok(("image/jpeg", "image")),
        "webp" => Ok(("image/webp", "image")),
        "gif" => Ok(("image/gif", "image")),
        "mp3" => Ok(("audio/mpeg", "audio")),
        "wav" => Ok(("audio/wav", "audio")),
        "ogg" => Ok(("audio/ogg", "audio")),
        "mp4" => Ok(("video/mp4", "video")),
        "webm" => Ok(("video/webm", "video")),
        "pdf" => Ok(("application/pdf", "file")),
        "txt" | "md" | "json" | "csv" | "rs" | "ts" | "tsx" | "toml" | "yml" | "yaml" => {
            Ok(("text/plain; charset=utf-8", "file"))
        }
        _ => Err(DesktopError::new(
            "unsupported_attachment",
            format!("Unsupported attachment extension '.{ext}'"),
        )),
    }
}

fn verify_regular_no_symlink(path: &Path, secrets: &[String]) -> Result<PathBuf, DesktopError> {
    if !path.is_absolute() {
        return Err(DesktopError::new(
            "invalid_attachment_path",
            "Attachment path must be absolute",
        ));
    }
    let sym_meta = fs::symlink_metadata(path).map_err(|err| {
        DesktopError::new(
            "attachment_not_found",
            redaction::redact_str(&err.to_string(), secrets),
        )
    })?;
    if sym_meta.file_type().is_symlink() || !sym_meta.is_file() {
        return Err(DesktopError::new(
            "attachment_denied",
            "Attachments must be regular non-symlink files",
        ));
    }
    let canonical = fs::canonicalize(path).map_err(|err| {
        DesktopError::new(
            "attachment_not_found",
            redaction::redact_str(&err.to_string(), secrets),
        )
    })?;
    // Ensure parent resolution matches canonical parent identity to prevent symlink traversal.
    if let (Some(canon_name), Some(orig_name)) = (canonical.file_name(), path.file_name())
        && canon_name != orig_name
    {
        return Err(DesktopError::new(
            "attachment_denied",
            "Attachment path points through a symlink target",
        ));
    }
    Ok(canonical)
}

pub fn stage_attachment_files(
    paths: &[String],
    drafts: &StdMutex<HashMap<String, StagedDraft>>,
    secrets: &[String],
) -> Result<Vec<StagedAttachmentDto>, DesktopError> {
    if paths.is_empty() {
        return Ok(Vec::new());
    }
    if paths.len() > MAX_STAGED_DRAFTS {
        return Err(DesktopError::new(
            "too_many_attachments",
            format!("At most {MAX_STAGED_DRAFTS} attachments may be staged at once"),
        ));
    }

    let mut prepared_items = Vec::with_capacity(paths.len());
    for raw_path in paths {
        let path = PathBuf::from(raw_path.trim());
        let canonical = verify_regular_no_symlink(&path, secrets)?;
        let (mime, kind) = infer_mime_and_kind(&canonical)?;

        let file = fs::File::open(&canonical).map_err(|err| {
            DesktopError::new(
                "attachment_read_error",
                redaction::redact_str(&err.to_string(), secrets),
            )
        })?;
        let meta = file.metadata().map_err(|err| {
            DesktopError::new(
                "attachment_read_error",
                redaction::redact_str(&err.to_string(), secrets),
            )
        })?;
        if !meta.is_file() || meta.len() > MAX_ATTACHMENT_BYTES as u64 {
            return Err(DesktopError::new(
                "attachment_too_large",
                "Attachment exceeds the 32 MiB limit",
            ));
        }

        let mut bytes = Vec::new();
        file.take(MAX_ATTACHMENT_BYTES as u64 + 1)
            .read_to_end(&mut bytes)
            .map_err(|err| {
                DesktopError::new(
                    "attachment_read_error",
                    redaction::redact_str(&err.to_string(), secrets),
                )
            })?;
        if bytes.len() > MAX_ATTACHMENT_BYTES {
            return Err(DesktopError::new(
                "attachment_too_large",
                "Attachment exceeds the 32 MiB limit",
            ));
        }

        let id = format!("draft_{}", Uuid::now_v7().simple());
        let filename = canonical
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "attachment".into());
        let path_str = canonical.to_string_lossy().into_owned();
        let dto = StagedAttachmentDto {
            id: id.clone(),
            token: format!("@{path_str}"),
            path: path_str,
            filename,
            mime: mime.to_owned(),
            size_bytes: bytes.len() as u64,
            kind: kind.to_owned(),
            preview_url: format!("{ATTACHMENT_SCHEME}://localhost/draft/{id}"),
        };
        prepared_items.push(StagedDraft { dto, bytes });
    }

    let mut guard = drafts
        .lock()
        .map_err(|_| DesktopError::new("state_lock", "staged attachment lock failed"))?;
    let mut existing_bytes: usize = guard.values().map(|d| d.bytes.len()).sum();
    for item in &prepared_items {
        existing_bytes = existing_bytes.saturating_add(item.bytes.len());
    }
    if existing_bytes > MAX_STAGED_TOTAL_BYTES || guard.len() + prepared_items.len() > 64 {
        guard.clear();
    }

    let mut out = Vec::with_capacity(prepared_items.len());
    for item in prepared_items {
        out.push(item.dto.clone());
        guard.insert(item.dto.id.clone(), item);
    }
    Ok(out)
}

fn percent_decode(input: &str) -> Result<String, ()> {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            if i + 2 >= bytes.len() {
                return Err(());
            }
            let hi = (bytes[i + 1] as char).to_digit(16).ok_or(())?;
            let lo = (bytes[i + 2] as char).to_digit(16).ok_or(())?;
            out.push(((hi << 4) | lo) as u8);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).map_err(|_| ())
}

pub fn resolve_attachment_request(
    scope: &StdMutex<Option<ActiveAttachmentScope>>,
    drafts: &StdMutex<HashMap<String, StagedDraft>>,
    method: &str,
    raw_uri: &str,
) -> AttachmentHttpResponse {
    if !method.eq_ignore_ascii_case("GET") {
        return AttachmentHttpResponse::error(StatusCode::METHOD_NOT_ALLOWED, "Method not allowed");
    }
    if raw_uri.contains('\0') {
        return AttachmentHttpResponse::error(StatusCode::BAD_REQUEST, "Malformed URI");
    }

    let uri = match raw_uri.parse::<tauri::http::Uri>() {
        Ok(u) => u,
        Err(_) => return AttachmentHttpResponse::error(StatusCode::BAD_REQUEST, "Invalid URI"),
    };
    let scheme = uri.scheme_str().unwrap_or("");
    let host = uri.host().unwrap_or("");
    let valid_scheme = scheme == ATTACHMENT_SCHEME || scheme == ATTACHMENT_SCHEME_LEGACY;
    let valid_http_local = (scheme == "http" || scheme == "https")
        && (host == format!("{ATTACHMENT_SCHEME}.localhost")
            || host == format!("{ATTACHMENT_SCHEME_LEGACY}.localhost"));
    if !valid_scheme && !valid_http_local {
        return AttachmentHttpResponse::error(StatusCode::FORBIDDEN, "Disallowed scheme");
    }

    let path = uri.path();
    if path.contains("..") || path.contains('\\') {
        return AttachmentHttpResponse::error(StatusCode::BAD_REQUEST, "Path traversal denied");
    }

    if let Some(draft_id) = path.strip_prefix("/draft/") {
        if draft_id.is_empty() || draft_id.contains('/') {
            return AttachmentHttpResponse::error(StatusCode::NOT_FOUND, "Draft not found");
        }
        let Ok(guard) = drafts.lock() else {
            return AttachmentHttpResponse::error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Attachment store lock failed",
            );
        };
        let Some(entry) = guard.get(draft_id) else {
            return AttachmentHttpResponse::error(StatusCode::NOT_FOUND, "Draft not found");
        };
        if entry.bytes.len() > MAX_ATTACHMENT_BYTES {
            return AttachmentHttpResponse::error(
                StatusCode::PAYLOAD_TOO_LARGE,
                "Attachment exceeds size limit",
            );
        }
        return AttachmentHttpResponse {
            status: StatusCode::OK,
            content_type: safe_response_mime(&entry.dto.mime),
            body: entry.bytes.clone(),
        };
    }

    if let Some(session_str) = path.strip_prefix("/media/") {
        let session_str = session_str.trim_end_matches('/');
        if session_str.is_empty() || session_str.contains('/') {
            return AttachmentHttpResponse::error(StatusCode::BAD_REQUEST, "Invalid media path");
        }
        let Ok(requested_session) =
            serde_json::from_value::<SessionId>(serde_json::json!(session_str))
        else {
            return AttachmentHttpResponse::error(StatusCode::BAD_REQUEST, "Invalid session ID");
        };
        let Some(ref_encoded) = uri
            .query()
            .and_then(|q| q.split('&').find_map(|pair| pair.strip_prefix("ref=")))
        else {
            return AttachmentHttpResponse::error(
                StatusCode::BAD_REQUEST,
                "Missing media reference",
            );
        };
        let Ok(reference) = percent_decode(ref_encoded) else {
            return AttachmentHttpResponse::error(
                StatusCode::BAD_REQUEST,
                "Invalid encoded media reference",
            );
        };

        let active_scope = match scope.lock() {
            Ok(guard) => guard.clone(),
            Err(_) => {
                return AttachmentHttpResponse::error(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "Attachment scope lock failed",
                );
            }
        };
        let Some(active) = active_scope else {
            return AttachmentHttpResponse::error(
                StatusCode::FORBIDDEN,
                "No active session for attachment access",
            );
        };
        if active.session_id != requested_session {
            return AttachmentHttpResponse::error(
                StatusCode::FORBIDDEN,
                "Attachment does not belong to the active session",
            );
        }

        let loaded = match active.media.load_media(requested_session, &reference) {
            Ok(loaded) => loaded,
            Err(_) => {
                return AttachmentHttpResponse::error(
                    StatusCode::NOT_FOUND,
                    "Session media artifact not found",
                );
            }
        };
        let Ok(bytes) = STANDARD.decode(loaded.data.as_bytes()) else {
            return AttachmentHttpResponse::error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Stored media payload is corrupt",
            );
        };
        if bytes.len() > MAX_ATTACHMENT_BYTES {
            return AttachmentHttpResponse::error(
                StatusCode::PAYLOAD_TOO_LARGE,
                "Attachment exceeds size limit",
            );
        }
        return AttachmentHttpResponse {
            status: StatusCode::OK,
            content_type: safe_response_mime(&loaded.mime),
            body: bytes,
        };
    }

    AttachmentHttpResponse::error(StatusCode::NOT_FOUND, "Unknown attachment route")
}

pub fn register_attachment_protocols(
    builder: tauri::Builder<tauri::Wry>,
) -> tauri::Builder<tauri::Wry> {
    let make_handler = || {
        move |ctx: tauri::UriSchemeContext<'_, tauri::Wry>,
              request: tauri::http::Request<Vec<u8>>| {
            let state = ctx.app_handle().state::<crate::DesktopState>();
            let resp = resolve_attachment_request(
                &state.attachment_scope,
                &state.staged_drafts,
                request.method().as_str(),
                &request.uri().to_string(),
            );
            resp.into_http_response()
        }
    };
    builder
        .register_uri_scheme_protocol(ATTACHMENT_SCHEME, make_handler())
        .register_uri_scheme_protocol(ATTACHMENT_SCHEME_LEGACY, make_handler())
}
