//! User-provided local media and durable, content-identified session snapshots.

use crate::{
    config::MediaCapabilities,
    contract::{BoundaryError, ErrorInfo, HistoryItem, SessionId},
    output_store::{OutputLease, OutputStore, SealedMedia},
};
use base64::{Engine, engine::general_purpose::STANDARD};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::Read,
    os::unix::fs::OpenOptionsExt,
    path::{Path, PathBuf},
    sync::Arc,
};

pub const MAX_IMAGE_BYTES: u64 = 8 * 1024 * 1024;
pub const MAX_AUDIO_VIDEO_BYTES: u64 = 32 * 1024 * 1024;
pub const MAX_MESSAGE_MEDIA_BYTES: u64 = 64 * 1024 * 1024;
pub const MEDIA_SCHEME: &str = "saya-media://";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
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
    pub sha256: String,
    pub reference: Option<String>,
    pub size: u64,
}

/// Verified snapshots indexed by their durable reference. The payload is never
/// serialized into the neutral context fingerprint or session history.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct MediaAttachments(BTreeMap<String, LoadedMedia>);
impl std::fmt::Debug for MediaAttachments {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MediaAttachments")
            .field("references", &self.0.keys().collect::<Vec<_>>())
            .finish()
    }
}
impl MediaAttachments {
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
    pub fn insert(&mut self, media: LoadedMedia) -> Result<(), BoundaryError> {
        let reference = media
            .reference
            .clone()
            .ok_or_else(|| failure("media_corrupt", "media snapshot has no durable reference"))?;
        if let Some(existing) = self.0.get(&reference)
            && existing != &media
        {
            return Err(failure(
                "media_corrupt",
                "media reference has conflicting snapshots",
            ));
        }
        self.0.insert(reference, media);
        Ok(())
    }
    fn get(&self, reference: &str) -> Option<&LoadedMedia> {
        self.0.get(reference)
    }
}

