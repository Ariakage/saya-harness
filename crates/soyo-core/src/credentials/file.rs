use super::{
    CredentialKind, CredentialMetadata, CredentialRef, CredentialScope, CredentialValue,
    McpCredentialStore, ResolvedCredential, SecretRegistry, SecretValue, error,
};
use crate::contract::BoundaryError;
use cap_std::fs::{Dir, OpenOptions, OpenOptionsExt};
use rmcp::transport::auth::StoredCredentials;
use serde::{Deserialize, Serialize};
use std::fs::{DirBuilder, File};
use std::io::{Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt as _};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};
use uuid::Uuid;

const MAX_RECORD_BYTES: usize = 1024 * 1024;
const MAX_RECORDS: usize = 256;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Record {
    version: u8,
    pub metadata: CredentialMetadata,
    material: Option<Material>,
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum Material {
    Secret { value: String },
    OAuth { credentials: Box<StoredCredentials> },
}

impl Material {
    fn from_public(value: CredentialValue) -> Self {
        match value {
            CredentialValue::Secret(value) => Self::Secret {
                value: value.expose().into(),
            },
            CredentialValue::OAuth(credentials) => Self::OAuth { credentials },
        }
    }
    fn into_public(self) -> Result<CredentialValue, BoundaryError> {
        match self {
            Self::Secret { value } => SecretValue::new(value).map(CredentialValue::Secret),
            Self::OAuth { credentials } => Ok(CredentialValue::OAuth(credentials)),
        }
    }
}

pub(super) struct Inner {
    pub dir: Dir,
    pub secrets: Arc<SecretRegistry>,
    path: PathBuf,
    #[cfg(test)]
    fail_directory_sync: std::sync::atomic::AtomicBool,
}

/// Plaintext files protected by private directory ownership, private modes and atomic replacement.
/// Async operations move blocking file and lock work off the runtime worker.
#[derive(Clone)]
pub struct FileCredentialStore {
    pub(super) inner: Arc<Inner>,
}

impl std::fmt::Debug for FileCredentialStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("FileCredentialStore([PRIVATE])")
    }
}

impl FileCredentialStore {
    /// Open an owned private directory. Existing permissive directories and symlinks are refused.
    pub fn open(
        path: impl AsRef<Path>,
        secrets: Arc<SecretRegistry>,
    ) -> Result<Self, BoundaryError> {
        let path = path.as_ref();
        match DirBuilder::new().mode(0o700).create(path) {
            Ok(()) => {}
            Err(error_value) if error_value.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(_) => {
                return Err(error(
                    "credential_io",
                    "Credential directory could not be created",
                ));
            }
        }
        let file = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(path)
            .map_err(|_| {
                error(
                    "credential_permissions",
                    "Credential root is unavailable or is a symlink",
                )
            })?;
        let metadata = file.metadata().map_err(|_| {
            error(
                "credential_io",
                "Credential directory metadata is unavailable",
            )
        })?;
        if !metadata.is_dir()
            || metadata.uid() != unsafe { libc::geteuid() }
            || metadata.mode() & 0o077 != 0
        {
            return Err(error(
                "credential_permissions",
                "Credential directory must be owned by this user with mode 0700",
            ));
        }
        let canonical = std::fs::canonicalize(path).map_err(|_| {
            error(
                "credential_io",
                "Credential directory path cannot be resolved",
            )
        })?;
        let named = std::fs::metadata(&canonical).map_err(|_| {
            error(
                "credential_io",
                "Credential directory path metadata is unavailable",
            )
        })?;
        if named.dev() != metadata.dev() || named.ino() != metadata.ino() {
            return Err(error(
                "credential_permissions",
                "Credential directory changed while it was opened",
            ));
        }
        Ok(Self {
            inner: Arc::new(Inner {
                dir: Dir::from_std_file(file),
                secrets,
                path: canonical,
                #[cfg(test)]
                fail_directory_sync: std::sync::atomic::AtomicBool::new(false),
            }),
        })
    }

