//! Sealed package snapshots and the extraction budget shared with installers.
use super::{
    MANIFEST_NAME, MAX_MANIFEST_BYTES, Manifest, Result, digest_hex, invalid, parse_manifest,
    validate_relative_path,
};
use cap_std::fs::{Dir, OpenOptions, OpenOptionsExt};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs,
    io::Read,
    os::unix::fs::{MetadataExt, OpenOptionsExt as StdOpenOptionsExt},
    path::Path,
};
use unicode_normalization::UnicodeNormalization;

pub const MAX_PACKAGE_FILES: usize = 4096;
pub const MAX_PACKAGE_ENTRIES: usize = 8192;
pub const MAX_PACKAGE_BYTES: u64 = 64 * 1024 * 1024;
pub const MAX_PACKAGE_FILE_BYTES: u64 = 16 * 1024 * 1024;
pub const TREE_DIGEST_ALGORITHM: &str = "saya-tree-sha256-v1";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Directory,
    File,
}
#[derive(Debug, Clone)]
struct BudgetEntry {
    original: String,
    kind: Kind,
    declared: bool,
}

/// Incremental path and expansion quota for a package candidate.
/// Reserve before creating or expanding an archive entry. Failed reservations
/// leave the budget unchanged. Reject archive links and special entry types in
/// the extractor before invoking these regular-file/directory methods.
#[derive(Debug, Default)]
pub struct FileBudget {
    entries: BTreeMap<String, BudgetEntry>,
    files: usize,
    bytes: u64,
}
impl FileBudget {
    pub fn reserve_file(&mut self, path: &str, bytes: u64) -> Result<()> {
        self.reserve(path, Kind::File, bytes)
    }
    pub fn reserve_directory(&mut self, path: &str) -> Result<()> {
        self.reserve(path, Kind::Directory, 0)
    }
    pub fn file_count(&self) -> usize {
        self.files
    }
    pub fn expanded_bytes(&self) -> u64 {
        self.bytes
    }

    fn reserve(&mut self, path: &str, kind: Kind, bytes: u64) -> Result<()> {
        validate_relative_path(path)?;
        if bytes > MAX_PACKAGE_FILE_BYTES {
            return Err(invalid("package file exceeds expansion limit"));
        }
        let files = self
            .files
            .checked_add(usize::from(kind == Kind::File))
            .ok_or_else(|| invalid("package file count overflow"))?;
        let total = self
            .bytes
            .checked_add(bytes)
            .ok_or_else(|| invalid("package expansion size overflow"))?;
        if files > MAX_PACKAGE_FILES || total > MAX_PACKAGE_BYTES {
            return Err(invalid("package expansion quota exceeded"));
        }
        let mut planned = Vec::new();
        let mut prefix = String::new();
        let parts: Vec<_> = path.split('/').collect();
        let mut declare_existing = None;
        for (index, part) in parts.iter().enumerate() {
            if index > 0 {
                prefix.push('/');
            }
            prefix.push_str(part);
            let final_part = index + 1 == parts.len();
            let entry_kind = if final_part { kind } else { Kind::Directory };
            let key = path_collision_key(&prefix);
            if let Some(existing) = self.entries.get(&key) {
                if existing.original != prefix
                    || existing.kind != entry_kind
                    || (final_part && (kind == Kind::File || existing.declared))
                {
                    return Err(invalid("package path collides with an existing entry"));
                }
                if final_part {
                    declare_existing = Some(key);
                }
            } else {
                planned.push((
                    key,
                    BudgetEntry {
                        original: prefix.clone(),
                        kind: entry_kind,
                        declared: final_part,
                    },
                ));
            }
        }
        if self
            .entries
            .len()
            .checked_add(planned.len())
            .is_none_or(|count| count > MAX_PACKAGE_ENTRIES)
        {
            return Err(invalid("package directory and file count exceeds limit"));
        }
        for (key, entry) in planned {
            self.entries.insert(key, entry);
        }
        if let Some(key) = declare_existing
            && let Some(entry) = self.entries.get_mut(&key)
        {
            entry.declared = true;
        }
        self.files = files;
        self.bytes = total;
        Ok(())
    }
    fn directory_exists(&self, path: &str) -> bool {
        self.entries
            .get(&path_collision_key(path))
            .is_some_and(|entry| entry.original == path && entry.kind == Kind::Directory)
    }
}

