//! Native single-file writes. The caller must authorize and persist intent
//! before dispatching; these APIs do not provide a subprocess sandbox.

use super::read::{ReadError, ReadTools, check_cancel, err, io_err};
use cap_std::fs::{Dir, OpenOptions, OpenOptionsExt};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    io::{self, Read, Write},
    path::{Component, Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

/// Default maximum bytes in a native write or patch input/output.
pub const DEFAULT_WRITE_MAX_BYTES: usize = 1024 * 1024;

/// Outcome of a successful single-file replacement.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WriteResult {
    pub path: PathBuf,
    pub version: String,
    pub created: bool,
}

#[cfg(test)]
mod private_path_tests {
    use super::*;
    use crate::tools::read::ReadLimits;
    use std::fs;

    #[test]
    fn writes_and_patches_reject_existing_and_future_managed_paths_in_bypass() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        fs::create_dir_all(root.join(".soyo/memory")).unwrap();
        let path = root.join(".soyo/MEMORY.md");
        fs::write(&path, "old\n").unwrap();
        let files =
            Arc::new(ReadTools::new(temp.path(), &[], true, ReadLimits::default()).unwrap());
        files
            .protect_paths(vec![path.clone(), root.join(".soyo/memory")])
            .unwrap();
        let writes = WriteTools::new(files, DEFAULT_WRITE_MAX_BYTES);
        let version = WriteTools::version(b"old\n");
        assert_eq!(
            writes
                .write_file(&path, "new", Some(&version), &path, None)
                .unwrap_err()
                .code,
            "path_denied"
        );
        assert_eq!(
            writes
                .apply_patch(
                    &path,
                    "--- a/.soyo/MEMORY.md\n+++ b/.soyo/MEMORY.md\n@@ -1 +1 @@\n-old\n+new\n",
                    &version,
                    &path,
                    None
                )
                .unwrap_err()
                .code,
            "path_denied"
        );
        let topic = root.join(".soyo/memory/new.md");
        assert_eq!(
            writes
                .write_file(&topic, "new", None, &topic, None)
                .unwrap_err()
                .code,
            "path_denied"
        );
        assert!(!topic.exists());
        assert_eq!(fs::read_to_string(&path).unwrap(), "old\n");
        let agents = root.join(".soyo/AGENTS.md");
        assert!(
            writes
                .write_file(&agents, "project instructions", None, &agents, None)
                .is_ok()
        );
    }

    #[test]
    fn missing_index_creation_and_late_protection_are_rechecked_before_publication() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        fs::create_dir(root.join(".soyo")).unwrap();
        let files =
            Arc::new(ReadTools::new(temp.path(), &[], false, ReadLimits::default()).unwrap());
        let index = root.join(".soyo/MEMORY.md");
        files.protect_paths(vec![index.clone()]).unwrap();
        let writes = WriteTools::new(files.clone(), DEFAULT_WRITE_MAX_BYTES);
        assert_eq!(
            writes
                .write_file(&index, "new", None, &index, None)
                .unwrap_err()
                .code,
            "path_denied"
        );
        let target = root.join("public.txt");
        fs::write(&target, "original").unwrap();
        let version = WriteTools::version(b"original");
        assert!(writes.destination(&target, Some(&version)).is_ok());
        files.protect_paths(vec![target.clone()]).unwrap();
        assert_eq!(
            writes
                .write_file(&target, "changed", Some(&version), &target, None)
                .unwrap_err()
                .code,
            "path_denied"
        );
        assert_eq!(fs::read_to_string(target).unwrap(), "original");
    }

    #[test]
    fn repository_control_targets_are_denied_without_private_registration() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        fs::create_dir_all(root.join(".git/hooks")).unwrap();
        fs::write(root.join(".git/config"), b"[core]\n").unwrap();
        let files =
            Arc::new(ReadTools::new(temp.path(), &[], true, ReadLimits::default()).unwrap());
        let writes = WriteTools::new(files.clone(), DEFAULT_WRITE_MAX_BYTES);
        for target in [
            root.join(".git/config"),
            root.join(".git/hooks/pre-commit"),
            root.join("sub/.git/config"),
        ] {
            assert_eq!(
                writes
                    .write_file(&target, "x", None, &target, None)
                    .unwrap_err()
                    .code,
                "path_denied",
                "{target:?}"
            );
        }
        assert!(
            writes
                .write_file(&root.join(".git"), "x", None, &root.join(".git"), None)
                .is_err()
        );
        assert_eq!(
            files
                .read_file(&root.join(".git/config"), 1, 10)
                .unwrap_err()
                .code,
            "path_denied"
        );
        assert_eq!(
            files
                .read_file(&root.join(".git/hooks/pre-commit"), 1, 10)
                .unwrap_err()
                .code,
            "path_denied"
        );
        let benign = root.join(".gitignore");
        assert!(
            writes
                .write_file(&benign, "*.log\n", None, &benign, None)
                .is_ok()
        );
        assert!(files.read_file(&benign, 1, 10).is_ok());
    }
}

/// Write primitives use the same allowed roots as the read tools. They do not
/// authorize operations or record execution intents; the dispatcher must do so.
struct WriteTools {
    files: Arc<ReadTools>,
    max_bytes: usize,
}
impl WriteTools {
    fn new(files: Arc<ReadTools>, max_bytes: usize) -> Self {
        Self { files, max_bytes }
    }

