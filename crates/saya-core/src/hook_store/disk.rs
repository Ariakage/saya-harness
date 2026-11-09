use super::*;
use cap_std::fs::{Dir, OpenOptions, OpenOptionsExt};
use std::{
    fs::{self, File},
    os::{
        fd::AsRawFd,
        unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt as _},
    },
    path::PathBuf,
};

const DATABASE: &str = "hooks.sqlite3";
const OWNER: &str = ".owner.lock";

pub(super) struct Disk {
    root: Dir,
    path: PathBuf,
    owner: File,
    database: File,
    root_identity: (u64, u64),
    owner_identity: (u64, u64),
    database_identity: (u64, u64),
    limit: u64,
}

impl Disk {
    pub(super) fn open(path: &Path, limit: u64) -> Result<(Self, bool), BoundaryError> {
        if !path.is_absolute()
            || path.as_os_str().len() > 4096
            || path
                .to_str()
                .is_none_or(|text| text.chars().any(crate::text::is_terminal_control))
        {
            return Err(error(
                "hook_journal_permissions",
                "Hook journal path is invalid",
            ));
        }
        match fs::DirBuilder::new().mode(0o700).create(path) {
            Ok(()) => {}
            Err(failure) if failure.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(_) => {
                return Err(error(
                    "hook_journal_permissions",
                    "Private hook journal directory cannot be created",
                ));
            }
        }
        let file = fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK)
            .open(path)
            .map_err(|_| {
                error(
                    "hook_journal_permissions",
                    "Hook journal directory cannot be opened safely",
                )
            })?;
        let metadata = private_directory(&file)?;
        let path = fs::canonicalize(path).map_err(|_| {
            error(
                "hook_journal_permissions",
                "Hook journal path cannot be resolved",
            )
        })?;
        let named = fs::symlink_metadata(&path).map_err(|_| {
            error(
                "hook_journal_permissions",
                "Hook journal directory identity is unavailable",
            )
        })?;
        if metadata.dev() != named.dev() || metadata.ino() != named.ino() {
            return Err(error(
                "hook_journal_permissions",
                "Hook journal directory changed while opening",
            ));
        }
        let root = Dir::from_std_file(file);
        let mut owner_options = options();
        owner_options
            .read(true)
            .write(true)
            .create(true)
            .mode(0o600);
        let owner = root
            .open_with(OWNER, &owner_options)
            .map_err(|_| {
                error(
                    "hook_journal_permissions",
                    "Hook journal owner lock cannot be opened safely",
                )
            })?
            .into_std();
        let owner_metadata = private_file(&owner)?;
        // SAFETY: flock operates only on this owned regular-file descriptor.
        if unsafe { libc::flock(owner.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
            return Err(error(
                if std::io::Error::last_os_error().kind() == std::io::ErrorKind::WouldBlock {
                    "hook_journal_busy"
                } else {
                    "hook_journal_permissions"
                },
                "Hook journal owner lease is unavailable",
            ));
        }
        let mut create = options();
        create.read(true).write(true).create_new(true).mode(0o600);
        let (database, created) = match root.open_with(DATABASE, &create) {
            Ok(file) => (file.into_std(), true),
            Err(failure) if failure.kind() == std::io::ErrorKind::AlreadyExists => {
                let mut read = options();
                read.read(true).write(true);
                (
                    root.open_with(DATABASE, &read)
                        .map_err(|_| {
                            error(
                                "hook_journal_permissions",
                                "Hook journal database cannot be opened safely",
                            )
                        })?
                        .into_std(),
                    false,
                )
            }
            Err(_) => {
                return Err(error(
                    "hook_journal_permissions",
                    "Hook journal database cannot be created safely",
                ));
            }
        };
        let database_metadata = private_file(&database)?;
        if (!created && database_metadata.len() == 0) || database_metadata.len() > limit {
            return Err(error(
                "hook_journal_corrupt",
                "Hook journal database size is invalid",
            ));
        }
        let disk = Self {
            root,
            path,
            owner,
            database,
            root_identity: (metadata.dev(), metadata.ino()),
            owner_identity: (owner_metadata.dev(), owner_metadata.ino()),
            database_identity: (database_metadata.dev(), database_metadata.ino()),
            limit,
        };
        disk.check()?;
        disk.sync()?;
        Ok((disk, created))
    }

