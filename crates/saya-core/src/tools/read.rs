//! Read-only workspace tools. Root checks constrain these file APIs, not processes.

use cap_std::{
    ambient_authority,
    fs::{Dir, OpenOptions, OpenOptionsExt},
};
use globset::Glob;
use ignore::{DirEntry, WalkBuilder};
use regex::Regex;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    io::{self, Read},
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, Ordering},
};

/// Read and search output and processing limits. Callers configure these; model
/// tool arguments must not increase them.
#[derive(Debug, Clone)]
pub struct ReadLimits {
    pub read_return_bytes: usize,
    pub file_bytes: usize,
    pub search_results: usize,
    pub search_bytes: usize,
    pub list_entries: usize,
    pub list_bytes: usize,
}
impl Default for ReadLimits {
    fn default() -> Self {
        Self {
            read_return_bytes: 256 * 1024,
            file_bytes: 16 * 1024 * 1024,
            search_results: 200,
            search_bytes: 128 * 1024,
            list_entries: 1000,
            list_bytes: 128 * 1024,
        }
    }
}

/// A typed, credential-free tool failure.
#[derive(Debug)]
pub struct ReadError {
    pub code: &'static str,
    pub message: String,
}
impl std::fmt::Display for ReadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}
impl std::error::Error for ReadError {}
fn err(code: &'static str, message: impl Into<String>) -> ReadError {
    ReadError {
        code,
        message: message.into(),
    }
}
fn io_err(e: io::Error) -> ReadError {
    err("io_error", e.to_string())
}
fn check_cancel(cancel: Option<&AtomicBool>) -> Result<(), ReadError> {
    if cancel.is_some_and(|flag| flag.load(Ordering::Relaxed)) {
        Err(err("cancelled", "tool cancelled"))
    } else {
        Ok(())
    }
}

struct Root {
    path: PathBuf,
    dir: Dir,
}
/// Roots are resolved and opened once. In bypass mode `/` is used; this is not
/// an OS sandbox and does not restrict subprocesses or external APIs.
pub struct ReadTools {
    workspace: PathBuf,
    roots: Vec<Root>,
    limits: ReadLimits,
    bypass: bool,
}

#[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct FileRead {
    pub content: String,
    pub start_line: usize,
    pub end_line: usize,
    pub version: String,
    pub truncated: bool,
}
#[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct DirItem {
    pub name: String,
    pub kind: String,
}
#[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct SearchHit {
    pub path: PathBuf,
    pub line: usize,
    pub text: String,
}
#[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Page<T> {
    pub items: Vec<T>,
    pub truncated: bool,
}

impl ReadTools {
    /// Open the workspace and additional roots. Missing or inaccessible roots
    /// return an error. `bypass` removes root restrictions; it does not grant
    /// permission to execute an operation or enable write access.
    pub fn new(
        workspace: &Path,
        additional_roots: &[PathBuf],
        bypass: bool,
        limits: ReadLimits,
    ) -> Result<Self, ReadError> {
        let workspace = std::fs::canonicalize(workspace).map_err(io_err)?;
        if !workspace.is_dir() {
            return Err(err("invalid_path", "workspace is not a directory"));
        }
        let paths = if bypass {
            vec![PathBuf::from("/")]
        } else {
            let mut paths = vec![workspace.clone()];
            for root in additional_roots {
                paths.push(std::fs::canonicalize(root).map_err(io_err)?);
            }
            paths
        };
        let mut roots = Vec::new();
        for path in paths {
            roots.push(Root {
                dir: Dir::open_ambient_dir(&path, ambient_authority()).map_err(io_err)?,
                path,
            });
        }
        // Longest root first, so nested explicit roots are interpreted consistently.
        roots.sort_by_key(|r| std::cmp::Reverse(r.path.components().count()));
        Ok(Self {
            workspace,
            roots,
            limits,
            bypass,
        })
    }

    fn resolve(&self, path: &Path) -> Result<(PathBuf, &Root, PathBuf), ReadError> {
        let input = if path.is_absolute() {
            path.to_owned()
        } else {
            self.workspace.join(path)
        };
        let target = std::fs::canonicalize(input).map_err(io_err)?;
        for root in &self.roots {
            if let Ok(relative) = target.strip_prefix(&root.path) {
                let rel = if relative.as_os_str().is_empty() {
                    PathBuf::from(".")
                } else {
                    relative.to_owned()
                };
                return Ok((target, root, rel));
            }
        }
        Err(err("path_denied", "target is outside permitted roots"))
    }