    /// Root path for the host's sandbox-protected path set. It contains no credential values.
    pub fn protected_path(&self) -> &Path {
        &self.inner.path
    }
    pub fn secret_registry(&self) -> Arc<SecretRegistry> {
        self.inner.secrets.clone()
    }

    pub(super) async fn run<T, F>(&self, operation: F) -> Result<T, BoundaryError>
    where
        T: Send + 'static,
        F: FnOnce(Arc<Inner>) -> Result<T, BoundaryError> + Send + 'static,
    {
        let inner = self.inner.clone();
        tokio::task::spawn_blocking(move || operation(inner))
            .await
            .map_err(|_| error("credential_io", "Credential worker did not complete"))?
    }

    /// Create a fresh opaque reference. An existing or revoked reference is never reused.
    pub async fn create(
        &self,
        scope: CredentialScope,
        value: CredentialValue,
    ) -> Result<CredentialRef, BoundaryError> {
        self.create_record(scope, value, None).await
    }
    /// Save a completed OAuth login with its discovered canonical resource.
    /// Its reference remains scoped to the exact configured transport endpoint.
    pub async fn create_oauth(
        &self,
        scope: CredentialScope,
        credentials: StoredCredentials,
        resource: String,
    ) -> Result<CredentialRef, BoundaryError> {
        let resource = scope.validate_oauth_resource(&resource)?;
        self.create_record(scope, CredentialValue::oauth(credentials), Some(resource))
            .await
    }
    async fn create_record(
        &self,
        scope: CredentialScope,
        value: CredentialValue,
        resource: Option<String>,
    ) -> Result<CredentialRef, BoundaryError> {
        scope.validate()?;
        value.validate_scope(&scope)?;
        self.inner.secrets.register(&value.secrets()?)?;
        self.inner.check_metadata_secrets(&scope)?;
        self.run(move |inner| {
            let _index = FileLock::acquire(&inner.dir, "index.lock")?;
            if inner.references()?.len() >= MAX_RECORDS {
                return Err(error(
                    "credential_limit",
                    "Credential record capacity has been reached",
                ));
            }
            let reference = CredentialRef(Uuid::now_v7());
            let kind = match &value {
                CredentialValue::Secret(_) => CredentialKind::Secret,
                CredentialValue::OAuth(_) => CredentialKind::OAuth,
            };
            let oauth_resource = if kind == CredentialKind::OAuth {
                Some(resource.unwrap_or_else(|| scope.audience.clone()))
            } else {
                None
            };
            let record = Record {
                version: 1,
                metadata: CredentialMetadata {
                    reference,
                    scope,
                    kind,
                    revision: 1,
                    revoked: false,
                    oauth_resource,
                },
                material: Some(Material::from_public(value)),
            };
            inner.check_record_metadata(&record.metadata)?;
            inner.write_record(&record)?;
            Ok(reference)
        })
        .await
    }

    pub async fn resolve(
        &self,
        reference: CredentialRef,
        scope: &CredentialScope,
    ) -> Result<ResolvedCredential, BoundaryError> {
        scope.validate()?;
        let scope = scope.clone();
        self.run(move |inner| {
            let _lock = inner.data_lock(reference)?;
            let record = inner.read_record(reference)?;
            check_scope(&record, &scope)?;
            if record.metadata.revoked {
                return Err(error(
                    "credential_revoked",
                    "Credential reference has been revoked",
                ));
            }
            let value = record
                .material
                .ok_or_else(|| error("credential_corrupt", "Active credential has no material"))?
                .into_public()?;
            inner.secrets.register(&value.secrets()?)?;
            Ok(ResolvedCredential {
                metadata: record.metadata,
                value,
            })
        })
        .await
    }

