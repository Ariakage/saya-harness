//! Offline runtime module toggle administration (`saya module`).

use clap::{Args, Subcommand, ValueEnum};
use saya_app::session;
use saya_core::{
    config::{Config, Output},
    contract::{BoundaryError, error_codes},
    modules::{self, ModuleKind, ModuleStateSource, ModuleStatus, ModuleStore},
    redaction,
};
use serde_json::{Value, json};
use std::io::{self, Write};

#[derive(Args)]
#[command(args_conflicts_with_subcommands = true)]
pub struct ModuleArgs {
    #[command(subcommand)]
    pub subcommand: Option<ModuleSubcommand>,
    /// Target module name (`browser` or `vm`).
    #[arg(value_name = "MODULE")]
    pub module: Option<String>,
    /// Operation to apply (`enable`, `disable`, or `status`).
    #[arg(value_name = "ACTION", value_enum)]
    pub action: Option<ModuleAction>,
}

#[derive(Subcommand)]
pub enum ModuleSubcommand {
    /// List runtime modules and their effective states.
    List,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum ModuleAction {
    /// Persist an enabled override for this module.
    Enable,
    /// Persist a disabled override for this module.
    Disable,
    /// Show the current effective status of this module.
    Status,
}

pub fn run(args: &ModuleArgs, config: &Config) -> Result<String, BoundaryError> {
    if matches!(args.subcommand, Some(ModuleSubcommand::List)) || args.module.is_none() {
        let items = modules::statuses(&config.data_dir, &config.browser)?;
        let value = envelope("list", json!({ "modules": items }), config)?;
        return format_output(config.output, &value, || format_list(&items));
    }

    let raw_module = args.module.as_deref().unwrap_or_default();
    let module = ModuleKind::parse(raw_module)?;
    let action = args.action.unwrap_or(ModuleAction::Status);

    match action {
        ModuleAction::Status => {
            let status = modules::status_of(&config.data_dir, &config.browser, module)?;
            let value = envelope("status", json!({ "module": status }), config)?;
            format_output(config.output, &value, || format_status(&status))
        }
        ModuleAction::Enable | ModuleAction::Disable => {
            session::check_roots(config, &config.cwd, config.mode).map_err(|error| {
                BoundaryError::new(error_codes::STORAGE_ERROR, error.to_string())
            })?;
            let enabled = matches!(action, ModuleAction::Enable);
            let mut store = ModuleStore::open(&config.data_dir)?;
            store.set(&config.browser, module, enabled)?;
            let status = modules::status_of(&config.data_dir, &config.browser, module)?;
            let action_name = if enabled { "enable" } else { "disable" };
            let value = envelope(action_name, json!({ "module": status }), config)?;
            format_output(config.output, &value, || format_status(&status))
        }
    }
}

pub fn write_line(output: &str) -> io::Result<()> {
    let mut stdout = io::stdout().lock();
    writeln!(stdout, "{output}")
}

fn envelope(action: &str, data: Value, config: &Config) -> Result<Value, BoundaryError> {
    let mut value = json!({
        "schema_version": 1,
        "kind": "module",
        "action": action,
        "data": data,
    });
    if config.output == Output::Jsonl {
        value["type"] = json!("module.result");
    }
    let secrets = config
        .secret_env_names()
        .iter()
        .map(|name| config.secret(name).map(str::to_owned))
        .collect::<Result<Vec<_>, _>>()?;
    redaction::redact(&mut value, &secrets)?;
    Ok(value)
}

fn format_output(
    output: Output,
    value: &Value,
    text: impl FnOnce() -> String,
) -> Result<String, BoundaryError> {
    match output {
        Output::Text => Ok(text()),
        Output::Json | Output::Jsonl => Ok(value.to_string()),
    }
}

fn format_list(items: &[ModuleStatus]) -> String {
    items
        .iter()
        .map(format_status)
        .collect::<Vec<_>>()
        .join("\n")
}

fn format_status(status: &ModuleStatus) -> String {
    let state = if status.enabled {
        "enabled"
    } else {
        "disabled"
    };
    let source = match status.source {
        ModuleStateSource::Config => "config",
        ModuleStateSource::ModuleStore => "module_store",
    };
    let mut line = format!(
        "{}: {} (source: {}, configured: {}, active: {})",
        status.module.as_str(),
        state,
        source,
        if status.configured_enabled {
            "enabled"
        } else {
            "disabled"
        },
        status.active,
    );
    if let Some(issue) = &status.issue {
        line.push_str(&format!("\n  note: {issue}"));
    }
    line
}
