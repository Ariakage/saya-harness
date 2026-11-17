//! Repository control data reachable through the writable roots. The paths
//! are split into two layers: the native sandbox keeps them readable but
//! denies writes, and file tools treat them as private managed storage.
//! Enumeration is bounded and tolerates missing entries so newly created
//! control files stay covered.
use super::normalize_path;
use std::{
    collections::BTreeSet,
    fs,
    path::{Path, PathBuf},
};

const GITDIR_FILE_MAX_BYTES: usize = 8 * 1024;
const MAX_GITDIRS: usize = 512;
const MAX_MODULE_DEPTH: usize = 8;
const MAX_WORKTREES: usize = 256;
const CONTROL_ENTRIES: &[&str] = &["config", "config.worktree", "hooks", "info"];
/// User-level files Git or login shells execute or read without prompting.
/// They are only protected when HOME itself sits inside a writable root.
const HOME_IMPLICIT: &[&str] = &[
    ".ssh",
    ".gitconfig",
    ".config/git",
    ".bashrc",
    ".bash_profile",
    ".bash_login",
    ".profile",
    ".zshrc",
    ".zprofile",
    ".zshenv",
    ".zlogin",
];

#[derive(Debug, Default)]
pub struct GitControlPaths {
    /// Readable paths the native sandbox still denies writes to.
    pub write_denied: Vec<PathBuf>,
    /// Paths file tools treat as private managed storage.
    pub private_paths: Vec<PathBuf>,
}

/// Enumerate repository control data below `roots`, plus the implicit
/// user-level execution files when `home` resolves inside a writable root.
pub fn enumerate(roots: &[PathBuf], home: Option<&Path>) -> GitControlPaths {
    let mut protection = GitControlPaths::default();
    let roots: Vec<PathBuf> = roots
        .iter()
        .filter_map(|root| normalize_path(root).ok())
        .collect();
    let mut gitdirs = BTreeSet::new();
    for root in &roots {
        match resolve_gitdir(&root.join(".git")) {
            Some((gitdir, pointer)) => {
                if let Some(pointer) = pointer {
                    protection.write_denied.push(pointer.clone());
                    protection.private_paths.push(pointer);
                }
                gitdirs.insert(gitdir);
            }
            // A missing `.git` may still be created by a payload, so its
            // future control entries are denied ahead of time. `git init`
            // then fails at the config write instead of arming a repository.
            None => {
                for entry in CONTROL_ENTRIES {
                    let path = root.join(".git").join(entry);
                    protection.write_denied.push(path.clone());
                    protection.private_paths.push(path);
                }
            }
        }
    }
    let mut remaining = MAX_GITDIRS;
    for gitdir in gitdirs {
        if remaining == 0 {
            break;
        }
        walk_gitdir(&gitdir, 0, &mut remaining, &mut protection);
    }
    if let Some(home) = home.and_then(|home| normalize_path(home).ok())
        && roots.iter().any(|root| home.starts_with(root))
    {
        for name in HOME_IMPLICIT {
            let path = home.join(name);
            protection.write_denied.push(path.clone());
            protection.private_paths.push(path);
        }
    }
    protection.write_denied.sort();
    protection.write_denied.dedup();
    protection.private_paths.sort();
    protection.private_paths.dedup();
    protection
}

/// Resolve one `.git` entry to its real git directory. A directory resolves
/// to itself; a worktree pointer file resolves through its `gitdir:` line;
/// symlinks resolve through canonicalization. The second return value is the
/// pointer path when `.git` is a file or symlink that must stay immutable.
fn resolve_gitdir(dotgit: &Path) -> Option<(PathBuf, Option<PathBuf>)> {
    let metadata = fs::metadata(dotgit).ok()?;
    let gitdir = normalize_path(dotgit).ok()?;
    if metadata.is_dir() {
        let pointer = fs::symlink_metadata(dotgit)
            .ok()
            .filter(|meta| meta.is_symlink())
            .map(|_| dotgit.to_path_buf());
        return Some((gitdir, pointer));
    }
    if !metadata.is_file() {
        return None;
    }
    let bytes = fs::read(dotgit).ok()?;
    let text = std::str::from_utf8(bytes.get(..GITDIR_FILE_MAX_BYTES).unwrap_or(&bytes)).ok()?;
    let rest = text.lines().next()?.trim().strip_prefix("gitdir:")?.trim();
    if rest.is_empty() {
        return None;
    }
    let candidate = PathBuf::from(rest);
    let joined = if candidate.is_absolute() {
        candidate
    } else {
        lexical_fold(&dotgit.parent()?.join(&candidate))
    };
    let gitdir = normalize_path(&joined).unwrap_or(joined);
    Some((gitdir, Some(dotgit.to_path_buf())))
}

/// Fold `.` and `..` components lexically so a relative `gitdir:` target can
/// be normalized even when parts of the path do not exist yet.
fn lexical_fold(path: &Path) -> PathBuf {
    let mut folded = PathBuf::new();
    for component in path.components() {
        match component {
            std::path::Component::ParentDir => {
                folded.pop();
            }
            std::path::Component::CurDir => {}
            other => folded.push(other.as_os_str()),
        }
    }
    folded
}