    fn destination(
        &self,
        path: &Path,
        expected_version: Option<&str>,
    ) -> Result<(PathBuf, Dir, PathBuf), ReadError> {
        let input = if path.is_absolute() {
            path.to_path_buf()
        } else {
            self.files.workspace.join(path)
        };
        self.files.check_private_path(&input)?;
        let target = if expected_version.is_some() {
            self.files.resolve(&input)?.0
        } else {
            match std::fs::symlink_metadata(&input) {
                Ok(_) => return Err(err("conflict", "new file target already exists")),
                Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                Err(e) => return Err(io_err(e)),
            }
            let name = input
                .file_name()
                .ok_or_else(|| err("invalid_path", "target must have a filename"))?;
            if !matches!(
                Path::new(name).components().next(),
                Some(Component::Normal(_))
            ) {
                return Err(err("invalid_path", "invalid filename"));
            }
            let parent = input
                .parent()
                .ok_or_else(|| err("invalid_path", "target must have a parent"))?;
            let parent = self.files.resolve(parent)?.0;
            parent.join(name)
        };
        self.files.check_private_path(&target)?;
        let parent = target
            .parent()
            .ok_or_else(|| err("invalid_path", "target must have a parent"))?;
        let name = target
            .file_name()
            .ok_or_else(|| err("invalid_path", "target must have a filename"))?
            .to_owned();
        let (_, root, rel) = self.files.resolve(parent)?;
        let dir = root.dir.open_dir(rel).map_err(io_err)?;
        self.files
            .check_open_file(parent, &dir.dir_metadata().map_err(io_err)?)?;
        Ok((target, dir, PathBuf::from(name)))
    }

    fn version(bytes: &[u8]) -> String {
        Sha256::digest(bytes)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect()
    }

    fn check_existing(&self, path: &Path, expected: &str) -> Result<Vec<u8>, ReadError> {
        let bytes = self.files.read_bytes(path, None)?;
        if bytes.contains(&0) || std::str::from_utf8(&bytes).is_err() {
            return Err(err("invalid_text", "existing file is not UTF-8 text"));
        }
        if Self::version(&bytes) != expected {
            return Err(err(
                "conflict",
                "file version does not match expected_version",
            ));
        }
        Ok(bytes)
    }

    fn check_in_directory(
        &self,
        dir: &Dir,
        name: &Path,
        expected: &str,
        target: &Path,
    ) -> Result<cap_std::fs::Permissions, ReadError> {
        let mut options = OpenOptions::new();
        options.read(true);
        #[cfg(unix)]
        options.custom_flags(libc::O_NONBLOCK | libc::O_NOFOLLOW);
        let file = dir.open_with(name, &options).map_err(io_err)?;
        let metadata = file.metadata().map_err(io_err)?;
        self.files.check_open_file(target, &metadata)?;
        if !metadata.is_file() {
            return Err(err("invalid_file", "target is not a regular file"));
        }
        let mut bytes = Vec::new();
        file.take(self.files.limits.file_bytes.saturating_add(1) as u64)
            .read_to_end(&mut bytes)
            .map_err(io_err)?;
        if bytes.len() > self.files.limits.file_bytes {
            return Err(err("file_too_large", "file exceeds processing limit"));
        }
        if bytes.contains(&0) || std::str::from_utf8(&bytes).is_err() {
            return Err(err("invalid_text", "existing file is not UTF-8 text"));
        }
        if Self::version(&bytes) != expected {
            return Err(err(
                "conflict",
                "file version does not match expected_version",
            ));
        }
        Ok(metadata.permissions())
    }

    /// Replace or create a file. `expected_version = None` requires a new
    /// target; existing files require their complete SHA-256 version. This
    /// function performs a real filesystem write and requires prior permission
    /// and durable-intent checks by its caller.
    fn write_file(
        &self,
        path: &Path,
        content: &str,
        expected_version: Option<&str>,
        bound_target: &Path,
        cancel: Option<&AtomicBool>,
    ) -> Result<WriteResult, ReadError> {
        check_cancel(cancel)?;
        if content.len() > self.max_bytes {
            return Err(err("write_too_large", "write exceeds configured limit"));
        }
        if content.as_bytes().contains(&0) {
            return Err(err("invalid_text", "binary content is not supported"));
        }
        let (target, dir, name) = self.destination(path, expected_version)?;
        if target != bound_target {
            return Err(err("conflict", "target changed after authorization"));
        }
        let old_permissions = if let Some(expected) = expected_version {
            Some(self.check_in_directory(&dir, &name, expected, &target)?)
        } else {
            None
        };
        let temp_name = format!(".soyo-{}.tmp", uuid::Uuid::now_v7());
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        let mut temp = dir.open_with(&temp_name, &options).map_err(io_err)?;
        let result = (|| {
            temp.write_all(content.as_bytes()).map_err(io_err)?;
            if let Some(permissions) = old_permissions {
                temp.set_permissions(permissions).map_err(io_err)?;
            }
            temp.sync_all().map_err(io_err)?;
            check_cancel(cancel)?;
            self.files.check_private_path(&target)?;
            if let Some(expected) = expected_version {
                self.check_in_directory(&dir, &name, expected, &target)?;
                check_cancel(cancel)?;
                dir.rename(&temp_name, &dir, &name).map_err(io_err)?;
            } else {
                check_cancel(cancel)?;
                dir.hard_link(&temp_name, &dir, &name).map_err(|e| {
                    if e.kind() == io::ErrorKind::AlreadyExists {
                        err("conflict", "new file target already exists")
                    } else {
                        io_err(e)
                    }
                })?;
                dir.remove_file(&temp_name).map_err(io_err)?;
            }
            Ok(WriteResult {
                path: target,
                version: Self::version(content.as_bytes()),
                created: expected_version.is_none(),
            })
        })();
        if result.is_err() {
            let _ = dir.remove_file(&temp_name);
        }
        result
    }

