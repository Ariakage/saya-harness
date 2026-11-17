use super::*;
use cap_std::fs::{Dir, DirBuilder, DirBuilderExt, OpenOptions, OpenOptionsExt};
use std::{
    ffi::{OsStr, OsString},
    fs::{self, File},
    io::{Read, Write},
    os::{
        fd::AsRawFd,
        unix::{
            ffi::OsStrExt,
            fs::{DirBuilderExt as _, MetadataExt, OpenOptionsExt as _, PermissionsExt},
        },
    },
    time::{Duration, Instant},
};
use unicode_normalization::UnicodeNormalization;
use uuid::Uuid;

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ViewLimits {
    pub max_views: usize,
    pub max_view_bytes: u64,
    pub max_total_bytes: u64,
    pub max_cleanup_entries: usize,
}
impl Default for ViewLimits {
    fn default() -> Self {
        Self {
            max_views: 32,
            max_view_bytes: 64 * 1024 * 1024,
            max_total_bytes: 256 * 1024 * 1024,
            max_cleanup_entries: 32_768,
        }
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ViewIdentity {
    pub id: String,
    pub root: PathBuf,
    pub package: PathBuf,
    pub scratch: PathBuf,
    pub package_digest: String,
    pub lease_id: String,
    pub session_id: SessionId,
    pub run_id: RunId,
    pub device: u64,
    pub inode: u64,
    pub package_bytes: u64,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ViewInventory {
    pub identity: ViewIdentity,
    pub state: String,
}

pub struct ViewPool {
    root: Dir,
    path: PathBuf,
    device: u64,
    inode: u64,
    limits: ViewLimits,
    #[cfg(test)]
    pub(super) fail_publication: AtomicBool,
}
impl std::fmt::Debug for ViewPool {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("ViewPool([PRIVATE])")
    }
}
pub struct PluginView {
    pool: Arc<ViewPool>,
    identity: ViewIdentity,
}
impl std::fmt::Debug for PluginView {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PluginView")
            .field("id", &self.identity.id)
            .finish_non_exhaustive()
    }
}
impl PluginView {
    pub fn identity(&self) -> &ViewIdentity {
        &self.identity
    }
    pub fn verify(&self) -> Result<(), BoundaryError> {
        let _lock = self.pool.lock()?;
        self.pool.verify_identity(&self.identity, true)
    }
    /// The caller has established that every owning process/service stopped.
    /// Dropping a handle never invokes removal or changes a lease outcome.
    pub fn remove(&self, cancel: &AtomicBool) -> Result<(), BoundaryError> {
        let _lock = self.pool.lock()?;
        self.pool.remove_identity(&self.identity, cancel)
    }
}
impl ViewPool {
    /// `path` is a host-selected private directory outside writable workspace
    /// roots and internal protected data/cache. It must never come from a plugin.
    pub fn open(path: &Path, limits: ViewLimits) -> Result<Self, BoundaryError> {
        if limits.max_views == 0
            || limits.max_views > 128
            || limits.max_view_bytes == 0
            || limits.max_view_bytes > crate::plugin_manifest::MAX_PACKAGE_BYTES
            || limits.max_total_bytes < limits.max_view_bytes
            || limits.max_cleanup_entries < crate::plugin_manifest::MAX_PACKAGE_ENTRIES
            || limits.max_cleanup_entries > 1_000_000
        {
            return Err(error("plugin_view_limit", "Plugin view limits are invalid"));
        }
        match fs::DirBuilder::new().mode(0o700).create(path) {
            Ok(()) => {}
            Err(failure) if failure.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(_) => return Err(io_error()),
        }
        let file = fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK)
            .open(path)
            .map_err(|_| io_error())?;
        let metadata = private_directory(&file, false)?;
        let canonical = fs::canonicalize(path).map_err(|_| io_error())?;
        let pool = Self {
            root: Dir::from_std_file(file),
            path: canonical,
            device: metadata.dev(),
            inode: metadata.ino(),
            limits,
            #[cfg(test)]
            fail_publication: AtomicBool::new(false),
        };
        let _lock = pool.lock()?;
        pool.cleanup_staging()?;
        Ok(pool)
    }
    pub fn path(&self) -> &Path {
        &self.path
    }
    pub fn validate_location(
        &self,
        workspace: &Path,
        protected: &[PathBuf],
        write_roots: &[PathBuf],
    ) -> Result<(), BoundaryError> {
        self.check_root()?;
        if overlaps(&self.path, workspace)? {
            return Err(error(
                "plugin_view_scope",
                "Plugin views must be outside the workspace",
            ));
        }
        for path in protected.iter().chain(write_roots) {
            let resolved = resolve(path)?;
            if overlaps(&self.path, &resolved)? {
                return Err(error(
                    "plugin_view_scope",
                    "Plugin views overlap a protected or host-writable root",
                ));
            }
        }
        Ok(())
    }
    pub fn inventory(&self) -> Result<Vec<ViewInventory>, BoundaryError> {
        let _lock = self.lock()?;
        self.cleanup_staging()?;
        let mut result = Vec::new();
        for name in self.view_names()? {
            let identity = self.owner(&name)?;
            self.verify_identity(&identity, false)?;
            result.push(ViewInventory {
                identity,
                state: "cleanup_requires_host_evidence".into(),
            });
        }
        Ok(result)
    }
    /// Recovery never infers process termination from missing owners. The host
    /// must establish stop evidence, remove the view, then confirm the matching
    /// store lease cleanup. Unknown keeps all files and durable pins.
    pub fn remove_recovered(
        &self,
        identity: &ViewIdentity,
        evidence: StopEvidence,
        cancel: &AtomicBool,
    ) -> Result<(), BoundaryError> {
        if evidence == StopEvidence::Unknown {
            return Err(error(
                "plugin_cleanup_pending",
                "Plugin view cleanup requires observed stop evidence",
            ));
        }
        cancelled(cancel)?;
        let _lock = self.lock()?;
        self.remove_identity(identity, cancel)
    }
    pub fn materialize(
        self: &Arc<Self>,
        package: &crate::plugin_manifest::VerifiedPackage,
        lease: &LeaseRecord,
        cancel: &AtomicBool,
    ) -> Result<PluginView, BoundaryError> {
        cancelled(cancel)?;
        let _lock = self.lock()?;
        self.cleanup_staging()?;
        if package.digest() != lease.digest
            || package.manifest().plugin.id != lease.plugin_id
            || !valid_hash(package.digest())
        {
            return Err(error(
                "plugin_view_identity",
                "Plugin lease differs from its sealed package",
            ));
        }
        let bytes = package
            .files()
            .iter()
            .map(|file| file.bytes().len() as u64)
            .sum::<u64>();
        if bytes > self.limits.max_view_bytes {
            return Err(error(
                "plugin_view_limit",
                "Plugin package exceeds its view byte limit",
            ));
        }
        let names = self.view_names()?;
        let mut total = bytes;
        for name in &names {
            let owner = self.owner(name)?;
            self.verify_identity(&owner, true)?;
            total = total
                .checked_add(owner.package_bytes)
                .ok_or_else(|| error("plugin_view_limit", "Plugin view quota overflows"))?;
        }
        if names.len() >= self.limits.max_views || total > self.limits.max_total_bytes {
            return Err(error(
                "plugin_view_limit",
                "Plugin view pool capacity is exhausted",
            ));
        }
        let id = Uuid::now_v7().to_string();
        let staging = format!(".stage-{id}");
        let final_name = format!("view-{id}");
        let directory = create_directory(&self.root, &staging)?;
        let package_dir = create_directory(&directory, "package")?;
        create_directory(&directory, "scratch")?;
        let result = (|| {
            for entry in package.files() {
                cancelled(cancel)?;
                let mut parent = package_dir.try_clone().map_err(|_| io_error())?;
                let parts: Vec<_> = entry.path().split('/').collect();
                let (name, parents) = parts
                    .split_last()
                    .ok_or_else(|| error("plugin_view_identity", "Sealed package path is empty"))?;
                for part in parents {
                    parent = create_directory(&parent, part)?;
                }
                let mut options = options(false);
                options.write(true).create_new(true).mode(0o600);
                let mut file = parent
                    .open_with(name, &options)
                    .map_err(|_| io_error())?
                    .into_std();
                for chunk in entry.bytes().chunks(16 * 1024) {
                    cancelled(cancel)?;
                    file.write_all(chunk).map_err(|_| io_error())?;
                }
                file.set_permissions(fs::Permissions::from_mode(0o400))
                    .map_err(|_| io_error())?;
                file.sync_all().map_err(|_| io_error())?;
            }
            seal(&package_dir, 0, &mut 0)?;
            let verified = crate::plugin_manifest::inspect_package_directory(&package_dir)?;
            if verified.digest() != package.digest() {
                return Err(error(
                    "plugin_view_identity",
                    "Materialized view differs from its sealed digest",
                ));
            }
            let metadata = directory
                .try_clone()
                .map_err(|_| io_error())?
                .into_std_file()
                .metadata()
                .map_err(|_| io_error())?;
            let root = self.path.join(&final_name);
            let identity = ViewIdentity {
                id: id.clone(),
                package: root.join("package"),
                scratch: root.join("scratch"),
                root,
                package_digest: package.digest().into(),
                lease_id: lease.lease_id.clone(),
                session_id: lease.session_id,
                run_id: lease.run_id,
                device: metadata.dev(),
                inode: metadata.ino(),
                package_bytes: bytes,
            };
            let owner = serde_json::to_vec(&identity).map_err(|_| {
                error(
                    "plugin_view_identity",
                    "Plugin view ownership cannot be encoded",
                )
            })?;
            let mut settings = options(false);
            settings.write(true).create_new(true).mode(0o600);
            let mut file = directory
                .open_with("owner.json", &settings)
                .map_err(|_| io_error())?
                .into_std();
            file.write_all(&owner).map_err(|_| io_error())?;
            file.sync_all().map_err(|_| io_error())?;
            sync(&directory)?;
            cancelled(cancel)?;
            self.root
                .rename(&staging, &self.root, &final_name)
                .map_err(|_| io_error())?;
            self.sync_publication()?;
            Ok(PluginView {
                pool: self.clone(),
                identity,
            })
        })();
        if result.is_err() {
            let _ = remove_tree(
                &self.root,
                OsStr::new(&staging),
                0,
                &mut 0,
                self.limits.max_cleanup_entries,
                &AtomicBool::new(false),
            );
        }
        result
    }
    fn sync_publication(&self) -> Result<(), BoundaryError> {
        #[cfg(test)]
        if self.fail_publication.swap(false, Ordering::Relaxed) {
            return Err(error(
                "plugin_view_unknown",
                "Plugin view publication durability is unknown",
            ));
        }
        sync(&self.root).map_err(|_| {
            error(
                "plugin_view_unknown",
                "Plugin view publication durability is unknown",
            )
        })
    }
    fn check_root(&self) -> Result<(), BoundaryError> {
        let named = fs::symlink_metadata(&self.path).map_err(|_| io_error())?;
        if !named.is_dir() || named.dev() != self.device || named.ino() != self.inode {
            return Err(error(
                "plugin_view_identity",
                "Plugin view parent identity changed",
            ));
        }
        private_directory(
            &self
                .root
                .try_clone()
                .map_err(|_| io_error())?
                .into_std_file(),
            false,
        )?;
        Ok(())
    }
    fn lock(&self) -> Result<File, BoundaryError> {
        self.check_root()?;
        let mut options = options(true);
        options.write(true).create(true).mode(0o600);
        let file = self
            .root
            .open_with(".lock", &options)
            .map_err(|_| io_error())?
            .into_std();
        private_file(&file, false)?;
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            // SAFETY: flock operates on this owned regular-file descriptor.
            if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 {
                break;
            }
            let failure = std::io::Error::last_os_error();
            if !matches!(
                failure.kind(),
                std::io::ErrorKind::Interrupted | std::io::ErrorKind::WouldBlock
            ) {
                return Err(io_error());
            }
            if Instant::now() >= deadline {
                return Err(error("plugin_view_busy", "Plugin view pool lock timed out"));
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        self.check_root()?;
        let actual = private_file(&file, false)?;
        let named = self
            .root
            .symlink_metadata(".lock")
            .map_err(|_| io_error())?;
        use cap_std::fs::MetadataExt as _;
        if actual.dev() != named.dev() || actual.ino() != named.ino() {
            return Err(error(
                "plugin_view_identity",
                "Plugin view lock identity changed",
            ));
        }
        Ok(file)
    }
    fn view_names(&self) -> Result<Vec<String>, BoundaryError> {
        let mut result = Vec::new();
        for name in names(&self.root, self.limits.max_views.saturating_add(16))? {
            let name = name
                .into_string()
                .map_err(|_| error("plugin_view_identity", "Plugin view name is not UTF-8"))?;
            if name == ".lock" || stage_id(&name).is_some() {
                continue;
            }
            if name.strip_prefix("view-").is_none_or(|id| !valid_id(id)) {
                return Err(error(
                    "plugin_view_identity",
                    "Plugin view parent contains an unknown entry",
                ));
            }
            result.push(name);
        }
        Ok(result)
    }
    fn cleanup_staging(&self) -> Result<(), BoundaryError> {
        for name in names(&self.root, self.limits.max_views.saturating_add(16))? {
            if name.to_str().is_some_and(|name| stage_id(name).is_some()) {
                remove_tree(
                    &self.root,
                    &name,
                    0,
                    &mut 0,
                    self.limits.max_cleanup_entries,
                    &AtomicBool::new(false),
                )?;
            }
        }
        sync(&self.root)
    }
    fn owner(&self, name: &str) -> Result<ViewIdentity, BoundaryError> {
        let directory = open_directory(&self.root, Path::new(name), false)?;
        let file = directory
            .open_with("owner.json", &options(true))
            .map_err(|_| io_error())?
            .into_std();
        private_file(&file, false)?;
        let mut bytes = Vec::new();
        file.take(8193)
            .read_to_end(&mut bytes)
            .map_err(|_| io_error())?;
        if bytes.len() > 8192 {
            return Err(error(
                "plugin_view_identity",
                "Plugin view ownership exceeds its limit",
            ));
        }
        let identity: ViewIdentity = serde_json::from_slice(&bytes)
            .map_err(|_| error("plugin_view_identity", "Plugin view ownership is invalid"))?;
        if !valid_id(&identity.id)
            || format!("view-{}", identity.id) != name
            || identity.root != self.path.join(name)
            || identity.package != identity.root.join("package")
            || identity.scratch != identity.root.join("scratch")
            || !valid_hash(&identity.package_digest)
            || !valid_id(&identity.lease_id)
            || identity.package_bytes > self.limits.max_view_bytes
        {
            return Err(error(
                "plugin_view_identity",
                "Plugin view ownership binding differs",
            ));
        }
        Ok(identity)
    }
    fn verify_identity(
        &self,
        identity: &ViewIdentity,
        contents: bool,
    ) -> Result<(), BoundaryError> {
        let name = format!("view-{}", identity.id);
        let recorded = self.owner(&name)?;
        if recorded != *identity {
            return Err(error(
                "plugin_view_identity",
                "Plugin view ownership changed",
            ));
        }
        let root = open_directory(&self.root, Path::new(&name), false)?;
        let metadata = root
            .try_clone()
            .map_err(|_| io_error())?
            .into_std_file()
            .metadata()
            .map_err(|_| io_error())?;
        if metadata.dev() != identity.device || metadata.ino() != identity.inode {
            return Err(error(
                "plugin_view_identity",
                "Plugin view directory identity changed",
            ));
        }
        if contents {
            let directory = open_directory(&root, Path::new("package"), true)?;
            readonly_tree(&directory, 0, &mut 0)?;
            let package = crate::plugin_manifest::inspect_package_directory(&directory)?;
            let bytes = package
                .files()
                .iter()
                .map(|file| file.bytes().len() as u64)
                .sum::<u64>();
            if package.digest() != identity.package_digest || bytes != identity.package_bytes {
                return Err(error("plugin_view_identity", "Plugin view content changed"));
            }
        }
        Ok(())
    }
    fn remove_identity(
        &self,
        identity: &ViewIdentity,
        cancel: &AtomicBool,
    ) -> Result<(), BoundaryError> {
        let name = format!("view-{}", identity.id);
        match self.root.symlink_metadata(&name) {
            Err(failure) if failure.kind() == std::io::ErrorKind::NotFound => {
                return sync(&self.root);
            }
            Err(_) => return Err(io_error()),
            Ok(_) => {}
        }
        self.verify_identity(identity, false)?;
        let root = open_directory(&self.root, Path::new(&name), false)?;
        if names(&root, 3)?
            .iter()
            .any(|name| !matches!(name.to_str(), Some("package" | "scratch" | "owner.json")))
        {
            return Err(error(
                "plugin_view_identity",
                "Plugin view contains an unknown entry",
            ));
        }
        let mut count = 0;
        for child in ["package", "scratch"] {
            match root.symlink_metadata(child) {
                Ok(_) => remove_tree(
                    &root,
                    OsStr::new(child),
                    0,
                    &mut count,
                    self.limits.max_cleanup_entries,
                    cancel,
                )?,
                Err(failure) if failure.kind() == std::io::ErrorKind::NotFound => {}
                Err(_) => return Err(io_error()),
            }
        }
        cancelled(cancel)?;
        root.remove_file("owner.json").map_err(|_| io_error())?;
        self.root.remove_dir(&name).map_err(|_| io_error())?;
        sync(&self.root)
    }
}

fn valid_id(value: &str) -> bool {
    Uuid::parse_str(value).is_ok_and(|id| id.to_string() == value)
}
fn stage_id(value: &str) -> Option<&str> {
    value.strip_prefix(".stage-").filter(|id| valid_id(id))
}
fn io_error() -> BoundaryError {
    error(
        "plugin_view_io",
        "Private plugin view filesystem operation failed",
    )
}
fn options(read: bool) -> OpenOptions {
    let mut options = OpenOptions::new();
    options
        .read(read)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK);
    options
}
fn private_directory(file: &File, readonly: bool) -> Result<fs::Metadata, BoundaryError> {
    let metadata = file.metadata().map_err(|_| io_error())?;
    // SAFETY: geteuid has no pointer arguments or ownership requirements.
    if !metadata.is_dir()
        || metadata.uid() != unsafe { libc::geteuid() }
        || metadata.mode() & 0o077 != 0
        || (readonly && metadata.mode() & 0o222 != 0)
    {
        return Err(error(
            "plugin_view_permissions",
            "Plugin view directories must be private, owned, and use their required write policy",
        ));
    }
    Ok(metadata)
}
fn private_file(file: &File, readonly: bool) -> Result<fs::Metadata, BoundaryError> {
    let metadata = file.metadata().map_err(|_| io_error())?;
    // SAFETY: geteuid has no pointer arguments or ownership requirements.
    if !metadata.is_file()
        || metadata.uid() != unsafe { libc::geteuid() }
        || metadata.mode() & 0o077 != 0
        || metadata.nlink() != 1
        || (readonly && metadata.mode() & 0o222 != 0)
    {
        return Err(error(
            "plugin_view_permissions",
            "Plugin view files must be private owned regular files without links",
        ));
    }
    Ok(metadata)
}
fn open_directory(parent: &Dir, path: &Path, readonly: bool) -> Result<Dir, BoundaryError> {
    let mut settings = options(true);
    settings
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK);
    let file = parent
        .open_with(path, &settings)
        .map_err(|_| io_error())?
        .into_std();
    private_directory(&file, readonly)?;
    Ok(Dir::from_std_file(file))
}
fn create_directory(parent: &Dir, name: &str) -> Result<Dir, BoundaryError> {
    let mut builder = DirBuilder::new();
    builder.mode(0o700);
    match parent.create_dir_with(name, &builder) {
        Ok(()) => {}
        Err(failure) if failure.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(_) => return Err(io_error()),
    }
    open_directory(parent, Path::new(name), false)
}
fn names(directory: &Dir, limit: usize) -> Result<Vec<OsString>, BoundaryError> {
    let mut names = Vec::new();
    for entry in directory.entries().map_err(|_| io_error())? {
        names.push(entry.map_err(|_| io_error())?.file_name());
        if names.len() > limit {
            return Err(error(
                "plugin_view_limit",
                "Plugin view entry count exceeds its processing limit",
            ));
        }
    }
    names.sort();
    Ok(names)
}
fn sync(directory: &Dir) -> Result<(), BoundaryError> {
    directory
        .try_clone()
        .map_err(|_| io_error())?
        .into_std_file()
        .sync_all()
        .map_err(|_| io_error())
}
fn seal(directory: &Dir, depth: usize, count: &mut usize) -> Result<(), BoundaryError> {
    *count += 1;
    if *count > crate::plugin_manifest::MAX_PACKAGE_ENTRIES || depth > 16 {
        return Err(error(
            "plugin_view_limit",
            "Plugin view directory tree exceeds its limit",
        ));
    }
    for name in names(directory, crate::plugin_manifest::MAX_PACKAGE_ENTRIES)? {
        if directory
            .symlink_metadata(Path::new(&name))
            .map_err(|_| io_error())?
            .is_dir()
        {
            seal(
                &open_directory(directory, Path::new(&name), false)?,
                depth + 1,
                count,
            )?;
        } else {
            *count += 1;
        }
    }
    let file = directory
        .try_clone()
        .map_err(|_| io_error())?
        .into_std_file();
    file.set_permissions(fs::Permissions::from_mode(0o500))
        .map_err(|_| io_error())?;
    file.sync_all().map_err(|_| io_error())
}
fn readonly_tree(directory: &Dir, depth: usize, count: &mut usize) -> Result<(), BoundaryError> {
    *count += 1;
    if depth > 16 || *count > crate::plugin_manifest::MAX_PACKAGE_ENTRIES {
        return Err(error(
            "plugin_view_limit",
            "Plugin readonly view exceeds its processing limit",
        ));
    }
    for name in names(directory, crate::plugin_manifest::MAX_PACKAGE_ENTRIES)? {
        if directory
            .symlink_metadata(Path::new(&name))
            .map_err(|_| io_error())?
            .is_dir()
        {
            readonly_tree(
                &open_directory(directory, Path::new(&name), true)?,
                depth + 1,
                count,
            )?;
        } else {
            *count += 1;
            if *count > crate::plugin_manifest::MAX_PACKAGE_ENTRIES {
                return Err(error(
                    "plugin_view_limit",
                    "Plugin readonly view exceeds its processing limit",
                ));
            }
            let file = directory
                .open_with(Path::new(&name), &options(true))
                .map_err(|_| io_error())?
                .into_std();
            private_file(&file, true)?;
        }
    }
    Ok(())
}
fn remove_tree(
    parent: &Dir,
    name: &OsStr,
    depth: usize,
    count: &mut usize,
    limit: usize,
    cancel: &AtomicBool,
) -> Result<(), BoundaryError> {
    cancelled(cancel)?;
    *count += 1;
    if *count > limit || depth > 64 {
        return Err(error(
            "plugin_view_limit",
            "Plugin view cleanup exceeds its bounded work limit",
        ));
    }
    let metadata = parent
        .symlink_metadata(Path::new(name))
        .map_err(|_| io_error())?;
    // SAFETY: geteuid does not dereference pointers.
    use cap_std::fs::MetadataExt as _;
    if metadata.uid() != unsafe { libc::geteuid() } {
        return Err(error(
            "plugin_view_permissions",
            "Plugin cleanup encountered another owner's entry",
        ));
    }
    if metadata.is_dir() {
        let directory = match open_directory(parent, Path::new(name), false) {
            Ok(directory) => directory,
            Err(_) => {
                let name = std::ffi::CString::new(name.as_bytes()).map_err(|_| io_error())?;
                let descriptor = parent.try_clone().map_err(|_| io_error())?.into_std_file();
                // SAFETY: the CString lives through the call and AT_SYMLINK_NOFOLLOW
                // prevents changing a replaced link's target during owned cleanup.
                if unsafe {
                    libc::fchmodat(
                        descriptor.as_raw_fd(),
                        name.as_ptr(),
                        0o700,
                        libc::AT_SYMLINK_NOFOLLOW,
                    )
                } != 0
                {
                    return Err(io_error());
                }
                open_directory(parent, Path::new(OsStr::from_bytes(name.as_bytes())), false)?
            }
        };
        directory
            .try_clone()
            .map_err(|_| io_error())?
            .into_std_file()
            .set_permissions(fs::Permissions::from_mode(0o700))
            .map_err(|_| io_error())?;
        for child in names(&directory, limit)? {
            remove_tree(&directory, &child, depth + 1, count, limit, cancel)?;
        }
        parent.remove_dir(Path::new(name)).map_err(|_| io_error())
    } else {
        parent.remove_file(Path::new(name)).map_err(|_| io_error())
    }
}
fn key(path: &Path) -> Result<String, BoundaryError> {
    let value = path
        .to_str()
        .ok_or_else(|| error("plugin_view_scope", "Plugin view host paths must be UTF-8"))?;
    if !path.is_absolute()
        || value.len() > 4096
        || value.chars().any(crate::text::is_terminal_control)
    {
        return Err(error(
            "plugin_view_scope",
            "Plugin view host path is invalid",
        ));
    }
    let composed: String = value.nfc().collect();
    Ok(unicase::UniCase::unicode(composed)
        .to_folded_case()
        .nfc()
        .collect())
}
pub(super) fn overlaps(a: &Path, b: &Path) -> Result<bool, BoundaryError> {
    let a = key(a)?;
    let b = key(b)?;
    let a = a.trim_end_matches('/');
    let b = b.trim_end_matches('/');
    Ok(a == b
        || a.strip_prefix(b).is_some_and(|tail| tail.starts_with('/'))
        || b.strip_prefix(a).is_some_and(|tail| tail.starts_with('/')))
}
pub(super) fn resolve(path: &Path) -> Result<PathBuf, BoundaryError> {
    let mut current = path.to_path_buf();
    let mut missing = Vec::new();
    loop {
        match fs::canonicalize(&current) {
            Ok(mut resolved) => {
                for part in missing.into_iter().rev() {
                    resolved.push(part);
                }
                return Ok(resolved);
            }
            Err(failure) if failure.kind() == std::io::ErrorKind::NotFound => {
                missing.push(current.file_name().ok_or_else(io_error)?.to_owned());
                current = current.parent().ok_or_else(io_error)?.to_owned();
            }
            Err(_) => return Err(io_error()),
        }
    }
}
