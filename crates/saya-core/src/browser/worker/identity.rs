use super::*;

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct IdentityManifest {
    pub(crate) schema_version: u32,
    pub(crate) installation_trees: InstallationTrees,
    pub(crate) node: VersionedIdentity,
    pub(crate) worker_cli_js: FileIdentity,
    pub(crate) mcp_package: VersionedIdentity,
    pub(crate) playwright_package: VersionedIdentity,
    pub(crate) playwright_core_package: VersionedIdentity,
    pub(crate) playwright_core_browsers_json: FileIdentity,
    pub(crate) chromium: ChromiumIdentity,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct FileIdentity {
    pub(crate) sha256: String,
    pub(crate) size: u64,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct VersionedIdentity {
    pub(crate) sha256: String,
    pub(crate) size: u64,
    pub(crate) version: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ChromiumIdentity {
    pub(crate) sha256: String,
    pub(crate) size: u64,
    pub(crate) version: String,
    pub(crate) revision: u32,
}

/// Absolute host inputs whose named files and installation trees were verified.
/// This does not attest the source or build provenance of an engine.
#[derive(Debug, Clone)]
pub struct VerifiedWorkerIdentity {
    pub node: PathBuf,
    pub worker_root: PathBuf,
    pub cli_js: PathBuf,
    pub chromium: PathBuf,
    pub manifest_sha256: String,
    pub(crate) local_images: Option<Arc<VerifiedLocalImages>>,
}

impl VerifiedWorkerIdentity {
    pub(super) fn check_local_executables(
        &self,
        checkpoint: &mut dyn FnMut() -> Result<(), BoundaryError>,
    ) -> Result<(), BoundaryError> {
        let images = self.local_images.as_ref().ok_or_else(|| {
            identity_error("Browser worker local executable identities are unavailable")
        })?;
        images.node.check_named(&self.node, checkpoint)?;
        images.chromium.check_named(&self.chromium, checkpoint)
    }
}

#[derive(Debug)]
pub(crate) struct VerifiedLocalImages {
    pub(super) node: PinnedWorkerImage,
    pub(super) chromium: PinnedWorkerImage,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct WorkerImageIdentity {
    pub(super) dev: u64,
    pub(super) ino: u64,
    pub(super) size: u64,
    pub(super) mode: u32,
    pub(super) uid: u32,
    pub(super) gid: u32,
    pub(super) links: u64,
    pub(super) modified: (i64, i64),
    pub(super) changed: (i64, i64),
}

impl WorkerImageIdentity {
    pub(super) fn from_metadata(metadata: &fs::Metadata) -> Self {
        Self {
            dev: metadata.dev(),
            ino: metadata.ino(),
            size: metadata.len(),
            mode: metadata.mode(),
            uid: metadata.uid(),
            gid: metadata.gid(),
            links: metadata.nlink(),
            modified: (metadata.mtime(), metadata.mtime_nsec()),
            changed: (metadata.ctime(), metadata.ctime_nsec()),
        }
    }
}

#[derive(Debug)]
pub(super) struct PinnedWorkerImage {
    pub(super) file: File,
    pub(super) identity: WorkerImageIdentity,
    pub(super) sha256: String,
}

impl PinnedWorkerImage {
    pub(super) fn check_named(
        &self,
        path: &Path,
        checkpoint: &mut dyn FnMut() -> Result<(), BoundaryError>,
    ) -> Result<(), BoundaryError> {
        checkpoint()?;
        let named = open_absolute_file(path)?;
        self.check_metadata(&named)?;
        if self.identity.size > MAX_IDENTITY_FILE_BYTES {
            return Err(identity_error(
                "Browser worker executable exceeds its size limit",
            ));
        }
        let mut hasher = Sha256::new();
        let mut buffer = [0u8; 64 * 1024];
        let mut total = 0u64;
        loop {
            checkpoint()?;
            let length = (self.identity.size - total).min(buffer.len() as u64).max(1) as usize;
            let count = self
                .file
                .read_at(&mut buffer[..length], total)
                .map_err(|_| identity_error("Browser worker executable cannot be reread"))?;
            if count == 0 {
                if total != self.identity.size {
                    return Err(identity_error("Browser worker executable ended early"));
                }
                break;
            }
            total += count as u64;
            if total > self.identity.size {
                return Err(identity_error(
                    "Browser worker executable grew after verification",
                ));
            }
            hasher.update(&buffer[..count]);
        }
        let digest = hasher
            .finalize()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        if digest != self.sha256 {
            return Err(identity_error(
                "Browser worker executable content changed after verification",
            ));
        }
        self.check_metadata(&named)?;
        checkpoint()?;
        Ok(())
    }

    pub(super) fn check_metadata(&self, named: &File) -> Result<(), BoundaryError> {
        for file in [&self.file, named, &self.file] {
            let metadata = file.metadata().map_err(|_| {
                identity_error("Browser worker executable identity cannot be rechecked")
            })?;
            if WorkerImageIdentity::from_metadata(&metadata) != self.identity {
                return Err(identity_error(
                    "Browser worker executable changed after verification",
                ));
            }
        }
        Ok(())
    }
}

#[derive(Deserialize)]
pub(crate) struct PackageJson {
    pub(crate) name: String,
    pub(crate) version: String,
}

#[derive(Deserialize)]
pub(crate) struct BrowsersJson {
    pub(crate) browsers: Vec<BrowserRecord>,
}

#[derive(Deserialize)]
pub(crate) struct BrowserRecord {
    pub(crate) name: String,
    pub(crate) revision: String,
    #[serde(rename = "browserVersion")]
    pub(crate) browser_version: Option<String>,
}

pub(super) struct ExpectedFile<'a> {
    pub(super) sha256: &'a str,
    pub(super) size: u64,
}

pub(super) fn expected(file: &FileIdentity) -> ExpectedFile<'_> {
    ExpectedFile {
        sha256: &file.sha256,
        size: file.size,
    }
}

fn expected_versioned(file: &VersionedIdentity) -> ExpectedFile<'_> {
    ExpectedFile {
        sha256: &file.sha256,
        size: file.size,
    }
}

fn expected_chromium(file: &ChromiumIdentity) -> ExpectedFile<'_> {
    ExpectedFile {
        sha256: &file.sha256,
        size: file.size,
    }
}