    /// Permanently revoke one reference. Old SDK refreshes cannot recreate its material.
    pub async fn revoke(
        &self,
        reference: CredentialRef,
        scope: &CredentialScope,
    ) -> Result<(), BoundaryError> {
        scope.validate()?;
        let scope = scope.clone();
        self.run(move |inner| {
            let _refresh = inner.refresh_lock(reference)?;
            let _data = inner.data_lock(reference)?;
            let mut record = inner.read_record(reference)?;
            check_scope(&record, &scope)?;
            if let Some(material) = record.material.take() {
                inner
                    .secrets
                    .register(&material.into_public()?.secrets()?)?;
            }
            if !record.metadata.revoked {
                record.metadata.revoked = true;
                record.metadata.revision = next_revision(record.metadata.revision)?;
            }
            inner.write_record(&record)?;
            Ok(())
        })
        .await
    }

    /// List only identities and scope metadata, including revocation tombstones.
    pub async fn list(&self) -> Result<Vec<CredentialMetadata>, BoundaryError> {
        self.run(|inner| {
            let _index = FileLock::acquire(&inner.dir, "index.lock")?;
            let mut result = Vec::new();
            for reference in inner.references()? {
                let _lock = inner.data_lock(reference)?;
                result.push(inner.read_record(reference)?.metadata);
            }
            // Later records can reveal values that also occur in earlier metadata.
            for metadata in &result {
                inner.check_record_metadata(metadata)?;
            }
            Ok(result)
        })
        .await
    }

    /// Bind the MCP SDK store to exactly one active OAuth reference and resource scope.
    pub fn oauth(
        &self,
        reference: CredentialRef,
        scope: CredentialScope,
    ) -> Result<McpCredentialStore, BoundaryError> {
        scope.validate()?;
        self.inner.check_metadata_secrets(&scope)?;
        Ok(McpCredentialStore {
            store: self.clone(),
            reference,
            scope,
        })
    }
}

impl Inner {
    fn references(&self) -> Result<Vec<CredentialRef>, BoundaryError> {
        let mut references = Vec::new();
        for entry in self
            .dir
            .entries()
            .map_err(|_| error("credential_io", "Credential directory cannot be listed"))?
        {
            let name = entry
                .map_err(|_| error("credential_io", "Credential directory entry is unavailable"))?
                .file_name();
            let Some(name) = name.to_str().and_then(|name| name.strip_suffix(".json")) else {
                continue;
            };
            let reference = name
                .parse::<CredentialRef>()
                .map_err(|_| error("credential_corrupt", "Credential filename is invalid"))?;
            references.push(reference);
            if references.len() > MAX_RECORDS {
                return Err(error(
                    "credential_limit",
                    "Credential directory exceeds its record capacity",
                ));
            }
        }
        references.sort();
        Ok(references)
    }

    pub(super) fn data_lock(&self, reference: CredentialRef) -> Result<FileLock, BoundaryError> {
        FileLock::acquire(&self.dir, &format!("{reference}.lock"))
    }
    pub(super) fn refresh_lock(&self, reference: CredentialRef) -> Result<FileLock, BoundaryError> {
        FileLock::acquire(&self.dir, &format!("{reference}.refresh.lock"))
    }

