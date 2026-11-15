use super::{MAX_IDENTITY_FILE_BYTES, identity_error, valid_sha256};
use crate::contract::BoundaryError;
use cap_std::fs::{Dir, Metadata, MetadataExt, OpenOptions, OpenOptionsExt};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, VecDeque},
    fs::File,
    io::{Read, Seek},
    path::{Component, Path},
};

pub(crate) const MAX_TREE_ENTRIES: u64 = 65_536;
pub(crate) const MAX_TREE_BYTES: u64 = 4 * 1024 * 1024 * 1024;
const MAX_DEPTH: usize = 128;
const MAX_PATH_BYTES: usize = 4096;

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct TreeIdentity {
    pub(crate) sha256: String,
    pub(crate) entries: u64,
    pub(crate) bytes: u64,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct InstallationTrees {
    pub(crate) node: TreeIdentity,
    pub(crate) worker: TreeIdentity,
    pub(crate) chromium: TreeIdentity,
}

impl InstallationTrees {
    pub(crate) fn validate(&self) -> Result<(), BoundaryError> {
        for tree in [&self.node, &self.worker, &self.chromium] {
            if !valid_sha256(&tree.sha256)
                || tree.entries == 0
                || tree.entries > MAX_TREE_ENTRIES
                || tree.bytes > MAX_TREE_BYTES
            {
                return Err(identity_error(
                    "Browser installation tree identity is invalid",
                ));
            }
        }
        Ok(())
    }
}

#[derive(Clone)]
enum Member {
    Directory,
    File,
    Link(String),
}

fn changed() -> BoundaryError {
    identity_error("Browser installation tree changed or is unsafe")
}

fn metadata_identity(metadata: &Metadata) -> (u64, u64, u64, u32, u64, i64, i64, i64, i64) {
    (
        metadata.dev(),
        metadata.ino(),
        metadata.len(),
        metadata.mode(),
        metadata.nlink(),
        metadata.mtime(),
        metadata.mtime_nsec(),
        metadata.ctime(),
        metadata.ctime_nsec(),
    )
}

fn open_options(directory: bool) -> OpenOptions {
    let mut options = OpenOptions::new();
    options.read(true).custom_flags(
        libc::O_NOFOLLOW
            | libc::O_CLOEXEC
            | libc::O_NONBLOCK
            | if directory { libc::O_DIRECTORY } else { 0 },
    );
    options
}

fn open_root(root: &Path) -> Result<Dir, BoundaryError> {
    if !root.is_absolute() {
        return Err(changed());
    }
    let mut directory = Dir::from_std_file(super::open_directory(Path::new("/"))?);
    for component in root.components() {
        match component {
            Component::RootDir => {}
            Component::Normal(name) => {
                directory = Dir::from_std_file(
                    directory
                        .open_with(name, &open_options(true))
                        .map_err(|_| changed())?
                        .into_std(),
                );
            }
            _ => return Err(changed()),
        }
    }
    Ok(directory)
}

fn valid_text(value: &str) -> bool {
    !value.is_empty() && value.len() <= MAX_PATH_BYTES && !value.chars().any(char::is_control)
}

struct Scanner<'a> {
    hash: Sha256,
    entries: u64,
    bytes: u64,
    device: u64,
    members: BTreeMap<String, Member>,
    checkpoint: &'a mut dyn FnMut() -> Result<(), BoundaryError>,
}