pub(super) fn open_directory(path: &Path) -> Result<File, BoundaryError> {
    let mut options = OpenOptions::new();
    options
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK);
    options
        .open(path)
        .map_err(|_| identity_error("Browser worker directory cannot be opened safely"))
}

pub(super) fn open_absolute_file(path: &Path) -> Result<File, BoundaryError> {
    let mut options = OpenOptions::new();
    options
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK);
    options
        .open(path)
        .map_err(|_| identity_error("Browser worker file cannot be opened safely"))
}

/// Open a worker-relative file by walking every component with `O_NOFOLLOW`.
/// The root may not be the final component of a symlink either.
fn open_under_root(root: &Path, relative: &Path) -> Result<File, BoundaryError> {
    let mut directory = Dir::from_std_file(open_directory(root)?);
    let components = relative.components().collect::<Vec<_>>();
    if components.is_empty() {
        return Err(identity_error("Browser worker identity path is empty"));
    }
    for (index, component) in components.iter().enumerate() {
        let Component::Normal(name) = component else {
            return Err(identity_error(
                "Browser worker identity path is not relative",
            ));
        };
        let mut options = CapOpenOptions::new();
        options.read(true);
        if index + 1 == components.len() {
            options.custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK);
            let file = directory
                .open_with(name, &options)
                .map_err(|_| identity_error("Browser worker file changed or is unsafe"))?;
            return Ok(file.into_std());
        }
        options.custom_flags(
            libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK,
        );
        let child = directory
            .open_with(name, &options)
            .map_err(|_| identity_error("Browser worker directory changed or is unsafe"))?
            .into_std();
        directory = Dir::from_std_file(child);
    }
    Err(identity_error("Browser worker identity path is empty"))
}

fn file_identity(metadata: &fs::Metadata) -> (u64, u64, u64, i64, i64, u32) {
    (
        metadata.dev(),
        metadata.ino(),
        metadata.len(),
        metadata.mtime(),
        metadata.mtime_nsec(),
        metadata.mode(),
    )
}

