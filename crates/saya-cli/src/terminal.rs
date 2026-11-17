use crate::{approval, cli_error, config_secrets};
use saya_core::{
    boundary::{Cancellation, Model, ModelRequest, ModelStream, ModelUpdate, Operation},
    config::Config,
    contract::{ApprovalRequest, BoundaryError, HistoryItem, ModelPurpose, Session},
    input::{InputHandler, InputRequest},
    runtime::ApprovalHandler,
};
use std::{
    fs::OpenOptions,
    io::{self, Read, Write},
    sync::Arc,
};

pub(crate) struct Terminal {
    pub(crate) file: tokio::sync::Mutex<std::fs::File>,
    pub(crate) secrets: Vec<String>,
}

struct PollGuard(Arc<std::sync::atomic::AtomicBool>);

impl Drop for PollGuard {
    fn drop(&mut self) {
        self.0.store(true, std::sync::atomic::Ordering::Release);
    }
}

pub(crate) fn safe_terminal_text(s: &str) -> String {
    s.chars()
        .map(|c| {
            if saya_core::text::is_terminal_control(c) && c != '\n' && c != '\t' {
                '\u{fffd}'
            } else {
                c
            }
        })
        .collect()
}

pub(crate) fn redact_value(value: &mut serde_json::Value, secrets: &[String], _depth: usize) {
    if saya_core::redaction::redact(value, secrets).is_err() {
        *value = serde_json::Value::String("sensitive details withheld".into());
    }
}

pub(crate) fn contains_encoded_secret(text: &str, secrets: &[String]) -> bool {
    saya_core::redaction::contains_secret(text, secrets)
}

pub(crate) fn redacted_text(text: &str, secrets: &[String]) -> String {
    let mut value = serde_json::Value::String(text.into());
    if saya_core::redaction::redact(&mut value, secrets).is_err() {
        return "sensitive details withheld".into();
    }
    safe_terminal_text(value.as_str().unwrap_or("sensitive details withheld"))
}

pub(crate) fn safe_output(text: &str, config: &Config) -> String {
    let Ok(secrets) = config_secrets(config) else {
        return "sensitive details withheld".into();
    };
    redacted_text(text, &secrets)
}

pub(crate) fn report_runtime_error(error: &BoundaryError, config: &Config) {
    let Ok(secrets) = config_secrets(config) else {
        eprintln!("saya: run failed; error details unavailable");
        return;
    };
    let mut message = serde_json::Value::String(error.to_string());
    redact_value(&mut message, &secrets, 0);
    let message = message.as_str().unwrap_or("run failed");
    if contains_encoded_secret(message, &secrets) {
        eprintln!("saya: run failed; sensitive error details withheld");
    } else {
        eprintln!("saya: {}", safe_terminal_text(message));
    }
    if let Some(observed) = &error.2 {
        if let Ok(mut value) = serde_json::to_value(observed) {
            redact_value(&mut value, &secrets, 0);
            let rendered = value.to_string();
            if contains_encoded_secret(&rendered, &secrets) {
                eprintln!("saya: observed tool result (not saved): sensitive details withheld");
            } else {
                eprintln!(
                    "saya: observed tool result (not saved): {}",
                    safe_terminal_text(&rendered)
                );
            }
        } else {
            eprintln!("saya: observed tool result could not be displayed");
        }
    }
}