    fn read_bytes(&self, path: &Path, cancel: Option<&AtomicBool>) -> Result<Vec<u8>, ReadError> {
        check_cancel(cancel)?;
        let (_, root, relative) = self.resolve(path)?;
        let mut options = OpenOptions::new();
        options.read(true);
        #[cfg(unix)]
        options.custom_flags(libc::O_NONBLOCK);
        let file = root.dir.open_with(relative, &options).map_err(io_err)?;
        if !file.metadata().map_err(io_err)?.is_file() {
            return Err(err("invalid_file", "target is not a regular file"));
        }
        let mut data = Vec::new();
        let mut reader = file.take(self.limits.file_bytes.saturating_add(1) as u64);
        let mut buffer = [0u8; 8192];
        loop {
            check_cancel(cancel)?;
            let count = reader.read(&mut buffer).map_err(io_err)?;
            if count == 0 {
                break;
            }
            data.extend_from_slice(&buffer[..count]);
        }
        if data.len() > self.limits.file_bytes {
            return Err(err("file_too_large", "file exceeds processing limit"));
        }
        Ok(data)
    }

    /// Read one UTF-8 file, hashing its complete content. Lines are one-based;
    /// `line_count` must be nonzero. Size, UTF-8 and path failures are errors.
    pub fn read_file(
        &self,
        path: &Path,
        start_line: usize,
        line_count: usize,
    ) -> Result<FileRead, ReadError> {
        self.read_file_inner(path, start_line, line_count, None)
    }
    fn read_file_inner(
        &self,
        path: &Path,
        start_line: usize,
        line_count: usize,
        cancel: Option<&AtomicBool>,
    ) -> Result<FileRead, ReadError> {
        if start_line == 0 || line_count == 0 {
            return Err(err(
                "invalid_args",
                "line numbers and count must be positive",
            ));
        }
        let bytes = self.read_bytes(path, cancel)?;
        let version: String = Sha256::digest(&bytes)
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        if bytes.contains(&0) {
            return Err(err("invalid_text", "binary input is not text"));
        }
        let text =
            std::str::from_utf8(&bytes).map_err(|_| err("invalid_text", "file is not UTF-8"))?;
        let mut result = String::new();
        let mut end_line = start_line.saturating_sub(1);
        let mut truncated = false;
        for (index, line) in text.split_inclusive('\n').enumerate().skip(start_line - 1) {
            if index >= start_line.saturating_sub(1).saturating_add(line_count) {
                truncated = true;
                break;
            }
            let remaining = self.limits.read_return_bytes.saturating_sub(result.len());
            if line.len() > remaining {
                let mut end = remaining.min(line.len());
                while !line.is_char_boundary(end) {
                    end -= 1;
                }
                result.push_str(&line[..end]);
                if end > 0 {
                    end_line = index + 1;
                }
                truncated = true;
                break;
            }
            result.push_str(line);
            end_line = index + 1;
        }
        Ok(FileRead {
            content: result,
            start_line,
            end_line,
            version,
            truncated,
        })
    }

    /// List direct children, sorted by name and bounded by configured limits.
    pub fn list_dir(&self, path: &Path) -> Result<Page<DirItem>, ReadError> {
        self.list_dir_inner(path, None)
    }
    fn list_dir_inner(
        &self,
        path: &Path,
        cancel: Option<&AtomicBool>,
    ) -> Result<Page<DirItem>, ReadError> {
        check_cancel(cancel)?;
        let (_, root, rel) = self.resolve(path)?;
        let mut entries = std::collections::BTreeMap::new();
        for entry in root.dir.read_dir(rel).map_err(io_err)? {
            check_cancel(cancel)?;
            let entry = entry.map_err(io_err)?;
            let name = entry
                .file_name()
                .into_string()
                .map_err(|_| err("invalid_path", "non-UTF-8 filename"))?;
            let kind = entry.file_type().map_err(io_err)?;
            let kind = if kind.is_dir() {
                "directory"
            } else if kind.is_file() {
                "file"
            } else if kind.is_symlink() {
                "symlink"
            } else {
                "other"
            };
            entries.insert(
                name.clone(),
                DirItem {
                    name,
                    kind: kind.into(),
                },
            );
            if entries.len() > self.limits.list_entries.saturating_add(1) {
                entries.pop_last();
            }
        }
        bounded_page(
            entries.into_values().collect(),
            self.limits.list_entries,
            self.limits.list_bytes,
        )
    }

