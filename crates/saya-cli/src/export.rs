//! Explicit offline session export command.
use super::*;
use saya_core::{
    export::{ExportOptions, ExportScope, SessionExport},
    sqlite::{SnapshotLimits, SqliteStore},
};

#[derive(Args)]
pub struct ExportArgs {
    session_id: String,
    #[arg(long, value_enum, default_value = "markdown")]
    format: Format,
    #[arg(long, value_enum, default_value = "transcript")]
    scope: Scope,
    #[arg(long)]
    include_tools: bool,
    #[arg(long)]
    file: PathBuf,
}
#[derive(Clone, Copy, ValueEnum)]
enum Format {
    Markdown,
    Json,
}
#[derive(Clone, Copy, ValueEnum)]
enum Scope {
    Transcript,
    Full,
}

async fn execute(
    config: &Config,
    args: &ExportArgs,
    cancel: Arc<Cancel>,
) -> Result<serde_json::Value, BoundaryError> {
    let id = serde_json::from_value(serde_json::json!(args.session_id))
        .map_err(|_| cli_error("invalid_session_id", "invalid session ID"))?;
    let snapshot = SqliteStore::read_snapshot(
        &config.data_dir,
        id,
        SnapshotLimits::default(),
        cancel.clone(),
    )
    .await?;
    let secrets = config
        .secret_env_names()
        .iter()
        .map(|name| config.secret(name).map(str::to_owned))
        .collect::<Result<Vec<_>, _>>()?;
    let options = ExportOptions {
        scope: match args.scope {
            Scope::Transcript => ExportScope::Transcript,
            Scope::Full => ExportScope::Full,
        },
        include_tools: args.include_tools,
        ..ExportOptions::default()
    };
    let workspace = config.cwd.clone();
    let mode = config.mode;
    let target = if args.file.is_absolute() {
        args.file.clone()
    } else {
        std::env::current_dir()
            .map_err(|_| cli_error("invalid_export_path", "current directory is unavailable"))?
            .join(&args.file)
    };
    let format = args.format;
    tokio::task::spawn_blocking(move || {
        if cancel.is_cancelled() {
            return Err(cli_error("cancelled", "export cancelled"));
        }
        let export = SessionExport::from_snapshot(&snapshot, options, &secrets)?;
        let bytes = match format {
            Format::Json => export.to_json()?,
            Format::Markdown => export.to_markdown()?,
        };
        if cancel.is_cancelled() {
            return Err(cli_error("cancelled", "export cancelled"));
        }
        let mut receipt =
            saya_app::export::publish(&target, &bytes, &workspace, mode, cancel.as_ref())?;
        receipt["session_id"] = serde_json::json!(id);
        receipt["format"] = match format {
            Format::Json => "json",
            Format::Markdown => "markdown",
        }
        .into();
        receipt["last_change_seq"] = snapshot.last_change_seq.into();
        receipt["last_event_seq"] = snapshot.last_event_seq.into();
        saya_core::redaction::redact(&mut receipt, &secrets)?;
        Ok(receipt)
    })
    .await
    .map_err(|_| cli_error("export_worker_failed", "export worker failed"))?
}

struct Response {
    text: String,
    stderr: bool,
    code: u8,
}
fn response(
    output: Output,
    outcome: Result<serde_json::Value, ErrorInfo>,
    timestamp: Option<String>,
) -> Response {
    let (data, error, code, kind) = match outcome {
        Ok(data) => (data, None, 0, "session.exported"),
        Err(error) => {
            let code = if error.code == "cancelled" { 130 } else { 2 };
            (
                serde_json::Value::Null,
                Some(error),
                code,
                "session.export.failed",
            )
        }
    };
    let text = match output {
        Output::Json => serde_json::json!({
            "schema_version": 1,
            "data": data,
            "error": error,
        })
        .to_string(),
        Output::Jsonl => serde_json::json!({
            "schema_version": 1,
            "type": kind,
            "timestamp": timestamp,
            "data": data,
            "error": error,
        })
        .to_string(),
        Output::Text => match error {
            Some(error) => format!(
                "saya: {}",
                safe_terminal_text(&format!("{}: {}", error.code, error.message))
            ),
            None => format!("Exported {}", safe_terminal_text(&data.to_string())),
        },
    };
    Response {
        text: format!("{text}\n"),
        stderr: matches!(output, Output::Text) && code != 0,
        code,
    }
}

pub async fn run(config: &Config, args: &ExportArgs) -> Result<u8, Box<dyn std::error::Error>> {
    let cancel = Arc::new(Cancel::new());
    let outcome = match signals::CancellationListener::start(cancel.clone()) {
        Err(_) => Err(cli_error(
            "signal_error",
            "cannot install export cancellation handler",
        )),
        Ok(_listener) => execute(config, args, cancel).await,
    };
    let outcome = outcome.map_err(|error| ErrorInfo {
        code: error.0.code,
        message: safe_output(&error.0.message, config),
    });
    let response = response(config.output, outcome, events::timestamp().ok());
    let written = if response.stderr {
        io::stderr().lock().write_all(response.text.as_bytes())
    } else {
        io::stdout().lock().write_all(response.text.as_bytes())
    };
    if written.is_err() {
        eprintln!("saya: cannot write export confirmation");
        return Ok(2);
    }
    Ok(response.code)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn error_confirmations_preserve_codes_and_cancellation_exit_status() {
        for output in [Output::Json, Output::Jsonl] {
            for (error_code, exit_code) in [("cancelled", 130), ("export_exists", 2)] {
                let rendered = response(
                    output,
                    Err(ErrorInfo {
                        code: error_code.into(),
                        message: "controlled failure".into(),
                    }),
                    Some("2026-11-08T00:00:00Z".into()),
                );
                assert_eq!(rendered.code, exit_code);
                assert!(!rendered.stderr);
                let value: serde_json::Value = serde_json::from_str(&rendered.text).unwrap();
                assert_eq!(value["error"]["code"], error_code);
                assert!(value["data"].is_null());
                if matches!(output, Output::Jsonl) {
                    assert_eq!(value["type"], "session.export.failed");
                    assert!(value["timestamp"].is_string());
                }
            }
        }
        let rendered = response(
            Output::Text,
            Err(ErrorInfo {
                code: "cancelled".into(),
                message: "unsafe \u{1b}[31m".into(),
            }),
            None,
        );
        assert!(rendered.stderr);
        assert!(!rendered.text.contains('\u{1b}'));
        assert_eq!(rendered.code, 130);
    }
}
