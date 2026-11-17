//! User-provided local media and durable, content-identified session snapshots.

use crate::{
    config::MediaCapabilities,
    contract::{BoundaryError, ErrorInfo, HistoryItem, SessionId, ToolResult},
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
pub const MAX_FILE_BYTES: u64 = 16 * 1024 * 1024;
pub const MAX_MESSAGE_MEDIA_BYTES: u64 = 64 * 1024 * 1024;
/// Maximum host-written tool images injected into one model request.
pub const MAX_TOOL_IMAGES: usize = 8;
pub const MEDIA_SCHEME: &str = "soyo-media://";
/// Scheme stored by pre-rename versions; history must still resolve it.
pub const MEDIA_SCHEME_LEGACY: &str = "saya-media://";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MediaKind {
    Image,
    Audio,
    Video,
    /// A bounded generic file artifact. It is stored by the same private
    /// object backend but is never expanded into model media content.
    File,
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

/// Why a host-written tool media reference is not eligible for model injection.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum OmittedReason {
    /// File artifacts are host-side evidence and are never model media.
    FileArtifact,
    /// Tool results may inject images only; audio and video stay host-side.
    NotAnImage,
    /// The selected profile does not enable the image modality.
    CapabilityDisabled,
    /// This request already carries the maximum number or byte budget of tool
    /// images. The object stays a GC pin but is not offered to the model.
    LimitExceeded,
}

/// Verified snapshots indexed by their durable reference. The payload is never
/// serialized into the neutral context fingerprint or session history.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct MediaAttachments {
    items: BTreeMap<String, LoadedMedia>,
    /// Tool-result references that the host deliberately keeps out of model
    /// content. Providers use this to distinguish a File or capability-gated
    /// reference from a genuinely missing object.
    omitted: BTreeMap<String, OmittedReason>,
}
impl std::fmt::Debug for MediaAttachments {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MediaAttachments")
            .field("references", &self.items.keys().collect::<Vec<_>>())
            .field("omitted", &self.omitted.keys().collect::<Vec<_>>())
            .finish()
    }
}
impl MediaAttachments {
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }
    pub fn insert(&mut self, media: LoadedMedia) -> Result<(), BoundaryError> {
        let reference = media
            .reference
            .clone()
            .ok_or_else(|| failure("media_corrupt", "media snapshot has no durable reference"))?;
        if let Some(existing) = self.items.get(&reference)
            && existing != &media
        {
            return Err(failure(
                "media_corrupt",
                "media reference has conflicting snapshots",
            ));
        }
        self.omitted.remove(&reference);
        self.items.insert(reference, media);
        Ok(())
    }
    fn omit(&mut self, reference: String, reason: OmittedReason) {
        self.items.remove(&reference);
        self.omitted.insert(reference, reason);
    }
    fn get(&self, reference: &str) -> Option<&LoadedMedia> {
        self.items.get(reference)
    }
    /// Image snapshots produced by host tool actions that are eligible for the
    /// next model request. Only Image objects from `ToolResult.media_refs` are
    /// considered; File artifacts and capability-gated objects are skipped.
    /// A reference that is neither loaded nor explicitly omitted fails closed.
    pub fn tool_result_images(
        &self,
        result: &ToolResult,
    ) -> Result<Vec<LoadedMedia>, BoundaryError> {
        let mut images = Vec::new();
        let mut seen = std::collections::BTreeSet::new();
        let mut retained = 0u64;
        let mut encoded = 0u64;
        for reference in &result.media_refs {
            parse_reference(reference)?;
            if !seen.insert(reference) {
                continue;
            }
            match self.items.get(reference) {
                Some(media) if media.kind == MediaKind::Image => {
                    retained = retained.saturating_add(media.size);
                    encoded = encoded.saturating_add(media.data.len() as u64);
                    if media.size == 0
                        || media.size > maximum(MediaKind::Image)
                        || retained > MAX_MESSAGE_MEDIA_BYTES
                        || encoded > MAX_MESSAGE_MEDIA_BYTES
                    {
                        return Err(failure(
                            "media_too_large",
                            "tool image media exceeds its byte limit",
                        ));
                    }
                    images.push(media.clone());
                }
                Some(_) => {
                    return Err(failure(
                        "media_corrupt",
                        "tool media snapshot kind is not injectable",
                    ));
                }
                None => match self.omitted.get(reference) {
                    Some(
                        OmittedReason::FileArtifact
                        | OmittedReason::NotAnImage
                        | OmittedReason::CapabilityDisabled
                        | OmittedReason::LimitExceeded,
                    ) => {}
                    None => {
                        return Err(failure(
                            "media_unavailable",
                            "tool media snapshot is unavailable",
                        ));
                    }
                },
            }
        }
        Ok(images)
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

/// Load durable snapshots that the model may actually expand this turn.
/// User-authored `UserText` markers remain eligible for Image/Audio/Video.
/// Host-written `ToolResult.media_refs` contribute only Image objects, and only
/// when the selected profile enables images; File artifacts never become model
/// media. Tool images are selected from the effective request history only:
/// entries already removed by compaction or retirement are never passed here,
/// and the loader caps one request at [`MAX_TOOL_IMAGES`] references whose raw
/// bytes and base64 encoding both stay within [`MAX_MESSAGE_MEDIA_BYTES`]. The
/// full session history still pins every reference through
/// [`history_references`].
pub fn load_history(
    history: &[HistoryItem],
    store: Option<&Arc<OutputStore>>,
    session_id: SessionId,
    capabilities: MediaCapabilities,
) -> Result<MediaAttachments, BoundaryError> {
    let mut snapshots = MediaAttachments::default();
    let mut tool_images = 0usize;
    let mut tool_raw_bytes = 0u64;
    let mut tool_base64_bytes = 0u64;
    for item in history {
        match item {
            HistoryItem::UserText(text) => {
                for reference in text_references(text) {
                    let store = store.ok_or_else(|| {
                        failure(
                            "media_unavailable",
                            "durable media storage is not configured",
                        )
                    })?;
                    // A File object is a host-side artifact, never model media.
                    // Check its header only so no base64 payload is produced.
                    let kind = store.media_kind(session_id, &reference)?;
                    if kind == MediaKind::File {
                        continue;
                    }
                    snapshots.insert(store.load_media(session_id, &reference)?)?;
                }
            }
            HistoryItem::ToolResult { result, .. } => {
                for reference in &result.media_refs {
                    parse_reference(reference)?;
                    let store = store.ok_or_else(|| {
                        failure(
                            "media_unavailable",
                            "durable media storage is not configured",
                        )
                    })?;
                    if snapshots.items.contains_key(reference) {
                        // The same object may be attached to several results;
                        // count and load it once per request.
                        continue;
                    }
                    if !capabilities.image {
                        // The profile cannot accept any tool image, so the
                        // header is unnecessary; the reference stays a pin.
                        snapshots.omit(reference.clone(), OmittedReason::CapabilityDisabled);
                        continue;
                    }
                    if tool_images >= MAX_TOOL_IMAGES {
                        // Do not open further objects: this request already has
                        // the maximum number of usable tool images.
                        snapshots.omit(reference.clone(), OmittedReason::LimitExceeded);
                        continue;
                    }
                    let (kind, size) = store.media_info(session_id, reference)?;
                    match kind {
                        MediaKind::Image => {
                            // Only the header was read. Skip objects that
                            // would exceed either aggregate budget, and never
                            // let one extra reference turn a valid request into
                            // an error.
                            if !tool_image_budget_allows(tool_raw_bytes, tool_base64_bytes, size) {
                                snapshots.omit(reference.clone(), OmittedReason::LimitExceeded);
                                continue;
                            }
                            let media = store.load_media(session_id, reference)?;
                            let encoded = media.data.len() as u64;
                            if media.size != size
                                || !tool_image_budget_allows(
                                    tool_raw_bytes,
                                    tool_base64_bytes,
                                    media.size,
                                )
                                || tool_base64_bytes
                                    .checked_add(encoded)
                                    .is_none_or(|total| total > MAX_MESSAGE_MEDIA_BYTES)
                            {
                                snapshots.omit(reference.clone(), OmittedReason::LimitExceeded);
                                continue;
                            }
                            tool_images += 1;
                            tool_raw_bytes = tool_raw_bytes.saturating_add(media.size);
                            tool_base64_bytes = tool_base64_bytes.saturating_add(encoded);
                            snapshots.insert(media)?;
                        }
                        MediaKind::File => {
                            snapshots.omit(reference.clone(), OmittedReason::FileArtifact);
                        }
                        MediaKind::Audio | MediaKind::Video => {
                            snapshots.omit(reference.clone(), OmittedReason::NotAnImage);
                        }
                    }
                }
            }
            _ => {}
        }
    }
    Ok(snapshots)
}

/// Return true when adding `size` raw bytes keeps both the raw and base64
/// aggregate within the per-message limit.
pub(crate) fn tool_image_budget_allows(current_raw: u64, current_base64: u64, size: u64) -> bool {
    if size == 0 || size > maximum(MediaKind::Image) {
        return false;
    }
    let Some(encoded) = size.checked_add(2).map(|value| value / 3 * 4) else {
        return false;
    };
    current_raw
        .checked_add(size)
        .is_some_and(|total| total <= MAX_MESSAGE_MEDIA_BYTES)
        && current_base64
            .checked_add(encoded)
            .is_some_and(|total| total <= MAX_MESSAGE_MEDIA_BYTES)
}

/// References which must be pinned when the caller runs OutputStore GC.
pub fn history_references(history: &[HistoryItem]) -> BTreeSet<String> {
    history
        .iter()
        .flat_map(|item| match item {
            HistoryItem::UserText(text) => text_references(text).into_iter().collect::<Vec<_>>(),
            // Only the host-written media reference list is authoritative. Page
            // text, tool output and model-supplied strings never contribute
            // pins, so a forged `saya-media://` reference cannot keep an
            // unrelated object alive.
            HistoryItem::ToolResult { result, .. } => result
                .media_refs
                .iter()
                .filter(|reference| crate::media::parse_reference(reference).is_ok())
                .cloned()
                .collect::<Vec<_>>(),
            _ => Vec::new(),
        })
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
        // File artifacts are host-side files, never model media.
        MediaKind::File => false,
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
            | (
                MediaKind::File,
                "application/octet-stream"
                    | "application/pdf"
                    | "application/zip"
                    | "text/plain"
                    | "text/csv"
                    | "application/json"
                    | "application/xml"
                    | "image/png"
                    | "image/jpeg"
                    | "image/gif"
                    | "image/webp",
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
        MediaKind::File => MAX_FILE_BYTES,
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
        self.path
            .to_str()?
            .strip_prefix(MEDIA_SCHEME)
            .or_else(|| self.path.to_str()?.strip_prefix(MEDIA_SCHEME_LEGACY))
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
            capabilities(),
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
    fn file_artifacts_are_pinned_by_host_media_refs_but_never_model_media() {
        let root = tempfile::tempdir().unwrap();
        let store = OutputStore::open(
            root.path().join("outputs"),
            Default::default(),
            Arc::new(SecretRegistry::default()),
        )
        .unwrap();
        let session = SessionId::new();
        let file = store
            .put_file(session, "text/plain", "note.txt", b"artifact")
            .unwrap();
        let reference = file.reference.clone();
        // A page or tool text that merely looks like a media reference is not
        // a pin; only the host-written ToolResult field is authoritative.
        let forged = HistoryItem::ToolResult {
            call_id: "call-1".into(),
            result: crate::contract::ToolResult {
                ok: true,
                data: Some(serde_json::json!({"body": format!("{MEDIA_SCHEME}{reference}")})),
                error: None,
                truncated: false,
                media_refs: Vec::new(),
            },
        };
        assert!(history_references(std::slice::from_ref(&forged)).is_empty());
        let pinned = HistoryItem::ToolResult {
            call_id: "call-2".into(),
            result: crate::contract::ToolResult {
                ok: true,
                data: None,
                error: None,
                truncated: false,
                media_refs: vec![reference.clone()],
            },
        };
        assert_eq!(
            history_references(&[pinned]),
            BTreeSet::from([reference.clone()])
        );
        drop(file);
        store
            .collect_garbage(&history_references(&[forged]), 8)
            .unwrap();
        assert_eq!(
            store.load_media(session, &reference).unwrap_err().0.code,
            "media_unavailable"
        );

        let file = store
            .put_file(session, "text/plain", "note.txt", b"artifact")
            .unwrap();
        let reference = file.reference.clone();
        let pinned = HistoryItem::ToolResult {
            call_id: "call-3".into(),
            result: crate::contract::ToolResult {
                ok: true,
                data: None,
                error: None,
                truncated: false,
                media_refs: vec![reference.clone()],
            },
        };
        drop(file);
        let collection = store
            .collect_garbage(&history_references(&[pinned]), 8)
            .unwrap();
        assert_eq!(collection.pinned, 1);
        let (bytes, media, _lease) = store.load_file(session, &reference).unwrap();
        assert_eq!(bytes, b"artifact");
        assert_eq!(media.kind, MediaKind::File);
        // A File artifact is never loaded into model media, even when user
        // text contains a marker that points at it.
        let text = format!("[Image 1](<{MEDIA_SCHEME}{reference}>)");
        let attachments = load_history(
            &[HistoryItem::UserText(text.clone())],
            Some(&Arc::new(store.clone())),
            session,
            capabilities(),
        )
        .unwrap();
        assert!(attachments.is_empty());
        assert_eq!(
            expand_user_with_snapshots(&text, capabilities(), &attachments)
                .unwrap_err()
                .0
                .code,
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
    #[test]
    fn tool_result_images_are_injected_only_for_enabled_image_profiles() {
        let root = tempfile::tempdir().unwrap();
        let store = OutputStore::open(
            root.path().join("outputs"),
            Default::default(),
            Arc::new(SecretRegistry::default()),
        )
        .unwrap();
        let session = SessionId::new();
        let image = store
            .put_media(
                session,
                MediaKind::Image,
                "image/png",
                "image.png",
                b"png image",
            )
            .unwrap();
        let reference = image.reference.clone();
        let result = crate::contract::ToolResult {
            ok: true,
            data: None,
            error: None,
            truncated: false,
            media_refs: vec![reference.clone()],
        };
        let pinned = HistoryItem::ToolResult {
            call_id: "tool-call".into(),
            result: result.clone(),
        };
        assert_eq!(
            history_references(std::slice::from_ref(&pinned)),
            BTreeSet::from([reference.clone()])
        );
        let attachments = load_history(
            std::slice::from_ref(&pinned),
            Some(&Arc::new(store.clone())),
            session,
            capabilities(),
        )
        .unwrap();
        let images = attachments.tool_result_images(&result).unwrap();
        assert_eq!(images.len(), 1);
        assert_eq!(images[0].kind, MediaKind::Image);
        assert_eq!(
            STANDARD.decode(&images[0].data).unwrap(),
            b"png image".to_vec()
        );

        // Disabled image capability keeps the reference as a GC pin but never
        // produces model content.
        let disabled = MediaCapabilities {
            image: false,
            audio: true,
            video: true,
        };
        let attachments = load_history(
            std::slice::from_ref(&pinned),
            Some(&Arc::new(store.clone())),
            session,
            disabled,
        )
        .unwrap();
        assert!(attachments.tool_result_images(&result).unwrap().is_empty());

        // File artifacts are never model media even with images enabled.
        let file = store
            .put_file(session, "text/plain", "note.txt", b"artifact")
            .unwrap();
        let file_result = crate::contract::ToolResult {
            ok: true,
            data: None,
            error: None,
            truncated: false,
            media_refs: vec![file.reference.clone()],
        };
        let file_pinned = HistoryItem::ToolResult {
            call_id: "file-call".into(),
            result: file_result.clone(),
        };
        let attachments = load_history(
            std::slice::from_ref(&file_pinned),
            Some(&Arc::new(store.clone())),
            session,
            capabilities(),
        )
        .unwrap();
        assert!(
            attachments
                .tool_result_images(&file_result)
                .unwrap()
                .is_empty()
        );

        // A syntactically valid host-written reference with no object fails
        // closed instead of silently disappearing.
        let missing = crate::contract::ToolResult {
            ok: true,
            data: None,
            error: None,
            truncated: false,
            media_refs: vec![format!("media_{}_{}", "0".repeat(32), "0".repeat(64))],
        };
        assert_eq!(
            attachments.tool_result_images(&missing).unwrap_err().0.code,
            "media_unavailable"
        );
    }

    #[test]
    fn tool_images_stop_at_the_reference_budget_without_failing_the_request() {
        let root = tempfile::tempdir().unwrap();
        let store = OutputStore::open(
            root.path().join("outputs"),
            Default::default(),
            Arc::new(SecretRegistry::default()),
        )
        .unwrap();
        let session = SessionId::new();
        let mut references = Vec::new();
        for index in 0..(MAX_TOOL_IMAGES + 2) {
            let image = store
                .put_media(
                    session,
                    MediaKind::Image,
                    "image/png",
                    "image.png",
                    format!("image-{index}").as_bytes(),
                )
                .unwrap();
            references.push(image.reference.clone());
        }
        let result = crate::contract::ToolResult {
            ok: true,
            data: None,
            error: None,
            truncated: false,
            media_refs: references.clone(),
        };
        let history = [HistoryItem::ToolResult {
            call_id: "many-images".into(),
            result: result.clone(),
        }];
        let attachments = load_history(
            &history,
            Some(&Arc::new(store.clone())),
            session,
            capabilities(),
        )
        .unwrap();
        let images = attachments.tool_result_images(&result).unwrap();
        assert_eq!(images.len(), MAX_TOOL_IMAGES);
        // Every reference still stays a GC pin.
        assert_eq!(
            history_references(&history),
            references.into_iter().collect()
        );
    }

    #[test]
    fn tool_image_budget_counts_raw_and_base64_bytes() {
        let encoded = |size: u64| size.div_ceil(3) * 4;
        let five = 5 * MAX_IMAGE_BYTES;
        assert!(!tool_image_budget_allows(
            five,
            5 * encoded(MAX_IMAGE_BYTES),
            MAX_IMAGE_BYTES
        ));
        let four = 4 * MAX_IMAGE_BYTES;
        assert!(tool_image_budget_allows(
            four,
            4 * encoded(MAX_IMAGE_BYTES),
            MAX_IMAGE_BYTES
        ));
        assert!(!tool_image_budget_allows(0, 0, 0));
        assert!(!tool_image_budget_allows(0, 0, MAX_IMAGE_BYTES + 1));
        assert!(!tool_image_budget_allows(
            MAX_MESSAGE_MEDIA_BYTES,
            0,
            MAX_IMAGE_BYTES
        ));
    }

    #[test]
    fn only_the_effective_request_history_offers_tool_images() {
        let root = tempfile::tempdir().unwrap();
        let store = OutputStore::open(
            root.path().join("outputs"),
            Default::default(),
            Arc::new(SecretRegistry::default()),
        )
        .unwrap();
        let session = SessionId::new();
        let dropped = store
            .put_media(
                session,
                MediaKind::Image,
                "image/png",
                "dropped.png",
                b"dropped",
            )
            .unwrap();
        let kept = store
            .put_media(session, MediaKind::Image, "image/png", "kept.png", b"kept")
            .unwrap();
        let kept_result = crate::contract::ToolResult {
            ok: true,
            data: None,
            error: None,
            truncated: false,
            media_refs: vec![kept.reference.clone()],
        };
        // The dropped ToolResult was replaced by a compact summary and is not
        // part of the effective request history passed to the loader.
        let projected = [
            HistoryItem::UserText(
                serde_json::json!({"soyo_context_summary_v1": "prior work"}).to_string(),
            ),
            HistoryItem::ToolResult {
                call_id: "kept".into(),
                result: kept_result.clone(),
            },
        ];
        let attachments = load_history(
            &projected,
            Some(&Arc::new(store.clone())),
            session,
            capabilities(),
        )
        .unwrap();
        assert_eq!(
            attachments.tool_result_images(&kept_result).unwrap().len(),
            1
        );
        let dropped_result = crate::contract::ToolResult {
            ok: true,
            data: None,
            error: None,
            truncated: false,
            media_refs: vec![dropped.reference.clone()],
        };
        // The dropped reference was never selected, so no snapshot exists and
        // no forged history item can resurrect it.
        assert_eq!(
            attachments
                .tool_result_images(&dropped_result)
                .unwrap_err()
                .0
                .code,
            "media_unavailable"
        );
        // The full durable history still pins both objects independently.
        assert!(
            history_references(&[
                HistoryItem::ToolResult {
                    call_id: "dropped".into(),
                    result: dropped_result,
                },
                HistoryItem::ToolResult {
                    call_id: "kept".into(),
                    result: kept_result,
                },
            ])
            .len()
                == 2
        );

        // A selected reference whose object disappeared fails explicitly.
        let missing_history = [HistoryItem::ToolResult {
            call_id: "missing".into(),
            result: crate::contract::ToolResult {
                ok: true,
                data: None,
                error: None,
                truncated: false,
                media_refs: vec![format!("media_{}_{}", "0".repeat(32), "0".repeat(64))],
            },
        }];
        assert_eq!(
            load_history(
                &missing_history,
                Some(&Arc::new(store)),
                session,
                capabilities(),
            )
            .unwrap_err()
            .0
            .code,
            "media_unavailable"
        );
    }
}