    fn walker(&self, path: &Path, include_ignored: bool) -> Result<ignore::Walk, ReadError> {
        let (target, root, rel) = self.resolve(path)?;
        if !root.dir.metadata(rel).map_err(io_err)?.is_dir() {
            return Err(err("invalid_path", "search root is not a directory"));
        }
        let mut builder = WalkBuilder::new(target);
        builder
            .require_git(false)
            .follow_links(false)
            .hidden(!include_ignored)
            .ignore(false)
            .git_ignore(false)
            .git_global(false)
            .git_exclude(false)
            .parents(false)
            .sort_by_file_name(|a, b| a.cmp(b));
        if !include_ignored {
            builder.filter_entry(|entry| !excluded(entry));
        }
        Ok(builder.build())
    }

    fn ignored_path(
        &self,
        path: &Path,
        base: &Path,
        is_dir: bool,
        cache: &mut HashMap<PathBuf, Option<ignore::gitignore::Gitignore>>,
        cancel: Option<&AtomicBool>,
    ) -> Result<bool, ReadError> {
        let (_, selected, _) = self.resolve(base)?;
        let anchor = if base.starts_with(&self.workspace) {
            self.workspace.as_path()
        } else if selected.path == Path::new("/") {
            base
        } else {
            selected.path.as_path()
        };
        let relative = path
            .strip_prefix(anchor)
            .map_err(|_| err("path_denied", "search entry outside root"))?;
        let components: Vec<_> = relative.components().collect();
        let mut parent = anchor.to_path_buf();
        let mut matchers = Vec::new();
        for (index, component) in components.iter().enumerate() {
            check_cancel(cancel)?;
            if !cache.contains_key(&parent) {
                let ignore_file = parent.join(".gitignore");
                let matcher = match std::fs::symlink_metadata(&ignore_file) {
                    Ok(_) => {
                        let bytes = self.read_bytes(&ignore_file, cancel)?;
                        let text = std::str::from_utf8(&bytes)
                            .map_err(|_| err("invalid_text", "gitignore is not UTF-8"))?;
                        let mut builder = ignore::gitignore::GitignoreBuilder::new(&parent);
                        for line in text.lines() {
                            builder
                                .add_line(Some(ignore_file.clone()), line)
                                .map_err(|e| err("invalid_args", e.to_string()))?;
                        }
                        Some(
                            builder
                                .build()
                                .map_err(|e| err("invalid_args", e.to_string()))?,
                        )
                    }
                    Err(e) if e.kind() == io::ErrorKind::NotFound => None,
                    Err(e) => return Err(io_err(e)),
                };
                // Bound the cache; clearing preserves behavior at the cost of rescanning.
                if cache.len() >= 1024 {
                    cache.clear();
                }
                cache.insert(parent.clone(), matcher);
            }
            if let Some(Some(matcher)) = cache.get(&parent) {
                matchers.push(matcher.clone());
            }
            let child = parent.join(component.as_os_str());
            let child_is_dir = index + 1 < components.len() || is_dir;
            let mut ignored = false;
            for matcher in &matchers {
                let matched = matcher.matched(&child, child_is_dir);
                if matched.is_ignore() {
                    ignored = true;
                } else if matched.is_whitelist() {
                    ignored = false;
                }
            }
            if ignored {
                return Ok(true);
            }
            parent = child;
        }
        Ok(false)
    }