    /// Replace exact literal text in an existing UTF-8 file after version verification.
    fn edit_file(
        &self,
        path: &Path,
        args: &EditArgs,
        bound_target: &Path,
        cancel: Option<&AtomicBool>,
    ) -> Result<WriteResult, ReadError> {
        check_cancel(cancel)?;
        if args.old_string.is_empty() {
            return Err(err("invalid_args", "old_string must be non-empty"));
        }
        if args.old_string == args.new_string {
            return Err(err(
                "invalid_args",
                "new_string must differ from old_string",
            ));
        }
        if args.old_string.len() > self.max_bytes || args.new_string.len() > self.max_bytes {
            return Err(err("write_too_large", "edit text exceeds configured limit"));
        }
        let (current, _, _) = self.destination(path, Some(&args.expected_version))?;
        if current != bound_target {
            return Err(err("conflict", "target changed after authorization"));
        }
        let bytes = self.check_existing(bound_target, &args.expected_version)?;
        let original =
            std::str::from_utf8(&bytes).map_err(|_| err("invalid_text", "file is not UTF-8"))?;
        let matches = original.match_indices(&args.old_string).count();
        if matches == 0 {
            return Err(err(
                "edit_conflict",
                "old_string was not found in the file; reread the file and copy exact text",
            ));
        }
        if matches > 1 && !args.replace_all {
            return Err(err(
                "edit_conflict",
                format!(
                    "old_string matched {matches} occurrences; include more surrounding context or set replace_all = true"
                ),
            ));
        }
        let updated = if args.replace_all {
            original.replace(&args.old_string, &args.new_string)
        } else {
            original.replacen(&args.old_string, &args.new_string, 1)
        };
        self.write_file(
            path,
            &updated,
            Some(&args.expected_version),
            bound_target,
            cancel,
        )
    }

    /// Apply one unified diff to an existing text file after checking its version.
    /// Multi-file, create, rename and delete patches are rejected.
    fn apply_patch(
        &self,
        path: &Path,
        patch_text: &str,
        expected_version: &str,
        bound_target: &Path,
        cancel: Option<&AtomicBool>,
    ) -> Result<WriteResult, ReadError> {
        check_cancel(cancel)?;
        use diffy::patch_set::{FileOperation, ParseOptions, PatchSet};
        if patch_text.len() > self.max_bytes {
            return Err(err("write_too_large", "patch exceeds configured limit"));
        }
        let (current, _, _) = self.destination(path, Some(expected_version))?;
        if current != bound_target {
            return Err(err("conflict", "target changed after authorization"));
        }
        let bytes = self.check_existing(bound_target, expected_version)?;
        let original =
            std::str::from_utf8(&bytes).map_err(|_| err("invalid_text", "file is not UTF-8"))?;
        let active_patch_text = match PatchSet::parse(patch_text, ParseOptions::unidiff())
            .collect::<Result<Vec<_>, _>>()
        {
            Ok(_) => patch_text.to_owned(),
            Err(first_error) => normalize_unified_diff_counts(patch_text)
                .ok_or_else(|| err("invalid_patch", invalid_patch_hint(&first_error)))?,
        };
        let patches: Vec<_> = PatchSet::parse(&active_patch_text, ParseOptions::unidiff())
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| err("invalid_patch", invalid_patch_hint(&e)))?;
        if patches.len() != 1 {
            return Err(err("invalid_patch", "exactly one file patch is required"));
        }
        let file = &patches[0];
        if !matches!(file.operation(), FileOperation::Modify { .. }) {
            return Err(err(
                "invalid_patch",
                "only modification patches are allowed",
            ));
        }
        let patch = file
            .patch()
            .as_text()
            .ok_or_else(|| err("invalid_patch", "binary patches are not supported"))?;
        let old = patch
            .original()
            .ok_or_else(|| err("invalid_patch", "missing original filename"))?;
        let new = patch
            .modified()
            .ok_or_else(|| err("invalid_patch", "missing modified filename"))?;
        let normalized = |path: &Path| {
            path.components()
                .filter(|component| *component != Component::CurDir)
                .collect::<PathBuf>()
        };
        let original_path = normalized(Path::new(old));
        let modified_path = normalized(Path::new(new));
        let (old, new) = match (
            original_path.strip_prefix("a"),
            modified_path.strip_prefix("b"),
        ) {
            (Ok(old), Ok(new)) => (old, new),
            _ => (original_path.as_path(), modified_path.as_path()),
        };
        if old != new || old != normalized(path) {
            let suggested = path
                .strip_prefix(&self.files.workspace)
                .unwrap_or(path)
                .display()
                .to_string();
            return Err(err(
                "invalid_patch",
                format!(
                    concat!(
                        "patch paths must match the requested file; ",
                        "use a workspace-relative path such as `{suggested}` ",
                        "and the same file in `--- a/{suggested}` and `+++ b/{suggested}`"
                    ),
                    suggested = suggested,
                ),
            ));
        }
        let updated = match diffy::apply(original, patch) {
            Ok(text) => text,
            Err(first_conflict) => {
                if let Some(relocated) = relocate_patch_by_context(original, &active_patch_text)? {
                    let reparsed: Vec<_> = PatchSet::parse(&relocated, ParseOptions::unidiff())
                        .collect::<Result<_, _>>()
                        .map_err(|_| err("patch_conflict", patch_conflict_hint(&first_conflict)))?;
                    if let Some(text_patch) = reparsed.first().and_then(|f| f.patch().as_text()) {
                        diffy::apply(original, text_patch).map_err(|conflict| {
                            err("patch_conflict", patch_conflict_hint(&conflict))
                        })?
                    } else {
                        return Err(err("patch_conflict", patch_conflict_hint(&first_conflict)));
                    }
                } else {
                    return Err(err("patch_conflict", patch_conflict_hint(&first_conflict)));
                }
            }
        };
        self.write_file(path, &updated, Some(expected_version), bound_target, cancel)
    }
}

