//! Host-side file broker for browser downloads and uploads.
//!
//! Downloads are accepted only from one explicit element action, validated for
//! path, size, declared type, magic and SHA-256, and then stored as a bounded
//! generic `File` artifact in the private OutputStore. Uploads accept only an
//! artifact reference owned by the same Session; bytes are copied into a
//! Run-private 0600 staging file and never read from a model-supplied host
//! path.

use crate::{
    contract::{BoundaryError, ErrorInfo, SessionId},
    media::{LoadedMedia, MAX_FILE_BYTES, MediaKind},
    output_store::{OutputLease, OutputStore, SealedMedia},
};
use std::{
    fs::{self, DirBuilder, OpenOptions},
    io::Write,
    os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt},
    path::PathBuf,
    sync::Arc,
};

const MAX_FILENAME_BYTES: usize = 128;
const STAGING_DIR: &str = "browser-staging";

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

pub(crate) struct BrowserArtifactStore {
    store: Arc<OutputStore>,
    session_id: SessionId,
    staging_root: PathBuf,
}

#[derive(Debug, Clone)]
pub(crate) struct StoredArtifact {
    pub(crate) reference: String,
    pub(crate) sha256: String,
    pub(crate) size: u64,
    pub(crate) filename: String,
    pub(crate) mime: String,
}

pub(crate) struct StagedUpload {
    pub(crate) reference: String,
    pub(crate) sha256: String,
    pub(crate) filename: String,
    pub(crate) mime: String,
    pub(crate) size: u64,
    pub(crate) path: PathBuf,
    _lease: OutputLease,
}

impl StagedUpload {
    pub(crate) fn media_size(&self) -> u64 {
        self.size
    }
}

impl std::fmt::Debug for StagedUpload {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("StagedUpload")
            .field("reference", &self.reference)
            .field("filename", &self.filename)
            .field("mime", &self.mime)
            .finish_non_exhaustive()
    }
}

impl Drop for StagedUpload {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

impl BrowserArtifactStore {
    pub(crate) fn new(
        store: Arc<OutputStore>,
        session_id: SessionId,
    ) -> Result<Self, BoundaryError> {
        let staging_root = store.protected_path().join(STAGING_DIR);
        match DirBuilder::new().mode(0o700).create(&staging_root) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(_) => {
                return Err(failure(
                    "file_broker",
                    "Browser artifact staging directory cannot be created",
                ));
            }
        }
        let metadata = fs::symlink_metadata(&staging_root)
            .map_err(|_| failure("file_broker", "Browser artifact staging is unavailable"))?;
        // SAFETY: geteuid has no pointer arguments or ownership requirements.
        if !metadata.is_dir()
            || metadata.file_type().is_symlink()
            || metadata.mode() & 0o077 != 0
            || metadata.uid() != unsafe { libc::geteuid() }
        {
            return Err(failure(
                "file_broker",
                "Browser artifact staging must be a private owned directory",
            ));
        }
        Ok(Self {
            store,
            session_id,
            staging_root,
        })
    }

    pub(crate) fn store_download(
        &self,
        filename: &str,
        mime: &str,
        bytes: &[u8],
        max_bytes: usize,
    ) -> Result<(StoredArtifact, SealedMedia), BoundaryError> {
        if bytes.is_empty() || bytes.len() > max_bytes || bytes.len() as u64 > MAX_FILE_BYTES {
            return Err(failure(
                "file_too_large",
                "Browser download is empty or exceeds its size limit",
            ));
        }
        let filename = sanitize_filename(filename)?;
        let mime = validate_magic(&filename, mime, bytes)?;
        let sealed = self
            .store
            .put_file(self.session_id, mime, &filename, bytes)?;
        let stored = StoredArtifact {
            reference: sealed.reference.clone(),
            sha256: sealed.sha256.clone(),
            size: sealed.size,
            filename,
            mime: mime.into(),
        };
        Ok((stored, sealed))
    }

    /// Validate one existing Session-owned artifact and copy it to a private
    /// staging file. The returned lease keeps the source object alive until the
    /// dispatch completes or is reported unknown.
    pub(crate) fn stage_upload(
        &self,
        reference: &str,
        max_bytes: usize,
    ) -> Result<StagedUpload, BoundaryError> {
        let (bytes, media, lease) = self.store.load_file(self.session_id, reference)?;
        if bytes.is_empty() || bytes.len() > max_bytes || bytes.len() as u64 > MAX_FILE_BYTES {
            return Err(failure(
                "file_too_large",
                "Browser upload artifact is empty or exceeds its size limit",
            ));
        }
        let filename = sanitize_filename(&media.filename)?;
        let mime = validate_magic(&filename, &media.mime, &bytes)?;
        let path = self.staging_root.join(format!(
            "upload_{}_{filename}",
            uuid::Uuid::now_v7().simple()
        ));
        let mut options = OpenOptions::new();
        options
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
        let mut file = options.open(&path).map_err(|_| {
            failure(
                "file_broker",
                "Browser upload staging file cannot be created",
            )
        })?;
        if file
            .write_all(&bytes)
            .and_then(|()| file.sync_all())
            .is_err()
        {
            // Never leave a partially written staging file behind: the next
            // attempt must not observe a truncated artifact.
            drop(file);
            let _ = fs::remove_file(&path);
            return Err(failure(
                "file_broker",
                "Browser upload staging file cannot be written or synced",
            ));
        }
        Ok(StagedUpload {
            reference: reference.into(),
            sha256: media.sha256,
            filename,
            mime: mime.into(),
            size: bytes.len() as u64,
            path,
            _lease: lease,
        })
    }