fn check_file_metadata(
    metadata: &fs::Metadata,
    expected: &ExpectedFile<'_>,
    executable: bool,
) -> Result<(), BoundaryError> {
    if !metadata.is_file() {
        return Err(identity_error(
            "Browser worker identity is not a regular file",
        ));
    }
    if metadata.len() != expected.size || metadata.len() > MAX_IDENTITY_FILE_BYTES {
        return Err(identity_error(
            "Browser worker identity size does not match its manifest",
        ));
    }
    let mode = metadata.permissions().mode();
    if mode & 0o022 != 0 || mode & 0o6000 != 0 {
        return Err(identity_error(
            "Browser worker identity is writable by another account or privileged",
        ));
    }
    if mode & 0o400 == 0 {
        return Err(identity_error("Browser worker identity is not readable"));
    }
    if executable && mode & 0o111 == 0 {
        return Err(identity_error(
            "Browser worker executable does not have an execute bit",
        ));
    }
    Ok(())
}

fn read_identity_bytes(
    mut file: File,
    expected: &ExpectedFile<'_>,
    checkpoint: &mut dyn FnMut() -> Result<(), BoundaryError>,
) -> Result<Vec<u8>, BoundaryError> {
    let before = file
        .metadata()
        .map_err(|_| identity_error("Browser worker identity cannot be inspected"))?;
    check_file_metadata(&before, expected, false)?;
    let mut bytes = Vec::with_capacity(before.len() as usize);
    let mut buffer = [0; 64 * 1024];
    loop {
        checkpoint()?;
        let count = file
            .read(&mut buffer)
            .map_err(|_| identity_error("Browser worker identity cannot be read"))?;
        if count == 0 {
            break;
        }
        if bytes.len() as u64 + count as u64 > expected.size {
            return Err(identity_error(
                "Browser worker identity changed while being read",
            ));
        }
        bytes.extend_from_slice(&buffer[..count]);
    }
    let after = file
        .metadata()
        .map_err(|_| identity_error("Browser worker identity cannot be rechecked"))?;
    if file_identity(&before) != file_identity(&after) || bytes.len() as u64 != before.len() {
        return Err(identity_error(
            "Browser worker identity changed while being read",
        ));
    }
    if sha256_hex(&bytes) != expected.sha256 {
        return Err(identity_error(
            "Browser worker identity digest does not match its manifest",
        ));
    }
    Ok(bytes)
}

fn verify_identity_digest(
    mut file: File,
    expected: &ExpectedFile<'_>,
    executable: bool,
    checkpoint: &mut dyn FnMut() -> Result<(), BoundaryError>,
) -> Result<PinnedWorkerImage, BoundaryError> {
    let before = file
        .metadata()
        .map_err(|_| identity_error("Browser worker identity cannot be inspected"))?;
    check_file_metadata(&before, expected, executable)?;
    let mut hasher = Sha256::new();
    let mut buffer = [0u8; 64 * 1024];
    let mut total = 0u64;
    loop {
        checkpoint()?;
        let count = file
            .read(&mut buffer)
            .map_err(|_| identity_error("Browser worker identity cannot be read"))?;
        if count == 0 {
            break;
        }
        total = total.saturating_add(count as u64);
        if total > expected.size {
            return Err(identity_error(
                "Browser worker identity size does not match its manifest",
            ));
        }
        hasher.update(&buffer[..count]);
    }
    let after = file
        .metadata()
        .map_err(|_| identity_error("Browser worker identity cannot be rechecked"))?;
    let identity = WorkerImageIdentity::from_metadata(&before);
    if identity != WorkerImageIdentity::from_metadata(&after) || total != expected.size {
        return Err(identity_error(
            "Browser worker identity changed while being read",
        ));
    }
    let digest = hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    if digest != expected.sha256 {
        return Err(identity_error(
            "Browser worker identity digest does not match its manifest",
        ));
    }
    Ok(PinnedWorkerImage {
        file,
        identity,
        sha256: digest,
    })
}

pub(crate) fn parse_package(
    bytes: &[u8],
    expected_name: &str,
) -> Result<PackageJson, BoundaryError> {
    let package: PackageJson = serde_json::from_slice(bytes)
        .map_err(|_| identity_error("Browser worker package metadata is invalid"))?;
    if package.name != expected_name || package.version.is_empty() {
        return Err(identity_error("Browser worker package identity is wrong"));
    }
    Ok(package)
}

pub(crate) fn parse_browsers(bytes: &[u8]) -> Result<BrowsersJson, BoundaryError> {
    serde_json::from_slice(bytes)
        .map_err(|_| identity_error("Browser worker browsers.json is invalid"))
}

fn manifest_expected_digest(settings: &BrowserWorkerSettings) -> &str {
    settings.identity_sha256.as_deref().unwrap_or_default()
}

