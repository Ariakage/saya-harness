//! User-provided local media referenced by TUI display tokens.

use crate::{
    config::MediaCapabilities,
    contract::{BoundaryError, ErrorInfo},
};
use base64::{Engine, engine::general_purpose::STANDARD};
use std::{
    fs,
    path::{Path, PathBuf},
};

pub const MAX_IMAGE_BYTES: u64 = 8 * 1024 * 1024;
pub const MAX_AUDIO_VIDEO_BYTES: u64 = 32 * 1024 * 1024;
pub const MAX_MESSAGE_MEDIA_BYTES: u64 = 64 * 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MediaKind {
    Image,
    Audio,
    Video,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LoadedMedia {
    pub kind: MediaKind,
    pub mime: String,
    pub filename: String,
    pub data: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ContentPart {
    Text(String),
    Media(LoadedMedia),
}

pub fn expand_user(
    text: &str,
    capabilities: MediaCapabilities,
) -> Result<Vec<ContentPart>, BoundaryError> {
    let markers = markers(text);
    if markers.is_empty() {
        return Ok(vec![ContentPart::Text(text.to_owned())]);
    }
    let mut parts = Vec::new();
    let mut copied = 0;
    let mut retained = 0u64;
    for marker in markers {
        if copied < marker.start {
            parts.push(ContentPart::Text(text[copied..marker.start].to_owned()));
        }
        let supported = match marker.kind {
            MediaKind::Image => capabilities.image,
            MediaKind::Audio => capabilities.audio,
            MediaKind::Video => capabilities.video,
        };
        if !supported {
            return Err(failure(
                "unsupported_media",
                "selected profile does not enable this media modality",
            ));
        }
        parts.push(ContentPart::Text(
            text[marker.start..marker.label_end].to_owned(),
        ));
        let declared = fs::metadata(&marker.path)
            .map_err(|_| failure("media_unavailable", "media file is unavailable"))?
            .len();
        let maximum = match marker.kind {
            MediaKind::Image => MAX_IMAGE_BYTES,
            MediaKind::Audio | MediaKind::Video => MAX_AUDIO_VIDEO_BYTES,
        };
        if declared == 0 || declared > maximum {
            return Err(failure(
                "media_too_large",
                "media file is empty or exceeds its byte limit",
            ));
        }
        let bytes = fs::read(&marker.path)
            .map_err(|_| failure("media_unavailable", "media file cannot be read"))?;
        retained = retained.saturating_add(bytes.len() as u64);
        if bytes.is_empty() || bytes.len() as u64 > maximum || retained > MAX_MESSAGE_MEDIA_BYTES {
            return Err(failure(
                "media_too_large",
                "media file is empty or exceeds its byte limit",
            ));
        }
        let mime = media_type(marker.kind, &marker.path)?;
        let filename = marker
            .path
            .file_name()
            .and_then(|name| name.to_str())
            .filter(|name| !name.is_empty())
            .unwrap_or("media")
            .to_owned();
        parts.push(ContentPart::Media(LoadedMedia {
            kind: marker.kind,
            mime: mime.into(),
            filename,
            data: STANDARD.encode(bytes),
        }));
        copied = marker.end;
    }
    if copied < text.len() {
        parts.push(ContentPart::Text(text[copied..].to_owned()));
    }
    Ok(parts
        .into_iter()
        .filter(|part| match part {
            ContentPart::Text(text) => !text.is_empty(),
            ContentPart::Media(_) => true,
        })
        .collect())
}

#[derive(Debug)]
struct Marker {
    start: usize,
    label_end: usize,
    end: usize,
    path: PathBuf,
    kind: MediaKind,
}

fn markers(text: &str) -> Vec<Marker> {
    let mut markers = Vec::new();
    for (prefix, kind) in [
        ("[Image ", MediaKind::Image),
        ("[Audio ", MediaKind::Audio),
        ("[Video ", MediaKind::Video),
    ] {
        for (start, _) in text.match_indices(prefix) {
            let digits = &text[start + prefix.len()..];
            let length = digits
                .bytes()
                .take_while(|byte| byte.is_ascii_digit())
                .count();
            if length == 0 || digits.as_bytes().get(length) != Some(&b']') {
                continue;
            }
            let label_end = start + prefix.len() + length + 1;
            let Some(destination) = text[label_end..].strip_prefix("(<") else {
                continue;
            };
            let Some(close) = destination.find(">)") else {
                continue;
            };
            let path = &destination[..close];
            if path.is_empty() || path.contains('\0') {
                continue;
            }
            markers.push(Marker {
                start,
                label_end,
                end: label_end + 2 + close + 2,
                path: PathBuf::from(path),
                kind,
            });
        }
    }
    markers.sort_by_key(|marker| marker.start);
    markers
}

fn media_type(kind: MediaKind, path: &Path) -> Result<&'static str, BoundaryError> {
    let extension = path
        .extension()
        .and_then(|extension| extension.to_str())
        .map(str::to_ascii_lowercase)
        .ok_or_else(|| failure("unsupported_media", "media file has no extension"))?;
    let mime = match (kind, extension.as_str()) {
        (MediaKind::Image, "png") => "image/png",
        (MediaKind::Image, "jpg" | "jpeg") => "image/jpeg",
        (MediaKind::Image, "gif") => "image/gif",
        (MediaKind::Image, "webp") => "image/webp",
        (MediaKind::Audio, "mp3") => "audio/mpeg",
        (MediaKind::Audio, "wav") => "audio/wav",
        (MediaKind::Video, "mp4" | "m4v") => "video/mp4",
        (MediaKind::Video, "mov") => "video/quicktime",
        (MediaKind::Video, "webm") => "video/webm",
        (MediaKind::Video, "mkv") => "video/x-matroska",
        (MediaKind::Video, "avi") => "video/x-msvideo",
        _ => {
            return Err(failure(
                "unsupported_media",
                "media format is unsupported by the selected protocol",
            ));
        }
    };
    Ok(mime)
}

pub fn audio_format(media: &LoadedMedia) -> Option<&'static str> {
    match media.mime.as_str() {
        "audio/mpeg" => Some("mp3"),
        "audio/wav" => Some("wav"),
        _ => None,
    }
}