    /// Find paths matching a glob relative to `path`. Results are deterministic,
    /// do not follow directory symlinks, and obey configured count and byte caps.
    pub fn find_files(
        &self,
        path: &Path,
        pattern: &str,
        include_ignored: bool,
    ) -> Result<Page<PathBuf>, ReadError> {
        self.find_files_inner(path, pattern, include_ignored, None)
    }
    fn find_files_inner(
        &self,
        path: &Path,
        pattern: &str,
        include_ignored: bool,
        cancel: Option<&AtomicBool>,
    ) -> Result<Page<PathBuf>, ReadError> {
        check_cancel(cancel)?;
        let (base, _, _) = self.resolve(path)?;
        let matcher = Glob::new(pattern)
            .map_err(|e| err("invalid_args", e.to_string()))?
            .compile_matcher();
        let mut items = Vec::new();
        let mut ignore_cache = HashMap::new();
        let mut output_bytes = 2usize;
        for entry in self.walker(path, include_ignored)? {
            check_cancel(cancel)?;
            let entry = entry.map_err(|e| err("search_error", e.to_string()))?;
            if !entry.file_type().is_some_and(|t| t.is_file()) {
                continue;
            }
            let candidate = entry.path();
            if !include_ignored
                && self.ignored_path(candidate, &base, false, &mut ignore_cache, cancel)?
            {
                continue;
            }
            let Ok(relative) = candidate.strip_prefix(&base) else {
                continue;
            };
            if matcher.is_match(relative) && self.resolve(candidate).is_ok() {
                let candidate = candidate.to_owned();
                if !append_bounded(
                    &mut items,
                    candidate,
                    &mut output_bytes,
                    self.limits.search_results,
                    self.limits.search_bytes,
                )? {
                    return Ok(Page {
                        items,
                        truncated: true,
                    });
                }
            }
        }
        Ok(Page {
            items,
            truncated: false,
        })
    }

    /// Search UTF-8 text line by line; binary and oversized files are skipped.
    /// The pattern is literal unless `regex` is true. Results include one-based lines.
    pub fn search_text(
        &self,
        path: &Path,
        pattern: &str,
        regex: bool,
        include_ignored: bool,
    ) -> Result<Page<SearchHit>, ReadError> {
        self.search_text_inner(path, pattern, regex, include_ignored, None)
    }
    fn search_text_inner(
        &self,
        path: &Path,
        pattern: &str,
        regex: bool,
        include_ignored: bool,
        cancel: Option<&AtomicBool>,
    ) -> Result<Page<SearchHit>, ReadError> {
        check_cancel(cancel)?;
        let compiled = if regex {
            Some(Regex::new(pattern).map_err(|e| err("invalid_args", e.to_string()))?)
        } else {
            None
        };
        let (base, _, _) = self.resolve(path)?;
        let mut ignore_cache = HashMap::new();
        let mut hits = Vec::new();
        let mut output_bytes = 2usize;
        for entry in self.walker(path, include_ignored)? {
            check_cancel(cancel)?;
            let entry = entry.map_err(|e| err("search_error", e.to_string()))?;
            if !entry.file_type().is_some_and(|t| t.is_file()) {
                continue;
            }
            if !include_ignored
                && self.ignored_path(entry.path(), &base, false, &mut ignore_cache, cancel)?
            {
                continue;
            }
            let bytes = match self.read_bytes(entry.path(), cancel) {
                Ok(bytes) => bytes,
                Err(e) if matches!(e.code, "file_too_large" | "invalid_file" | "path_denied") => {
                    continue;
                }
                Err(e) => return Err(e),
            };
            if bytes.contains(&0) {
                continue;
            }
            let Ok(text) = std::str::from_utf8(&bytes) else {
                continue;
            };
            for (index, line) in text.lines().enumerate() {
                check_cancel(cancel)?;
                let matched = compiled
                    .as_ref()
                    .map_or_else(|| line.contains(pattern), |re| re.is_match(line));
                if matched {
                    if line.len() > self.limits.search_bytes {
                        return Ok(Page {
                            items: hits,
                            truncated: true,
                        });
                    }
                    let hit = SearchHit {
                        path: entry.path().to_owned(),
                        line: index + 1,
                        text: line.to_owned(),
                    };
                    if !append_bounded(
                        &mut hits,
                        hit,
                        &mut output_bytes,
                        self.limits.search_results,
                        self.limits.search_bytes,
                    )? {
                        return Ok(Page {
                            items: hits,
                            truncated: true,
                        });
                    }
                }
            }
        }
        Ok(Page {
            items: hits,
            truncated: false,
        })
    }
}
fn excluded(entry: &DirEntry) -> bool {
    entry.depth() > 0
        && entry.file_type().is_some_and(|t| t.is_dir())
        && matches!(
            entry.file_name().to_str(),
            Some(".git" | "target" | "node_modules")
        )
}
fn append_bounded<T: Serialize>(
    items: &mut Vec<T>,
    item: T,
    bytes: &mut usize,
    count: usize,
    max_bytes: usize,
) -> Result<bool, ReadError> {
    let size = serde_json::to_vec(&item)
        .map_err(|e| err("serialization_error", e.to_string()))?
        .len();
    let next = bytes
        .saturating_add(size)
        .saturating_add(usize::from(!items.is_empty()));
    if items.len() >= count || next > max_bytes {
        return Ok(false);
    }
    *bytes = next;
    items.push(item);
    Ok(true)
}
fn bounded_page<T: Serialize>(
    items: Vec<T>,
    count: usize,
    max_bytes: usize,
) -> Result<Page<T>, ReadError> {
    let mut result = Vec::new();
    let mut bytes = 2usize;
    let mut truncated = false;
    for item in items {
        let item_bytes = serde_json::to_vec(&item)
            .map_err(|e| err("serialization_error", e.to_string()))?
            .len();
        let next = bytes
            .saturating_add(item_bytes)
            .saturating_add(usize::from(!result.is_empty()));
        if result.len() == count || next > max_bytes {
            truncated = true;
            break;
        }
        bytes = next;
        result.push(item);
    }
    Ok(Page {
        items: result,
        truncated,
    })
}