fn invalid_patch_hint(error: &impl std::fmt::Display) -> String {
    format!(
        "{error}; expected a standard unified diff with `--- a/<path>`, `+++ b/<path>`, and \
`@@ -old_start,old_count +new_start,new_count @@` where old_count = context + `-` lines \
and new_count = context + `+` lines; for a one-line replacement both counts equal the hunk \
body line count minus 1 (never include `*** Begin Patch` or `*** End Patch`)"
    )
}

fn patch_conflict_hint(error: &impl std::fmt::Display) -> String {
    format!(
        "{error}; reread the file and copy exact consecutive context; verify hunk \
old_start/new_start against the actual 1-based lines and counts from that read, and include \
preceding hunks' net line delta in later new_start values"
    )
}

struct ParsedHunk<'a> {
    old_start: usize,
    new_start: usize,
    suffix: String,
    body_lines: Vec<&'a str>,
    old_count: usize,
    new_count: usize,
}

fn strip_line_ending(line: &str) -> &str {
    let no_lf = line.strip_suffix('\n').unwrap_or(line);
    no_lf.strip_suffix('\r').unwrap_or(no_lf)
}

fn parse_hunk_header(line: &str) -> Option<(usize, usize, String)> {
    let trimmed = strip_line_ending(line);
    let rest = trimmed.strip_prefix("@@ ")?;
    let end = rest.find(" @@")?;
    let (ranges, suffix) = (&rest[..end], &rest[end..]);
    let mut parts = ranges.split_whitespace();
    let old_part = parts.next()?.strip_prefix('-')?;
    let new_part = parts.next()?.strip_prefix('+')?;
    if parts.next().is_some() {
        return None;
    }
    let old_start = old_part.split(',').next()?.parse::<usize>().ok()?;
    let new_start = new_part.split(',').next()?.parse::<usize>().ok()?;
    Some((old_start, new_start, suffix.to_string()))
}

fn parse_unified_patch_parts<'a>(
    patch_text: &'a str,
) -> Option<(Vec<&'a str>, Vec<ParsedHunk<'a>>)> {
    if patch_text.contains("*** Begin Patch") || patch_text.contains("*** End Patch") {
        return None;
    }
    let mut prefix = Vec::new();
    let mut hunks: Vec<ParsedHunk<'a>> = Vec::new();
    let mut saw_headers = false;
    for line in patch_text.split_inclusive('\n') {
        if let Some((old_start, new_start, suffix)) = parse_hunk_header(line) {
            saw_headers = true;
            hunks.push(ParsedHunk {
                old_start,
                new_start,
                suffix,
                body_lines: Vec::new(),
                old_count: 0,
                new_count: 0,
            });
            continue;
        }
        if let Some(current) = hunks.last_mut() {
            if line.starts_with(' ') {
                current.old_count += 1;
                current.new_count += 1;
            } else if line.starts_with('-') {
                current.old_count += 1;
            } else if line.starts_with('+') {
                current.new_count += 1;
            } else if !line.starts_with('\\') {
                return None;
            }
            current.body_lines.push(line);
        } else {
            prefix.push(line);
        }
    }
    if !saw_headers || hunks.is_empty() {
        return None;
    }
    Some((prefix, hunks))
}

fn normalize_unified_diff_counts(patch_text: &str) -> Option<String> {
    let (prefix, hunks) = parse_unified_patch_parts(patch_text)?;
    let mut out = String::with_capacity(patch_text.len() + 32);
    for line in prefix {
        out.push_str(line);
    }
    for hunk in hunks {
        let old_start = if hunk.old_count > 0 && hunk.old_start == 0 {
            1
        } else {
            hunk.old_start
        };
        let new_start = if hunk.new_count > 0 && hunk.new_start == 0 {
            1
        } else {
            hunk.new_start
        };
        out.push_str(&format!(
            "@@ -{old_start},{} +{new_start},{}{}\n",
            hunk.old_count, hunk.new_count, hunk.suffix
        ));
        for line in hunk.body_lines {
            out.push_str(line);
        }
    }
    Some(out)
}

