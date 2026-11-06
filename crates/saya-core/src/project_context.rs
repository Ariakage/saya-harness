//! Discovery of explicitly supported instruction and Skill locations.
//! Source content is untrusted task context, never a runtime policy or grant.
use crate::contract::{BoundaryError, ErrorInfo};
use cap_std::{
    ambient_authority,
    fs::{Dir, OpenOptions, OpenOptionsExt},
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    io::{self, Read},
    path::{Path, PathBuf},
};

const MAX_INSTRUCTION: u64 = 256 * 1024;
const MAX_SKILL: u64 = 256 * 1024;
const MAX_FRONTMATTER: usize = 16 * 1024;
const MAX_SOURCES: usize = 128;
const MAX_SKILLS: usize = 1024;
const MAX_TOTAL: usize = 4 * 1024 * 1024;
fn failure(message: &'static str) -> BoundaryError {
    BoundaryError(
        ErrorInfo {
            code: "project_context_error".into(),
            message: message.into(),
        },
        None,
        None,
    )
}

/// A specific, hashed source; outer sources precede applicable inner sources.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct InstructionSource {
    pub path: PathBuf,
    pub scope: &'static str,
    pub sha256: String,
    pub content: String,
}
/// Discovered metadata. Discovery does not execute or authorize any skill.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct Skill {
    pub name: String,
    pub description: String,
    pub path: PathBuf,
    pub scope: &'static str,
    pub sha256: String,
}
/// Context serialized as a labeled data item in model history, not as system instructions.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct Snapshot {
    pub instructions: Vec<InstructionSource>,
    pub skills: Vec<Skill>,
}
impl Snapshot {
    pub fn fingerprint(&self) -> Result<String, BoundaryError> {
        let bytes =
            serde_json::to_vec(self).map_err(|_| failure("cannot encode project context"))?;
        Ok(hex(&bytes))
    }
}
fn hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// A pair of explicit roots. No arbitrary ancestors, products, or projects are scanned.
pub struct ProjectContext {
    workspace: PathBuf,
    workspace_dir: Dir,
    home: Option<(PathBuf, Dir)>,
}
impl ProjectContext {
    pub fn new(workspace: &Path, home: Option<&Path>) -> Result<Self, BoundaryError> {
        if home.is_some_and(|path| !path.is_absolute()) {
            return Err(failure("home directory must be absolute"));
        }
        let workspace =
            std::fs::canonicalize(workspace).map_err(|_| failure("workspace is unavailable"))?;
        if !workspace.is_dir() {
            return Err(failure("workspace is not a directory"));
        }
        let home_path = home
            .map(std::fs::canonicalize)
            .transpose()
            .map_err(|_| failure("home directory is unavailable"))?;
        if home_path.as_ref().is_some_and(|p| !p.is_dir()) {
            return Err(failure("home is not a directory"));
        }
        let workspace_dir = Dir::open_ambient_dir(&workspace, ambient_authority())
            .map_err(|_| failure("cannot open workspace"))?;
        let home = home_path
            .filter(|path| path != &workspace)
            .map(|path| {
                let dir = Dir::open_ambient_dir(&path, ambient_authority())
                    .map_err(|_| failure("cannot open home directory"))?;
                Ok::<_, BoundaryError>((path, dir))
            })
            .transpose()?;
        Ok(Self {
            workspace,
            workspace_dir,
            home,
        })
    }
    /// Read current instructions and Skill metadata. `target` is resolved only
    /// within the workspace; a target outside it never causes an ancestor scan.
    /// Missing files are ignored. Unsafe links, I/O failures and size limits
    /// return a bounded error without echoing file content.
    pub fn snapshot(&self, target: Option<&Path>) -> Result<Snapshot, BoundaryError> {
        let workspace = &self.workspace_dir;
        let mut out = Snapshot {
            instructions: vec![],
            skills: vec![],
        };
        let mut candidates = BTreeMap::new();
        let mut total = 0usize;
        if let Some((home, global)) = &self.home {
            for path in [".agents/AGENTS.md", ".saya/AGENTS.md"] {
                add_instruction(
                    global,
                    home,
                    Path::new(path),
                    "user",
                    &mut out.instructions,
                    &mut total,
                )?;
            }
            for path in [".agents/skills", ".saya/skills"] {
                discover_skills(
                    global,
                    home,
                    Path::new(path),
                    "user",
                    &mut candidates,
                    &mut total,
                )?;
            }
        }
        for path in ["AGENTS.md", ".agents/AGENTS.md", ".saya/AGENTS.md"] {
            add_instruction(
                workspace,
                &self.workspace,
                Path::new(path),
                "workspace",
                &mut out.instructions,
                &mut total,
            )?;
        }
        for path in [".agents/skills", ".saya/skills"] {
            discover_skills(
                workspace,
                &self.workspace,
                Path::new(path),
                "workspace",
                &mut candidates,
                &mut total,
            )?;
        }
        if let Some(target) = target
            && let Some(directory) = self.target_directory(target)?
        {
            let relative = directory
                .strip_prefix(&self.workspace)
                .map_err(|_| failure("invalid scoped target"))?;
            let mut prefix = PathBuf::new();
            for part in relative.components() {
                prefix.push(part.as_os_str());
                add_instruction(
                    workspace,
                    &self.workspace,
                    &prefix.join("AGENTS.md"),
                    "workspace",
                    &mut out.instructions,
                    &mut total,
                )?;
            }
        }
        out.skills = candidates.into_values().collect();
        Ok(out)
    }
    fn target_directory(&self, target: &Path) -> Result<Option<PathBuf>, BoundaryError> {
        let target = if target.is_absolute() {
            target.to_owned()
        } else {
            self.workspace.join(target)
        };
        // Missing new-file targets are scoped by their nearest existing ancestor.
        let mut current = target.as_path();
        loop {
            match std::fs::canonicalize(current) {
                Ok(path) => {
                    if !path.starts_with(&self.workspace) {
                        return Ok(None);
                    }
                    return Ok(Some(if path.is_dir() {
                        path
                    } else {
                        path.parent()
                            .ok_or_else(|| failure("invalid target path"))?
                            .to_path_buf()
                    }));
                }
                Err(e) if e.kind() == io::ErrorKind::NotFound => {
                    current = current
                        .parent()
                        .ok_or_else(|| failure("invalid target path"))?;
                }
                Err(_) => return Err(failure("cannot resolve target path")),
            }
        }
    }
}
fn read_bounded(root: &Dir, path: &Path, max: u64) -> Result<Option<Vec<u8>>, BoundaryError> {
    let mut options = OpenOptions::new();
    options.read(true).custom_flags(libc::O_NONBLOCK);
    let file = match root.open_with(path, &options) {
        Ok(file) => file,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err(failure("cannot safely open context source")),
    };
    let metadata = file
        .metadata()
        .map_err(|_| failure("cannot inspect context source"))?;
    if !metadata.is_file() || metadata.len() > max {
        return Err(failure("context source is not a bounded regular file"));
    }
    let mut bytes = Vec::new();
    file.take(max + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| failure("cannot read context source"))?;
    if bytes.len() as u64 > max {
        return Err(failure("context source exceeds size limit"));
    }
    Ok(Some(bytes))
}
fn account(total: &mut usize, n: usize) -> Result<(), BoundaryError> {
    *total = total
        .checked_add(n)
        .ok_or_else(|| failure("context size limit exceeded"))?;
    if *total > MAX_TOTAL {
        return Err(failure("context size limit exceeded"));
    }
    Ok(())
}
fn add_instruction(
    root: &Dir,
    base: &Path,
    path: &Path,
    scope: &'static str,
    out: &mut Vec<InstructionSource>,
    total: &mut usize,
) -> Result<(), BoundaryError> {
    if let Some(bytes) = read_bounded(root, path, MAX_INSTRUCTION)? {
        if out.len() >= MAX_SOURCES {
            return Err(failure("too many instruction sources"));
        }
        account(total, bytes.len())?;
        let content = std::str::from_utf8(&bytes)
            .map_err(|_| failure("instruction source is not UTF-8"))?
            .to_owned();
        let full = base.join(path);
        // The same location can be both home and workspace. Keep the later,
        // higher-precedence workspace entry rather than injecting it twice.
        out.retain(|s| s.path != full);
        out.push(InstructionSource {
            path: full,
            scope,
            sha256: hex(&bytes),
            content,
        });
    }
    Ok(())
}
#[derive(Deserialize)]
struct Frontmatter {
    name: String,
    description: String,
}
fn discover_skills(
    root: &Dir,
    base: &Path,
    path: &Path,
    scope: &'static str,
    out: &mut BTreeMap<String, Skill>,
    total: &mut usize,
) -> Result<(), BoundaryError> {
    let skill_root = match root.open_dir(path) {
        Ok(v) => v,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(_) => return Err(failure("cannot safely enumerate Skills")),
    };
    let entries = skill_root
        .entries()
        .map_err(|_| failure("cannot enumerate Skills"))?;
    let mut names = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|_| failure("cannot enumerate Skills"))?;
        if entry
            .file_type()
            .map_err(|_| failure("cannot inspect Skill entry"))?
            .is_file()
        {
            continue;
        }
        names.push(entry.file_name());
        if names.len() > MAX_SKILLS {
            return Err(failure("too many Skill entries"));
        }
    }
    names.sort();
    for name in names {
        let Some(directory) = name.to_str() else {
            continue;
        };
        if !valid_skill_name(directory) {
            continue;
        }
        let rel = path.join(directory).join("SKILL.md");
        let Some(bytes) = read_bounded(
            &skill_root,
            &Path::new(directory).join("SKILL.md"),
            MAX_SKILL,
        )?
        else {
            continue;
        };
        account(total, bytes.len())?;
        let Ok(text) = std::str::from_utf8(&bytes) else {
            continue;
        };
        let Some(front) = text
            .strip_prefix("---\n")
            .or_else(|| text.strip_prefix("---\r\n"))
        else {
            continue;
        };
        let Some(end) = front.match_indices("\n---").find_map(|(i, _)| {
            let rest = &front[i + 4..];
            (rest.is_empty() || rest.starts_with('\n') || rest.starts_with("\r\n")).then_some(i)
        }) else {
            continue;
        };
        if end > MAX_FRONTMATTER || has_yaml_alias(&front[..end]) {
            continue;
        }
        let Ok(meta) = serde_yaml_ng::from_str::<Frontmatter>(&front[..end]) else {
            continue;
        };
        if meta.name != directory
            || meta.description.trim().is_empty()
            || meta.description.chars().count() > 1024
        {
            continue;
        }
        out.insert(
            meta.name.clone(),
            Skill {
                name: meta.name,
                description: meta.description,
                path: base.join(rel),
                scope,
                sha256: hex(&bytes),
            },
        );
        if out.len() > MAX_SKILLS {
            return Err(failure("too many Skills"));
        }
    }
    Ok(())
}
// Aliases are unnecessary for Skill metadata and can expand recursively.
// Reject their token forms before passing untrusted frontmatter to the parser.
fn has_yaml_alias(text: &str) -> bool {
    let bytes = text.as_bytes();
    bytes.iter().enumerate().any(|(i, byte)| {
        matches!(byte, b'&' | b'*')
            && (i == 0 || bytes[i - 1].is_ascii_whitespace() || b"[{,:".contains(&bytes[i - 1]))
            && bytes
                .get(i + 1)
                .is_some_and(|c| c.is_ascii_alphanumeric() || *c == b'_' || *c == b'-')
    })
}
fn valid_skill_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && !name.starts_with('-')
        && !name.ends_with('-')
        && !name.contains("--")
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}