/// Holds object leases until the rewritten user text has committed. Orphaned
/// objects remain bounded and collectable if the session transaction fails.
#[derive(Debug)]
pub struct MediaCapture {
    pub text: String,
    _objects: Vec<SealedMedia>,
    _leases: Vec<OutputLease>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ContentPart {
    Text(String),
    Media(LoadedMedia),
}

/// Compatibility entry for direct adapter use and legacy path-based history.
/// New persisted inputs should pass through `capture_user` first.
pub fn expand_user(
    text: &str,
    capabilities: MediaCapabilities,
) -> Result<Vec<ContentPart>, BoundaryError> {
    expand_user_with_snapshots(text, capabilities, &MediaAttachments::default())
}

pub fn expand_user_with_snapshots(
    text: &str,
    capabilities: MediaCapabilities,
    snapshots: &MediaAttachments,
) -> Result<Vec<ContentPart>, BoundaryError> {
    expand_user_for_purpose(text, capabilities, snapshots, true)
}

/// Non-execution requests carry media markers as historical text only. They
/// must never expand into provider content or depend on local file access.
pub fn expand_user_for_purpose(
    text: &str,
    capabilities: MediaCapabilities,
    snapshots: &MediaAttachments,
    expand: bool,
) -> Result<Vec<ContentPart>, BoundaryError> {
    if !expand {
        return Ok(vec![ContentPart::Text(display(text))]);
    }
    let markers = markers(text);
    if markers.is_empty() {
        return Ok(vec![ContentPart::Text(text.to_owned())]);
    }
    let mut parts = Vec::new();
    let mut copied = 0;
    let mut retained = 0u64;
    for marker in markers {
        if marker.start < copied {
            continue;
        }
        if copied < marker.start {
            parts.push(ContentPart::Text(text[copied..marker.start].to_owned()));
        }
        ensure_supported(marker.kind, capabilities)?;
        parts.push(ContentPart::Text(
            text[marker.start..marker.label_end].to_owned(),
        ));
        let media = match marker.reference() {
            Some(reference) => {
                let media = snapshots.get(reference).ok_or_else(|| {
                    failure("media_unavailable", "durable media snapshot is unavailable")
                })?;
                if media.kind != marker.kind {
                    return Err(failure(
                        "media_corrupt",
                        "media snapshot kind differs from its marker",
                    ));
                }
                media.clone()
            }
            None => {
                let local = load_local(&marker)?;
                local.loaded()
            }
        };
        retained = retained.saturating_add(media.size);
        if media.size == 0
            || media.size > maximum(marker.kind)
            || retained > MAX_MESSAGE_MEDIA_BYTES
        {
            return Err(failure(
                "media_too_large",
                "media file is empty or exceeds its byte limit",
            ));
        }
        parts.push(ContentPart::Media(media));
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

/// Capture every local media marker once, then persist the returned text in the
/// same session history operation. Existing durable references are verified and
/// leased before they may enter history. New objects receive best-effort cleanup
/// when capture fails; any remaining orphans stay bounded and collectable.
pub fn capture_user(
    text: &str,
    capabilities: MediaCapabilities,
    store: &OutputStore,
    session_id: SessionId,
) -> Result<MediaCapture, BoundaryError> {
    let markers = markers(text);
    if markers.is_empty() {
        return Ok(MediaCapture {
            text: text.to_owned(),
            _objects: Vec::new(),
            _leases: Vec::new(),
        });
    }
    let mut output = String::with_capacity(text.len());
    let mut copied = 0;
    let mut retained = 0u64;
    let mut objects = Vec::new();
    let mut leases = Vec::new();
    let result = (|| -> Result<(), BoundaryError> {
        for marker in markers {
            if marker.start < copied {
                continue;
            }
            output.push_str(&text[copied..marker.start]);
            output.push_str(&text[marker.start..marker.path_start]);
            if let Some(reference) = marker.reference() {
                parse_reference(reference)?;
                ensure_supported(marker.kind, capabilities)?;
                let (media, lease) = store.load_media_with_lease(session_id, reference)?;
                if media.kind != marker.kind {
                    return Err(failure(
                        "media_corrupt",
                        "media snapshot kind differs from its marker",
                    ));
                }
                retained = retained.saturating_add(media.size);
                if media.size == 0
                    || media.size > maximum(marker.kind)
                    || retained > MAX_MESSAGE_MEDIA_BYTES
                {
                    return Err(failure(
                        "media_too_large",
                        "media files exceed the per-message byte limit",
                    ));
                }
                leases.push(lease);
                output.push_str(MEDIA_SCHEME);
                output.push_str(reference);
                copied = marker.path_end;
                continue;
            }
            ensure_supported(marker.kind, capabilities)?;
            let media = load_local(&marker)?;
            retained = retained.saturating_add(media.size());
            if media.size() == 0
                || media.size() > maximum(marker.kind)
                || retained > MAX_MESSAGE_MEDIA_BYTES
            {
                return Err(failure(
                    "media_too_large",
                    "media files exceed the per-message byte limit",
                ));
            }
            let object = store.put_media(
                session_id,
                media.kind,
                &media.mime,
                &media.filename,
                &media.bytes,
            )?;
            output.push_str(MEDIA_SCHEME);
            output.push_str(&object.reference);
            objects.push(object);
            copied = marker.path_end;
        }
        output.push_str(&text[copied..]);
        Ok(())
    })();
    if let Err(failure) = result {
        let references = objects
            .iter()
            .map(|object| object.reference.clone())
            .collect::<Vec<_>>();
        objects.clear();
        for reference in references {
            let _ = store.remove_media(session_id, &reference);
        }
        return Err(failure);
    }
    Ok(MediaCapture {
        text: output,
        _objects: objects,
        _leases: leases,
    })
}

/// Human-readable projection which hides durable references and destinations.
pub fn display(text: &str) -> String {
    let markers = markers(text);
    if markers.is_empty() {
        return text.to_owned();
    }
    let mut output = String::with_capacity(text.len());
    let mut copied = 0;
    for marker in markers {
        if marker.start < copied {
            continue;
        }
        output.push_str(&text[copied..marker.start]);
        output.push_str(&text[marker.start..marker.label_end]);
        copied = marker.end;
    }
    output.push_str(&text[copied..]);
    output
}

/// Load all durable snapshots referenced by a request's ordered history.
pub fn load_history(
    history: &[HistoryItem],
    store: Option<&Arc<OutputStore>>,
    session_id: SessionId,
) -> Result<MediaAttachments, BoundaryError> {
    let mut snapshots = MediaAttachments::default();
    for reference in history_references(history) {
        let store = store.ok_or_else(|| {
            failure(
                "media_unavailable",
                "durable media storage is not configured",
            )
        })?;
        snapshots.insert(store.load_media(session_id, &reference)?)?;
    }
    Ok(snapshots)
}

/// References which must be pinned when the caller runs OutputStore GC.
pub fn history_references(history: &[HistoryItem]) -> BTreeSet<String> {
    history
        .iter()
        .filter_map(|item| match item {
            HistoryItem::UserText(text) => Some(text_references(text)),
            _ => None,
        })
        .flatten()
        .collect()
}

pub fn text_references(text: &str) -> BTreeSet<String> {
    markers(text)
        .iter()
        .filter_map(|marker| marker.reference().map(str::to_owned))
        .collect()
}

pub fn has_media(text: &str) -> bool {
    !markers(text).is_empty()
}

fn ensure_supported(kind: MediaKind, capabilities: MediaCapabilities) -> Result<(), BoundaryError> {
    let supported = match kind {
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
    Ok(())
}

struct LocalMedia {
    kind: MediaKind,
    mime: String,
    filename: String,
    bytes: Vec<u8>,
}
impl LocalMedia {
    fn size(&self) -> u64 {
        self.bytes.len() as u64
    }
    fn loaded(self) -> LoadedMedia {
        LoadedMedia {
            kind: self.kind,
            mime: self.mime,
            filename: self.filename,
            size: self.bytes.len() as u64,
            data: STANDARD.encode(&self.bytes),
            sha256: digest(&self.bytes),
            reference: None,
        }
    }
}

fn load_local(marker: &Marker) -> Result<LocalMedia, BoundaryError> {
    let mime = media_type(marker.kind, &marker.path)?.into();
    let bytes = read_bounded(&marker.path, maximum(marker.kind))?;
    let filename = marker
        .path
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .unwrap_or("media")
        .to_owned();
    Ok(LocalMedia {
        kind: marker.kind,
        mime,
        filename,
        bytes,
    })
}

fn read_bounded(path: &Path, maximum: u64) -> Result<Vec<u8>, BoundaryError> {
    read_bounded_after_open(path, maximum, || {})
}

fn read_bounded_after_open(
    path: &Path,
    maximum: u64,
    after_open: impl FnOnce(),
) -> Result<Vec<u8>, BoundaryError> {
    let file = fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NONBLOCK | libc::O_CLOEXEC)
        .open(path)
        .map_err(|_| failure("media_unavailable", "media file cannot be opened"))?;
    after_open();
    let metadata = file
        .metadata()
        .map_err(|_| failure("media_unavailable", "media file cannot be inspected"))?;
    if !metadata.is_file() {
        return Err(failure(
            "media_unavailable",
            "media path is not a regular file",
        ));
    }
    let mut bytes = Vec::new();
    file.take(maximum.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|_| failure("media_unavailable", "media file cannot be read"))?;
    if bytes.is_empty() || bytes.len() as u64 > maximum {
        return Err(failure(
            "media_too_large",
            "media file is empty or exceeds its byte limit",
        ));
    }
    Ok(bytes)
}

pub(crate) fn digest(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

pub(crate) fn validate_metadata(
    kind: MediaKind,
    mime: &str,
    filename: &str,
    bytes: &[u8],
) -> Result<(), BoundaryError> {
    let expected = matches!(
        (kind, mime),
        (
            MediaKind::Image,
            "image/png" | "image/jpeg" | "image/gif" | "image/webp"
        ) | (MediaKind::Audio, "audio/mpeg" | "audio/wav")
            | (
                MediaKind::Video,
                "video/mp4"
                    | "video/quicktime"
                    | "video/webm"
                    | "video/x-matroska"
                    | "video/x-msvideo",
            )
    );
    if !expected
        || filename.is_empty()
        || filename.len() > 1024
        || filename.chars().any(char::is_control)
        || bytes.is_empty()
        || bytes.len() as u64 > maximum(kind)
    {
        return Err(failure(
            "media_invalid",
            "media snapshot metadata or size is invalid",
        ));
    }
    Ok(())
}

pub(crate) fn maximum(kind: MediaKind) -> u64 {
    match kind {
        MediaKind::Image => MAX_IMAGE_BYTES,
        MediaKind::Audio | MediaKind::Video => MAX_AUDIO_VIDEO_BYTES,
    }
}

pub(crate) fn valid_media_id(id: &str) -> bool {
    id.strip_prefix("media_").is_some_and(|id| {
        id.len() == 32
            && id
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    })
}

pub(crate) fn parse_reference(reference: &str) -> Result<(&str, &str), BoundaryError> {
    let Some((id, digest)) = reference.rsplit_once('_') else {
        return Err(failure("media_reference", "media reference is invalid"));
    };
    let valid_digest = digest.len() == 64
        && digest
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte));
    if !valid_media_id(id) || !valid_digest {
        return Err(failure("media_reference", "media reference is invalid"));
    }
    Ok((id, digest))
}

#[derive(Debug)]
struct Marker {
    start: usize,
    label_end: usize,
    end: usize,
    path_start: usize,
    path_end: usize,
    path: PathBuf,
    kind: MediaKind,
}
impl Marker {
    fn reference(&self) -> Option<&str> {
        self.path.to_str()?.strip_prefix(MEDIA_SCHEME)
    }
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
            let path_start = label_end + 2;
            markers.push(Marker {
                start,
                label_end,
                end: path_start + close + 2,
                path_start,
                path_end: path_start + close,
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
    use crate::credentials::SecretRegistry;
    use std::{
        ffi::CString,
        io::{Seek, SeekFrom, Write},
        os::unix::ffi::OsStrExt,
    };

    fn capabilities() -> MediaCapabilities {
        MediaCapabilities {
            image: true,
            audio: true,
            video: true,
        }
    }

    #[test]
    fn enabled_media_expands_into_text_and_base64_parts() {
        let root = tempfile::tempdir().unwrap();
        let image = root.path().join("image.png");
        fs::write(&image, b"png").unwrap();
        let text = format!("Inspect [Image 1](<{}>) now", image.display());
        let parts = expand_user(&text, capabilities()).unwrap();
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

    #[test]
    fn replacement_after_open_cannot_change_or_exceed_the_open_snapshot() {
        let root = tempfile::tempdir().unwrap();
        let image = root.path().join("image.png");
        let replacement = root.path().join("replacement.png");
        fs::write(&image, b"original").unwrap();
        fs::write(&replacement, vec![b'x'; 1024]).unwrap();
        let bytes = read_bounded_after_open(&image, 64, || {
            fs::rename(&replacement, &image).unwrap();
            fs::write(&image, b"path now contains different bytes").unwrap();
        })
        .unwrap();
        assert_eq!(bytes, b"original");
        assert_eq!(
            fs::read(&image).unwrap(),
            b"path now contains different bytes"
        );
    }

    #[test]
    fn reads_are_bounded_when_the_file_exceeds_its_limit() {
        let root = tempfile::tempdir().unwrap();
        let image = root.path().join("large.png");
        let mut file = fs::File::create(&image).unwrap();
        file.write_all(&[b'x'; 65]).unwrap();
        assert_eq!(
            read_bounded(&image, 64).unwrap_err().0.code,
            "media_too_large"
        );
    }

    #[test]
    fn nested_markers_do_not_panic_or_reinterpret_the_outer_path() {
        let root = tempfile::tempdir().unwrap();
        let image = root.path().join("b.png");
        fs::write(&image, b"png").unwrap();
        let text = format!("[Image 1](<a[Image 2](<{}>)>)", image.display());
        assert_eq!(
            expand_user(&text, capabilities()).unwrap_err().0.code,
            "media_unavailable"
        );
        let store = OutputStore::open(
            root.path().join("outputs"),
            Default::default(),
            Arc::new(SecretRegistry::default()),
        )
        .unwrap();
        assert_eq!(
            capture_user(&text, capabilities(), &store, SessionId::new())
                .unwrap_err()
                .0
                .code,
            "media_unavailable"
        );
        assert_eq!(text_references(&text).len(), 0);
        assert_eq!(display(&text), "[Image 1]>)");
    }

    #[test]
    fn non_regular_files_are_rejected_without_blocking() {
        let root = tempfile::tempdir().unwrap();
        let fifo = root.path().join("pipe.png");
        let name = CString::new(fifo.as_os_str().as_bytes()).unwrap();
        // SAFETY: the path is a valid NUL-terminated string and mode is private.
        assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
        let text = format!("[Image 1](<{}>)", fifo.display());
        assert_eq!(
            expand_user(&text, capabilities()).unwrap_err().0.code,
            "media_unavailable"
        );
    }

    #[test]
    fn existing_references_are_verified_before_entering_history() {
        let root = tempfile::tempdir().unwrap();
        let store = OutputStore::open(
            root.path().join("outputs"),
            Default::default(),
            Arc::new(SecretRegistry::default()),
        )
        .unwrap();
        let owner = SessionId::new();
        let object = store
            .put_media(
                owner,
                MediaKind::Image,
                "image/png",
                "image.png",
                b"snapshot",
            )
            .unwrap();
        let text = format!("[Image 1](<{}{}>)", MEDIA_SCHEME, object.reference);
        let other = SessionId::new();
        assert_eq!(
            capture_user(&text, capabilities(), &store, other)
                .unwrap_err()
                .0
                .code,
            "media_denied"
        );
        assert_eq!(
            capture_user(&text, MediaCapabilities::default(), &store, owner)
                .unwrap_err()
                .0
                .code,
            "unsupported_media"
        );
        assert_eq!(
            capture_user(&text, capabilities(), &store, owner)
                .unwrap()
                .text,
            text
        );
    }

    #[test]
    fn existing_reference_capture_retains_its_lease_until_commit() {
        let root = tempfile::tempdir().unwrap();
        let store = OutputStore::open(
            root.path().join("outputs"),
            Default::default(),
            Arc::new(SecretRegistry::default()),
        )
        .unwrap();
        let owner = SessionId::new();
        let object = store
            .put_media(
                owner,
                MediaKind::Image,
                "image/png",
                "image.png",
                b"existing snapshot",
            )
            .unwrap();
        let text = format!("[Image 1](<{}{}>)", MEDIA_SCHEME, object.reference);
        let reference = object.reference.clone();
        drop(object);
        let capture = capture_user(&text, capabilities(), &store, owner).unwrap();
        let collection = store.collect_garbage(&BTreeSet::new(), 8).unwrap();
        assert_eq!(collection.removed, 0);
        assert_eq!(collection.leased, 1);
        assert_eq!(
            store.load_media(owner, &reference).unwrap().data,
            STANDARD.encode(b"existing snapshot")
        );
        drop(capture);
        assert_eq!(
            store.collect_garbage(&BTreeSet::new(), 8).unwrap().removed,
            1
        );
    }

    #[test]
    fn failed_capture_removes_snapshots_created_for_the_rejected_message() {
        let root = tempfile::tempdir().unwrap();
        let store = OutputStore::open(
            root.path().join("outputs"),
            Default::default(),
            Arc::new(SecretRegistry::default()),
        )
        .unwrap();
        let image = root.path().join("image.png");
        fs::write(&image, b"png").unwrap();
        let text = format!(
            "[Image 1](<{}>) [Image 2](</missing/second.png>)",
            image.display()
        );
        assert_eq!(
            capture_user(&text, capabilities(), &store, SessionId::new())
                .unwrap_err()
                .0
                .code,
            "media_unavailable"
        );
        let media = fs::read_dir(store.protected_path())
            .unwrap()
            .filter_map(Result::ok)
            .filter(|entry| entry.file_name().to_string_lossy().contains("media_"))
            .count();
        assert_eq!(media, 0);

        let owner = SessionId::new();
        let existing = store
            .put_media(
                owner,
                MediaKind::Image,
                "image/png",
                "existing.png",
                b"existing snapshot",
            )
            .unwrap();
        let reference = existing.reference.clone();
        drop(existing);
        let mixed =
            format!("[Image 1](<{MEDIA_SCHEME}{reference}>) [Image 2](</missing/second.png>)");
        assert_eq!(
            capture_user(&mixed, capabilities(), &store, owner)
                .unwrap_err()
                .0
                .code,
            "media_unavailable"
        );
        assert_eq!(
            store.load_media(owner, &reference).unwrap().data,
            STANDARD.encode(b"existing snapshot")
        );
    }

    #[test]
    fn durable_snapshot_survives_source_changes_and_reopening() {
        let root = tempfile::tempdir().unwrap();
        let image = root.path().join("image.png");
        fs::write(&image, b"original image").unwrap();
        let store = OutputStore::open(
            root.path().join("outputs"),
            Default::default(),
            Arc::new(SecretRegistry::default()),
        )
        .unwrap();
        let session = SessionId::new();
        let input = format!("Inspect [Image 1](<{}>)", image.display());
        let capture = capture_user(&input, capabilities(), &store, session).unwrap();
        assert!(!capture.text.contains(&image.display().to_string()));
        let references = text_references(&capture.text);
        assert_eq!(references.len(), 1);
        fs::write(&image, b"changed image").unwrap();
        fs::remove_file(&image).unwrap();
        let captured_text = capture.text.clone();
        drop(capture);

        let reopened = OutputStore::open(
            store.protected_path(),
            Default::default(),
            Arc::new(SecretRegistry::default()),
        )
        .unwrap();
        let attachments = load_history(
            &[HistoryItem::UserText(captured_text.clone())],
            Some(&Arc::new(reopened)),
            session,
        )
        .unwrap();
        let parts =
            expand_user_with_snapshots(&captured_text, capabilities(), &attachments).unwrap();
        let media = parts.iter().find_map(|part| match part {
            ContentPart::Media(media) => Some(media),
            _ => None,
        });
        assert_eq!(
            media.and_then(|media| STANDARD.decode(&media.data).ok()),
            Some(b"original image".to_vec())
        );
        assert_eq!(
            media.map(|media| media.sha256.as_str()),
            Some(digest(b"original image").as_str())
        );
    }

    #[test]
    fn snapshot_references_are_session_scoped_and_fail_closed_when_missing() {
        let root = tempfile::tempdir().unwrap();
        let store = OutputStore::open(
            root.path().join("outputs"),
            Default::default(),
            Arc::new(SecretRegistry::default()),
        )
        .unwrap();
        let session = SessionId::new();
        let object = store
            .put_media(
                session,
                MediaKind::Image,
                "image/png",
                "image.png",
                b"private image",
            )
            .unwrap();
        let reference = object.reference.clone();
        assert_eq!(
            store
                .load_media(SessionId::new(), &reference)
                .unwrap_err()
                .0
                .code,
            "media_denied"
        );
        assert_eq!(
            store.collect_garbage(&BTreeSet::new(), 8).unwrap().leased,
            1
        );
        drop(object);
        store
            .collect_garbage(&BTreeSet::from([reference.clone()]), 8)
            .unwrap();
        assert_eq!(
            store.load_media(session, &reference).unwrap().data,
            STANDARD.encode(b"private image")
        );
        assert_eq!(
            store.collect_garbage(&BTreeSet::new(), 8).unwrap().removed,
            1
        );
        assert_eq!(
            store.load_media(session, &reference).unwrap_err().0.code,
            "media_unavailable"
        );
    }

    #[test]
    fn changed_snapshot_bytes_fail_the_content_integrity_check() {
        let root = tempfile::tempdir().unwrap();
        let store = OutputStore::open(
            root.path().join("outputs"),
            Default::default(),
            Arc::new(SecretRegistry::default()),
        )
        .unwrap();
        let session = SessionId::new();
        let object = store
            .put_media(
                session,
                MediaKind::Image,
                "image/png",
                "image.png",
                b"original",
            )
            .unwrap();
        let reference = object.reference.clone();
        let (id, _) = parse_reference(&reference).unwrap();
        let path = store.protected_path().join(format!("{id}.media"));
        drop(object);
        let mut file = fs::OpenOptions::new().write(true).open(path).unwrap();
        file.seek(SeekFrom::End(-1)).unwrap();
        file.write_all(b"X").unwrap();
        drop(file);
        assert_eq!(
            store.load_media(session, &reference).unwrap_err().0.code,
            "media_corrupt"
        );
    }
}