fn path_collision_key(path: &str) -> String {
    // Normalize comparison keys only. Sealed paths and tree-hash inputs retain
    // their original UTF-8 bytes. Case folding may introduce decompositions.
    let composed: String = path.nfc().collect();
    let folded = unicase::UniCase::unicode(composed).to_folded_case();
    folded.nfc().collect()
}

/// Immutable bytes used to build this candidate's digest. Installers should
/// consume these bytes rather than reopen the mutable source directory.
#[derive(Debug)]
pub struct PackageFile {
    path: String,
    bytes: Vec<u8>,
    sha256: String,
}
impl PackageFile {
    pub fn path(&self) -> &str {
        &self.path
    }
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }
    pub fn sha256(&self) -> &str {
        &self.sha256
    }
}

#[derive(Debug)]
pub struct VerifiedPackage {
    manifest: Manifest,
    digest: String,
    capability_fingerprint: String,
    files: Vec<PackageFile>,
}
impl VerifiedPackage {
    pub fn manifest(&self) -> &Manifest {
        &self.manifest
    }
    pub fn digest(&self) -> &str {
        &self.digest
    }
    pub fn capability_fingerprint(&self) -> &str {
        &self.capability_fingerprint
    }
    pub fn files(&self) -> &[PackageFile] {
        &self.files
    }
    pub fn file(&self, path: &str) -> Option<&PackageFile> {
        self.files
            .binary_search_by(|file| file.path.as_str().cmp(path))
            .ok()
            .map(|index| &self.files[index])
    }
}

/// Read a directory candidate without executing its entries or fetching data.
/// Every opened component rejects symlinks, every file must be regular and
/// unlinked elsewhere, and all reads are bounded. The returned identity covers
/// the sealed bytes read here, not a promise that the source tree stays static.
/// Files changing while read are rejected. Cross-file snapshot atomicity is
/// supplied by installation of these sealed bytes in the later package store.
pub fn inspect_package(path: &Path) -> Result<VerifiedPackage> {
    let root = fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
        .map_err(|_| invalid("package root cannot be opened safely"))?;
    let directory = Dir::from_std_file(root);
    inspect_package_directory(&directory)
}

/// Inspect an already-open package root. Reads remain anchored to this
/// capability even if its ambient path is replaced by another directory.
pub fn inspect_package_directory(directory: &Dir) -> Result<VerifiedPackage> {
    let mut budget = FileBudget::default();
    let mut files = Vec::new();
    inspect_directory(directory, "", &mut budget, &mut files)?;
    files.sort_by(|left, right| left.path.as_bytes().cmp(right.path.as_bytes()));
    let manifest_file = files
        .iter()
        .find(|file| file.path == MANIFEST_NAME)
        .ok_or_else(|| invalid("package manifest is missing"))?;
    if manifest_file.bytes.len() > MAX_MANIFEST_BYTES {
        return Err(invalid("manifest exceeds its byte limit"));
    }
    let text = std::str::from_utf8(&manifest_file.bytes)
        .map_err(|_| invalid("package manifest is not UTF-8"))?;
    let manifest = parse_manifest(text)?;
    verify_entry_paths(&manifest, &files, &budget)?;
    let mut hash = Sha256::new();
    hash.update(b"saya-plugin-tree-v1\0");
    for file in &files {
        let path_bytes = file.path.as_bytes();
        let length =
            u32::try_from(path_bytes.len()).map_err(|_| invalid("package path length overflow"))?;
        hash.update(length.to_be_bytes());
        hash.update(path_bytes);
        hash.update((file.bytes.len() as u64).to_be_bytes());
        hash.update(Sha256::digest(&file.bytes));
    }
    let digest = hash
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    let capability_fingerprint = manifest.capability_fingerprint()?;
    Ok(VerifiedPackage {
        manifest,
        digest,
        capability_fingerprint,
        files,
    })
}

