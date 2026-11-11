use super::*;
use cap_std::fs::{Dir, OpenOptions, OpenOptionsExt};
use std::{
    fs::{DirBuilder, File},
    io::{Read, Write},
    os::{
        fd::AsRawFd,
        unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt as _},
    },
    path::PathBuf,
    time::{Duration, Instant},
};

const MAGIC: &[u8; 8] = b"SAYAOUT1";
const MEDIA_MAGIC: &[u8; 8] = b"SAYAMED1";
const MAX_HEADER_BYTES: usize = 64 * 1024;

fn media_error(code: &str, message: &str) -> BoundaryError {
    error(code, message)
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct MediaHeader {
    version: u8,
    id: String,
    session_id: SessionId,
    kind: crate::media::MediaKind,
    mime: String,
    filename: String,
    byte_len: u64,
    sha256: String,
}

pub(super) struct Inner {
    pub(super) dir: Dir,
    pub(super) path: PathBuf,
    pub(super) limits: OutputLimits,
    pub(super) secrets: Arc<SecretRegistry>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct HeaderSegment {
    source_window: OutputRange,
    bytes: usize,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Header {
    version: u8,
    id: String,
    scope: OutputScope,
    capture_limit_bytes: usize,
    total_bytes_seen: u64,
    completion: CaptureCompletion,
    segments: Vec<HeaderSegment>,
    withheld_reason: Option<WithheldReason>,
    sha256: String,
}
impl Header {
    fn from_payload(payload: &Payload, sha256: String) -> Self {
        Self {
            version: payload.version,
            id: payload.id.clone(),
            scope: payload.scope.clone(),
            capture_limit_bytes: payload.capture_limit_bytes,
            total_bytes_seen: payload.total_bytes_seen,
            completion: payload.completion,
            segments: payload
                .segments
                .iter()
                .map(|segment| HeaderSegment {
                    source_window: segment.source_window,
                    bytes: segment.text.len(),
                })
                .collect(),
            withheld_reason: payload.withheld_reason,
            sha256,
        }
    }
    fn bytes(&self) -> Result<usize, BoundaryError> {
        self.segments
            .iter()
            .try_fold(0usize, |sum, segment| sum.checked_add(segment.bytes))
            .ok_or_else(|| error("output_corrupt", "Output segment sizes overflow"))
    }
    fn validate(&self, id: &str, limits: OutputLimits) -> Result<(), BoundaryError> {
        if self.version != 1
            || self.id != id
            || !valid_id(&self.id)
            || !valid_digest(&self.sha256)
            || self.capture_limit_bytes > limits.max_object_bytes
            || self.segments.len() > 2
            || self.bytes()? > self.capture_limit_bytes
        {
            return Err(error("output_corrupt", "Output header is invalid"));
        }
        let mut end = 0;
        for segment in &self.segments {
            if segment.source_window.start < end
                || segment.source_window.start >= segment.source_window.end
                || segment.source_window.end > self.total_bytes_seen
            {
                return Err(error("output_corrupt", "Output source ranges are invalid"));
            }
            end = segment.source_window.end;
        }
        Ok(())
    }
}

impl Inner {
    pub(super) fn open(
        path: &Path,
        limits: OutputLimits,
        secrets: Arc<SecretRegistry>,
    ) -> Result<Self, BoundaryError> {
        match DirBuilder::new().mode(0o700).create(path) {
            Ok(()) => {}
            Err(failure) if failure.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(_) => return Err(error("output_io", "Output directory cannot be created")),
        }
        let file = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(path)
            .map_err(|_| {
                error(
                    "output_permissions",
                    "Output directory is unavailable or linked",
                )
            })?;
        let metadata = file
            .metadata()
            .map_err(|_| error("output_io", "Output directory metadata is unavailable"))?;
        // SAFETY: geteuid has no pointer arguments or ownership requirements.
        if !metadata.is_dir()
            || metadata.uid() != unsafe { libc::geteuid() }
            || metadata.mode() & 0o077 != 0
        {
            return Err(error(
                "output_permissions",
                "Output directory must be owned and private",
            ));
        }
        let canonical = std::fs::canonicalize(path)
            .map_err(|_| error("output_io", "Output directory cannot be resolved"))?;
        let named = std::fs::metadata(&canonical)
            .map_err(|_| error("output_io", "Output directory identity is unavailable"))?;
        if metadata.dev() != named.dev() || metadata.ino() != named.ino() {
            return Err(error(
                "output_permissions",
                "Output directory changed while opening",
            ));
        }
        let inner = Self {
            dir: Dir::from_std_file(file),
            path: canonical,
            limits,
            secrets,
        };
        let _lock = inner.directory_lock()?;
        inner.repair_atomic_links()?;
        Ok(inner)
    }

    fn open_file(&self, name: &str) -> Result<File, BoundaryError> {
        let mut options = OpenOptions::new();
        options
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK);
        let file = self
            .dir
            .open_with(name, &options)
            .map_err(|_| {
                error(
                    "output_unavailable",
                    "Output object is missing or inaccessible",
                )
            })?
            .into_std();
        private_file(&file)?;
        Ok(file)
    }
    fn directory_lock(&self) -> Result<File, BoundaryError> {
        let mut options = OpenOptions::new();
        options
            .read(true)
            .write(true)
            .create(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK);
        let file = self
            .dir
            .open_with(".lock", &options)
            .map_err(|_| error("output_permissions", "Output lock is unavailable"))?
            .into_std();
        private_file(&file)?;
        lock(&file, libc::LOCK_EX)?;
        private_file(&file)?;
        Ok(file)
    }
    fn names(&self, strict: bool) -> Result<Vec<String>, BoundaryError> {
        let mut names = Vec::new();
        for entry in self
            .dir
            .entries()
            .map_err(|_| error("output_io", "Output directory cannot be listed"))?
        {
            let name = entry
                .map_err(|_| error("output_io", "Output entry cannot be read"))?
                .file_name()
                .into_string()
                .map_err(|_| error("output_corrupt", "Output entry name is invalid"))?;
            names.push(name);
            if names.len() > self.limits.max_objects.saturating_mul(4).saturating_add(16) {
                if strict {
                    return Err(error(
                        "output_quota",
                        "Output directory entry limit exceeded",
                    ));
                }
                break;
            }
        }
        names.sort();
        Ok(names)
    }
    fn repair_atomic_links(&self) -> Result<(), BoundaryError> {
        // Installation uses a no-replace hard link, then removes its temporary
        // name. After a crash, accept exactly those two names inside this root;
        // an unknown/external hard link still fails closed.
        let mut linked = std::collections::BTreeMap::<(u64, u64), Vec<(String, bool)>>::new();
        let names = self.names(false)?;
        let partial = names.len() > self.limits.max_objects.saturating_mul(4).saturating_add(16);
        for name in names {
            let temporary = temporary_name(&name);
            if !temporary
                && object_id(&name).is_none()
                && media_id(&name).is_none()
                && pending_id(&name).is_none()
                && published_id(&name).is_none()
            {
                continue;
            }
            let mut options = OpenOptions::new();
            options
                .read(true)
                .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK);
            let file = self
                .dir
                .open_with(&name, &options)
                .map_err(|_| {
                    error(
                        "output_permissions",
                        "Output recovery entry cannot be safely opened",
                    )
                })?
                .into_std();
            let metadata = file
                .metadata()
                .map_err(|_| error("output_io", "Output recovery metadata is unavailable"))?;
            // SAFETY: geteuid has no pointer arguments or ownership requirements.
            if !metadata.is_file()
                || metadata.uid() != unsafe { libc::geteuid() }
                || metadata.mode() & 0o077 != 0
                || !(1..=2).contains(&metadata.nlink())
            {
                return Err(error(
                    "output_permissions",
                    "Output recovery entry has unsafe ownership or links",
                ));
            }
            if metadata.nlink() == 2 {
                linked
                    .entry((metadata.dev(), metadata.ino()))
                    .or_default()
                    .push((name, temporary));
            }
        }
        let changed = !linked.is_empty();
        for names in linked.into_values() {
            if names.len() != 2 || names.iter().filter(|(_, temporary)| *temporary).count() != 1 {
                if partial {
                    continue;
                }
                return Err(error(
                    "output_permissions",
                    "Output object has an external or unrecognized hard link",
                ));
            }
            let temporary = names
                .iter()
                .find(|(_, temporary)| *temporary)
                .map(|(name, _)| name)
                .ok_or_else(|| error("output_permissions", "Output recovery link is invalid"))?;
            self.dir
                .remove_file(temporary)
                .map_err(|_| error("output_io", "Output recovery temporary cleanup failed"))?;
        }
        if changed {
            self.sync_directory()?;
        }
        Ok(())
    }
    fn pending(&self, id: &str) -> Result<Reservation, BoundaryError> {
        let mut file = self.open_file(&pending_name(id))?;
        let bytes = read_bounded(&mut file, MAX_HEADER_BYTES)?;
        let record: Reservation = serde_json::from_slice(&bytes)
            .map_err(|_| error("output_corrupt", "Output reservation is invalid"))?;
        if record.version != 1
            || record.id != id
            || record.capture_limit_bytes > self.limits.max_object_bytes
        {
            return Err(error(
                "output_corrupt",
                "Output reservation identity is invalid",
            ));
        }
        record.scope.validate(&self.secrets.snapshot()?)?;
        Ok(record)
    }
    fn header(&self, file: &mut File, id: &str) -> Result<Header, BoundaryError> {
        let mut prefix = [0u8; 12];
        file.read_exact(&mut prefix)
            .map_err(|_| error("output_corrupt", "Output header is incomplete"))?;
        if &prefix[..8] != MAGIC {
            return Err(error("output_corrupt", "Output format is unsupported"));
        }
        let length = u32::from_be_bytes([prefix[8], prefix[9], prefix[10], prefix[11]]) as usize;
        if length == 0 || length > MAX_HEADER_BYTES {
            return Err(error("output_corrupt", "Output header size is invalid"));
        }
        let mut bytes = vec![0; length];
        file.read_exact(&mut bytes)
            .map_err(|_| error("output_corrupt", "Output header is incomplete"))?;
        let header: Header = serde_json::from_slice(&bytes)
            .map_err(|_| error("output_corrupt", "Output header cannot be decoded"))?;
        header.validate(id, self.limits)?;
        let expected = 12usize
            .checked_add(length)
            .and_then(|size| size.checked_add(header.bytes().ok()?))
            .ok_or_else(|| error("output_corrupt", "Output record size overflows"))?;
        if file
            .metadata()
            .map_err(|_| error("output_io", "Output file metadata is unavailable"))?
            .len()
            != expected as u64
        {
            return Err(error(
                "output_corrupt",
                "Output file length does not match its header",
            ));
        }
        Ok(header)
    }
    fn published(&self, id: &str, digest: &str) -> Result<bool, BoundaryError> {
        let name = published_name(id);
        if !self
            .dir
            .try_exists(&name)
            .map_err(|_| error("output_io", "Output publication state is unavailable"))?
        {
            return Ok(false);
        }
        let mut file = self.open_file(&name)?;
        let bytes = read_bounded(&mut file, 128)?;
        if bytes != digest.as_bytes() {
            return Err(error(
                "output_corrupt",
                "Output publication identity is invalid",
            ));
        }
        Ok(true)
    }

    fn media_header(&self, file: &mut File, id: &str) -> Result<MediaHeader, BoundaryError> {
        let mut prefix = [0u8; 12];
        file.read_exact(&mut prefix)
            .map_err(|_| media_error("media_corrupt", "Media header is incomplete"))?;
        if &prefix[..8] != MEDIA_MAGIC {
            return Err(media_error(
                "media_corrupt",
                "Media object format is unsupported",
            ));
        }
        let length = u32::from_be_bytes([prefix[8], prefix[9], prefix[10], prefix[11]]) as usize;
        if length == 0 || length > MAX_HEADER_BYTES {
            return Err(media_error("media_corrupt", "Media header size is invalid"));
        }
        let mut bytes = vec![0; length];
        file.read_exact(&mut bytes)
            .map_err(|_| media_error("media_corrupt", "Media header is incomplete"))?;
        let header: MediaHeader = serde_json::from_slice(&bytes)
            .map_err(|_| media_error("media_corrupt", "Media header cannot be decoded"))?;
        crate::media::validate_metadata(header.kind, &header.mime, &header.filename, &[0])
            .map_err(|_| media_error("media_corrupt", "Media metadata is invalid"))?;
        if header.version != 1
            || header.id != id
            || media_id(&media_name(id)) != Some(id)
            || !valid_digest(&header.sha256)
            || header.byte_len == 0
            || header.byte_len > crate::media::maximum(header.kind)
        {
            return Err(media_error("media_corrupt", "Media identity is invalid"));
        }
        let expected = 12u64
            .checked_add(length as u64)
            .and_then(|size| size.checked_add(header.byte_len))
            .ok_or_else(|| media_error("media_corrupt", "Media object size overflows"))?;
        if file
            .metadata()
            .map_err(|_| media_error("media_io", "Media metadata is unavailable"))?
            .len()
            != expected
        {
            return Err(media_error(
                "media_corrupt",
                "Media object length does not match its header",
            ));
        }
        Ok(header)
    }

    fn ensure_media_capacity(&self, bytes: usize) -> Result<(), BoundaryError> {
        let names = self.names(true)?;
        let sealed = names
            .iter()
            .filter_map(|name| object_id(name))
            .collect::<BTreeSet<_>>();
        let mut total = 0usize;
        let mut count = 0usize;
        for name in &names {
            let size = if temporary_name(name) {
                self.open_file(name)?
                    .metadata()
                    .map_err(|_| error("output_io", "Output temporary metadata is unavailable"))?
                    .len()
                    .try_into()
                    .map_err(|_| error("output_quota", "Output temporary size exceeds capacity"))?
            } else if let Some(id) = object_id(name) {
                count += 1;
                self.header(&mut self.open_file(name)?, id)?.bytes()?
            } else if let Some(id) = media_id(name) {
                count += 1;
                usize::try_from(self.media_header(&mut self.open_file(name)?, id)?.byte_len)
                    .map_err(|_| error("output_quota", "Media size exceeds capacity"))?
            } else if let Some(id) = pending_id(name) {
                if sealed.contains(id) {
                    continue;
                }
                count += 1;
                self.pending(id)?.capture_limit_bytes
            } else {
                continue;
            };
            total = total
                .checked_add(size)
                .ok_or_else(|| error("output_quota", "Output store usage overflows"))?;
        }
        if count >= self.limits.max_objects
            || total
                .checked_add(bytes)
                .is_none_or(|total| total > self.limits.max_store_bytes)
        {
            return Err(error("output_quota", "Media snapshot exceeds store quota"));
        }
        Ok(())
    }

    pub(super) fn put_media(
        &self,
        session_id: SessionId,
        kind: crate::media::MediaKind,
        mime: &str,
        filename: &str,
        bytes: &[u8],
    ) -> Result<SealedMedia, BoundaryError> {
        let _lock = self.directory_lock()?;
        self.repair_atomic_links()?;
        self.ensure_media_capacity(bytes.len())?;
        let id = format!("media_{}", uuid::Uuid::now_v7().simple());
        let sha256 = crate::media::digest(bytes);
        let header = MediaHeader {
            version: 1,
            id: id.clone(),
            session_id,
            kind,
            mime: mime.into(),
            filename: filename.into(),
            byte_len: bytes.len() as u64,
            sha256: sha256.clone(),
        };
        let header_bytes = serde_json::to_vec(&header)
            .map_err(|_| media_error("media_io", "Media header cannot be encoded"))?;
        ensure_safe(&header_bytes, &self.secrets.snapshot()?)?;
        if header_bytes.len() > MAX_HEADER_BYTES {
            return Err(media_error(
                "media_limit",
                "Media metadata exceeds its limit",
            ));
        }
        let mut record = Vec::with_capacity(12 + header_bytes.len() + bytes.len());
        record.extend_from_slice(MEDIA_MAGIC);
        record.extend_from_slice(&(header_bytes.len() as u32).to_be_bytes());
        record.extend_from_slice(&header_bytes);
        record.extend_from_slice(bytes);
        self.create_atomic(&media_name(&id), &record)?;
        self.create_atomic(&published_name(&id), sha256.as_bytes())?;
        let file = self
            .open_file(&media_name(&id))
            .map_err(|_| media_error("media_unavailable", "Media snapshot cannot be reopened"))?;
        lock(&file, libc::LOCK_SH)?;
        Ok(SealedMedia {
            reference: format!("{id}_{sha256}"),
            sha256,
            size: bytes.len() as u64,
            _lease: OutputLease { _file: file },
        })
    }

    pub(super) fn remove_media(
        &self,
        session_id: SessionId,
        reference: &str,
    ) -> Result<(), BoundaryError> {
        let (id, digest) = crate::media::parse_reference(reference)?;
        let _lock = self.directory_lock()?;
        let mut file = match self.open_file(&media_name(id)) {
            Ok(file) => file,
            Err(failure) if failure.0.code == "output_unavailable" => return Ok(()),
            Err(failure) => return Err(failure),
        };
        private_file(&file)?;
        // SAFETY: flock operates only on this owned regular-file descriptor.
        if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
            let failure = std::io::Error::last_os_error();
            if failure.kind() == std::io::ErrorKind::WouldBlock {
                return Ok(());
            }
            return Err(media_error("media_io", "Media lease cannot be checked"));
        }
        let header = self.media_header(&mut file, id)?;
        if header.sha256 != digest || header.session_id != session_id {
            return Err(media_error(
                "media_denied",
                "Media snapshot cannot be removed by this session",
            ));
        }
        // Keep the exclusive lease through unlink so no new reader can acquire
        // the object between the lock check and its removal.
        self.dir
            .remove_file(media_name(id))
            .map_err(|_| media_error("media_io", "Media snapshot removal failed"))?;
        let marker = published_name(id);
        if self
            .dir
            .try_exists(&marker)
            .map_err(|_| media_error("media_io", "Media publication state is unavailable"))?
        {
            let _marker = self.open_file(&marker)?;
            self.dir
                .remove_file(&marker)
                .map_err(|_| media_error("media_io", "Media publication cleanup failed"))?;
        }
        self.sync_directory()
    }

    pub(super) fn load_media(
        &self,
        session_id: SessionId,
        reference: &str,
    ) -> Result<(crate::media::LoadedMedia, OutputLease), BoundaryError> {
        let (id, digest) = crate::media::parse_reference(reference)?;
        let mut file = self.open_file(&media_name(id)).map_err(|_| {
            media_error(
                "media_unavailable",
                "Media snapshot is missing or inaccessible",
            )
        })?;
        lock(&file, libc::LOCK_SH)?;
        private_file(&file)?;
        let header = self.media_header(&mut file, id)?;
        if header.sha256 != digest {
            return Err(media_error(
                "media_corrupt",
                "Media reference digest does not match",
            ));
        }
        if header.session_id != session_id {
            return Err(media_error(
                "media_denied",
                "Media snapshot belongs to another session",
            ));
        }
        if !self.published(id, digest)? {
            return Err(media_error(
                "media_unavailable",
                "Media snapshot has not been durably published",
            ));
        }
        let size = usize::try_from(header.byte_len)
            .map_err(|_| media_error("media_corrupt", "Media size exceeds host capacity"))?;
        let mut bytes = vec![0; size];
        for chunk in bytes.chunks_mut(64 * 1024) {
            file.read_exact(chunk)
                .map_err(|_| media_error("media_corrupt", "Media content is incomplete"))?;
        }
        if crate::media::digest(&bytes) != digest {
            return Err(media_error(
                "media_corrupt",
                "Media content integrity check failed",
            ));
        }
        Ok((
            crate::media::LoadedMedia {
                kind: header.kind,
                mime: header.mime,
                filename: header.filename,
                data: base64::Engine::encode(&base64::engine::general_purpose::STANDARD, &bytes),
                sha256: digest.into(),
                reference: Some(reference.into()),
                size: header.byte_len,
            },
            OutputLease { _file: file },
        ))
    }

    pub(super) fn reserve(
        &self,
        scope: OutputScope,
        requested: usize,
    ) -> Result<(Reservation, OutputLease), BoundaryError> {
        scope.validate(&self.secrets.snapshot()?)?;
        let _lock = self.directory_lock()?;
        self.repair_atomic_links()?;
        let names = self.names(true)?;
        let mut total = 0usize;
        let mut run = 0usize;
        let mut count = 0usize;
        let sealed = names
            .iter()
            .filter_map(|name| object_id(name))
            .collect::<BTreeSet<_>>();
        for name in &names {
            if temporary_name(name) {
                let size = self
                    .open_file(name)?
                    .metadata()
                    .map_err(|_| error("output_io", "Output temporary metadata is unavailable"))?
                    .len();
                let size = usize::try_from(size)
                    .map_err(|_| error("output_quota", "Output temporary size exceeds capacity"))?;
                total = total
                    .checked_add(size)
                    .ok_or_else(|| error("output_quota", "Output store usage overflows"))?;
                continue;
            }
            if let Some(id) = media_id(name) {
                let bytes = self.media_header(&mut self.open_file(name)?, id)?.byte_len;
                let bytes = usize::try_from(bytes)
                    .map_err(|_| error("output_quota", "Media size exceeds capacity"))?;
                count += 1;
                total = total
                    .checked_add(bytes)
                    .ok_or_else(|| error("output_quota", "Output store usage overflows"))?;
                continue;
            }
            let (record_scope, bytes) = if let Some(id) = object_id(name) {
                let header = self.header(&mut self.open_file(name)?, id)?;
                (header.scope.clone(), header.bytes()?)
            } else if let Some(id) = pending_id(name) {
                if sealed.contains(id) {
                    continue;
                }
                let pending = self.pending(id)?;
                (pending.scope, pending.capture_limit_bytes)
            } else {
                continue;
            };
            count += 1;
            total = total
                .checked_add(bytes)
                .ok_or_else(|| error("output_quota", "Output store usage overflows"))?;
            if record_scope.session_id == scope.session_id && record_scope.run_id == scope.run_id {
                run = run
                    .checked_add(bytes)
                    .ok_or_else(|| error("output_quota", "Run output usage overflows"))?;
            }
        }
        if count >= self.limits.max_objects {
            return Err(error("output_quota", "Output object limit exceeded"));
        }
        let cap = requested
            .min(self.limits.max_object_bytes)
            .min(self.limits.max_run_bytes.saturating_sub(run))
            .min(self.limits.max_store_bytes.saturating_sub(total));
        let record = Reservation {
            version: 1,
            id: format!("out_{}", uuid::Uuid::now_v7().simple()),
            scope,
            capture_limit_bytes: cap,
        };
        let bytes = serde_json::to_vec(&record)
            .map_err(|_| error("output_io", "Output reservation cannot be encoded"))?;
        ensure_safe(&bytes, &self.secrets.snapshot()?)?;
        self.create_atomic(&pending_name(&record.id), &bytes)?;
        let file = self.open_file(&pending_name(&record.id))?;
        lock(&file, libc::LOCK_SH)?;
        Ok((record, OutputLease { _file: file }))
    }

    pub(super) fn seal(
        &self,
        reservation: &Reservation,
        payload: Payload,
    ) -> Result<SealedOutput, BoundaryError> {
        let _lock = self.directory_lock()?;
        let pending = self.pending(&reservation.id)?;
        if pending.scope != reservation.scope
            || pending.capture_limit_bytes != reservation.capture_limit_bytes
        {
            return Err(error(
                "output_corrupt",
                "Output reservation changed before sealing",
            ));
        }
        let digest = payload.digest()?;
        let header = Header::from_payload(&payload, digest.clone());
        header.validate(&reservation.id, self.limits)?;
        let header_bytes = serde_json::to_vec(&header)
            .map_err(|_| error("output_io", "Output header cannot be encoded"))?;
        ensure_safe(&header_bytes, &self.secrets.snapshot()?)?;
        if header_bytes.len() > MAX_HEADER_BYTES {
            return Err(error("output_limit", "Output metadata exceeds its limit"));
        }
        let mut bytes = Vec::with_capacity(12 + header_bytes.len() + header.bytes()?);
        bytes.extend_from_slice(MAGIC);
        bytes.extend_from_slice(&(header_bytes.len() as u32).to_be_bytes());
        bytes.extend_from_slice(&header_bytes);
        for segment in &payload.segments {
            bytes.extend_from_slice(segment.text.as_bytes());
        }
        self.create_atomic(&object_name(&reservation.id), &bytes)?;
        let file = self.open_file(&object_name(&reservation.id))?;
        lock(&file, libc::LOCK_SH)?;
        self.dir
            .remove_file(pending_name(&reservation.id))
            .map_err(|_| error("output_io", "Output reservation cleanup failed"))?;
        self.sync_directory()?;
        Ok(SealedOutput {
            metadata: payload.metadata(&digest, self.limits.preview_bytes),
            _lease: OutputLease { _file: file },
        })
    }

    /// Returns a lease with the verified payload. The exact original digest is
    /// kept when newly registered credentials change only its returned text.
    pub(super) fn load(
        &self,
        session: SessionId,
        reference: &str,
        require_published: bool,
    ) -> Result<(Payload, String, OutputLease), BoundaryError> {
        self.load_cancellable(
            session,
            reference,
            require_published,
            &AtomicBool::new(false),
        )
    }
    pub(super) fn load_cancellable(
        &self,
        session: SessionId,
        reference: &str,
        require_published: bool,
        cancel: &AtomicBool,
    ) -> Result<(Payload, String, OutputLease), BoundaryError> {
        check_cancelled(cancel)?;
        let (id, digest) = parse_reference(reference)?;
        let mut file = self.open_file(&object_name(id))?;
        lock_cancellable(&file, libc::LOCK_SH, cancel)?;
        check_cancelled(cancel)?;
        private_file(&file)?;
        let header = self.header(&mut file, id)?;
        check_cancelled(cancel)?;
        if header.sha256 != digest {
            return Err(error(
                "output_corrupt",
                "Output reference digest does not match",
            ));
        }
        if header.scope.session_id != session {
            return Err(error(
                "output_denied",
                "Output reference belongs to another session",
            ));
        }
        if require_published && !self.published(id, digest)? {
            return Err(error(
                "output_unavailable",
                "Output object has not been durably published",
            ));
        }
        let mut segments = Vec::with_capacity(header.segments.len());
        for segment in &header.segments {
            check_cancelled(cancel)?;
            let mut bytes = vec![0; segment.bytes];
            for chunk in bytes.chunks_mut(64 * 1024) {
                check_cancelled(cancel)?;
                file.read_exact(chunk)
                    .map_err(|_| error("output_corrupt", "Output content is incomplete"))?;
            }
            let text = String::from_utf8(bytes)
                .map_err(|_| error("output_corrupt", "Output content is not UTF-8"))?;
            segments.push(Segment {
                source_window: segment.source_window,
                text,
            });
        }
        let mut payload = Payload {
            version: header.version,
            id: header.id,
            scope: header.scope,
            capture_limit_bytes: header.capture_limit_bytes,
            total_bytes_seen: header.total_bytes_seen,
            completion: header.completion,
            segments,
            withheld_reason: header.withheld_reason,
        };
        if payload.digest_cancellable(cancel)? != digest {
            return Err(error(
                "output_corrupt",
                "Output content integrity check failed",
            ));
        }
        let secrets = self.secrets.snapshot()?;
        payload.scope.validate(&secrets)?;
        let mut sanitized = Vec::new();
        for mut segment in payload.segments {
            check_cancelled(cancel)?;
            let mut value = serde_json::Value::String(segment.text);
            if crate::redaction::redact(&mut value, &secrets).is_err() {
                payload.withheld_reason = Some(WithheldReason::RedactionFailed);
                continue;
            }
            let serde_json::Value::String(text) = value else {
                return Err(error("output_corrupt", "Output text projection failed"));
            };
            segment.text = text;
            sanitized.push(segment);
        }
        payload.segments = sanitized;
        check_cancelled(cancel)?;
        let metadata = serde_json::to_vec(&payload.metadata(digest, self.limits.preview_bytes))
            .map_err(|_| error("output_corrupt", "Output metadata cannot be projected"))?;
        ensure_safe(&metadata, &secrets)?;
        Ok((payload, digest.into(), OutputLease { _file: file }))
    }
    pub(super) fn publish(&self, session: SessionId, reference: &str) -> Result<(), BoundaryError> {
        let _lock = self.directory_lock()?;
        let (_, digest, _lease) = self.load(session, reference, false)?;
        let (id, _) = parse_reference(reference)?;
        if self.published(id, &digest)? {
            return Ok(());
        }
        ensure_safe(digest.as_bytes(), &self.secrets.snapshot()?)?;
        self.create_atomic(&published_name(id), digest.as_bytes())
    }

    pub(super) fn inventory(&self) -> Result<Vec<OutputInventory>, BoundaryError> {
        let _lock = self.directory_lock()?;
        self.repair_atomic_links()?;
        let names = self.names(true)?;
        let sealed = names
            .iter()
            .filter_map(|name| object_id(name))
            .collect::<BTreeSet<_>>();
        let mut records = Vec::new();
        for name in &names {
            if let Some(id) = object_id(name) {
                let header = self.header(&mut self.open_file(name)?, id)?;
                header.scope.validate(&self.secrets.snapshot()?)?;
                let published = self.published(id, &header.sha256)?;
                records.push(OutputInventory {
                    reservation_id: id.into(),
                    scope: header.scope,
                    state: if published {
                        OutputObjectState::Published
                    } else {
                        OutputObjectState::Sealed
                    },
                    output_ref: Some(format!("{id}_{}", header.sha256)),
                });
            } else if let Some(id) = pending_id(name)
                && !sealed.contains(id)
            {
                let pending = self.pending(id)?;
                records.push(OutputInventory {
                    reservation_id: id.into(),
                    scope: pending.scope,
                    state: OutputObjectState::Pending,
                    output_ref: None,
                });
            }
        }
        if records.len() > self.limits.max_objects {
            return Err(error("output_quota", "Output object limit exceeded"));
        }
        records.sort_by(|left, right| left.reservation_id.cmp(&right.reservation_id));
        Ok(records)
    }
    pub(super) fn collect_garbage(
        &self,
        pinned_refs: &BTreeSet<String>,
        max_remove: usize,
    ) -> Result<GarbageCollection, BoundaryError> {
        if max_remove > self.limits.max_objects || pinned_refs.len() > self.limits.max_objects {
            return Err(error("output_limit", "Output GC limits are invalid"));
        }
        if max_remove == 0 {
            return Ok(GarbageCollection::default());
        }
        let pinned_ids = pinned_refs
            .iter()
            .map(|reference| {
                if let Ok((id, _)) = crate::media::parse_reference(reference) {
                    Ok(id.to_owned())
                } else {
                    parse_reference(reference)
                        .map(|(id, _)| id.to_owned())
                        .map_err(|_| error("output_reference", "Output reference is invalid"))
                }
            })
            .collect::<Result<BTreeSet<_>, _>>()?;
        let _lock = self.directory_lock()?;
        self.repair_atomic_links()?;
        let mut report = GarbageCollection::default();
        let mut candidates = Vec::new();
        for name in self.names(false)? {
            let priority =
                if let Some(id) = object_id(&name).or_else(|| media_id(&name)) {
                    if self.dir.try_exists(published_name(id)).map_err(|_| {
                        error("output_io", "Output publication state is unavailable")
                    })? {
                        2
                    } else {
                        1
                    }
                } else if pending_id(&name).is_some()
                    || published_id(&name).is_some()
                    || temporary_name(&name)
                {
                    0
                } else {
                    continue;
                };
            candidates.push((priority, name));
        }
        // UUIDv7 identifiers sort by creation time. Compare the identifier,
        // not the full file name, so media_* and out_* share one reserve order.
        candidates.sort_by(|(left_priority, left), (right_priority, right)| {
            left_priority
                .cmp(right_priority)
                .then_with(|| order_key(left).cmp(order_key(right)))
                .then_with(|| left.cmp(right))
        });
        for (_, name) in candidates {
            let id = object_id(&name)
                .or_else(|| media_id(&name))
                .or_else(|| pending_id(&name))
                .or_else(|| published_id(&name));
            if id.is_none() && !temporary_name(&name) {
                continue;
            }
            if let Some(marker_id) = published_id(&name)
                && (self
                    .dir
                    .try_exists(object_name(marker_id))
                    .map_err(|_| error("output_io", "Output publication target is unavailable"))?
                    || self.dir.try_exists(media_name(marker_id)).map_err(|_| {
                        error("output_io", "Media publication target is unavailable")
                    })?)
            {
                continue;
            }
            if id.is_some_and(|id| pinned_ids.contains(id)) {
                report.pinned += 1;
                continue;
            }
            if report.removed == max_remove {
                break;
            }
            let file = match self.open_file(&name) {
                Ok(file) => file,
                Err(failure)
                    if failure.0.code == "output_permissions"
                        || failure.0.code == "output_unavailable" =>
                {
                    report.deferred += 1;
                    continue;
                }
                Err(failure) => return Err(failure),
            };
            // SAFETY: flock operates only on this owned regular-file descriptor.
            if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
                let failure = std::io::Error::last_os_error();
                if failure.kind() == std::io::ErrorKind::WouldBlock {
                    report.leased += 1;
                    continue;
                }
                return Err(error("output_io", "Output object lease cannot be checked"));
            }
            private_file(&file)?;
            self.dir
                .remove_file(&name)
                .map_err(|_| error("output_io", "Output GC removal failed"))?;
            if let Some(id) = object_id(&name).or_else(|| media_id(&name)) {
                let marker = published_name(id);
                if self
                    .dir
                    .try_exists(&marker)
                    .map_err(|_| error("output_io", "Output publication state is unavailable"))?
                {
                    let _marker = self.open_file(&marker)?;
                    self.dir
                        .remove_file(&marker)
                        .map_err(|_| error("output_io", "Output publication cleanup failed"))?;
                }
            }
            report.removed += 1;
        }
        self.sync_directory()?;
        Ok(report)
    }

    fn create_atomic(&self, destination: &str, bytes: &[u8]) -> Result<(), BoundaryError> {
        let temporary = format!(".tmp-{}", uuid::Uuid::now_v7().simple());
        let mut options = OpenOptions::new();
        options
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
        let outcome = (|| {
            let mut file = self.dir.open_with(&temporary, &options).map_err(|_| {
                error(
                    "output_io",
                    "Private output temporary object cannot be created",
                )
            })?;
            private_file(
                &file
                    .try_clone()
                    .map_err(|_| error("output_io", "Output file cannot be inspected"))?
                    .into_std(),
            )?;
            file.write_all(bytes)
                .and_then(|()| file.sync_all())
                .map_err(|_| error("output_io", "Output object could not be sealed"))?;
            self.dir
                .hard_link(&temporary, &self.dir, destination)
                .map_err(|_| {
                    error(
                        "output_io",
                        "Output object could not be atomically installed",
                    )
                })?;
            self.dir
                .remove_file(&temporary)
                .map_err(|_| error("output_io", "Output temporary cleanup failed"))?;
            self.sync_directory()
        })();
        if outcome.is_err() {
            let _ = self.dir.remove_file(&temporary);
        }
        outcome
    }
    fn sync_directory(&self) -> Result<(), BoundaryError> {
        self.dir
            .try_clone()
            .map_err(|_| error("output_io", "Output directory cannot be cloned"))?
            .into_std_file()
            .sync_all()
            .map_err(|_| {
                error(
                    "output_io",
                    "Output directory durability could not be confirmed",
                )
            })
    }
}

