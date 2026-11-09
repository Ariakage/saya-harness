use super::{ReadError, err, io_err};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    os::unix::fs::MetadataExt,
    path::{Component, Path, PathBuf},
    sync::{Arc, RwLock},
    time::{Duration, Instant},
};
use unicode_normalization::UnicodeNormalization;

const MAX_PATHS: usize = 256;
const MAX_SCAN_ENTRIES: usize = 65_536;
const MAX_TARGET_ENTRIES: usize = 16_384;
const MAX_SCAN_DEPTH: usize = 32;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ProtectedPathSnapshot {
    pub paths: Vec<PathBuf>,
    pub private_files: Vec<(u64, u64)>,
    pub private_directories: Vec<(u64, u64)>,
    pub safe_hardlinks: Vec<ProtectedFileVersion>,
    pub scan_truncated: bool,
    pub private_scan_incomplete: bool,
}
impl ProtectedPathSnapshot {
    pub fn validate(&self) -> Result<(), ReadError> {
        if self.paths.len() > MAX_PATHS
            || self.private_files.len() > MAX_SCAN_ENTRIES
            || self.private_directories.len() > MAX_SCAN_ENTRIES
            || self.safe_hardlinks.len() > MAX_TARGET_ENTRIES
        {
            return Err(err(
                "path_denied",
                "Private protection snapshot exceeds its bounded capacity",
            ));
        }
        if self.paths.iter().any(|path| {
            !path.is_absolute()
                || path.as_os_str().len() > 4096
                || path
                    .to_str()
                    .is_none_or(|value| value.chars().any(crate::text::is_terminal_control))
        }) {
            return Err(err(
                "invalid_path",
                "Private protection snapshot paths are invalid",
            ));
        }
        if self.private_scan_incomplete && !self.safe_hardlinks.is_empty() {
            return Err(err(
                "host_protection_unavailable",
                "Incomplete private identity evidence cannot authorize hardlinks",
            ));
        }
        let private: BTreeSet<_> = self.private_files.iter().copied().collect();
        let mut safe = BTreeSet::new();
        for version in &self.safe_hardlinks {
            if version.links < 2
                || private.contains(&(version.device, version.inode))
                || !safe.insert((version.device, version.inode))
            {
                return Err(err(
                    "path_denied",
                    "Private protection snapshot has conflicting file identities",
                ));
            }
        }
        Ok(())
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProtectedFileVersion {
    pub device: u64,
    pub inode: u64,
    pub links: u64,
    pub size: u64,
    pub modified: (i64, i64),
    pub changed: (i64, i64),
}
impl ProtectedFileVersion {
    fn from_std(metadata: &fs::Metadata) -> Self {
        Self {
            device: metadata.dev(),
            inode: metadata.ino(),
            links: metadata.nlink(),
            size: metadata.len(),
            modified: (metadata.mtime(), metadata.mtime_nsec()),
            changed: (metadata.ctime(), metadata.ctime_nsec()),
        }
    }
    pub(super) fn from_cap(metadata: &cap_std::fs::Metadata) -> Self {
        use cap_std::fs::MetadataExt as _;
        Self {
            device: metadata.dev(),
            inode: metadata.ino(),
            links: metadata.nlink(),
            size: metadata.len(),
            modified: (metadata.mtime(), metadata.mtime_nsec()),
            changed: (metadata.ctime(), metadata.ctime_nsec()),
        }
    }
}

#[derive(Clone, Default)]
pub(super) struct ProtectedPaths {
    paths: BTreeSet<PathBuf>,
    path_keys: BTreeSet<String>,
    file_ids: BTreeSet<(u64, u64)>,
    directory_ids: BTreeSet<(u64, u64)>,
    sealed: bool,
    safe_hardlinks: BTreeMap<(u64, u64), ProtectedFileVersion>,
    truncated: bool,
    private_scan_incomplete: bool,
}
impl ProtectedPaths {
    pub(super) fn extend(&mut self, paths: Vec<PathBuf>) -> Result<(), ReadError> {
        if self.sealed {
            return Err(err(
                "path_denied",
                "A worker protection snapshot cannot be changed",
            ));
        }
        if paths.len() > MAX_PATHS {
            return Err(err("path_denied", "Private path count exceeds its limit"));
        }
        let mut next = self.clone();
        for path in paths {
            if !path.is_absolute() || path.as_os_str().len() > 4096 {
                return Err(err(
                    "invalid_path",
                    "Private paths must be bounded absolute paths",
                ));
            }
            next.paths.insert(lexical(&path));
            next.paths.insert(normalize_missing(&path)?);
        }
        if next.paths.len() > MAX_PATHS {
            return Err(err("path_denied", "Private path count exceeds its limit"));
        }
        next.path_keys = next
            .paths
            .iter()
            .filter_map(|path| path_key(path))
            .collect();
        next.refresh_ids()?;
        *self = next;
        Ok(())
    }
    fn contains_path(&self, path: &Path) -> bool {
        if self.paths.iter().any(|private| path.starts_with(private)) {
            return true;
        }
        let Some(target) = path_key(path) else {
            return false;
        };
        let mut ancestor = target.as_str();
        loop {
            if self.path_keys.contains(ancestor) {
                return true;
            }
            match ancestor.rfind('/') {
                Some(0) => return self.path_keys.contains("/"),
                Some(index) => ancestor = &ancestor[..index],
                None => return false,
            }
        }
    }
    fn aliases_directory(&self, path: &Path) -> Result<bool, ReadError> {
        let mut roots = BTreeSet::new();
        for private in self.paths.iter().filter(|_| !self.sealed) {
            match fs::metadata(private) {
                Ok(metadata) if metadata.is_dir() => {
                    roots.insert((metadata.dev(), metadata.ino()));
                }
                Ok(_) => {}
                Err(failure) if failure.kind() == std::io::ErrorKind::NotFound => {}
                Err(_) => {
                    return Err(err(
                        "path_denied",
                        "Private directory identity cannot be inspected",
                    ));
                }
            }
        }
        if roots.is_empty() && self.directory_ids.is_empty() {
            return Ok(false);
        }
        for ancestor in path.ancestors() {
            match fs::metadata(ancestor) {
                Ok(metadata)
                    if metadata.is_dir()
                        && (roots.contains(&(metadata.dev(), metadata.ino()))
                            || self
                                .directory_ids
                                .contains(&(metadata.dev(), metadata.ino()))) =>
                {
                    return Ok(true);
                }
                Ok(_) => {}
                Err(failure) if failure.kind() == std::io::ErrorKind::NotFound => {}
                Err(_) => {
                    return Err(err(
                        "path_denied",
                        "Target ancestor identity cannot be inspected",
                    ));
                }
            }
        }
        Ok(false)
    }
    fn refresh_ids(&mut self) -> Result<(), ReadError> {
        let mut pending: Vec<_> = self
            .paths
            .iter()
            .map(|path| (path.clone(), 0usize))
            .collect();
        let mut visited = BTreeSet::new();
        let mut count = 0usize;
        let deadline = Instant::now() + Duration::from_secs(1);
        while let Some((path, depth)) = pending.pop() {
            count += 1;
            if count > MAX_SCAN_ENTRIES || Instant::now() >= deadline {
                self.private_scan_incomplete = true;
                break;
            }
            if depth > MAX_SCAN_DEPTH {
                self.private_scan_incomplete = true;
                continue;
            }
            let metadata = match fs::symlink_metadata(&path) {
                Ok(metadata) => metadata,
                Err(failure) if failure.kind() == std::io::ErrorKind::NotFound => continue,
                Err(_) => {
                    return Err(err(
                        "path_denied",
                        "Private path identity cannot be inspected",
                    ));
                }
            };
            if metadata.is_file() {
                if !self.file_ids.contains(&(metadata.dev(), metadata.ino()))
                    && self.file_ids.len() >= MAX_SCAN_ENTRIES
                {
                    self.private_scan_incomplete = true;
                    continue;
                }
                self.file_ids.insert((metadata.dev(), metadata.ino()));
            } else if metadata.is_dir() && visited.insert((metadata.dev(), metadata.ino())) {
                if !self
                    .directory_ids
                    .contains(&(metadata.dev(), metadata.ino()))
                    && self.directory_ids.len() >= MAX_SCAN_ENTRIES
                {
                    self.private_scan_incomplete = true;
                    continue;
                }
                self.directory_ids.insert((metadata.dev(), metadata.ino()));
                for entry in fs::read_dir(&path).map_err(|_| {
                    err(
                        "path_denied",
                        "Private directory identities cannot be inspected",
                    )
                })? {
                    if pending.len() + count >= MAX_SCAN_ENTRIES {
                        self.private_scan_incomplete = true;
                        break;
                    }
                    pending.push((
                        entry
                            .map_err(|_| {
                                err(
                                    "path_denied",
                                    "Private directory identity cannot be inspected",
                                )
                            })?
                            .path(),
                        depth + 1,
                    ));
                }
            }
        }
        if self.file_ids.len() > MAX_SCAN_ENTRIES || self.directory_ids.len() > MAX_SCAN_ENTRIES {
            return Err(err(
                "path_denied",
                "Private file identity capacity is exhausted",
            ));
        }
        Ok(())
    }

    pub(super) fn snapshot(
        &mut self,
        tool: &str,
        target: &Path,
        ignore_anchor: &Path,
    ) -> Result<ProtectedPathSnapshot, ReadError> {
        if self.sealed {
            return Err(err(
                "path_denied",
                "A worker cannot export fresh host protection evidence",
            ));
        }
        self.refresh_ids()?;
        let mut snapshot = ProtectedPathSnapshot {
            paths: self.paths.iter().cloned().collect(),
            private_files: self.file_ids.iter().copied().collect(),
            private_directories: self.directory_ids.iter().copied().collect(),
            private_scan_incomplete: self.private_scan_incomplete,
            scan_truncated: self.private_scan_incomplete,
            ..Default::default()
        };
        if self.paths.is_empty() && self.file_ids.is_empty() && self.directory_ids.is_empty() {
            return Ok(snapshot);
        }
        if self.private_scan_incomplete {
            snapshot.validate()?;
            return Ok(snapshot);
        }
        let max_depth = match tool {
            "list_dir" => 1,
            "find_files" | "search_text" => MAX_SCAN_DEPTH,
            _ => 0,
        };
        let mut pending = vec![(target.to_path_buf(), 0usize)];
        if matches!(tool, "find_files" | "search_text") {
            for ancestor in target
                .ancestors()
                .take_while(|path| path.starts_with(ignore_anchor))
                .take(MAX_SCAN_DEPTH + 1)
            {
                pending.push((ancestor.join(".gitignore"), max_depth));
            }
        }
        let mut seen = BTreeSet::new();
        let mut safe = BTreeMap::new();
        let deadline = Instant::now() + Duration::from_millis(250);
        let mut count = 0;
        while let Some((path, depth)) = pending.pop() {
            if count >= MAX_TARGET_ENTRIES || Instant::now() >= deadline {
                snapshot.scan_truncated = true;
                break;
            }
            count += 1;
            if self.contains_path(&path) {
                continue;
            }
            let metadata = match fs::symlink_metadata(&path) {
                Ok(metadata) => metadata,
                Err(failure) if failure.kind() == std::io::ErrorKind::NotFound => continue,
                Err(_) => {
                    snapshot.scan_truncated = true;
                    continue;
                }
            };
            let id = (metadata.dev(), metadata.ino());
            if self.file_ids.contains(&id) || self.directory_ids.contains(&id) {
                continue;
            }
            if metadata.is_file() && metadata.nlink() > 1 {
                safe.insert(id, ProtectedFileVersion::from_std(&metadata));
            } else if metadata.is_dir() && seen.insert(id) {
                if depth >= max_depth {
                    if matches!(tool, "find_files" | "search_text") {
                        snapshot.scan_truncated = true;
                    }
                    continue;
                }
                let entries = match fs::read_dir(&path) {
                    Ok(entries) => entries,
                    Err(_) => {
                        snapshot.scan_truncated = true;
                        continue;
                    }
                };
                for entry in entries {
                    if pending.len() + count >= MAX_TARGET_ENTRIES {
                        snapshot.scan_truncated = true;
                        break;
                    }
                    match entry {
                        Ok(entry) => pending.push((entry.path(), depth + 1)),
                        Err(_) => snapshot.scan_truncated = true,
                    }
                }
            }
        }
        snapshot.safe_hardlinks = safe.into_values().collect();
        snapshot.validate()?;
        Ok(snapshot)
    }
    pub(super) fn install(&mut self, snapshot: ProtectedPathSnapshot) -> Result<(), ReadError> {
        snapshot.validate()?;
        if !self.paths.is_empty()
            || !self.file_ids.is_empty()
            || !self.directory_ids.is_empty()
            || self.sealed
        {
            return Err(err(
                "path_denied",
                "A worker snapshot cannot replace existing host protections",
            ));
        }
        self.paths = snapshot
            .paths
            .into_iter()
            .map(|path| lexical(&path))
            .collect();
        self.path_keys = self
            .paths
            .iter()
            .filter_map(|path| path_key(path))
            .collect();
        self.file_ids = snapshot.private_files.into_iter().collect();
        self.directory_ids = snapshot.private_directories.into_iter().collect();
        self.safe_hardlinks = snapshot
            .safe_hardlinks
            .into_iter()
            .map(|version| ((version.device, version.inode), version))
            .collect();
        self.truncated = snapshot.scan_truncated || snapshot.private_scan_incomplete;
        self.private_scan_incomplete = snapshot.private_scan_incomplete;
        self.sealed = true;
        Ok(())
    }
    pub(super) fn truncated(&self) -> bool {
        self.truncated
    }
}

pub(super) fn check(protected: &Arc<RwLock<ProtectedPaths>>, path: &Path) -> Result<(), ReadError> {
    let lexical = lexical(path);
    {
        let guard = protected
            .read()
            .map_err(|_| err("path_denied", "Private path protection is unavailable"))?;
        if guard.paths.is_empty() && guard.file_ids.is_empty() && guard.directory_ids.is_empty() {
            return Ok(());
        }
        if guard.contains_path(&lexical) {
            return Err(err(
                "path_denied",
                "Target belongs to host-managed private storage",
            ));
        }
    }
    let resolved = match fs::canonicalize(path) {
        Ok(path) => path,
        Err(failure) if failure.kind() == std::io::ErrorKind::NotFound => normalize_missing(path)?,
        Err(_) => {
            return Err(err(
                "path_denied",
                "Target cannot be checked against private storage",
            ));
        }
    };
    {
        let guard = protected
            .read()
            .map_err(|_| err("path_denied", "Private path protection is unavailable"))?;
        if guard.contains_path(&resolved) || guard.aliases_directory(&resolved)? {
            return Err(err(
                "path_denied",
                "Target resolves to host-managed private storage",
            ));
        }
    }
    match fs::metadata(&resolved) {
        Ok(metadata) if metadata.is_file() => {
            check_identity(protected, ProtectedFileVersion::from_std(&metadata))
        }
        Ok(_) => Ok(()),
        Err(failure) if failure.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(_) => Err(err(
            "path_denied",
            "Target identity cannot be checked against private storage",
        )),
    }
}

pub(super) fn check_identity(
    protected: &Arc<RwLock<ProtectedPaths>>,
    version: ProtectedFileVersion,
) -> Result<(), ReadError> {
    let mut guard = protected
        .write()
        .map_err(|_| err("path_denied", "Private path protection is unavailable"))?;
    if guard.paths.is_empty() && guard.file_ids.is_empty() && guard.directory_ids.is_empty() {
        return Ok(());
    }
    let (device, inode) = (version.device, version.inode);
    if guard.sealed {
        if guard.file_ids.contains(&(device, inode)) {
            return Err(err(
                "path_denied",
                "Target aliases a host-managed private file",
            ));
        }
        if version.links > 1 && guard.safe_hardlinks.get(&(device, inode)) != Some(&version) {
            guard.truncated = true;
            return Err(err(
                if guard.private_scan_incomplete {
                    "host_protection_unavailable"
                } else {
                    "path_denied"
                },
                "Hardlink identity lacks current host safety evidence",
            ));
        }
        return Ok(());
    }
    let mut current_files = Vec::new();
    for path in &guard.paths {
        match fs::metadata(path) {
            Ok(metadata) if metadata.is_file() => {
                current_files.push((metadata.dev(), metadata.ino()))
            }
            Ok(_) => {}
            Err(failure) if failure.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => {
                return Err(err(
                    "path_denied",
                    "Private file identity cannot be inspected",
                ));
            }
        }
    }
    for id in current_files {
        if guard.file_ids.contains(&id) || guard.file_ids.len() < MAX_SCAN_ENTRIES {
            guard.file_ids.insert(id);
        } else {
            guard.private_scan_incomplete = true;
        }
    }
    if version.links > 1 {
        guard.refresh_ids()?;
    }
    if guard.file_ids.contains(&(device, inode)) {
        return Err(err(
            "path_denied",
            "Target aliases a host-managed private file",
        ));
    }
    if version.links > 1 && guard.private_scan_incomplete {
        return Err(err(
            "host_protection_unavailable",
            "Private identity coverage is incomplete for a hardlink",
        ));
    }
    Ok(())
}

fn path_key(path: &Path) -> Option<String> {
    let composed: String = path.to_str()?.nfc().collect();
    Some(
        unicase::UniCase::unicode(composed)
            .to_folded_case()
            .nfc()
            .collect(),
    )
}

fn lexical(path: &Path) -> PathBuf {
    let mut result = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                result.pop();
            }
            _ => result.push(component.as_os_str()),
        }
    }
    result
}
fn normalize_missing(path: &Path) -> Result<PathBuf, ReadError> {
    let mut ancestor = path.to_path_buf();
    let mut suffix = Vec::new();
    loop {
        match fs::canonicalize(&ancestor) {
            Ok(mut resolved) => {
                for name in suffix.into_iter().rev() {
                    resolved.push(name);
                }
                return Ok(lexical(&resolved));
            }
            Err(failure) if failure.kind() == std::io::ErrorKind::NotFound => {
                if fs::symlink_metadata(&ancestor).is_ok() {
                    return Err(err(
                        "path_denied",
                        "A dangling link cannot identify private storage safely",
                    ));
                }
                suffix.push(
                    ancestor
                        .file_name()
                        .ok_or_else(|| {
                            err("invalid_path", "Private target has no existing ancestor")
                        })?
                        .to_owned(),
                );
                ancestor.pop();
            }
            Err(failure) => return Err(io_err(failure)),
        }
    }
}