pub fn data_url(media: &LoadedMedia) -> String {
    format!("data:{};base64,{}", media.mime, media.data)
}

fn failure(code: &str, message: &str) -> BoundaryError {
    BoundaryError(
        ErrorInfo {
            code: code.into(),
            message: message.into(),
        },
        None,
        None,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enabled_media_expands_into_text_and_base64_parts() {
        let root = tempfile::tempdir().unwrap();
        let image = root.path().join("image.png");
        fs::write(&image, b"png").unwrap();
        let text = format!("Inspect [Image 1](<{}>) now", image.display());
        let parts = expand_user(
            &text,
            MediaCapabilities {
                image: true,
                ..MediaCapabilities::default()
            },
        )
        .unwrap();
        assert!(matches!(&parts[0], ContentPart::Text(value) if value == "Inspect "));
        assert!(matches!(&parts[1], ContentPart::Text(value) if value == "[Image 1]"));
        assert!(matches!(&parts[2], ContentPart::Media(media) if media.mime == "image/png"));
        assert!(matches!(&parts[3], ContentPart::Text(value) if value == " now"));
    }

    #[test]
    fn disabled_or_missing_media_fails_before_provider_io() {
        let root = tempfile::tempdir().unwrap();
        let audio = root.path().join("audio.mp3");
        fs::write(&audio, b"audio").unwrap();
        let text = format!("[Audio 1](<{}>)", audio.display());
        assert_eq!(
            expand_user(&text, MediaCapabilities::default())
                .unwrap_err()
                .0
                .code,
            "unsupported_media"
        );
        assert_eq!(
            expand_user(
                "[Video 1](</missing/video.mp4>)",
                MediaCapabilities {
                    video: true,
                    ..MediaCapabilities::default()
                }
            )
            .unwrap_err()
            .0
            .code,
            "media_unavailable"
        );
    }
}