    /// Load a stored artifact for a later browser upload. This validates that
    /// the reference is a `File` artifact owned by this Session.
    pub(crate) fn load_artifact(&self, reference: &str) -> Result<LoadedArtifact, BoundaryError> {
        let (bytes, media, lease) = self.store.load_file(self.session_id, reference)?;
        Ok(LoadedArtifact {
            bytes,
            media,
            _lease: lease,
        })
    }
}

pub(crate) struct LoadedArtifact {
    pub(crate) bytes: Vec<u8>,
    #[allow(dead_code)]
    pub(crate) media: LoadedMedia,
    _lease: OutputLease,
}

/// Store a screenshot as a bounded image file artifact. It is not injected as
/// model media in this stage.
pub(crate) fn store_image_artifact(
    store: &BrowserArtifactStore,
    filename: &str,
    mime: &str,
    width: u32,
    height: u32,
    bytes: Vec<u8>,
) -> Result<(StoredArtifact, SealedMedia), BoundaryError> {
    let _ = (width, height);
    let filename = sanitize_filename(filename)?;
    let mime = validate_magic(&filename, mime, &bytes)?;
    let sealed =
        store
            .store
            .put_media(store.session_id, MediaKind::Image, mime, &filename, &bytes)?;
    Ok((
        StoredArtifact {
            reference: sealed.reference.clone(),
            sha256: sealed.sha256.clone(),
            size: sealed.size,
            filename,
            mime: mime.into(),
        },
        sealed,
    ))
}

pub(crate) fn sanitize_filename(value: &str) -> Result<String, BoundaryError> {
    if value.is_empty()
        || value.len() > 4 * MAX_FILENAME_BYTES
        || value.chars().any(char::is_control)
        || value.contains('\0')
    {
        return Err(failure(
            "file_name_invalid",
            "Browser file name is empty or invalid",
        ));
    }
    // Accept only the final component, defeating traversal and absolute paths.
    let base = value
        .rsplit(['/', '\\'])
        .next()
        .filter(|name| !name.is_empty() && *name != "." && *name != "..")
        .ok_or_else(|| failure("file_name_invalid", "Browser file name is invalid"))?;
    let mut sanitized = String::with_capacity(base.len());
    for character in base.chars() {
        if character.is_ascii_alphanumeric() || matches!(character, '.' | '-' | '_' | '+' | ' ') {
            sanitized.push(character);
        } else if character == '\u{202e}' {
            // Bidirectional override is never allowed in a displayed name.
            return Err(failure("file_name_invalid", "Browser file name is unsafe"));
        } else {
            sanitized.push('_');
        }
    }
    let sanitized = sanitized.trim_matches('.').trim().to_owned();
    if sanitized.is_empty() || sanitized.len() > MAX_FILENAME_BYTES || sanitized.starts_with('.') {
        return Err(failure("file_name_invalid", "Browser file name is unsafe"));
    }
    Ok(sanitized)
}

/// Detect an allowed file type from its leading bytes. Executable and script
/// containers are rejected; the declared MIME must agree with the detected
/// type (or be the generic octet-stream).
pub(crate) fn validate_magic<'a>(
    filename: &str,
    declared: &'a str,
    bytes: &[u8],
) -> Result<&'a str, BoundaryError> {
    let detected = if bytes.starts_with(b"\x7fELF")
        || bytes.starts_with(b"MZ")
        || bytes.starts_with(&[0xcf, 0xfa, 0xed, 0xfe])
        || bytes.starts_with(&[0xfe, 0xed, 0xfa, 0xcf])
        || bytes.starts_with(&[0xca, 0xfe, 0xba, 0xbe])
    {
        return Err(failure(
            "file_type_denied",
            "Browser file type is not allowed",
        ));
    } else if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        "image/png"
    } else if bytes.starts_with(&[0xff, 0xd8, 0xff]) {
        "image/jpeg"
    } else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        "image/gif"
    } else if bytes.len() > 12 && bytes.starts_with(b"RIFF") && &bytes[8..12] == b"WEBP" {
        "image/webp"
    } else if bytes.starts_with(b"%PDF-") {
        "application/pdf"
    } else if bytes.starts_with(b"PK\x03\x04") {
        "application/zip"
    } else if std::str::from_utf8(bytes).ok().is_some_and(|text| {
        !text
            .chars()
            .any(|c| c == '\0' || (c.is_control() && !c.is_whitespace()))
    }) {
        if filename.to_ascii_lowercase().ends_with(".json") {
            "application/json"
        } else if filename.to_ascii_lowercase().ends_with(".csv") {
            "text/csv"
        } else if filename.to_ascii_lowercase().ends_with(".xml") {
            "application/xml"
        } else {
            "text/plain"
        }
    } else {
        "application/octet-stream"
    };
    let accepted = matches!(
        declared,
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
            | "image/webp"
    );
    if !accepted || !(declared == detected || declared == "application/octet-stream") {
        return Err(failure(
            "file_type_mismatch",
            "Browser file type does not match its content",
        ));
    }
    Ok(detected)
}