/// One of the four read-only tool adapters.
#[derive(Debug, Clone, Copy)]
pub enum ReadToolKind {
    ReadFile,
    ListDir,
    FindFiles,
    SearchText,
}

/// Adapts read-only operations to the runtime tool contract. Callers must still
/// apply the mode and permission policy before dispatch.
pub struct ReadTool {
    files: std::sync::Arc<ReadTools>,
    kind: ReadToolKind,
}
impl ReadTool {
    pub fn new(files: std::sync::Arc<ReadTools>, kind: ReadToolKind) -> Self {
        Self { files, kind }
    }
}
impl ReadTool {
    fn execute_blocking(
        &self,
        call: &crate::contract::ToolCall,
        context: &crate::boundary::ToolContext,
        cancel: &AtomicBool,
    ) -> Result<(serde_json::Value, bool), ReadError> {
        use crate::boundary::Tool;
        use crate::contract::PermissionMode;
        (|| -> Result<(serde_json::Value, bool), ReadError> {
            if std::fs::canonicalize(&context.workspace).map_err(io_err)? != self.files.workspace {
                return Err(err("invalid_workspace", "tool workspace differs from run"));
            }
            if self.files.bypass && context.run.permissions != PermissionMode::Bypass {
                return Err(err(
                    "path_denied",
                    "unrestricted roots require bypass permissions",
                ));
            }
            let expected = self.definition().name;
            if call.name != expected {
                return Err(err("invalid_args", "tool name does not match adapter"));
            }
            fn parse<T: serde::de::DeserializeOwned>(s: &str) -> Result<T, ReadError> {
                serde_json::from_str(s).map_err(|e| err("invalid_args", e.to_string()))
            }
            fn value<T: Serialize>(v: T) -> Result<serde_json::Value, ReadError> {
                serde_json::to_value(v).map_err(|e| err("serialization_error", e.to_string()))
            }
            match self.kind {
                ReadToolKind::ReadFile => {
                    let a: ReadArgs = parse(&call.arguments)?;
                    let v = self.files.read_file_inner(
                        &a.path,
                        a.start_line,
                        a.line_count,
                        Some(cancel),
                    )?;
                    let truncated = v.truncated;
                    Ok((value(v)?, truncated))
                }
                ReadToolKind::ListDir => {
                    let a: ListArgs = parse(&call.arguments)?;
                    let v = self.files.list_dir_inner(&a.path, Some(cancel))?;
                    let truncated = v.truncated;
                    Ok((value(v)?, truncated))
                }
                ReadToolKind::FindFiles => {
                    let a: FindArgs = parse(&call.arguments)?;
                    let v = self.files.find_files_inner(
                        &a.path,
                        &a.glob,
                        a.include_ignored,
                        Some(cancel),
                    )?;
                    let truncated = v.truncated;
                    Ok((value(v)?, truncated))
                }
                ReadToolKind::SearchText => {
                    let a: SearchArgs = parse(&call.arguments)?;
                    let v = self.files.search_text_inner(
                        &a.path,
                        &a.pattern,
                        a.regex,
                        a.include_ignored,
                        Some(cancel),
                    )?;
                    let truncated = v.truncated;
                    Ok((value(v)?, truncated))
                }
            }
        })()
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ReadArgs {
    path: PathBuf,
    start_line: usize,
    line_count: usize,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ListArgs {
    path: PathBuf,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FindArgs {
    path: PathBuf,
    glob: String,
    #[serde(default)]
    include_ignored: bool,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SearchArgs {
    path: PathBuf,
    pattern: String,
    #[serde(default)]
    regex: bool,
    #[serde(default)]
    include_ignored: bool,
}

impl crate::boundary::Tool for ReadTool {
    fn definition(&self) -> crate::boundary::ToolDefinition {
        use serde_json::json;
        let (name, description, properties, required) = match self.kind {
            ReadToolKind::ReadFile => (
                "read_file",
                "Read a UTF-8 file with line range and version",
                json!({"path":{"type":"string"},"start_line":{"type":"integer","minimum":1},"line_count":{"type":"integer","minimum":1}}),
                json!(["path", "start_line", "line_count"]),
            ),
            ReadToolKind::ListDir => (
                "list_dir",
                "List direct directory entries",
                json!({"path":{"type":"string"}}),
                json!(["path"]),
            ),
            ReadToolKind::FindFiles => (
                "find_files",
                "Find files using a glob",
                json!({"path":{"type":"string"},"glob":{"type":"string"},"include_ignored":{"type":"boolean"}}),
                json!(["path", "glob"]),
            ),
            ReadToolKind::SearchText => (
                "search_text",
                "Search text files",
                json!({"path":{"type":"string"},"pattern":{"type":"string"},"regex":{"type":"boolean"},"include_ignored":{"type":"boolean"}}),
                json!(["path", "pattern"]),
            ),
        };
        crate::boundary::ToolDefinition {
            name: name.into(),
            description: description.into(),
            input_schema: json!({"type":"object","properties":properties,"required":required,"additionalProperties":false}),
        }
    }
    fn execute<'a>(
        &'a self,
        call: &'a crate::contract::ToolCall,
        context: &'a crate::boundary::ToolContext,
        cancel: &'a dyn crate::boundary::Cancellation,
    ) -> crate::boundary::Operation<'a, crate::contract::ToolResult> {
        Box::pin(async move {
            use crate::contract::{BoundaryError, ErrorInfo, ToolResult};
            if cancel.is_cancelled() {
                return Err(BoundaryError(
                    ErrorInfo {
                        code: "cancelled".into(),
                        message: "tool cancelled".into(),
                    },
                    None,
                ));
            }
            let handle = tokio::runtime::Handle::try_current().map_err(|_| {
                BoundaryError(
                    ErrorInfo {
                        code: "runtime_error".into(),
                        message: "Tokio runtime required for file tools".into(),
                    },
                    None,
                )
            })?;
            let flag = std::sync::Arc::new(AtomicBool::new(false));
            let worker_flag = flag.clone();
            let adapter = ReadTool {
                files: self.files.clone(),
                kind: self.kind,
            };
            let call = call.clone();
            let context = context.clone();
            let mut task = handle
                .spawn_blocking(move || adapter.execute_blocking(&call, &context, &worker_flag));
            let outcome = tokio::select! {
                biased;
                _ = cancel.cancelled() => {
                    flag.store(true, Ordering::Relaxed);
                    let _ = task.await;
                    return Err(BoundaryError(ErrorInfo { code: "cancelled".into(), message: "tool cancelled".into() }, None));
                }
                outcome = &mut task => outcome.map_err(|e| BoundaryError(ErrorInfo { code: "tool_error".into(), message: e.to_string() }, None))?,
            };
            if cancel.is_cancelled() {
                return Err(BoundaryError(
                    ErrorInfo {
                        code: "cancelled".into(),
                        message: "tool cancelled".into(),
                    },
                    None,
                ));
            }
            Ok(match outcome {
                Ok((data, truncated)) => ToolResult {
                    ok: true,
                    data: Some(data),
                    error: None,
                    truncated,
                },
                Err(e) => ToolResult {
                    ok: false,
                    data: None,
                    error: Some(ErrorInfo {
                        code: e.code.into(),
                        message: e.message,
                    }),
                    truncated: false,
                },
            })
        })
    }
}
