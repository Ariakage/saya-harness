//! Bounded system-clipboard fallback for image and text paste.

use std::{
    fs,
    io::{self, Read, Write},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::atomic::{AtomicU64, Ordering},
    time::{SystemTime, UNIX_EPOCH},
};

#[cfg(any(target_os = "linux", test))]
use std::fs::OpenOptions;

#[cfg(all(unix, any(target_os = "linux", test)))]
use std::os::unix::fs::OpenOptionsExt;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

const MAX_IMAGE_BYTES: u64 = 32 * 1024 * 1024;
const MAX_TEXT_BYTES: u64 = 1024 * 1024;
static NEXT_FILE: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Paste {
    Image(PathBuf),
    Text(String),
    Empty,
}

pub(crate) fn read(home: &Path) -> io::Result<Paste> {
    #[cfg(target_os = "macos")]
    if let Some(path) = macos_image(home)? {
        return Ok(Paste::Image(path));
    }
    #[cfg(target_os = "linux")]
    if let Some(path) = linux_image(home)? {
        return Ok(Paste::Image(path));
    }
    Ok(match read_text()? {
        Some(text) if !text.is_empty() => Paste::Text(text),
        _ => Paste::Empty,
    })
}

/// Write selected text to the local system clipboard when a platform helper is
/// available. Remote terminals still receive the OSC 52 fallback from the caller.
pub(crate) fn write_text(text: &str) -> bool {
    #[cfg(target_os = "macos")]
    {
        Command::new("pbcopy").pipe_text(text).unwrap_or(false)
    }
    #[cfg(target_os = "linux")]
    {
        let mut commands = Vec::new();
        if std::env::var_os("WAYLAND_DISPLAY").is_some() {
            let mut command = Command::new("wl-copy");
            command.arg("--type").arg("text/plain;charset=utf-8");
            commands.push(command);
        }
        let mut xclip = Command::new("xclip");
        xclip.args(["-selection", "clipboard"]);
        commands.push(xclip);
        commands
            .into_iter()
            .any(|command| command.pipe_text(text).unwrap_or(false))
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        let _ = text;
        false
    }
}

trait TextWriter {
    fn pipe_text(&mut self, text: &str) -> io::Result<bool>;
}

impl TextWriter for Command {
    fn pipe_text(&mut self, text: &str) -> io::Result<bool> {
        let mut child = match self
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
        {
            Ok(child) => child,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
            Err(error) => return Err(error),
        };
        let written = child
            .stdin
            .take()
            .ok_or_else(|| io::Error::other("clipboard helper has no stdin"))?
            .write_all(text.as_bytes());
        if let Err(error) = written {
            let _ = child.kill();
            let _ = child.wait();
            return Err(error);
        }
        Ok(child.wait()?.success())
    }
}

fn paste_directory(home: &Path) -> PathBuf {
    home.join(".saya").join("paste_image")
}

fn prepare_directory(directory: &Path) -> io::Result<()> {
    fs::create_dir_all(directory)?;
    let metadata = fs::symlink_metadata(directory)?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err(io::Error::other("paste image directory is unsafe"));
    }
    #[cfg(unix)]
    fs::set_permissions(directory, fs::Permissions::from_mode(0o700))?;
    Ok(())
}

fn destination(home: &Path, extension: &str) -> io::Result<PathBuf> {
    let directory = paste_directory(home);
    prepare_directory(&directory)?;
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(io::Error::other)?
        .as_nanos();
    let sequence = NEXT_FILE.fetch_add(1, Ordering::Relaxed);
    Ok(directory.join(format!(
        "image-{}-{timestamp}-{sequence}.{extension}",
        std::process::id()
    )))
}

#[cfg(target_os = "macos")]
fn macos_image(home: &Path) -> io::Result<Option<PathBuf>> {
    for (class, extension) in [("PNGf", "png"), ("TIFF", "tiff")] {
        let path = destination(home, extension)?;
        let script = format!(
            "set imageData to the clipboard as «class {class}»\n\
             set outputFile to open for access (POSIX file (item 1 of argv)) with write permission\n\
             set eof outputFile to 0\n\
             write imageData to outputFile\n\
             close access outputFile"
        );
        let status = Command::new("osascript")
            .arg("-e")
            .arg("on run argv")
            .arg("-e")
            .arg(script)
            .arg("-e")
            .arg("end run")
            .arg("--")
            .arg(&path)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        match status {
            Ok(status) if status.success() => return Ok(Some(validate_image(path)?)),
            Ok(_) => {
                let _ = fs::remove_file(&path);
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error),
        }
    }
    Ok(None)
}

