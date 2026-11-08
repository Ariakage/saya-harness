//! Read-only local log browsing and rotation-aware following.
use clap::{Args, Subcommand};
use saya_app::interaction::RunCancellation;
use saya_core::{
    config::{Config, Output},
    logging::query::{LogCursor, LogPage, LogQuery, LogReader},
};
use std::{
    io::{self, Write},
    sync::Arc,
    time::Duration,
};

#[derive(Subcommand)]
pub enum LogsAction {
    /// Read a bounded page from current and retained log files.
    Show {
        #[command(flatten)]
        query: LogArgs,
        #[arg(long, default_value_t = 0)]
        offset: usize,
    },
    /// Follow completed records across file rotation until interrupted.
    Tail(LogArgs),
}
#[derive(Args)]
pub struct LogArgs {
    #[arg(long)]
    session: Option<String>,
    #[arg(long)]
    run: Option<String>,
    #[arg(long, default_value = "trace", value_parser = ["trace", "debug", "info", "warn", "error"])]
    level: String,
    /// Inclusive RFC 3339 timestamp with an explicit timezone.
    #[arg(long)]
    since: Option<String>,
    /// Inclusive RFC 3339 timestamp with an explicit timezone.
    #[arg(long)]
    until: Option<String>,
    /// Page size, including each batch emitted by tail.
    #[arg(long, default_value_t = 100)]
    limit: usize,
}
impl LogArgs {
    fn query(&self, offset: usize) -> Result<LogQuery, Box<dyn std::error::Error>> {
        Ok(LogQuery {
            minimum: self.level.parse()?,
            session_id: self.session.clone(),
            run_id: self.run.clone(),
            since: self.since.clone(),
            until: self.until.clone(),
            offset,
            limit: self.limit,
        })
    }
}
struct CancelOnDrop(Arc<RunCancellation>);
impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        self.0.cancel();
    }
}

pub async fn run(config: &Config, action: &LogsAction) -> Result<u8, Box<dyn std::error::Error>> {
    let secrets = config
        .secret_env_names()
        .iter()
        .map(|name| config.secret(name).map(str::to_owned))
        .collect::<Result<Vec<_>, _>>()?;
    let reader = Arc::new(LogReader::new(&config.state_dir, secrets));
    let cancellation = Arc::new(RunCancellation::new());
    let _cancel_on_drop = CancelOnDrop(cancellation.clone());
    let _stdout = NonblockingStdout::new()?;
    let mut terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    let operation = async {
        match action {
            LogsAction::Show { query, offset } => {
                let query = query.query(*offset)?;
                let page = tokio::task::spawn_blocking(move || {
                    reader.page(&query, Some(cancellation.as_ref()))
                })
                .await??;
                emit(&page, config.output, false).await?;
                Ok(0)
            }
            LogsAction::Tail(args) => {
                if config.output == Output::Json {
                    return Err("logs tail supports text or JSONL output".into());
                }
                let query = args.query(0)?;
                let mut cursor = LogCursor::default();
                loop {
                    let reader = reader.clone();
                    let query = query.clone();
                    let cancellation = cancellation.clone();
                    let (page, next) = tokio::task::spawn_blocking(move || {
                        let page = reader.follow(&query, &mut cursor, Some(cancellation.as_ref()));
                        (page, cursor)
                    })
                    .await?;
                    cursor = next;
                    let page = page?;
                    emit(&page, config.output, true).await?;
                    if page.next_offset.is_none() {
                        tokio::time::sleep(Duration::from_millis(200)).await;
                    }
                }
            }
        }
    };
    tokio::select! {
        result = operation => result,
        signal = tokio::signal::ctrl_c() => { signal?; Ok(130) },
        _ = terminate.recv() => Ok(130),
    }
}
struct NonblockingStdout(i32);
impl NonblockingStdout {
    fn new() -> io::Result<Self> {
        // SAFETY: fcntl operates on the live standard output descriptor.
        let flags = unsafe { libc::fcntl(libc::STDOUT_FILENO, libc::F_GETFL) };
        if flags < 0
            || unsafe { libc::fcntl(libc::STDOUT_FILENO, libc::F_SETFL, flags | libc::O_NONBLOCK) }
                < 0
        {
            return Err(io::Error::last_os_error());
        }
        Ok(Self(flags))
    }
}
impl Drop for NonblockingStdout {
    fn drop(&mut self) {
        // SAFETY: restore the flags captured for the same descriptor.
        unsafe {
            libc::fcntl(libc::STDOUT_FILENO, libc::F_SETFL, self.0);
        }
    }
}
async fn emit(
    page: &LogPage,
    output: Output,
    following: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut stdout = Vec::new();
    match output {
        Output::Json => {
            serde_json::to_writer(
                &mut stdout,
                &serde_json::json!({"schema_version":1,"data":page}),
            )?;
            stdout.write_all(b"\n")?;
        }
        Output::Jsonl => {
            for record in &page.records {
                serde_json::to_writer(
                    &mut stdout,
                    &serde_json::json!({"schema_version":1,"type":"log.entry","data":record}),
                )?;
                stdout.write_all(b"\n")?;
            }
            if !following {
                serde_json::to_writer(
                    &mut stdout,
                    &serde_json::json!({"schema_version":1,"type":"logs.page","data":{"next_offset":page.next_offset,"malformed_lines":page.malformed_lines}}),
                )?;
                stdout.write_all(b"\n")?;
            }
        }
        Output::Text => {
            for record in &page.records {
                // JSON escaping protects the terminal while preserving Unicode.
                serde_json::to_writer(&mut stdout, record)?;
                stdout.write_all(b"\n")?;
            }
            if !following && page.malformed_lines > 0 {
                writeln!(
                    stdout,
                    "Skipped {} malformed or partial log rows.",
                    page.malformed_lines
                )?;
            }
            if let Some(offset) = page.next_offset {
                writeln!(stdout, "Next offset {offset}")?;
            }
        }
    }
    let serialized = String::from_utf8(stdout)?;
    let mut safe = String::with_capacity(serialized.len());
    for character in serialized.chars() {
        if saya_core::text::is_terminal_control(character) && character != '\n' {
            use std::fmt::Write as _;
            write!(&mut safe, "\\u{:04x}", u32::from(character))?;
        } else {
            safe.push(character);
        }
    }
    let stdout = safe.as_bytes();
    let mut offset = 0;
    while offset < stdout.len() {
        let end = (offset + 4096).min(stdout.len());
        // SAFETY: the slice is live and write retains no pointer after returning.
        let written = unsafe {
            libc::write(
                libc::STDOUT_FILENO,
                stdout[offset..end].as_ptr().cast(),
                end - offset,
            )
        };
        if written > 0 {
            offset += written as usize;
            tokio::task::yield_now().await;
        } else if written == 0 {
            return Err(io::Error::from(io::ErrorKind::WriteZero).into());
        } else {
            match io::Error::last_os_error().kind() {
                io::ErrorKind::WouldBlock => tokio::time::sleep(Duration::from_millis(10)).await,
                io::ErrorKind::Interrupted => tokio::task::yield_now().await,
                _ => return Err(io::Error::last_os_error().into()),
            }
        }
    }
    Ok(())
}