fn relocate_patch_by_context(
    original: &str,
    patch_text: &str,
) -> Result<Option<String>, ReadError> {
    let Some((prefix, hunks)) = parse_unified_patch_parts(patch_text) else {
        return Ok(None);
    };
    let orig_lines: Vec<&str> = original.lines().collect();
    let mut cursor = 0usize;
    let mut cumulative_delta: isize = 0;
    let mut out = String::with_capacity(patch_text.len() + 32);
    for line in prefix {
        out.push_str(line);
    }
    for hunk in hunks {
        let old_lines: Vec<&str> = hunk
            .body_lines
            .iter()
            .filter_map(|line| {
                let first = line.as_bytes().first().copied()?;
                if first == b' ' || first == b'-' {
                    Some(strip_line_ending(&line[1..]))
                } else {
                    None
                }
            })
            .collect();
        if old_lines.is_empty() || old_lines.len() > orig_lines.len() {
            return Ok(None);
        }
        let search_limit = orig_lines.len() - old_lines.len();
        if cursor > search_limit {
            return Ok(None);
        }
        let matches: Vec<usize> = (cursor..=search_limit)
            .filter(|&pos| orig_lines[pos..pos + old_lines.len()] == old_lines[..])
            .collect();
        if matches.len() > 1 {
            return Err(err(
                "patch_conflict",
                format!(
                    "hunk context matched {} locations; add more surrounding context or verify old_start/new_start from the latest read",
                    matches.len()
                ),
            ));
        }
        let Some(&pos) = matches.first() else {
            return Ok(None);
        };
        let old_start = pos + 1;
        let new_start = (old_start as isize + cumulative_delta).max(1) as usize;
        cursor = pos + old_lines.len();
        cumulative_delta += hunk.new_count as isize - hunk.old_count as isize;
        out.push_str(&format!(
            "@@ -{old_start},{} +{new_start},{}{}\n",
            hunk.old_count, hunk.new_count, hunk.suffix
        ));
        for line in hunk.body_lines {
            out.push_str(line);
        }
    }
    Ok(Some(out))
}

/// Authorization hook supplied by the runtime. A successful decision must be
/// bound to this specific operation and invalidated when its context changes.
pub trait WriteGate: Send + Sync {
    fn authorize(
        &self,
        call: &crate::contract::ToolCall,
        context: &crate::boundary::ToolContext,
        normalized_target: &Path,
    ) -> Result<bool, crate::contract::BoundaryError>;
    /// Release any dispatch lease after completion, cancellation or error.
    fn finish(&self) -> Result<(), crate::contract::BoundaryError> {
        Ok(())
    }
}
struct WriteLease<'a>(&'a dyn WriteGate);
impl Drop for WriteLease<'_> {
    fn drop(&mut self) {
        let _ = self.0.finish();
    }
}

/// Native write operation selected by the tool registry.
#[derive(Debug, Clone, Copy)]
pub enum WriteToolKind {
    WriteFile,
    EditFile,
    ApplyPatch,
}