/// Verify named files and closed installation trees against a v2 manifest.
/// Reads are bounded by the configured connection deadline; no process is launched.
pub fn verify_worker_identity(
    settings: &BrowserWorkerSettings,
) -> Result<VerifiedWorkerIdentity, BoundaryError> {
    verify_worker_identity_checked(settings, None)
}

fn read_verified_manifest(
    manifest_path: &Path,
    expected_sha256: &str,
) -> Result<(IdentityManifest, String), BoundaryError> {
    let mut manifest_file = open_absolute_file(manifest_path)?;
    let before = manifest_file
        .metadata()
        .map_err(|_| identity_error("Browser worker manifest cannot be inspected"))?;
    if !before.is_file() || before.len() > MAX_MANIFEST_BYTES {
        return Err(identity_error(
            "Browser worker manifest is not a bounded regular file",
        ));
    }
    let mode = before.permissions().mode();
    if mode & 0o022 != 0 || mode & 0o6000 != 0 || mode & 0o400 == 0 {
        return Err(identity_error(
            "Browser worker manifest permissions are unsafe",
        ));
    }
    let mut manifest_bytes = Vec::with_capacity(before.len() as usize);
    manifest_file
        .read_to_end(&mut manifest_bytes)
        .map_err(|_| identity_error("Browser worker manifest cannot be read"))?;
    let after = manifest_file
        .metadata()
        .map_err(|_| identity_error("Browser worker manifest cannot be rechecked"))?;
    if file_identity(&before) != file_identity(&after)
        || manifest_bytes.len() as u64 != before.len()
    {
        return Err(identity_error(
            "Browser worker manifest changed while being read",
        ));
    }
    let manifest_digest = sha256_hex(&manifest_bytes);
    if manifest_digest != expected_sha256 {
        return Err(identity_error(
            "Browser worker manifest digest does not match the host input",
        ));
    }
    let manifest: IdentityManifest = serde_json::from_slice(&manifest_bytes)
        .map_err(|_| identity_error("Browser worker manifest is invalid"))?;
    if manifest.schema_version != 2 {
        return Err(identity_error(
            "Browser worker manifest schema version is unsupported",
        ));
    }
    manifest.installation_trees.validate()?;
    for digest in [
        &manifest.node.sha256,
        &manifest.worker_cli_js.sha256,
        &manifest.mcp_package.sha256,
        &manifest.playwright_package.sha256,
        &manifest.playwright_core_package.sha256,
        &manifest.playwright_core_browsers_json.sha256,
        &manifest.chromium.sha256,
    ] {
        if !valid_sha256(digest) {
            return Err(identity_error(
                "Browser worker manifest contains an invalid digest",
            ));
        }
    }
    Ok((manifest, manifest_digest))
}

fn verify_pinned_package(
    worker_root: &Path,
    relative_path: &str,
    package_name: &str,
    identity: &VersionedIdentity,
    pinned_version: &str,
    mismatch_message: &'static str,
    checkpoint: &mut dyn FnMut() -> Result<(), BoundaryError>,
) -> Result<(), BoundaryError> {
    let bytes = read_identity_bytes(
        open_under_root(worker_root, Path::new(relative_path))?,
        &expected_versioned(identity),
        checkpoint,
    )?;
    let package = parse_package(&bytes, package_name)?;
    if package.version != pinned_version || identity.version != pinned_version {
        return Err(identity_error(mismatch_message));
    }
    Ok(())
}

fn verify_chromium_manifest_record(
    worker_root: &Path,
    manifest: &IdentityManifest,
    checkpoint: &mut dyn FnMut() -> Result<(), BoundaryError>,
) -> Result<(), BoundaryError> {
    let browsers_bytes = read_identity_bytes(
        open_under_root(worker_root, Path::new(BROWSERS_JSON_PATH))?,
        &expected(&manifest.playwright_core_browsers_json),
        checkpoint,
    )?;
    let browsers = parse_browsers(&browsers_bytes)?;
    let Some(record) = browsers
        .browsers
        .iter()
        .find(|record| record.name == "chromium")
    else {
        return Err(identity_error(
            "Browser worker browsers.json does not declare Chromium",
        ));
    };
    let revision = record
        .revision
        .parse::<u32>()
        .map_err(|_| identity_error("Browser worker Chromium revision is invalid"))?;
    if revision != CHROMIUM_REVISION
        || record.browser_version.as_deref() != Some(CHROMIUM_VERSION)
        || manifest.chromium.revision != CHROMIUM_REVISION
        || manifest.chromium.version != CHROMIUM_VERSION
    {
        return Err(identity_error(
            "Browser worker Chromium version does not match the pinned version",
        ));
    }
    Ok(())
}