fn private_file(file: &File) -> Result<(), BoundaryError> {
    let metadata = file
        .metadata()
        .map_err(|_| error("output_io", "Output file metadata is unavailable"))?;
    // SAFETY: geteuid has no pointer arguments or ownership requirements.
    if !metadata.is_file()
        || metadata.uid() != unsafe { libc::geteuid() }
        || metadata.mode() & 0o077 != 0
        || metadata.nlink() != 1
    {
        return Err(error(
            "output_permissions",
            "Output objects must be private owned regular files without links",
        ));
    }
    Ok(())
}
fn lock(file: &File, mode: i32) -> Result<(), BoundaryError> {
    lock_cancellable(file, mode, &AtomicBool::new(false))
}
fn lock_cancellable(file: &File, mode: i32, cancel: &AtomicBool) -> Result<(), BoundaryError> {
    let deadline = Instant::now() + Duration::from_millis(200);
    loop {
        check_cancelled(cancel)?;
        // SAFETY: flock operates only on this owned regular-file descriptor.
        if unsafe { libc::flock(file.as_raw_fd(), mode | libc::LOCK_NB) } == 0 {
            return Ok(());
        }
        let failure = std::io::Error::last_os_error();
        if !matches!(
            failure.kind(),
            std::io::ErrorKind::WouldBlock | std::io::ErrorKind::Interrupted
        ) {
            return Err(error("output_io", "Output file lock failed"));
        }
        if Instant::now() >= deadline {
            return Err(error("output_busy", "Output file lock timed out"));
        }
        std::thread::sleep(Duration::from_millis(2));
    }
}
fn read_bounded(file: &mut File, limit: usize) -> Result<Vec<u8>, BoundaryError> {
    let mut bytes = Vec::new();
    file.take(limit as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| error("output_io", "Output metadata cannot be read"))?;
    if bytes.len() > limit {
        return Err(error("output_corrupt", "Output metadata exceeds its limit"));
    }
    Ok(bytes)
}
fn ensure_safe(bytes: &[u8], secrets: &[String]) -> Result<(), BoundaryError> {
    let text = std::str::from_utf8(bytes)
        .map_err(|_| error("output_corrupt", "Output metadata is not UTF-8"))?;
    if crate::redaction::contains_secret(text, secrets) {
        return Err(error(
            "sensitive_data",
            "Output metadata contains protected data",
        ));
    }
    Ok(())
}
fn object_name(id: &str) -> String {
    format!("{id}.out")
}
fn media_name(id: &str) -> String {
    format!("{id}.media")
}
fn pending_name(id: &str) -> String {
    format!(".pending-{id}.json")
}
fn published_name(id: &str) -> String {
    format!(".published-{id}")
}
fn object_id(name: &str) -> Option<&str> {
    name.strip_suffix(".out").filter(|id| valid_id(id))
}
fn media_id(name: &str) -> Option<&str> {
    let id = name.strip_suffix(".media")?;
    crate::media::valid_media_id(id).then_some(id)
}
fn pending_id(name: &str) -> Option<&str> {
    name.strip_prefix(".pending-")?
        .strip_suffix(".json")
        .filter(|id| valid_id(id))
}
fn published_id(name: &str) -> Option<&str> {
    name.strip_prefix(".published-")
        .filter(|id| valid_id(id) || media_id(&media_name(id)) == Some(*id))
}
fn temporary_name(name: &str) -> bool {
    name.strip_prefix(".tmp-")
        .is_some_and(|id| id.len() == 32 && id.bytes().all(|byte| byte.is_ascii_hexdigit()))
}
fn order_key(name: &str) -> &str {
    let id = object_id(name)
        .or_else(|| media_id(name))
        .or_else(|| pending_id(name))
        .or_else(|| published_id(name));
    id.map_or(name, |id| {
        id.strip_prefix("media_")
            .or_else(|| id.strip_prefix("out_"))
            .unwrap_or(id)
    })
}
fn valid_id(id: &str) -> bool {
    id.strip_prefix("out_").is_some_and(|id| {
        id.len() == 32
            && id
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    })
}
fn valid_digest(digest: &str) -> bool {
    digest.len() == 64
        && digest
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}
fn parse_reference(reference: &str) -> Result<(&str, &str), BoundaryError> {
    let Some((id, digest)) = reference.rsplit_once('_') else {
        return Err(error("output_reference", "Output reference is invalid"));
    };
    if !valid_id(id) || !valid_digest(digest) {
        return Err(error("output_reference", "Output reference is invalid"));
    }
    Ok((id, digest))
}