pub(crate) fn tui_error(message: &str) -> u8 {
    use std::os::fd::RawFd;
    let descriptor: RawFd = libc::STDERR_FILENO;
    // SAFETY: fcntl and write operate on the existing stderr descriptor.
    let flags = unsafe { libc::fcntl(descriptor, libc::F_GETFL) };
    if flags < 0 {
        return 1;
    }
    if unsafe { libc::fcntl(descriptor, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
        return 1;
    }
    let message = format!("saya: {}\n", safe_terminal_text(message));
    let bytes = message.as_bytes();
    let mut offset = 0;
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(100);
    while offset < bytes.len() && std::time::Instant::now() < deadline {
        let written = unsafe {
            libc::write(
                descriptor,
                bytes[offset..].as_ptr().cast(),
                bytes.len() - offset,
            )
        };
        if written > 0 {
            offset += written as usize;
            continue;
        }
        let error = io::Error::last_os_error();
        if error.kind() == io::ErrorKind::WouldBlock {
            std::thread::sleep(std::time::Duration::from_millis(5));
        } else if error.kind() != io::ErrorKind::Interrupted {
            break;
        }
    }
    unsafe {
        libc::fcntl(descriptor, libc::F_SETFL, flags);
    }
    1
}

impl Terminal {
    pub(crate) fn open(secrets: Vec<String>) -> io::Result<Self> {
        use std::os::unix::fs::OpenOptionsExt;
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .custom_flags(libc::O_NONBLOCK)
            .open("/dev/tty")?;
        Ok(Self {
            file: tokio::sync::Mutex::new(file),
            secrets,
        })
    }

    pub(crate) async fn read_line(
        &self,
        prompt: &str,
        cancel: &dyn Cancellation,
    ) -> Result<Option<String>, BoundaryError> {
        let mut tty = self.file.lock().await;
        let prompt = redacted_text(prompt, &self.secrets);
        let mut written = 0;
        while written < prompt.len() {
            if cancel.is_cancelled() {
                return Ok(None);
            }
            match tty.write(&prompt.as_bytes()[written..]) {
                Ok(0) => return Err(cli_error("terminal_error", "controlling terminal closed")),
                Ok(count) => written += count,
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                    tokio::select! {
                        biased;
                        _ = cancel.cancelled() => return Ok(None),
                        _ = tokio::time::sleep(std::time::Duration::from_millis(5)) => {}
                    }
                }
                Err(_) => {
                    return Err(cli_error(
                        "terminal_error",
                        "cannot write to controlling terminal",
                    ));
                }
            }
        }
        let mut input = tty
            .try_clone()
            .map_err(|_| cli_error("terminal_error", "cannot read controlling terminal"))?;
        let stopped = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let _guard = PollGuard(stopped.clone());
        let mut task = tokio::task::spawn_blocking(move || {
            use std::{os::fd::AsRawFd, sync::atomic::Ordering};
            let mut answer = Vec::new();
            while !stopped.load(Ordering::Acquire) {
                let mut descriptor = libc::pollfd {
                    fd: input.as_raw_fd(),
                    events: libc::POLLIN,
                    revents: 0,
                };
                // SAFETY: descriptor is a live pollfd for the owned File; timeout is bounded.
                let ready = unsafe { libc::poll(&mut descriptor, 1, 100) };
                if ready < 0 {
                    return None;
                }
                if ready == 0 {
                    continue;
                }
                let mut byte = [0u8; 1];
                match input.read(&mut byte) {
                    Ok(0) => return None,
                    Ok(_) if byte[0] == b'\n' => {
                        return String::from_utf8(answer)
                            .ok()
                            .map(|v| v.trim_end_matches('\r').to_owned());
                    }
                    Ok(_) if answer.len() < 65536 => answer.push(byte[0]),
                    Ok(_) => return None,
                    Err(e) if e.kind() == io::ErrorKind::WouldBlock => continue,
                    Err(_) => return None,
                }
            }
            None
        });
        tokio::select! {
            biased;
            () = cancel.cancelled() => Ok(None),
            result = &mut task => result.map_err(|_| cli_error("terminal_error", "terminal reader failed"))
        }
    }
}