    pub(super) fn read_record(&self, reference: CredentialRef) -> Result<Record, BoundaryError> {
        let mut options = OpenOptions::new();
        options
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
        let mut file = self
            .dir
            .open_with(format!("{reference}.json"), &options)
            .map_err(|error_value| {
                if error_value.kind() == std::io::ErrorKind::NotFound {
                    error("credential_not_found", "Credential reference was not found")
                } else {
                    error(
                        "credential_permissions",
                        "Credential file is unavailable or is a symlink",
                    )
                }
            })?;
        private_file(&file)?;
        if file
            .metadata()
            .map_err(|_| error("credential_io", "Credential file metadata is unavailable"))?
            .len()
            > MAX_RECORD_BYTES as u64
        {
            return Err(error(
                "credential_corrupt",
                "Credential record exceeds its size limit",
            ));
        }
        let mut bytes = Vec::new();
        Read::by_ref(&mut file)
            .take((MAX_RECORD_BYTES + 1) as u64)
            .read_to_end(&mut bytes)
            .map_err(|_| error("credential_io", "Credential record could not be read"))?;
        if bytes.len() > MAX_RECORD_BYTES {
            return Err(error(
                "credential_corrupt",
                "Credential record exceeds its size limit",
            ));
        }
        let record: Record = serde_json::from_slice(&bytes)
            .map_err(|_| error("credential_corrupt", "Credential record is malformed"))?;
        record.metadata.scope.validate()?;
        if record.version != 1
            || record.metadata.reference != reference
            || record.metadata.revision == 0
            || record.metadata.revoked != record.material.is_none()
        {
            return Err(error(
                "credential_corrupt",
                "Credential record identity or state is invalid",
            ));
        }
        match (&record.metadata.kind, &record.material) {
            (_, None)
            | (CredentialKind::Secret, Some(Material::Secret { .. }))
            | (CredentialKind::OAuth, Some(Material::OAuth { .. })) => {}
            _ => {
                return Err(error(
                    "credential_corrupt",
                    "Credential material and metadata kinds do not match",
                ));
            }
        }
        if let Some(material) = &record.material {
            let values = match material {
                Material::Secret { value } => {
                    CredentialValue::Secret(SecretValue::new(value.clone())?).secrets()?
                }
                Material::OAuth { credentials } => {
                    let value = CredentialValue::OAuth(credentials.clone());
                    value.validate_scope(&record.metadata.scope)?;
                    value.secrets()?
                }
            };
            self.secrets.register(&values)?;
        }
        self.check_record_metadata(&record.metadata)?;
        Ok(record)
    }

    fn check_record_metadata(&self, metadata: &CredentialMetadata) -> Result<(), BoundaryError> {
        self.check_metadata_secrets(&metadata.scope)?;
        if let Some(resource) = &metadata.oauth_resource {
            if metadata.kind != CredentialKind::OAuth {
                return Err(error(
                    "credential_corrupt",
                    "Bearer credential contains OAuth resource metadata",
                ));
            }
            metadata.scope.validate_oauth_resource(resource)?;
        }
        let encoded = serde_json::to_string(metadata)
            .map_err(|_| error("credential_scope", "Credential metadata cannot be encoded"))?;
        if crate::redaction::contains_secret(&encoded, &self.secrets.snapshot()?) {
            return Err(error(
                "credential_scope",
                "Credential metadata contains a known credential value",
            ));
        }
        Ok(())
    }

    fn check_metadata_secrets(&self, scope: &CredentialScope) -> Result<(), BoundaryError> {
        let encoded = serde_json::to_string(scope)
            .map_err(|_| error("credential_scope", "Credential scope cannot be encoded"))?;
        if crate::redaction::contains_secret(&encoded, &self.secrets.snapshot()?) {
            return Err(error(
                "credential_scope",
                "Credential scope metadata contains a known credential value",
            ));
        }
        Ok(())
    }

    pub(super) fn write_record(&self, record: &Record) -> Result<(), BoundaryError> {
        let bytes = serde_json::to_vec(record)
            .map_err(|_| error("credential_io", "Credential record could not be encoded"))?;
        if bytes.len() > MAX_RECORD_BYTES {
            return Err(error(
                "credential_limit",
                "Credential record exceeds its size limit",
            ));
        }
        let temporary = format!(".tmp-{}", Uuid::now_v7());
        let mut options = OpenOptions::new();
        options
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
        let outcome = (|| {
            let mut file = self.dir.open_with(&temporary, &options).map_err(|_| {
                error(
                    "credential_io",
                    "Private credential temporary file could not be created",
                )
            })?;
            private_file(&file)?;
            file.write_all(&bytes)
                .and_then(|()| file.sync_all())
                .map_err(|_| error("credential_io", "Credential record could not be saved"))?;
            self.dir
                .rename(
                    &temporary,
                    &self.dir,
                    format!("{}.json", record.metadata.reference),
                )
                .map_err(|_| error("credential_io", "Credential record replacement failed"))?;
            self.sync_directory()?;
            Ok(())
        })();
        if outcome.is_err() {
            let _ = self.dir.remove_file(&temporary);
        }
        outcome
    }