/// Write tool adapter with mandatory authorization and execution-intent store.
/// In-memory storage is suitable only for isolated tests; real workspace writes
/// require the runtime's durable store and dispatch policy.
pub struct WriteTool {
    files: Arc<ReadTools>,
    store: Arc<dyn crate::boundary::Store>,
    gate: Arc<dyn WriteGate>,
    kind: WriteToolKind,
    max_bytes: usize,
    known_secrets: Vec<String>,
}
impl WriteTool {
    pub fn new(
        files: Arc<ReadTools>,
        store: Arc<dyn crate::boundary::Store>,
        gate: Arc<dyn WriteGate>,
        kind: WriteToolKind,
        max_bytes: usize,
    ) -> Self {
        Self {
            files,
            store,
            gate,
            kind,
            max_bytes,
            known_secrets: Vec::new(),
        }
    }
    /// Supply trusted credential values to sanitize persisted intents and
    /// results. File contents are written unchanged after authorization.
    pub fn with_known_secrets(mut self, known_secrets: Vec<String>) -> Self {
        self.known_secrets = known_secrets;
        self
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WriteArgs {
    path: PathBuf,
    content: String,
    #[serde(deserialize_with = "required_version")]
    expected_version: Option<String>,
}
fn required_version<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<String>, D::Error> {
    Option::<String>::deserialize(deserializer)
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PatchArgs {
    path: PathBuf,
    patch: String,
    expected_version: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EditArgs {
    path: PathBuf,
    old_string: String,
    new_string: String,
    #[serde(default)]
    replace_all: bool,
    expected_version: String,
}
enum WriteAction {
    Write(WriteArgs),
    Edit(EditArgs),
    Patch(PatchArgs),
}

pub(crate) async fn save_result(
    store: &dyn crate::boundary::Store,
    call: &crate::contract::ToolCall,
    context: &crate::boundary::ToolContext,
    result: &crate::contract::ToolResult,
) -> Result<(), crate::contract::BoundaryError> {
    save_result_with_outputs(store, call, context, result, &[]).await
}

pub(crate) async fn save_result_with_outputs(
    store: &dyn crate::boundary::Store,
    call: &crate::contract::ToolCall,
    context: &crate::boundary::ToolContext,
    result: &crate::contract::ToolResult,
    outputs: &[crate::output_store::OutputMetadata],
) -> Result<(), crate::contract::BoundaryError> {
    use crate::boundary::StoreChange;
    use crate::contract::{HistoryItem, ToolState};
    let mut changes = vec![
        StoreChange::ToolFinished {
            run_id: context.run.id,
            call_id: call.call_id.clone(),
            result: result.clone(),
        },
        StoreChange::History {
            session_id: context.run.session_id,
            item: HistoryItem::ToolResult {
                call_id: call.call_id.clone(),
                result: result.clone(),
            },
        },
    ];
    if !outputs.is_empty() {
        changes.push(StoreChange::OutputReferences {
            run_id: context.run.id,
            call_id: call.call_id.clone(),
            outputs: outputs.to_vec(),
        });
    }
    let state = result
        .error
        .as_ref()
        .and_then(|error| match error.code.as_str() {
            "permission_denied" => Some(ToolState::Denied),
            "outcome_unknown" => Some(ToolState::Unknown),
            _ => None,
        });
    if let Some(state) = state {
        changes.push(StoreChange::ToolState {
            run_id: context.run.id,
            call_id: call.call_id.clone(),
            state,
        });
    }
    if let Err(mut error) = store.commit(&changes).await {
        error.2 = Some(Box::new(result.clone()));
        return Err(error);
    }
    Ok(())
}

pub(crate) async fn save_result_with_secrets(
    store: &dyn crate::boundary::Store,
    call: &crate::contract::ToolCall,
    context: &crate::boundary::ToolContext,
    result: &crate::contract::ToolResult,
    known_secrets: &[String],
) -> Result<crate::contract::ToolResult, crate::contract::BoundaryError> {
    let result = sanitize_result(result, known_secrets)?;
    save_result(store, call, context, &result).await?;
    Ok(result)
}

pub(crate) fn sanitize_result(
    result: &crate::contract::ToolResult,
    known_secrets: &[String],
) -> Result<crate::contract::ToolResult, crate::contract::BoundaryError> {
    crate::redaction::tool_result(result, known_secrets).map_err(|mut error| {
        // The filesystem/process may already have changed. Preserve that
        // observation while withholding any data that could not be sanitized.
        error.2 = Some(Box::new(crate::contract::ToolResult {
            ok: result.ok,
            data: None,
            error: result.error.as_ref().map(|_| error.0.clone()),
            truncated: true,
            media_refs: Vec::new(),
        }));
        error
    })
}

impl crate::boundary::Tool for WriteTool {
    fn definition(&self) -> crate::boundary::ToolDefinition {
        use serde_json::json;
        let (name, desc, properties) = match self.kind {
            WriteToolKind::WriteFile => (
                "write_file",
                concat!(
                    "Create one UTF-8 file or replace an existing one after version validation. ",
                    "Existing files require the complete SHA-256 version from the latest read; ",
                    "null is valid only for a target that does not yet exist. ",
                    "Parent directories are not created automatically."
                ),
                json!({
                    "path": {
                        "type": "string",
                        "description": "Workspace-relative target file path"
                    },
                    "content": {"type": "string"},
                    "expected_version": {
                        "type": ["string", "null"],
                        "description": concat!(
                            "Complete SHA-256 version from the latest read_file, ",
                            "or null for a new file"
                        )
                    }
                }),
            ),
            WriteToolKind::EditFile => (
                "edit_file",
                concat!(
                    "Replace exact literal text in one existing UTF-8 file after checking ",
                    "expected_version from the latest read. Prefer edit_file over apply_patch for ",
                    "focused text replacements. old_string must be non-empty and match verbatim ",
                    "(including indentation and line endings), matching once unless replace_all = true."
                ),
                json!({
                    "path": {
                        "type": "string",
                        "description": "Workspace-relative path of the existing file"
                    },
                    "old_string": {
                        "type": "string",
                        "description": "Exact existing literal text to replace (non-empty)"
                    },
                    "new_string": {
                        "type": "string",
                        "description": "Replacement text (must differ from old_string)"
                    },
                    "replace_all": {
                        "type": "boolean",
                        "description": concat!(
                            "Replace every occurrence of old_string when true ",
                            "(defaults to false, requiring a unique match)"
                        )
                    },
                    "expected_version": {
                        "type": "string",
                        "description": "Complete SHA-256 version from the latest read_file"
                    }
                }),
            ),
            WriteToolKind::ApplyPatch => (
                "apply_patch",
                concat!(
                    "Modify one existing UTF-8 file with a standard unified diff; never use ",
                    "`*** Begin Patch` or `*** End Patch`. Supply the complete expected_version from ",
                    "the latest read. Use a workspace-relative path and matching headers such as ",
                    "`--- a/src/lib.rs` and `+++ b/src/lib.rs`. Before submitting each hunk, copy exact ",
                    "consecutive context lines from the latest read without skipping intervening lines. ",
                    "Calculate `old_count` as context plus `-` lines and `new_count` as context plus `+` ",
                    "lines. For a one-line replacement, both counts equal the hunk body line count ",
                    "minus 1; never use the total hunk body line count. One context line before and ",
                    "after a one-line replacement therefore uses `@@ -start,3 +start,3 @@`. Use actual ",
                    "1-based line numbers from the latest read for hunk origins, never guesses from ",
                    "function order. For multiple hunks, `old_start` uses the original file and ",
                    "`new_start` includes the cumulative line delta from preceding hunks. Every hunk line ",
                    "starts with ` `, `-`, or `+` and ends with a newline. Multiple hunks for the file ",
                    "must be in ascending line order. For distant or multi-line edits, prefer one small ",
                    "hunk per call and reread before the next edit, using its newly returned version. ",
                    "Creation, deletion, rename, and multi-file patches are rejected; use write_file ",
                    "for new files.\n\n",
                    "Example patch (2 context + 1 removed = 3 old lines; 2 context + 1 added = 3 new lines):\n",
                    "--- a/src/lib.rs\n",
                    "+++ b/src/lib.rs\n",
                    "@@ -2,3 +2,3 @@\n",
                    " fn total(a: i32, b: i32) -> i32 {\n",
                    "-    a - b\n",
                    "+    a + b\n",
                    " }\n"
                ),
                json!({
                    "path": {
                        "type": "string",
                        "description": concat!(
                            "Workspace-relative path of the existing file ",
                            "(must match --- a/<path> and +++ b/<path>)"
                        )
                    },
                    "patch": {
                        "type": "string",
                        "description": concat!(
                            "Standard unified diff with exact consecutive context. Calculate ",
                            "old_count = context + removed lines and new_count = context + added ",
                            "lines independently. For a one-line replacement, both counts equal ",
                            "hunk_body_lines - 1; never use the total hunk body line count. Do not ",
                            "skip intervening context lines. Use actual line numbers from the ",
                            "latest read; for multiple hunks, old_start is in the original file ",
                            "and new_start includes preceding net line changes. For distant or ",
                            "multi-line edits, prefer one hunk per call and reread before the next edit."
                        )
                    },
                    "expected_version": {
                        "type": "string",
                        "description": "Complete SHA-256 version from the latest read_file"
                    }
                }),
            ),
        };
        let required = match self.kind {
            WriteToolKind::WriteFile => json!(["path", "content", "expected_version"]),
            WriteToolKind::EditFile => {
                json!(["path", "old_string", "new_string", "expected_version"])
            }
            WriteToolKind::ApplyPatch => json!(["path", "patch", "expected_version"]),
        };
        crate::boundary::ToolDefinition {
            name: name.into(),
            description: desc.into(),
            input_schema: json!({"type":"object","properties":properties,"required":required,"additionalProperties":false}),
            examples: match name {
                "edit_file" => vec![
                    json!({
                        "path": "crates/soyo-core/src/config.rs",
                        "old_string": "pub const DEFAULT_TIMEOUT_SECONDS: u64 = 30;",
                        "new_string": "pub const DEFAULT_TIMEOUT_SECONDS: u64 = 60;",
                        "expected_version": "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
                    }),
                    json!({
                        "path": "crates/soyo-core/src/tools/discovery.rs",
                        "old_string": "MAX_RESULTS",
                        "new_string": "MAX_SEARCH_LIMIT",
                        "replace_all": true,
                        "expected_version": "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824"
                    }),
                ],
                "apply_patch" => vec![json!({
                    "path": "src/lib.rs",
                    "patch": "--- a/src/lib.rs\n+++ b/src/lib.rs\n@@ -2,3 +2,3 @@\n fn total(a: i32, b: i32) -> i32 {\n-    a - b\n+    a + b\n }\n",
                    "expected_version": "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824"
                })],
                _ => Vec::new(),
            },
        }
    }
    fn execute<'a>(
        &'a self,
        call: &'a crate::contract::ToolCall,
        context: &'a crate::boundary::ToolContext,
        cancel: &'a dyn crate::boundary::Cancellation,
    ) -> crate::boundary::Operation<'a, crate::contract::ToolResult> {
        Box::pin(async move {
            use crate::boundary::StoreChange;
            use crate::contract::{
                BoundaryError, PermissionMode, ToolResult, WorkMode, error_codes,
            };
            let cancelled = || BoundaryError::new(error_codes::CANCELLED, "tool cancelled");
            if cancel.is_cancelled() {
                return Err(cancelled());
            }
            if context.run.work_mode == WorkMode::Plan {
                return Ok(ToolResult::failure(
                    error_codes::MODE_DENIED,
                    "PLAN does not allow writes",
                ));
            }
            if std::fs::canonicalize(&context.workspace)
                .map_err(|e| BoundaryError::new(error_codes::INVALID_WORKSPACE, e.to_string()))?
                != self.files.workspace
            {
                return Ok(ToolResult::failure(
                    error_codes::INVALID_WORKSPACE,
                    "tool workspace differs from run",
                ));
            }
            if self.files.bypass && context.run.permissions != PermissionMode::Bypass {
                return Ok(ToolResult::failure(
                    error_codes::PATH_DENIED,
                    "unrestricted roots require bypass permissions",
                ));
            }
            if call.name != self.definition().name {
                return Ok(ToolResult::failure(
                    error_codes::INVALID_ARGS,
                    "tool name does not match adapter",
                ));
            }
            let action = match self.kind {
                WriteToolKind::WriteFile => {
                    serde_json::from_str::<WriteArgs>(&call.arguments).map(WriteAction::Write)
                }
                WriteToolKind::EditFile => {
                    serde_json::from_str::<EditArgs>(&call.arguments).map(WriteAction::Edit)
                }
                WriteToolKind::ApplyPatch => {
                    serde_json::from_str::<PatchArgs>(&call.arguments).map(WriteAction::Patch)
                }
            };
            let action = match action {
                Ok(v) => v,
                Err(e) => {
                    return Ok(ToolResult::failure(
                        error_codes::INVALID_ARGS,
                        e.to_string(),
                    ));
                }
            };
            let path_and_version = match &action {
                WriteAction::Write(args) => (args.path.as_path(), args.expected_version.as_deref()),
                WriteAction::Edit(args) => {
                    (args.path.as_path(), Some(args.expected_version.as_str()))
                }
                WriteAction::Patch(args) => {
                    (args.path.as_path(), Some(args.expected_version.as_str()))
                }
            };
            let bound_target = match WriteTools::new(self.files.clone(), self.max_bytes)
                .destination(path_and_version.0, path_and_version.1)
            {
                Ok((target, _, _)) => target,
                Err(error) => return Ok(ToolResult::failure(error.code, error.message)),
            };
            if !self.gate.authorize(call, context, &bound_target)? {
                return Ok(ToolResult::failure(
                    error_codes::PERMISSION_DENIED,
                    "write not authorized",
                ));
            }
            let _lease = WriteLease(self.gate.as_ref());
            let handle = tokio::runtime::Handle::try_current().map_err(|_| {
                BoundaryError::new(
                    error_codes::RUNTIME_ERROR,
                    "Tokio runtime required for file tools",
                )
            })?;
            let mut saved_call = call.clone();
            let mut arguments = serde_json::Value::String(call.arguments.clone());
            crate::redaction::redact(&mut arguments, &self.known_secrets)?;
            saved_call.arguments = arguments.as_str().unwrap_or_default().to_owned();
            self.store
                .commit(&[StoreChange::ToolIntent {
                    run_id: context.run.id,
                    call: saved_call,
                }])
                .await?;
            if cancel.is_cancelled() {
                let result =
                    ToolResult::failure(error_codes::CANCELLED, "tool cancelled before dispatch");
                save_result_with_secrets(
                    self.store.as_ref(),
                    call,
                    context,
                    &result,
                    &self.known_secrets,
                )
                .await?;
                return Err(cancelled());
            }
            match self.gate.authorize(call, context, &bound_target) {
                Ok(true) => {}
                Ok(false) => {
                    let result = ToolResult::failure(
                        error_codes::PERMISSION_DENIED,
                        "write authorization no longer valid",
                    );
                    return save_result_with_secrets(
                        self.store.as_ref(),
                        call,
                        context,
                        &result,
                        &self.known_secrets,
                    )
                    .await;
                }
                Err(gate_error) => {
                    let result = ToolResult::failure(gate_error.code(), gate_error.message());
                    save_result_with_secrets(
                        self.store.as_ref(),
                        call,
                        context,
                        &result,
                        &self.known_secrets,
                    )
                    .await?;
                    return Err(gate_error);
                }
            }
            let flag = Arc::new(AtomicBool::new(false));
            let worker_flag = flag.clone();
            let files = self.files.clone();
            let max_bytes = self.max_bytes;
            let mut task = handle.spawn_blocking(move || {
                let tools = WriteTools::new(files, max_bytes);
                match action {
                    WriteAction::Write(a) => tools.write_file(
                        &a.path,
                        &a.content,
                        a.expected_version.as_deref(),
                        &bound_target,
                        Some(&worker_flag),
                    ),
                    WriteAction::Edit(a) => {
                        tools.edit_file(&a.path, &a, &bound_target, Some(&worker_flag))
                    }
                    WriteAction::Patch(a) => tools.apply_patch(
                        &a.path,
                        &a.patch,
                        &a.expected_version,
                        &bound_target,
                        Some(&worker_flag),
                    ),
                }
            });
            let outcome = tokio::select! {
                biased;
                _ = cancel.cancelled() => { flag.store(true, Ordering::Relaxed); task.await },
                outcome = &mut task => outcome,
            }
            .map_err(|e| BoundaryError::new(error_codes::TOOL_ERROR, e.to_string()))?;
            let result = match outcome {
                Ok(data) => match serde_json::to_value(data) {
                    Ok(value) => ToolResult::success(Some(value)),
                    Err(e) => ToolResult::failure(error_codes::SERIALIZATION_ERROR, e.to_string()),
                },
                Err(e) => ToolResult::failure(e.code, e.message),
            };
            let result = save_result_with_secrets(
                self.store.as_ref(),
                call,
                context,
                &result,
                &self.known_secrets,
            )
            .await?;
            if result.has_error_code(error_codes::CANCELLED) {
                return Err(cancelled());
            }
            Ok(result)
        })
    }
}