impl ApprovalHandler for Terminal {
    fn confirm<'a>(
        &'a self,
        request: &'a ApprovalRequest,
        cancel: &'a dyn Cancellation,
    ) -> Operation<'a, Option<bool>> {
        Box::pin(async move {
            let arguments = approval::argument_lines(&request.tool, &request.arguments);
            let operation = if request.operation_summary.is_empty() {
                String::new()
            } else {
                format!(
                    "Operation: {}\n",
                    safe_terminal_text(&request.operation_summary)
                )
            };
            let prompt = format!(
                "\nApproval required\nTool: {}\n{operation}{arguments}Working directory: {}\nMode: {:?}\nAllow this call? [y/N] ",
                safe_terminal_text(&request.tool),
                safe_terminal_text(&request.cwd.to_string_lossy()),
                request.work_mode
            );
            Ok(self
                .read_line(&prompt, cancel)
                .await?
                .map(|answer| matches!(answer.trim().to_ascii_lowercase().as_str(), "y" | "yes")))
        })
    }
}

impl InputHandler for Terminal {
    fn ask<'a>(
        &'a self,
        request: &'a InputRequest,
        cancel: &'a dyn Cancellation,
    ) -> Operation<'a, Option<String>> {
        Box::pin(async move {
            let mut prompt = format!("\n{}\n", safe_terminal_text(&request.question));
            for (index, option) in request.options.iter().enumerate() {
                prompt.push_str(&format!(
                    "  {}. {}\n",
                    index + 1,
                    safe_terminal_text(option)
                ));
            }
            prompt.push_str("Answer: ");
            self.read_line(&prompt, cancel).await
        })
    }
}

pub(crate) struct PrintModel(pub(crate) Arc<dyn Model>);

struct PrintStream {
    inner: Box<dyn ModelStream>,
    // Provider adapters bound per-response text before delivering these updates.
    // Keep the raw displayed prefix separately for each model request.
    displayed: String,
}

impl PrintStream {
    fn write_text(text: &str) -> Result<(), BoundaryError> {
        if text.is_empty() {
            return Ok(());
        }
        let mut stdout = io::stdout().lock();
        stdout
            .write_all(safe_terminal_text(text).as_bytes())
            .and_then(|()| stdout.flush())
            .map_err(|_| cli_error("output_error", "cannot write text output"))
    }
}

impl ModelStream for PrintStream {
    fn next<'a>(&'a mut self, cancel: &'a dyn Cancellation) -> Operation<'a, Option<ModelUpdate>> {
        Box::pin(async move {
            let item = self.inner.next(cancel).await?;
            match &item {
                Some(ModelUpdate::TextDelta(text)) => {
                    Self::write_text(text)?;
                    self.displayed.push_str(text);
                }
                Some(
                    ModelUpdate::Finished { items, .. }
                    | ModelUpdate::OutputLimitTruncated { items, .. },
                ) => {
                    // AssistantText is the adapter's validated visible projection.
                    let completed: String = items
                        .iter()
                        .filter_map(|item| {
                            if let HistoryItem::AssistantText(text) = item {
                                Some(text.as_str())
                            } else {
                                None
                            }
                        })
                        .collect();
                    if let Some(suffix) = completed.strip_prefix(&self.displayed) {
                        Self::write_text(suffix)?;
                    } else if !completed.is_empty() {
                        // Interleaved output items can arrive in a different order.
                        // Preserve the stream and show the complete validated answer.
                        Self::write_text("\n")?;
                        Self::write_text(&completed)?;
                    }
                }
                _ => {}
            }
            Ok(item)
        })
    }
}

impl Model for PrintModel {
    fn request_counter(&self) -> Option<&dyn saya_core::context_builder::RequestCounter> {
        self.0.request_counter()
    }

    fn validate_session(&self, s: &Session) -> Result<(), BoundaryError> {
        self.0.validate_session(s)
    }

    fn start<'a>(
        &'a self,
        r: &'a ModelRequest,
        c: &'a dyn Cancellation,
    ) -> Operation<'a, Box<dyn ModelStream>> {
        Box::pin(async move {
            let stream = self.0.start(r, c).await?;
            if r.purpose == ModelPurpose::Execution {
                Ok(Box::new(PrintStream {
                    inner: stream,
                    displayed: String::new(),
                }) as Box<dyn ModelStream>)
            } else {
                Ok(stream)
            }
        })
    }
}