    fn sync_directory(&self) -> Result<(), BoundaryError> {
        #[cfg(test)]
        if self
            .fail_directory_sync
            .swap(false, std::sync::atomic::Ordering::AcqRel)
        {
            return Err(error("credential_io", "Credential directory sync failed"));
        }
        self.dir
            .try_clone()
            .map_err(|_| error("credential_io", "Credential directory could not be cloned"))?
            .into_std_file()
            .sync_all()
            .map_err(|_| {
                error(
                    "credential_io",
                    "Credential commit durability could not be confirmed",
                )
            })
    }

    pub(super) fn load_oauth(
        &self,
        reference: CredentialRef,
        scope: &CredentialScope,
    ) -> Result<Option<StoredCredentials>, BoundaryError> {
        let _lock = self.data_lock(reference)?;
        let record = self.read_record(reference)?;
        check_scope(&record, scope)?;
        if record.metadata.revoked {
            return Ok(None);
        }
        match record.material {
            Some(Material::OAuth { credentials }) => {
                self.secrets
                    .register(&CredentialValue::OAuth(credentials.clone()).secrets()?)?;
                Ok(Some(*credentials))
            }
            _ => Err(error(
                "credential_kind",
                "Credential reference does not contain OAuth material",
            )),
        }
    }

    pub(super) fn save_oauth(
        &self,
        reference: CredentialRef,
        scope: &CredentialScope,
        credentials: StoredCredentials,
    ) -> Result<(), BoundaryError> {
        let _lock = self.data_lock(reference)?;
        let mut record = self.read_record(reference)?;
        check_scope(&record, scope)?;
        if record.metadata.revoked {
            return Err(error(
                "credential_revoked",
                "Revoked credentials cannot be restored by a refresh",
            ));
        }
        let Some(Material::OAuth { credentials: old }) = record.material.as_ref() else {
            return Err(error(
                "credential_kind",
                "Credential reference does not contain OAuth material",
            ));
        };
        if (!old.client_id.is_empty() && old.client_id != credentials.client_id)
            || old
                .issuer
                .as_ref()
                .is_some_and(|issuer| Some(issuer) != credentials.issuer.as_ref())
        {
            return Err(error(
                "credential_scope",
                "OAuth client or issuer differs from the bound credential",
            ));
        }
        let mut known = CredentialValue::OAuth(old.clone()).secrets()?;
        known.extend(CredentialValue::oauth(credentials.clone()).secrets()?);
        self.secrets.register(&known)?;
        CredentialValue::oauth(credentials.clone()).validate_scope(scope)?;
        self.check_record_metadata(&record.metadata)?;
        record.material = Some(Material::OAuth {
            credentials: Box::new(credentials),
        });
        record.metadata.revision = next_revision(record.metadata.revision)?;
        self.write_record(&record)
    }

    pub(super) fn reject_oauth(
        &self,
        reference: CredentialRef,
        scope: &CredentialScope,
        rejected: &StoredCredentials,
    ) -> Result<(), BoundaryError> {
        let _lock = self.data_lock(reference)?;
        let mut record = self.read_record(reference)?;
        check_scope(&record, scope)?;
        if record.metadata.revoked {
            return self.write_record(&record);
        }
        let Some(Material::OAuth {
            credentials: current,
        }) = record.material.as_ref()
        else {
            return Err(error(
                "credential_kind",
                "Credential reference does not contain OAuth material",
            ));
        };
        let encoded_current = serde_json::to_vec(current).map_err(|_| {
            error(
                "credential_corrupt",
                "Stored OAuth credential cannot be compared",
            )
        })?;
        let encoded_rejected = serde_json::to_vec(rejected).map_err(|_| {
            error(
                "credential_corrupt",
                "Rejected OAuth credential cannot be compared",
            )
        })?;
        if encoded_current != encoded_rejected {
            return Ok(());
        }
        self.secrets
            .register(&CredentialValue::OAuth(current.clone()).secrets()?)?;
        record.material = None;
        record.metadata.revoked = true;
        record.metadata.revision = next_revision(record.metadata.revision)?;
        self.write_record(&record)
    }
}

