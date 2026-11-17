use super::*;
use cap_std::fs::{Dir, DirBuilder, DirBuilderExt, OpenOptions, OpenOptionsExt};
use std::{
    fs::{self, File},
    io::{Read, Write},
    os::{
        fd::AsRawFd,
        unix::fs::{DirBuilderExt as _, MetadataExt, OpenOptionsExt as _, PermissionsExt},
    },
    time::{Duration, Instant},
};

const MAX_LOCK_BYTES: usize = 256 * 1024;
const MAX_DISK_ENTRIES: usize = 8192 + 16;

pub(super) struct Disk {
    root: Dir,
    path: PathBuf,
    identity: (u64, u64),
    #[cfg(test)]
    pub(super) fail_publication_sync: std::sync::atomic::AtomicBool,
}

impl Disk {
    pub(super) fn open(path: &Path) -> Result<Self, BoundaryError> {
        match fs::DirBuilder::new().mode(0o700).create(path) {
            Ok(()) => {}
            Err(failure) if failure.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(_) => {
                return Err(error(
                    "plugin_io",
                    "Private plugin directory cannot be created",
                ));
            }
        }
        let file = fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK)
            .open(path)
            .map_err(|_| {
                error(
                    "plugin_permissions",
                    "Private plugin directory is unavailable or linked",
                )
            })?;
        let metadata = private_directory(&file, false)?;
        let path = fs::canonicalize(path)
            .map_err(|_| error("plugin_io", "Plugin directory cannot be resolved"))?;
        let named = fs::symlink_metadata(&path)
            .map_err(|_| error("plugin_io", "Plugin directory identity is unavailable"))?;
        if metadata.dev() != named.dev() || metadata.ino() != named.ino() {
            return Err(error(
                "plugin_permissions",
                "Plugin directory changed while opening",
            ));
        }
        let disk = Self {
            root: Dir::from_std_file(file),
            path,
            identity: (metadata.dev(), metadata.ino()),
            #[cfg(test)]
            fail_publication_sync: std::sync::atomic::AtomicBool::new(false),
        };
        let _lock = disk.lock_index()?;
        for name in ["objects", "staging", "leases"] {
            create_directory(&disk.root, name)?;
        }
        sync_directory(&disk.root)?;
        Ok(disk)
    }

    pub(super) fn path(&self) -> &Path {
        &self.path
    }

    pub(super) fn lock_index(&self) -> Result<File, BoundaryError> {
        self.check_root()?;
        let mut options = file_options(true);
        options.write(true).create(true).mode(0o600);
        let file = self
            .root
            .open_with("index.lock", &options)
            .map_err(|_| error("plugin_permissions", "Plugin index lock is unavailable"))?
            .into_std();
        private_file(&file, false)?;
        let deadline = Instant::now() + Duration::from_secs(10);
        while !try_lock(&file)? {
            if Instant::now() >= deadline {
                return Err(error("plugin_busy", "Plugin index lock timed out"));
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        self.check_root()?;
        let named = self
            .root
            .symlink_metadata("index.lock")
            .map_err(|_| error("plugin_permissions", "Plugin index lock changed"))?;
        use cap_std::fs::MetadataExt as _;
        let actual = private_file(&file, false)?;
        if actual.dev() != named.dev() || actual.ino() != named.ino() {
            return Err(error("plugin_permissions", "Plugin index lock changed"));
        }
        Ok(file)
    }

    fn check_root(&self) -> Result<(), BoundaryError> {
        let named = fs::symlink_metadata(&self.path)
            .map_err(|_| error("plugin_permissions", "Plugin root identity is unavailable"))?;
        if !named.is_dir() || named.dev() != self.identity.0 || named.ino() != self.identity.1 {
            return Err(error("plugin_permissions", "Plugin root identity changed"));
        }
        private_directory(
            &self.root.try_clone().map_err(io_error)?.into_std_file(),
            false,
        )?;
        Ok(())
    }

    pub(super) fn read_index(&self) -> Result<Option<Index>, BoundaryError> {
        let mut file = match self.root.open_with("index.json", &file_options(true)) {
            Ok(file) => file.into_std(),
            Err(failure) if failure.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(_) => {
                return Err(error(
                    "plugin_permissions",
                    "Plugin index cannot be opened safely",
                ));
            }
        };
        private_file(&file, false)?;
        let bytes = read_bounded(&mut file, MAX_INDEX_BYTES)?;
        serde_json::from_slice(&bytes)
            .map(Some)
            .map_err(|_| error("plugin_corrupt", "Plugin index cannot be decoded"))
    }

    pub(super) fn write_index(&self, index: &Index) -> Result<(), BoundaryError> {
        let bytes = serde_json::to_vec(index)
            .map_err(|_| error("plugin_corrupt", "Plugin index cannot be encoded"))?;
        if bytes.len() > MAX_INDEX_BYTES {
            return Err(error("plugin_limit", "Plugin index exceeds its byte limit"));
        }
        atomic_write(&self.root, "index.json", &bytes)?;
        #[cfg(test)]
        if self
            .fail_publication_sync
            .swap(false, std::sync::atomic::Ordering::SeqCst)
        {
            return Err(error(
                "plugin_commit_unknown",
                "Plugin index publication durability is unknown; inspect current state",
            ));
        }
        sync_directory(&self.root).map_err(|_| {
            error(
                "plugin_commit_unknown",
                "Plugin index publication durability is unknown; inspect current state",
            )
        })
    }

    pub(super) fn stage_object(&self, package: &VerifiedPackage) -> Result<(), BoundaryError> {
        let objects = open_directory(&self.root, "objects", false)?;
        match open_directory(&objects, package.digest(), false) {
            Ok(_) => {
                let existing = self.load_package(package.digest())?;
                if existing.digest() != package.digest() {
                    return Err(error(
                        "plugin_corrupt",
                        "Existing plugin object identity differs",
                    ));
                }
                return Ok(());
            }
            Err(_) if objects.symlink_metadata(package.digest()).is_ok() => {
                return Err(error("plugin_permissions", "Plugin object path is unsafe"));
            }
            Err(_) => {}
        }
        let staging = open_directory(&self.root, "staging", false)?;
        let name = format!("stage-{}", Uuid::now_v7());
        create_directory(&staging, &name)?;
        let result = self.write_stage(&staging, &name, &objects, package);
        if result.is_err() {
            let _ = remove_tree(&staging, &name, &mut 0, 0);
        }
        result
    }

    fn write_stage(
        &self,
        staging: &Dir,
        name: &str,
        objects: &Dir,
        package: &VerifiedPackage,
    ) -> Result<(), BoundaryError> {
        let candidate = open_directory(staging, name, false)?;
        let content = create_directory(&candidate, "package")?;
        for entry in package.files() {
            let mut directory = content.try_clone().map_err(io_error)?;
            let components: Vec<_> = entry.path().split('/').collect();
            let (filename, parents) = components
                .split_last()
                .ok_or_else(|| error("plugin_corrupt", "Sealed package path is empty"))?;
            for component in parents {
                directory = create_directory(&directory, component)?;
            }
            let mut options = file_options(false);
            options.write(true).create_new(true).mode(0o600);
            let mut file = directory
                .open_with(filename, &options)
                .map_err(io_error)?
                .into_std();
            file.write_all(entry.bytes()).map_err(io_error)?;
            file.set_permissions(fs::Permissions::from_mode(0o400))
                .map_err(io_error)?;
            file.sync_all().map_err(io_error)?;
        }
        seal_directories(&content, 0, &mut 0)?;
        let inspected = crate::plugin_manifest::inspect_package_directory(&content)?;
        if inspected.digest() != package.digest() || inspected.manifest() != package.manifest() {
            return Err(error(
                "plugin_corrupt",
                "Sealed installation differs from the verified snapshot",
            ));
        }
        sync_directory(&candidate)?;
        staging
            .rename(name, objects, package.digest())
            .map_err(io_error)?;
        sync_directory(objects).map_err(|_| {
            error(
                "plugin_commit_unknown",
                "Plugin object publication durability is unknown",
            )
        })?;
        sync_directory(staging).map_err(|_| {
            error(
                "plugin_commit_unknown",
                "Plugin staging publication durability is unknown",
            )
        })
    }

    fn object_directory(&self, digest: &str) -> Result<Dir, BoundaryError> {
        if !valid_digest(digest) {
            return Err(error("plugin_identity", "Plugin digest is invalid"));
        }
        open_directory(
            &open_directory(&self.root, "objects", false)?,
            digest,
            false,
        )
    }
    pub(super) fn load_package(&self, digest: &str) -> Result<VerifiedPackage, BoundaryError> {
        let object = self.object_directory(digest)?;
        let package = open_directory(&object, "package", true)?;
        check_readonly_tree(&package, 0, &mut 0)?;
        Ok(crate::plugin_manifest::inspect_package_directory(&package)?)
    }
    pub(super) fn write_lock(&self, digest: &str, bytes: &[u8]) -> Result<(), BoundaryError> {
        if bytes.len() > MAX_LOCK_BYTES {
            return Err(error(
                "plugin_limit",
                "Plugin dependency lock exceeds its byte limit",
            ));
        }
        let object = self.object_directory(digest)?;
        atomic_write(&object, "plugin.lock", bytes)?;
        sync_directory(&object).map_err(|_| {
            error(
                "plugin_commit_unknown",
                "Plugin dependency lock publication durability is unknown",
            )
        })
    }
    pub(super) fn read_lock(&self, digest: &str) -> Result<Vec<u8>, BoundaryError> {
        let object = self.object_directory(digest)?;
        let mut file = object
            .open_with("plugin.lock", &file_options(true))
            .map_err(io_error)?
            .into_std();
        private_file(&file, false)?;
        read_bounded(&mut file, MAX_LOCK_BYTES)
    }
    pub(super) fn package_path(&self, digest: &str) -> PathBuf {
        self.path.join("objects").join(digest).join("package")
    }

    pub(super) fn create_lease(&self, lease_id: &str) -> Result<File, BoundaryError> {
        let name = lease_name(lease_id)?;
        let directory = open_directory(&self.root, "leases", false)?;
        let mut options = file_options(true);
        options.write(true).create_new(true).mode(0o600);
        let file = directory
            .open_with(&name, &options)
            .map_err(io_error)?
            .into_std();
        private_file(&file, false)?;
        if !try_lock(&file)? {
            return Err(error(
                "plugin_busy",
                "New plugin lease is unexpectedly owned",
            ));
        }
        file.sync_all().map_err(io_error)?;
        sync_directory(&directory)?;
        Ok(file)
    }
    pub(super) fn lease_active(&self, lease_id: &str) -> Result<bool, BoundaryError> {
        let name = lease_name(lease_id)?;
        let directory = open_directory(&self.root, "leases", false)?;
        let file = match directory.open_with(&name, &file_options(true)) {
            Ok(file) => file.into_std(),
            Err(failure) if failure.kind() == std::io::ErrorKind::NotFound => return Ok(false),
            Err(_) => return Err(error("plugin_permissions", "Plugin lease file is unsafe")),
        };
        private_file(&file, false)?;
        // Dropping this independently opened descriptor releases a successful
        // probe lock. The live owner's descriptor is never duplicated here.
        Ok(!try_lock(&file)?)
    }
    pub(super) fn remove_lease(&self, lease_id: &str) -> Result<(), BoundaryError> {
        let name = lease_name(lease_id)?;
        let directory = open_directory(&self.root, "leases", false)?;
        match directory.remove_file(name) {
            Ok(()) => sync_directory(&directory),
            Err(failure) if failure.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(_) => Err(error("plugin_io", "Plugin lease file cannot be removed")),
        }
    }
    pub(super) fn remove_object(&self, digest: &str) -> Result<(), BoundaryError> {
        if !valid_digest(digest) {
            return Err(error("plugin_identity", "Plugin digest is invalid"));
        }
        let objects = open_directory(&self.root, "objects", false)?;
        if objects
            .symlink_metadata(digest)
            .is_err_and(|failure| failure.kind() == std::io::ErrorKind::NotFound)
        {
            return Ok(());
        }
        remove_tree(&objects, digest, &mut 0, 0)?;
        sync_directory(&objects)
    }
    pub(super) fn cleanup_staging(&self) -> Result<(), BoundaryError> {
        let staging = open_directory(&self.root, "staging", false)?;
        for name in names(&staging)? {
            if name.strip_prefix("stage-").is_none_or(|id| !valid_uuid(id)) {
                return Err(error(
                    "plugin_corrupt",
                    "Plugin staging has an unknown entry",
                ));
            }
            remove_tree(&staging, &name, &mut 0, 0)?;
        }
        sync_directory(&staging)
    }
    pub(super) fn cleanup_unindexed(
        &self,
        retained: &BTreeSet<String>,
    ) -> Result<(), BoundaryError> {
        let objects = open_directory(&self.root, "objects", false)?;
        for name in names(&objects)? {
            if !valid_digest(&name) {
                return Err(error(
                    "plugin_corrupt",
                    "Plugin object directory has an unknown entry",
                ));
            }
            if !retained.contains(&name) {
                remove_tree(&objects, &name, &mut 0, 0)?;
            }
        }
        sync_directory(&objects)
    }
    pub(super) fn cleanup_auxiliary(
        &self,
        retained_leases: &BTreeSet<String>,
    ) -> Result<(), BoundaryError> {
        let leases = open_directory(&self.root, "leases", false)?;
        for name in names(&leases)? {
            let id = name
                .strip_suffix(".lease")
                .filter(|id| valid_uuid(id))
                .ok_or_else(|| {
                    error(
                        "plugin_corrupt",
                        "Plugin lease directory has an unknown entry",
                    )
                })?;
            if !retained_leases.contains(id) {
                if self.lease_active(id)? {
                    return Err(error(
                        "plugin_corrupt",
                        "An unindexed plugin lease still has an owner",
                    ));
                }
                leases.remove_file(&name).map_err(io_error)?;
            }
        }
        sync_directory(&leases)?;
        for name in names(&self.root)? {
            if name.strip_prefix(".tmp-").is_some_and(valid_uuid) {
                let file = self
                    .root
                    .open_with(&name, &file_options(true))
                    .map_err(io_error)?
                    .into_std();
                private_file(&file, false)?;
                self.root.remove_file(&name).map_err(io_error)?;
            }
        }
        sync_directory(&self.root)
    }
}

fn io_error(_: std::io::Error) -> BoundaryError {
    error("plugin_io", "Private plugin filesystem operation failed")
}
fn file_options(read: bool) -> OpenOptions {
    let mut options = OpenOptions::new();
    options
        .read(read)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK);
    options
}
fn private_directory(file: &File, readonly: bool) -> Result<fs::Metadata, BoundaryError> {
    let metadata = file.metadata().map_err(io_error)?;
    // SAFETY: geteuid has no pointer arguments or ownership requirements.
    if !metadata.is_dir()
        || metadata.uid() != unsafe { libc::geteuid() }
        || metadata.mode() & 0o077 != 0
        || (readonly && metadata.mode() & 0o222 != 0)
    {
        return Err(error(
            "plugin_permissions",
            "Plugin directory must be private, owned, and use the required write policy",
        ));
    }
    Ok(metadata)
}
fn private_file(file: &File, readonly: bool) -> Result<fs::Metadata, BoundaryError> {
    let metadata = file.metadata().map_err(io_error)?;
    // SAFETY: geteuid has no pointer arguments or ownership requirements.
    if !metadata.is_file()
        || metadata.uid() != unsafe { libc::geteuid() }
        || metadata.nlink() != 1
        || metadata.mode() & 0o077 != 0
        || (readonly && metadata.mode() & 0o222 != 0)
    {
        return Err(error(
            "plugin_permissions",
            "Plugin files must be private, owned regular files with the required write policy",
        ));
    }
    Ok(metadata)
}
fn open_directory(parent: &Dir, name: &str, readonly: bool) -> Result<Dir, BoundaryError> {
    let mut options = file_options(true);
    options.custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK);
    let file = parent
        .open_with(name, &options)
        .map_err(io_error)?
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
        Err(_) => {
            return Err(error(
                "plugin_io",
                "Private plugin child directory cannot be created",
            ));
        }
    }
    open_directory(parent, name, false)
}
fn sync_directory(directory: &Dir) -> Result<(), BoundaryError> {
    directory
        .try_clone()
        .map_err(io_error)?
        .into_std_file()
        .sync_all()
        .map_err(io_error)
}
fn atomic_write(directory: &Dir, name: &str, bytes: &[u8]) -> Result<(), BoundaryError> {
    let temporary = format!(".tmp-{}", Uuid::now_v7());
    let mut options = file_options(false);
    options.write(true).create_new(true).mode(0o600);
    let result = (|| {
        let mut file = directory
            .open_with(&temporary, &options)
            .map_err(io_error)?
            .into_std();
        private_file(&file, false)?;
        file.write_all(bytes).map_err(io_error)?;
        file.sync_all().map_err(io_error)?;
        directory
            .rename(&temporary, directory, name)
            .map_err(io_error)
    })();
    if result.is_err() {
        let _ = directory.remove_file(&temporary);
    }
    result
}
fn read_bounded(file: &mut File, limit: usize) -> Result<Vec<u8>, BoundaryError> {
    let mut bytes = Vec::new();
    file.take(limit as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(io_error)?;
    if bytes.len() > limit {
        return Err(error(
            "plugin_limit",
            "Plugin metadata exceeds its byte limit",
        ));
    }
    Ok(bytes)
}
fn try_lock(file: &File) -> Result<bool, BoundaryError> {
    // SAFETY: flock operates only on this owned regular-file descriptor.
    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 {
        return Ok(true);
    }
    match std::io::Error::last_os_error().kind() {
        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::Interrupted => Ok(false),
        _ => Err(error("plugin_io", "Plugin file locking failed")),
    }
}
fn valid_uuid(id: &str) -> bool {
    Uuid::parse_str(id).is_ok_and(|uuid| uuid.to_string() == id)
}
fn lease_name(id: &str) -> Result<String, BoundaryError> {
    if !valid_uuid(id) {
        return Err(error("plugin_identity", "Plugin lease ID is invalid"));
    }
    Ok(format!("{id}.lease"))
}
fn names(directory: &Dir) -> Result<Vec<String>, BoundaryError> {
    let mut names = Vec::new();
    for entry in directory.entries().map_err(io_error)? {
        let name = entry
            .map_err(io_error)?
            .file_name()
            .into_string()
            .map_err(|_| error("plugin_corrupt", "Plugin internal entry name is not UTF-8"))?;
        names.push(name);
        if names.len() > MAX_DISK_ENTRIES {
            return Err(error(
                "plugin_limit",
                "Plugin directory entry count exceeds its limit",
            ));
        }
    }
    names.sort();
    Ok(names)
}
fn count_entry(count: &mut usize, depth: usize) -> Result<(), BoundaryError> {
    *count += 1;
    if *count > MAX_DISK_ENTRIES || depth > 18 {
        return Err(error(
            "plugin_limit",
            "Plugin tree traversal exceeds its limit",
        ));
    }
    Ok(())
}
fn seal_directories(directory: &Dir, depth: usize, count: &mut usize) -> Result<(), BoundaryError> {
    count_entry(count, depth)?;
    for name in names(directory)? {
        let metadata = directory.symlink_metadata(&name).map_err(io_error)?;
        if metadata.is_dir() {
            seal_directories(&open_directory(directory, &name, false)?, depth + 1, count)?;
        } else {
            count_entry(count, depth + 1)?;
        }
    }
    let file = directory.try_clone().map_err(io_error)?.into_std_file();
    file.set_permissions(fs::Permissions::from_mode(0o500))
        .map_err(io_error)?;
    file.sync_all().map_err(io_error)
}
fn check_readonly_tree(
    directory: &Dir,
    depth: usize,
    count: &mut usize,
) -> Result<(), BoundaryError> {
    count_entry(count, depth)?;
    for name in names(directory)? {
        let metadata = directory.symlink_metadata(&name).map_err(io_error)?;
        if metadata.is_dir() {
            check_readonly_tree(&open_directory(directory, &name, true)?, depth + 1, count)?;
        } else {
            count_entry(count, depth + 1)?;
            let file = directory
                .open_with(&name, &file_options(true))
                .map_err(io_error)?
                .into_std();
            private_file(&file, true)?;
        }
    }
    Ok(())
}
fn remove_tree(
    parent: &Dir,
    name: &str,
    count: &mut usize,
    depth: usize,
) -> Result<(), BoundaryError> {
    count_entry(count, depth)?;
    let metadata = parent.symlink_metadata(name).map_err(io_error)?;
    if metadata.is_dir() {
        let directory = open_directory(parent, name, false)?;
        directory
            .try_clone()
            .map_err(io_error)?
            .into_std_file()
            .set_permissions(fs::Permissions::from_mode(0o700))
            .map_err(io_error)?;
        for child in names(&directory)? {
            remove_tree(&directory, &child, count, depth + 1)?;
        }
        parent.remove_dir(name).map_err(io_error)
    } else {
        // Unlink the directory entry itself, including hostile links, without
        // following it or changing the target's permissions.
        parent.remove_file(name).map_err(io_error)
    }
}

pub(super) fn workspace_scope(path: &Path) -> Result<PluginScope, BoundaryError> {
    let canonical = fs::canonicalize(path)
        .map_err(|_| error("plugin_scope", "Workspace cannot be resolved"))?;
    let file = fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
        .open(&canonical)
        .map_err(|_| error("plugin_scope", "Workspace identity cannot be opened"))?;
    let metadata = file.metadata().map_err(io_error)?;
    let scope = PluginScope::Workspace {
        path: canonical,
        device: metadata.dev(),
        inode: metadata.ino(),
    };
    scope.validate()?;
    Ok(scope)
}
