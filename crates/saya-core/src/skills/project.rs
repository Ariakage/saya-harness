use super::*;
use cap_std::fs::{Dir, OpenOptions, OpenOptionsExt};
use std::{
    fs::{self, File},
    io::Read,
    os::unix::fs::{MetadataExt, OpenOptionsExt as _},
};

struct Root {
    identity: CatalogRoot,
    directory: Dir,
}
pub(super) struct Roots {
    entries: Vec<Root>,
}
impl Roots {
    pub(super) fn new(workspace: &Path, home: Option<&Path>) -> Result<Self, BoundaryError> {
        let workspace = open_root(workspace, "workspace")?;
        let mut entries = vec![workspace];
        if let Some(home) = home {
            let home = open_root(home, "user")?;
            if home.identity.path != entries[0].identity.path {
                entries.push(home);
            }
        }
        Ok(Self { entries })
    }
    pub(super) fn check(&self) -> Result<(), BoundaryError> {
        for root in &self.entries {
            root.check()?;
        }
        Ok(())
    }
    pub(super) fn validate_workspace(&self, workspace: &Path) -> Result<(), BoundaryError> {
        self.check()?;
        let expected = self
            .entries
            .first()
            .ok_or_else(|| error("skill_scope", "Skill workspace root is missing"))?;
        let canonical = fs::canonicalize(workspace).map_err(io_error)?;
        let actual = fs::symlink_metadata(&canonical).map_err(io_error)?;
        if canonical != expected.identity.path
            || actual.dev() != expected.identity.device
            || actual.ino() != expected.identity.inode
        {
            return Err(error(
                "skill_scope",
                "Skill manager belongs to a different workspace identity",
            ));
        }
        Ok(())
    }
    pub(super) fn locate(
        &self,
        skill: &project_context::Skill,
    ) -> Result<(usize, String, SkillOrigin), BoundaryError> {
        if !valid_name(&skill.name) || !valid_hash(&skill.sha256) {
            return Err(error(
                "skill_catalog",
                "Discovered Skill identity is invalid",
            ));
        }
        let (index, root) = self
            .entries
            .iter()
            .enumerate()
            .find(|(_, root)| {
                root.identity.scope == skill.scope && skill.path.starts_with(&root.identity.path)
            })
            .ok_or_else(|| {
                error(
                    "skill_catalog",
                    "Discovered Skill is outside its host-selected catalog root",
                )
            })?;
        let relative = skill
            .path
            .strip_prefix(&root.identity.path)
            .map_err(|_| error("skill_catalog", "Skill source is outside its catalog root"))?;
        let expected_agents = Path::new(".agents")
            .join("skills")
            .join(&skill.name)
            .join("SKILL.md");
        let expected_saya = Path::new(".saya")
            .join("skills")
            .join(&skill.name)
            .join("SKILL.md");
        if relative != expected_agents && relative != expected_saya {
            return Err(error(
                "skill_catalog",
                "Skill source does not match a discovered catalog location",
            ));
        }
        let directory = relative
            .parent()
            .and_then(Path::to_str)
            .ok_or_else(|| error("skill_catalog", "Skill directory is not UTF-8"))?
            .to_owned();
        Ok((
            index,
            directory,
            SkillOrigin::Project {
                root: root.identity.clone(),
                path: skill.path.clone(),
                scope: skill.scope.into(),
            },
        ))
    }
    pub(super) fn read(
        &self,
        root: usize,
        directory: &str,
        relative: &str,
        limit: usize,
        cancel: &AtomicBool,
    ) -> Result<Vec<u8>, BoundaryError> {
        resource_path(relative)?;
        let root = self
            .entries
            .get(root)
            .ok_or_else(|| error("skill_catalog", "Skill catalog root is missing"))?;
        root.check()?;
        let mut skill_directory = root.directory.try_clone().map_err(io_error)?;
        for component in directory.split('/') {
            skill_directory = open_directory(&skill_directory, component)?;
        }
        let mut parent = skill_directory;
        let parts: Vec<_> = relative.split('/').collect();
        let (name, parents) = parts
            .split_last()
            .ok_or_else(|| error("invalid_args", "Skill resource filename is missing"))?;
        for component in parents {
            parent = open_directory(&parent, component)?;
        }
        let mut file = parent
            .open_with(name, &options(false))
            .map_err(io_error)?
            .into_std();
        let before = regular(&file)?;
        if before.len() > limit as u64 {
            return Err(error("skill_limit", "Skill text exceeds its read limit"));
        }
        let mut bytes = Vec::new();
        let mut chunk = [0u8; 16 * 1024];
        loop {
            cancelled(cancel)?;
            let count = file.read(&mut chunk).map_err(io_error)?;
            if count == 0 {
                break;
            }
            if bytes.len().saturating_add(count) > limit {
                return Err(error("skill_limit", "Skill text exceeds its read limit"));
            }
            bytes.extend_from_slice(&chunk[..count]);
        }
        let after = regular(&file)?;
        let named = parent
            .open_with(name, &options(false))
            .map_err(io_error)?
            .into_std();
        let named = regular(&named)?;
        if identity(&before) != identity(&after)
            || identity(&after) != identity(&named)
            || bytes.len() as u64 != after.len()
        {
            return Err(error(
                "skill_source_changed",
                "Skill source changed while reading",
            ));
        }
        root.check()?;
        // Rewalk every directory to ensure the held parent still occupies the
        // approved catalog-relative location after the bounded read.
        let mut current = root.directory.try_clone().map_err(io_error)?;
        for component in directory.split('/').chain(parents.iter().copied()) {
            current = open_directory(&current, component)?;
        }
        let a = current.into_std_file().metadata().map_err(io_error)?;
        let b = parent.into_std_file().metadata().map_err(io_error)?;
        if a.dev() != b.dev() || a.ino() != b.ino() {
            return Err(error(
                "skill_source_changed",
                "Skill resource parent identity changed",
            ));
        }
        Ok(bytes)
    }
}
impl Root {
    fn check(&self) -> Result<(), BoundaryError> {
        let metadata = fs::symlink_metadata(&self.identity.path).map_err(io_error)?;
        if !metadata.is_dir()
            || metadata.dev() != self.identity.device
            || metadata.ino() != self.identity.inode
        {
            return Err(error(
                "skill_source_changed",
                "Skill catalog root identity changed",
            ));
        }
        Ok(())
    }
}
fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && !name.starts_with('-')
        && !name.ends_with('-')
        && !name.contains("--")
        && name
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
}
fn open_root(path: &Path, scope: &str) -> Result<Root, BoundaryError> {
    let path = fs::canonicalize(path).map_err(io_error)?;
    if !path.is_absolute()
        || path.to_str().is_none_or(|path| {
            path.len() > 4096 || path.chars().any(crate::text::is_terminal_control)
        })
    {
        return Err(error("skill_catalog", "Skill catalog root path is invalid"));
    }
    let file = fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
        .open(&path)
        .map_err(io_error)?;
    let metadata = file.metadata().map_err(io_error)?;
    let root = Root {
        identity: CatalogRoot {
            path,
            device: metadata.dev(),
            inode: metadata.ino(),
            scope: scope.into(),
        },
        directory: Dir::from_std_file(file),
    };
    root.check()?;
    Ok(root)
}
fn options(directory: bool) -> OpenOptions {
    let mut options = OpenOptions::new();
    options.read(true).custom_flags(
        libc::O_NOFOLLOW
            | libc::O_NONBLOCK
            | libc::O_CLOEXEC
            | if directory { libc::O_DIRECTORY } else { 0 },
    );
    options
}
fn open_directory(parent: &Dir, name: &str) -> Result<Dir, BoundaryError> {
    Ok(Dir::from_std_file(
        parent
            .open_with(name, &options(true))
            .map_err(io_error)?
            .into_std(),
    ))
}
fn regular(file: &File) -> Result<fs::Metadata, BoundaryError> {
    let metadata = file.metadata().map_err(io_error)?;
    if !metadata.is_file() || metadata.nlink() != 1 {
        return Err(error(
            "skill_unsupported",
            "Skill resources must be ordinary files without links",
        ));
    }
    Ok(metadata)
}
fn identity(metadata: &fs::Metadata) -> (u64, u64, u64, i64, i64, i64, i64, u32) {
    (
        metadata.dev(),
        metadata.ino(),
        metadata.len(),
        metadata.mtime(),
        metadata.mtime_nsec(),
        metadata.ctime(),
        metadata.ctime_nsec(),
        metadata.mode(),
    )
}
fn io_error(_: std::io::Error) -> BoundaryError {
    error(
        "skill_source_changed",
        "Skill source is missing, linked, or inaccessible",
    )
}