    pub(super) fn path(&self) -> &Path {
        &self.path
    }
    pub(super) fn database_path(&self) -> PathBuf {
        self.path.join(DATABASE)
    }
    pub(super) fn sync(&self) -> Result<(), BoundaryError> {
        self.root
            .try_clone()
            .map_err(|_| error("hook_journal", "Hook journal directory cannot be retained"))?
            .into_std_file()
            .sync_all()
            .map_err(|_| {
                error(
                    "hook_journal_unknown",
                    "Hook journal directory durability is unconfirmed",
                )
            })
    }
    pub(super) fn check(&self) -> Result<(), BoundaryError> {
        let root = self
            .root
            .try_clone()
            .map_err(|_| {
                error(
                    "hook_journal_permissions",
                    "Hook journal directory cannot be inspected",
                )
            })?
            .into_std_file();
        private_directory(&root)?;
        let named = fs::symlink_metadata(&self.path).map_err(|_| {
            error(
                "hook_journal_permissions",
                "Hook journal directory identity changed",
            )
        })?;
        if !named.is_dir() || (named.dev(), named.ino()) != self.root_identity {
            return Err(error(
                "hook_journal_permissions",
                "Hook journal directory identity changed",
            ));
        }
        self.check_file(OWNER, &self.owner, self.owner_identity)?;
        let database = self.check_file(DATABASE, &self.database, self.database_identity)?;
        if database.len() > self.limit {
            return Err(error(
                "hook_journal_full",
                "Hook journal database exceeds its byte limit",
            ));
        }
        for name in [
            "hooks.sqlite3-wal",
            "hooks.sqlite3-shm",
            "hooks.sqlite3-journal",
        ] {
            match self.root.symlink_metadata(name) {
                Ok(_) => {
                    let mut read = options();
                    read.read(true);
                    let file = self
                        .root
                        .open_with(name, &read)
                        .map_err(|_| {
                            error(
                                "hook_journal_permissions",
                                "Hook journal sidecar cannot be opened safely",
                            )
                        })?
                        .into_std();
                    let metadata = private_file(&file)?;
                    if metadata.len() > self.limit.saturating_add(1024 * 1024) {
                        return Err(error(
                            "hook_journal_full",
                            "Hook journal sidecar exceeds its byte limit",
                        ));
                    }
                }
                Err(failure) if failure.kind() == std::io::ErrorKind::NotFound => {}
                Err(_) => {
                    return Err(error(
                        "hook_journal_permissions",
                        "Hook journal sidecar identity is unavailable",
                    ));
                }
            }
        }
        Ok(())
    }
    fn check_file(
        &self,
        name: &str,
        file: &File,
        expected: (u64, u64),
    ) -> Result<fs::Metadata, BoundaryError> {
        use cap_std::fs::MetadataExt as _;
        let metadata = private_file(file)?;
        let named = self.root.symlink_metadata(name).map_err(|_| {
            error(
                "hook_journal_permissions",
                "Hook journal file identity changed",
            )
        })?;
        if !named.is_file()
            || (named.dev(), named.ino()) != expected
            || (metadata.dev(), metadata.ino()) != expected
        {
            return Err(error(
                "hook_journal_permissions",
                "Hook journal file identity changed",
            ));
        }
        Ok(metadata)
    }
}

fn options() -> OpenOptions {
    let mut options = OpenOptions::new();
    options.custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK);
    options
}
fn private_directory(file: &File) -> Result<fs::Metadata, BoundaryError> {
    let metadata = file.metadata().map_err(|_| {
        error(
            "hook_journal_permissions",
            "Hook journal metadata is unavailable",
        )
    })?;
    // SAFETY: geteuid takes no pointers and retains no ownership.
    if !metadata.is_dir()
        || metadata.uid() != unsafe { libc::geteuid() }
        || metadata.mode() & 0o077 != 0
    {
        return Err(error(
            "hook_journal_permissions",
            "Hook journal directory must be owned and private",
        ));
    }
    Ok(metadata)
}
fn private_file(file: &File) -> Result<fs::Metadata, BoundaryError> {
    let metadata = file.metadata().map_err(|_| {
        error(
            "hook_journal_permissions",
            "Hook journal metadata is unavailable",
        )
    })?;
    // SAFETY: geteuid takes no pointers and retains no ownership.
    if !metadata.is_file()
        || metadata.nlink() != 1
        || metadata.uid() != unsafe { libc::geteuid() }
        || metadata.mode() & 0o077 != 0
    {
        return Err(error(
            "hook_journal_permissions",
            "Hook journal files must be owned private regular files",
        ));
    }
    Ok(metadata)
}