pub(super) fn verify_worker_identity_checked(
    settings: &BrowserWorkerSettings,
    cancel: Option<&dyn Cancellation>,
) -> Result<VerifiedWorkerIdentity, BoundaryError> {
    settings.validate()?;
    let deadline = Instant::now() + Duration::from_millis(settings.connect_timeout_ms);
    let mut checkpoint = || {
        if cancel.is_some_and(Cancellation::is_cancelled) {
            return Err(error(
                "cancelled",
                "Browser worker identity verification cancelled",
            ));
        }
        if Instant::now() >= deadline {
            return Err(identity_error(
                "Browser worker identity verification exceeded its deadline",
            ));
        }
        Ok(())
    };
    checkpoint()?;
    let node = settings
        .node
        .as_ref()
        .ok_or_else(|| invalid_config("Browser worker node path is missing"))?;
    let worker_root = settings
        .worker_root
        .as_ref()
        .ok_or_else(|| invalid_config("Browser worker root is missing"))?;
    let chromium = settings
        .chromium
        .as_ref()
        .ok_or_else(|| invalid_config("Browser worker Chromium path is missing"))?;
    let manifest_path = settings
        .identity_manifest
        .as_ref()
        .ok_or_else(|| invalid_config("Browser worker identity manifest is missing"))?;

    let (manifest, manifest_digest) =
        read_verified_manifest(manifest_path, manifest_expected_digest(settings))?;
    let node_image = verify_identity_digest(
        open_absolute_file(node)?,
        &expected_versioned(&manifest.node),
        true,
        &mut checkpoint,
    )?;
    if manifest.node.version.is_empty() {
        return Err(identity_error("Browser worker node version is missing"));
    }

    let cli_bytes = read_identity_bytes(
        open_under_root(worker_root, Path::new(CLI_JS_PATH))?,
        &expected(&manifest.worker_cli_js),
        &mut checkpoint,
    )?;
    if cli_bytes.is_empty() {
        return Err(identity_error("Browser worker CLI is empty"));
    }

    verify_pinned_package(
        worker_root,
        MCP_PACKAGE_PATH,
        "@playwright/mcp",
        &manifest.mcp_package,
        PLAYWRIGHT_MCP_VERSION,
        "Browser worker @playwright/mcp version does not match the pinned version",
        &mut checkpoint,
    )?;
    verify_pinned_package(
        worker_root,
        PLAYWRIGHT_PACKAGE_PATH,
        "playwright",
        &manifest.playwright_package,
        PLAYWRIGHT_VERSION,
        "Browser worker playwright version does not match the pinned version",
        &mut checkpoint,
    )?;
    verify_pinned_package(
        worker_root,
        PLAYWRIGHT_CORE_PACKAGE_PATH,
        "playwright-core",
        &manifest.playwright_core_package,
        PLAYWRIGHT_VERSION,
        "Browser worker playwright-core version does not match the pinned version",
        &mut checkpoint,
    )?;
    verify_chromium_manifest_record(worker_root, &manifest, &mut checkpoint)?;

    let chromium_image = verify_identity_digest(
        open_absolute_file(chromium)?,
        &expected_chromium(&manifest.chromium),
        true,
        &mut checkpoint,
    )?;
    for (root, expected) in [
        (node.parent(), &manifest.installation_trees.node),
        (
            Some(worker_root.as_path()),
            &manifest.installation_trees.worker,
        ),
        (chromium.parent(), &manifest.installation_trees.chromium),
    ] {
        installation::verify_tree(
            root.ok_or_else(|| identity_error("Browser installation root is missing"))?,
            expected,
            &mut checkpoint,
        )?;
    }

    let identity = VerifiedWorkerIdentity {
        node: node.clone(),
        worker_root: worker_root.clone(),
        cli_js: worker_root.join(CLI_JS_PATH),
        chromium: chromium.clone(),
        manifest_sha256: manifest_digest,
        local_images: Some(Arc::new(VerifiedLocalImages {
            node: node_image,
            chromium: chromium_image,
        })),
    };
    checkpoint()?;
    identity.check_local_executables(&mut checkpoint)?;
    Ok(identity)
}