impl Scanner<'_> {
    fn record(&mut self, kind: u8, path: &str, mode: u32) -> Result<(), BoundaryError> {
        (self.checkpoint)()?;
        if self.entries >= MAX_TREE_ENTRIES || path.len() > MAX_PATH_BYTES {
            return Err(identity_error(
                "Browser installation tree exceeds its entry limit",
            ));
        }
        self.entries += 1;
        self.hash.update([kind]);
        self.text(path);
        self.hash.update((mode & 0o7777).to_be_bytes());
        Ok(())
    }

    fn text(&mut self, value: &str) {
        self.hash.update((value.len() as u32).to_be_bytes());
        self.hash.update(value.as_bytes());
    }

    fn safe_metadata(&self, metadata: &Metadata) -> Result<(), BoundaryError> {
        let mode = metadata.mode();
        if metadata.dev() != self.device
            || mode & 0o022 != 0
            || mode & 0o6000 != 0
            || mode & 0o400 == 0
            || (metadata.is_dir() && mode & 0o100 == 0)
        {
            return Err(changed());
        }
        Ok(())
    }

    fn file(&mut self, mut file: File, before: &Metadata, path: &str) -> Result<(), BoundaryError> {
        let opened = Metadata::from_file(&file).map_err(|_| changed())?;
        if metadata_identity(before) != metadata_identity(&opened)
            || !opened.is_file()
            || opened.nlink() != 1
            || opened.len() > MAX_IDENTITY_FILE_BYTES
            || opened.len() > MAX_TREE_BYTES.saturating_sub(self.bytes)
        {
            return Err(changed());
        }
        self.safe_metadata(&opened)?;
        let mut hashes = [Sha256::new(), Sha256::new()];
        let mut buffer = [0; 64 * 1024];
        for hash in &mut hashes {
            file.rewind().map_err(|_| changed())?;
            let mut size = 0u64;
            loop {
                (self.checkpoint)()?;
                let count = file.read(&mut buffer).map_err(|_| changed())?;
                if count == 0 {
                    break;
                }
                size = size.saturating_add(count as u64);
                if size > opened.len() {
                    return Err(changed());
                }
                hash.update(&buffer[..count]);
            }
            if size != opened.len() {
                return Err(changed());
            }
        }
        let [first, second] = hashes.map(|hash| hash.finalize());
        let after = Metadata::from_file(&file).map_err(|_| changed())?;
        if first != second || metadata_identity(&opened) != metadata_identity(&after) {
            return Err(changed());
        }
        self.record(b'F', path, opened.mode())?;
        self.hash.update(opened.len().to_be_bytes());
        self.hash.update(first);
        self.bytes += opened.len();
        self.members.insert(path.to_owned(), Member::File);
        Ok(())
    }

    fn directory(
        &mut self,
        directory: &Dir,
        path: &str,
        depth: usize,
    ) -> Result<(), BoundaryError> {
        (self.checkpoint)()?;
        if depth > MAX_DEPTH {
            return Err(identity_error(
                "Browser installation tree exceeds its depth limit",
            ));
        }
        let before = directory.dir_metadata().map_err(|_| changed())?;
        self.safe_metadata(&before)?;
        self.record(b'D', path, before.mode())?;
        self.members.insert(path.to_owned(), Member::Directory);
        let mut names = Vec::new();
        for entry in directory.entries().map_err(|_| changed())? {
            (self.checkpoint)()?;
            let name = entry.map_err(|_| changed())?.file_name();
            let name = name.to_str().ok_or_else(changed)?.to_owned();
            if !valid_text(&name)
                || name == "."
                || name == ".."
                || names.len() as u64 >= MAX_TREE_ENTRIES.saturating_sub(self.entries)
            {
                return Err(changed());
            }
            names.push(name);
        }
        names.sort();
        for name in names {
            (self.checkpoint)()?;
            let child_path = if path.is_empty() {
                name.clone()
            } else {
                format!("{path}/{name}")
            };
            if child_path.len() > MAX_PATH_BYTES {
                return Err(changed());
            }
            let metadata = directory.symlink_metadata(&name).map_err(|_| changed())?;
            if metadata.is_dir() {
                let child = Dir::from_std_file(
                    directory
                        .open_with(&name, &open_options(true))
                        .map_err(|_| changed())?
                        .into_std(),
                );
                if metadata_identity(&metadata)
                    != metadata_identity(&child.dir_metadata().map_err(|_| changed())?)
                {
                    return Err(changed());
                }
                self.directory(&child, &child_path, depth + 1)?;
            } else if metadata.is_file() {
                let child = directory
                    .open_with(&name, &open_options(false))
                    .map_err(|_| changed())?
                    .into_std();
                self.file(child, &metadata, &child_path)?;
            } else if metadata.is_symlink() {
                let target = directory.read_link_contents(&name).map_err(|_| changed())?;
                let target = target.to_str().ok_or_else(changed)?;
                if !valid_text(target)
                    || target.starts_with('/')
                    || metadata.dev() != self.device
                    || metadata.nlink() != 1
                {
                    return Err(changed());
                }
                if metadata_identity(&metadata)
                    != metadata_identity(&directory.symlink_metadata(&name).map_err(|_| changed())?)
                {
                    return Err(changed());
                }
                self.record(b'L', &child_path, 0o777)?;
                self.text(target);
                self.members
                    .insert(child_path, Member::Link(target.to_owned()));
            } else {
                return Err(changed());
            }
        }
        let after = directory.dir_metadata().map_err(|_| changed())?;
        if metadata_identity(&before) != metadata_identity(&after) {
            return Err(changed());
        }
        Ok(())
    }

    fn validate_links(&mut self) -> Result<(), BoundaryError> {
        for (path, member) in &self.members {
            let Member::Link(target) = member else {
                continue;
            };
            (self.checkpoint)()?;
            let mut components = path.split('/').map(str::to_owned).collect::<Vec<_>>();
            components.pop();
            let mut pending = target
                .split('/')
                .map(str::to_owned)
                .collect::<VecDeque<_>>();
            let mut links = 0;
            while let Some(component) = pending.pop_front() {
                (self.checkpoint)()?;
                match component.as_str() {
                    "" | "." => continue,
                    ".." => {
                        if components.pop().is_none() {
                            return Err(changed());
                        }
                    }
                    _ => components.push(component),
                }
                match self
                    .members
                    .get(&components.join("/"))
                    .ok_or_else(changed)?
                {
                    Member::Link(next) => {
                        links += 1;
                        if links > 40 {
                            return Err(changed());
                        }
                        components.pop();
                        for component in next.split('/').rev() {
                            pending.push_front(component.to_owned());
                        }
                    }
                    Member::File if !pending.is_empty() => return Err(changed()),
                    _ => {}
                }
            }
        }
        Ok(())
    }
}

pub(crate) fn verify_tree(
    root: &Path,
    expected: &TreeIdentity,
    checkpoint: &mut dyn FnMut() -> Result<(), BoundaryError>,
) -> Result<(), BoundaryError> {
    checkpoint()?;
    let directory = open_root(root)?;
    let mut hash = Sha256::new();
    hash.update(b"saya-browser-installation-v2\0");
    let mut scanner = Scanner {
        hash,
        entries: 0,
        bytes: 0,
        device: directory.dir_metadata().map_err(|_| changed())?.dev(),
        members: BTreeMap::new(),
        checkpoint,
    };
    scanner.directory(&directory, "", 0)?;
    scanner.validate_links()?;
    let actual = TreeIdentity {
        sha256: scanner
            .hash
            .finalize()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect(),
        entries: scanner.entries,
        bytes: scanner.bytes,
    };
    if &actual != expected {
        return Err(identity_error(
            "Browser installation tree differs from its manifest",
        ));
    }
    Ok(())
}