#[cfg(target_os = "linux")]
fn linux_image(home: &Path) -> io::Result<Option<PathBuf>> {
    let mut command = if std::env::var_os("WAYLAND_DISPLAY").is_some() {
        let mut command = Command::new("wl-paste");
        command.args(["--no-newline", "--type", "image/png"]);
        command
    } else {
        let mut command = Command::new("xclip");
        command.args(["-selection", "clipboard", "-t", "image/png", "-o"]);
        command
    };
    let Some(bytes) = bounded_output(&mut command, MAX_IMAGE_BYTES)? else {
        return Ok(None);
    };
    if bytes.is_empty() {
        return Ok(None);
    }
    let path = destination(home, "png")?;
    write_private(&path, &bytes)?;
    Ok(Some(path))
}

#[cfg(target_os = "macos")]
fn validate_image(path: PathBuf) -> io::Result<PathBuf> {
    let size = fs::metadata(&path)?.len();
    if size == 0 || size > MAX_IMAGE_BYTES {
        let _ = fs::remove_file(&path);
        return Err(io::Error::other("clipboard image is empty or too large"));
    }
    #[cfg(unix)]
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600))?;
    Ok(path)
}

fn read_text() -> io::Result<Option<String>> {
    #[cfg(target_os = "macos")]
    let mut command = {
        let mut command = Command::new("pbpaste");
        command.arg("-Prefer");
        command.arg("txt");
        command
    };
    #[cfg(target_os = "linux")]
    let mut command = if std::env::var_os("WAYLAND_DISPLAY").is_some() {
        let mut command = Command::new("wl-paste");
        command.args(["--no-newline", "--type", "text/plain;charset=utf-8"]);
        command
    } else {
        let mut command = Command::new("xclip");
        command.args(["-selection", "clipboard", "-o"]);
        command
    };
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    let mut command = Command::new("__saya_clipboard_unavailable__");
    Ok(bounded_output(&mut command, MAX_TEXT_BYTES)?
        .and_then(|bytes| String::from_utf8(bytes).ok()))
}

fn bounded_output(command: &mut Command, limit: u64) -> io::Result<Option<Vec<u8>>> {
    let mut child = match command.stdout(Stdio::piped()).stderr(Stdio::null()).spawn() {
        Ok(child) => child,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    let mut bytes = Vec::new();
    if let Some(stdout) = child.stdout.take() {
        stdout.take(limit + 1).read_to_end(&mut bytes)?;
    }
    if bytes.len() as u64 > limit {
        let _ = child.kill();
        let _ = child.wait();
        return Err(io::Error::other("clipboard content exceeds its byte limit"));
    }
    let status = child.wait()?;
    Ok(status.success().then_some(bytes))
}

#[cfg(any(target_os = "linux", test))]
fn write_private(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    options.mode(0o600);
    let mut file = options.open(path)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn image_files_are_private_and_bounded() {
        let root = tempfile::tempdir().unwrap();
        let directory = paste_directory(root.path());
        assert_eq!(directory, root.path().join(".saya/paste_image"));
        prepare_directory(&directory).unwrap();
        let path = destination(root.path(), "png").unwrap();
        write_private(&path, b"png").unwrap();
        assert!(path.starts_with(directory));
        assert_eq!(fs::read(&path).unwrap(), b"png");
        #[cfg(unix)]
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }

    #[test]
    fn missing_clipboard_tools_report_no_paste() {
        let mut command = Command::new("__saya_missing_clipboard_tool__");
        assert_eq!(bounded_output(&mut command, 1024).unwrap(), None);
    }

    #[cfg(unix)]
    #[test]
    fn clipboard_text_writer_pipes_the_selection() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("clipboard");
        let mut command = Command::new("sh");
        command.args(["-c", "cat > \"$1\"", "sh"]).arg(&path);
        assert!(command.pipe_text("界e\u{301}水").unwrap());
        assert_eq!(fs::read_to_string(path).unwrap(), "界e\u{301}水");
    }
}