/// Record the writable control paths of one git directory and recurse into
/// registered submodule gitdirs and linked worktree admin directories.
fn walk_gitdir(
    gitdir: &Path,
    depth: usize,
    remaining: &mut usize,
    protection: &mut GitControlPaths,
) {
    *remaining = remaining.saturating_sub(1);
    for entry in CONTROL_ENTRIES {
        let path = gitdir.join(entry);
        protection.write_denied.push(path.clone());
        protection.private_paths.push(path);
    }
    if depth == 0
        && let Ok(entries) = fs::read_dir(gitdir.join("worktrees"))
    {
        for entry in entries.flatten().take(MAX_WORKTREES) {
            if entry.file_type().is_ok_and(|kind| kind.is_dir()) {
                let path = entry.path();
                protection.write_denied.push(path.clone());
                protection.private_paths.push(path);
            }
        }
    }
    if depth >= MAX_MODULE_DEPTH || *remaining == 0 {
        return;
    }
    if let Ok(entries) = fs::read_dir(gitdir.join("modules")) {
        for entry in entries.flatten() {
            if *remaining == 0 {
                break;
            }
            if entry.file_type().is_ok_and(|kind| kind.is_dir()) {
                walk_gitdir(&entry.path(), depth + 1, remaining, protection);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn gitdir_directory_enumerates_control_paths() {
        let root = tempfile::tempdir().expect("tempdir");
        let gitdir = root.path().join(".git");
        fs::create_dir_all(gitdir.join("hooks")).unwrap();
        fs::create_dir_all(gitdir.join("info")).unwrap();
        fs::write(gitdir.join("config"), b"[core]").unwrap();
        let protection = enumerate(&[root.path().to_path_buf()], None);
        let gitdir = fs::canonicalize(&gitdir).unwrap();
        for name in CONTROL_ENTRIES {
            assert!(
                protection.write_denied.contains(&gitdir.join(name)),
                "{name}"
            );
            assert!(
                protection.private_paths.contains(&gitdir.join(name)),
                "{name}"
            );
        }
    }
    #[test]
    fn worktree_pointer_file_resolves_gitdir_and_stays_immutable() {
        let root = tempfile::tempdir().expect("tempdir");
        let real = root.path().join("real.git");
        fs::create_dir_all(real.join("hooks")).unwrap();
        let worktree = root.path().join("linked");
        fs::create_dir_all(&worktree).unwrap();
        fs::write(worktree.join(".git"), b"gitdir: ../real.git\n").unwrap();
        let protection = enumerate(std::slice::from_ref(&worktree), None);
        let real = fs::canonicalize(&real).unwrap();
        let pointer = fs::canonicalize(worktree.join(".git")).unwrap();
        assert!(protection.write_denied.contains(&real.join("config")));
        assert!(protection.write_denied.contains(&real.join("hooks")));
        assert!(protection.write_denied.contains(&pointer));
        assert!(protection.private_paths.contains(&pointer));
    }
    #[test]
    fn submodule_gitdirs_are_enumerated() {
        let root = tempfile::tempdir().expect("tempdir");
        let module = root.path().join(".git/modules/sub/modules/deep");
        fs::create_dir_all(&module).unwrap();
        let protection = enumerate(&[root.path().to_path_buf()], None);
        let base = fs::canonicalize(root.path().join(".git")).unwrap();
        assert!(
            protection
                .write_denied
                .contains(&base.join("modules/sub/config"))
        );
        assert!(
            protection
                .write_denied
                .contains(&base.join("modules/sub/modules/deep/config"))
        );
    }
    #[test]
    fn worktree_admin_dirs_are_write_denied() {
        let root = tempfile::tempdir().expect("tempdir");
        let admin = root.path().join(".git/worktrees/wt1");
        fs::create_dir_all(&admin).unwrap();
        let protection = enumerate(&[root.path().to_path_buf()], None);
        let admin = fs::canonicalize(&admin).unwrap();
        assert!(protection.write_denied.contains(&admin));
        assert!(protection.private_paths.contains(&admin));
    }
    #[test]
    fn missing_gitdir_still_denies_future_control_entries() {
        let root = tempfile::tempdir().expect("tempdir");
        let protection = enumerate(&[root.path().to_path_buf()], None);
        let gitdir = fs::canonicalize(root.path()).unwrap().join(".git");
        for name in CONTROL_ENTRIES {
            assert!(
                protection.write_denied.contains(&gitdir.join(name)),
                "{name}"
            );
            assert!(
                protection.private_paths.contains(&gitdir.join(name)),
                "{name}"
            );
        }
    }
    #[test]
    fn home_implicit_files_only_apply_inside_a_root() {
        let root = tempfile::tempdir().expect("tempdir");
        let inside = root.path().join("home");
        fs::create_dir_all(&inside).unwrap();
        let protection = enumerate(&[root.path().to_path_buf()], Some(&inside));
        let home = fs::canonicalize(&inside).unwrap();
        assert!(protection.write_denied.contains(&home.join(".gitconfig")));
        assert!(protection.private_paths.contains(&home.join(".ssh")));
        let outside = tempfile::tempdir().expect("tempdir");
        let protection = enumerate(&[root.path().to_path_buf()], Some(outside.path()));
        assert!(
            !protection
                .write_denied
                .iter()
                .any(|path| path.starts_with(outside.path()))
        );
    }
}