fn inspect_directory(
    directory: &Dir,
    prefix: &str,
    budget: &mut FileBudget,
    files: &mut Vec<PackageFile>,
) -> Result<()> {
    for entry in directory
        .entries()
        .map_err(|_| invalid("cannot enumerate package directory"))?
    {
        let entry = entry.map_err(|_| invalid("cannot enumerate package entry"))?;
        let name = entry
            .file_name()
            .into_string()
            .map_err(|_| invalid("package path is not UTF-8"))?;
        let path = if prefix.is_empty() {
            name.clone()
        } else {
            format!("{prefix}/{name}")
        };
        validate_relative_path(&path)?;
        let file_type = entry
            .file_type()
            .map_err(|_| invalid("cannot inspect package entry"))?;
        if file_type.is_symlink() {
            return Err(invalid("package symlinks are forbidden"));
        }
        if file_type.is_dir() {
            budget.reserve_directory(&path)?;
            let mut options = OpenOptions::new();
            options
                .read(true)
                .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_NONBLOCK);
            let child = directory
                .open_with(&name, &options)
                .map_err(|_| invalid("package directory changed or is unsafe"))?;
            inspect_directory(&Dir::from_std_file(child.into_std()), &path, budget, files)?;
        } else if file_type.is_file() {
            let mut options = OpenOptions::new();
            options
                .read(true)
                .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
            let mut file = directory
                .open_with(&name, &options)
                .map_err(|_| invalid("package file changed or is unsafe"))?
                .into_std();
            let before = file
                .metadata()
                .map_err(|_| invalid("cannot inspect opened package file"))?;
            if !before.is_file() || before.nlink() != 1 {
                return Err(invalid("package file is special or hard-linked"));
            }
            if path == MANIFEST_NAME && before.len() > MAX_MANIFEST_BYTES as u64 {
                return Err(invalid("manifest exceeds its byte limit"));
            }
            budget.reserve_file(&path, before.len())?;
            let mut bytes = Vec::new();
            (&mut file)
                .take(MAX_PACKAGE_FILE_BYTES + 1)
                .read_to_end(&mut bytes)
                .map_err(|_| invalid("cannot read package file"))?;
            let after = file
                .metadata()
                .map_err(|_| invalid("cannot recheck package file"))?;
            if bytes.len() as u64 != before.len() || file_identity(&before) != file_identity(&after)
            {
                return Err(invalid("package file changed while being read"));
            }
            let sha256 = digest_hex(&bytes);
            files.push(PackageFile {
                path,
                bytes,
                sha256,
            });
        } else {
            return Err(invalid("package special files are forbidden"));
        }
    }
    Ok(())
}
fn file_identity(metadata: &fs::Metadata) -> (u64, u64, u64, u64, i64, i64, i64, i64) {
    (
        metadata.dev(),
        metadata.ino(),
        metadata.nlink(),
        metadata.len(),
        metadata.mtime(),
        metadata.mtime_nsec(),
        metadata.ctime(),
        metadata.ctime_nsec(),
    )
}
fn verify_entry_paths(
    manifest: &Manifest,
    files: &[PackageFile],
    budget: &FileBudget,
) -> Result<()> {
    let has_file = |path: &str| files.iter().any(|file| file.path == path);
    if manifest
        .plugin
        .license_file
        .as_ref()
        .is_some_and(|path| !has_file(path))
    {
        return Err(invalid("declared license file is missing"));
    }
    for skill in &manifest.skills {
        if !budget.directory_exists(&skill.path) || !has_file(&format!("{}/SKILL.md", skill.path)) {
            return Err(invalid("declared Skill directory or SKILL.md is missing"));
        }
    }
    for resource in &manifest.resources {
        if !has_file(&resource.path) {
            return Err(invalid("declared resource file is missing"));
        }
    }
    let args = manifest
        .mcp
        .iter()
        .filter_map(|entry| match entry {
            super::McpEntry::Stdio { args, .. } => Some(args.as_slice()),
            super::McpEntry::Http { .. } => None,
        })
        .chain(manifest.hooks.iter().map(|entry| entry.args.as_slice()));
    for args in args {
        for arg in args {
            if let Some(path) = arg.strip_prefix("@plugin/")
                && !has_file(path)
                && !budget.directory_exists(path)
            {
                return Err(invalid("plugin argument refers to a missing package path"));
            }
        }
    }
    Ok(())
}