fn check_scope(record: &Record, scope: &CredentialScope) -> Result<(), BoundaryError> {
    if &record.metadata.scope != scope {
        return Err(error(
            "credential_scope",
            "Credential reference is bound to a different server, audience, or scope",
        ));
    }
    Ok(())
}
fn next_revision(revision: u64) -> Result<u64, BoundaryError> {
    revision.checked_add(1).ok_or_else(|| {
        error(
            "credential_limit",
            "Credential revision capacity has been reached",
        )
    })
}

fn private_file(file: &cap_std::fs::File) -> Result<(), BoundaryError> {
    use cap_std::fs::MetadataExt as _;
    let metadata = file
        .metadata()
        .map_err(|_| error("credential_io", "Credential file metadata is unavailable"))?;
    if !metadata.is_file()
        || metadata.uid() != unsafe { libc::geteuid() }
        || metadata.mode() & 0o077 != 0
        || metadata.nlink() != 1
    {
        return Err(error(
            "credential_permissions",
            "Credential files must be private, owned regular files without hard links",
        ));
    }
    Ok(())
}

pub(super) struct FileLock {
    file: File,
}
impl FileLock {
    fn acquire(dir: &Dir, name: &str) -> Result<Self, BoundaryError> {
        let mut options = OpenOptions::new();
        options
            .read(true)
            .write(true)
            .create(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
        let file = dir.open_with(name, &options).map_err(|_| {
            error(
                "credential_permissions",
                "Credential lock file could not be opened",
            )
        })?;
        private_file(&file)?;
        let file = file.into_std();
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            // SAFETY: the owned descriptor stays live through this call and guard lifetime.
            if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 {
                return Ok(Self { file });
            }
            let cause = std::io::Error::last_os_error();
            if cause.kind() != std::io::ErrorKind::WouldBlock
                && cause.kind() != std::io::ErrorKind::Interrupted
            {
                return Err(error("credential_io", "Credential lock acquisition failed"));
            }
            if Instant::now() >= deadline {
                return Err(error(
                    "credential_busy",
                    "Credential lock acquisition timed out",
                ));
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    }
}
impl Drop for FileLock {
    fn drop(&mut self) {
        // SAFETY: the descriptor belongs to this live guard.
        let _ = unsafe { libc::flock(self.file.as_raw_fd(), libc::LOCK_UN) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rmcp::transport::auth::CredentialStore as _;
    use std::collections::BTreeSet;
    use std::sync::atomic::Ordering;

    #[tokio::test]
    async fn revoked_retry_repeats_directory_sync_after_a_partial_commit() {
        let directory = tempfile::tempdir().unwrap();
        let store = FileCredentialStore::open(
            directory.path().join("credentials"),
            Arc::new(SecretRegistry::default()),
        )
        .unwrap();
        let scope =
            CredentialScope::for_mcp("fixture", "https://example.invalid/mcp", BTreeSet::new())
                .unwrap();
        let tokens=serde_json::from_value(serde_json::json!({"access_token":"sync-access","refresh_token":"sync-refresh","token_type":"Bearer"})).unwrap();
        let credential = StoredCredentials::new("client".into(), Some(tokens), Vec::new(), None)
            .with_issuer(Some("https://issuer.invalid".into()));
        let reference = store
            .create(scope.clone(), CredentialValue::oauth(credential.clone()))
            .await
            .unwrap();
        store
            .inner
            .fail_directory_sync
            .store(true, Ordering::Release);
        assert!(store.revoke(reference, &scope).await.is_err());
        assert!(store.list().await.unwrap()[0].revoked);
        store
            .inner
            .fail_directory_sync
            .store(true, Ordering::Release);
        assert!(store.revoke(reference, &scope).await.is_err());
        store.revoke(reference, &scope).await.unwrap();
        assert_eq!(store.list().await.unwrap()[0].revision, 2);
        let bridge = store.oauth(reference, scope).unwrap();
        store
            .inner
            .fail_directory_sync
            .store(true, Ordering::Release);
        assert!(bridge.on_refresh_token_rejected(&credential).await.is_err());
        bridge.on_refresh_token_rejected(&credential).await.unwrap();
    }
}
