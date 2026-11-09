//! Host-side MCP administration. Credential material never appears in command arguments or output.
use clap::{Args, Subcommand};
use saya_core::{
    config::{Config, Output},
    contract::BoundaryError,
    credentials::{
        CredentialRef, CredentialScope, CredentialValue, FileCredentialStore, SecretRegistry,
        SecretValue,
    },
    mcp::config::{HttpMcpSettings, McpTransportSettings},
};
use std::{
    collections::BTreeSet,
    io::{self, IsTerminal},
    sync::Arc,
    time::Duration,
};

#[derive(Subcommand)]
pub enum McpAction {
    /// Authorize one HTTP server using a loopback OAuth callback.
    Login(LoginArgs),
    /// List, import, or revoke host-only credential references.
    Credentials {
        #[command(subcommand)]
        action: CredentialAction,
    },
}
#[derive(Args)]
pub struct LoginArgs {
    server: String,
    #[arg(long)]
    scope: Vec<String>,
    /// Explicitly approve this endpoint and the requested scope set.
    #[arg(long)]
    approve_scopes: bool,
    #[arg(long)]
    client_id: Option<String>,
    #[arg(long, default_value_t = 0)]
    callback_port: u16,
    #[arg(long, default_value_t = 300, value_parser = clap::value_parser!(u64).range(1..=600))]
    timeout_seconds: u64,
    /// Open the returned authorization URL using the system URL handler.
    #[arg(long)]
    open_browser: bool,
}
#[derive(Subcommand)]
pub enum CredentialAction {
    List,
    /// Import a bearer token from a named environment variable.
    Import {
        server: String,
        #[arg(long)]
        from_env: String,
        #[arg(long)]
        scope: Vec<String>,
        #[arg(long)]
        approve_scopes: bool,
    },
    Revoke {
        reference: String,
    },
}
fn failure(code: &str, message: &str) -> BoundaryError {
    super::cli_error(code, message)
}
fn known_credentials(config: &Config) -> Result<Vec<String>, BoundaryError> {
    config
        .secret_env_names()
        .iter()
        .map(|name| config.secret(name).map(str::to_owned))
        .collect()
}
fn ensure_clean_metadata(value: &str, secrets: &[String]) -> Result<(), BoundaryError> {
    if secrets.iter().all(String::is_empty) {
        return Ok(());
    }
    let sensitive = || {
        failure(
            "sensitive_data",
            "MCP authorization metadata contains a credential",
        )
    };
    let mut view = value.to_owned();
    for _ in 0..8 {
        if saya_core::redaction::contains_secret(&view, secrets) {
            return Err(sensitive());
        }
        let mut decoded = Vec::with_capacity(view.len());
        let mut cursor = 0;
        let bytes = view.as_bytes();
        let mut changed = false;
        while cursor < bytes.len() {
            if let Some(encoded) = bytes.get(cursor..cursor.saturating_add(3))
                && encoded[0] == b'%'
                && let (Some(high), Some(low)) = (
                    char::from(encoded[1]).to_digit(16),
                    char::from(encoded[2]).to_digit(16),
                )
            {
                decoded.push((high * 16 + low) as u8);
                cursor += 3;
                changed = true;
            } else if bytes[cursor] == b'+' {
                decoded.push(b' ');
                cursor += 1;
                changed = true;
            } else {
                decoded.push(bytes[cursor]);
                cursor += 1;
            }
        }
        if !changed {
            return Ok(());
        }
        view = String::from_utf8_lossy(&decoded).into_owned();
    }
    Err(sensitive())
}
fn settings<'a>(config: &'a Config, server: &str) -> Result<&'a HttpMcpSettings, BoundaryError> {
    let configured = config
        .mcp
        .servers
        .get(server)
        .filter(|server| server.enabled)
        .ok_or_else(|| failure("invalid_config", "Enabled MCP server was not found"))?;
    match &configured.transport {
        McpTransportSettings::Http(http) => Ok(http),
        _ => Err(failure(
            "invalid_config",
            "Credential administration requires an HTTP MCP server",
        )),
    }
}
fn open_store(config: &Config) -> Result<FileCredentialStore, BoundaryError> {
    let registry = Arc::new(SecretRegistry::default());
    let values = known_credentials(config)?;
    registry.register(&values)?;
    std::fs::create_dir_all(&config.state_dir).map_err(|_| {
        failure(
            "credential_io",
            "Credential parent directory could not be created",
        )
    })?;
    FileCredentialStore::open(config.state_dir.join("credentials"), registry)
}
async fn approve(
    scope: &CredentialScope,
    approved: bool,
    secrets: &[String],
    cancel: &dyn saya_core::boundary::Cancellation,
) -> Result<(), BoundaryError> {
    if approved {
        return Ok(());
    }
    if !io::stdin().is_terminal() || !io::stderr().is_terminal() {
        return Err(failure(
            "needs_input",
            "Explicit approval of the MCP endpoint and scopes is required",
        ));
    }
    let terminal = super::Terminal::open(secrets.to_vec())
        .map_err(|_| failure("needs_input", "A controlling terminal is required"))?;
    let scopes = scope.scopes.iter().cloned().collect::<Vec<_>>().join(" ");
    let prompt = format!(
        "\nMCP server {}\nResource {}\nRequested scopes [{}]\nApprove this authorization? [y/N] ",
        scope.server_id, scope.audience, scopes
    );
    if terminal
        .read_line(&prompt, cancel)
        .await?
        .is_some_and(|answer| matches!(answer.trim(), "y" | "Y" | "yes"))
    {
        Ok(())
    } else {
        Err(failure(
            "permission_denied",
            "MCP authorization was not approved",
        ))
    }
}
fn scope(
    config: &Config,
    server: &str,
    extra: &[String],
    secrets: &[String],
) -> Result<CredentialScope, BoundaryError> {
    let http = settings(config, server)?;
    let requested: BTreeSet<_> = http.scopes.iter().chain(extra).cloned().collect();
    let scope = CredentialScope::for_mcp(server, &http.endpoint, requested)?;
    ensure_clean_metadata(
        &serde_json::json!({"scope":scope,"endpoint":http.endpoint}).to_string(),
        secrets,
    )?;
    Ok(scope)
}
async fn perform(config: &Config, action: &McpAction) -> Result<serde_json::Value, BoundaryError> {
    let cancel = Arc::new(super::Cancel::default());
    let _listener = super::signals::CancellationListener::start(cancel.clone())
        .map_err(|_| failure("signal_error", "Signal handlers could not be installed"))?;
    match action {
        McpAction::Credentials {
            action: CredentialAction::List,
        } => serde_json::to_value(open_store(config)?.list().await?)
            .map_err(|_| failure("output_error", "Credential metadata could not be encoded")),
        McpAction::Credentials {
            action: CredentialAction::Revoke { reference },
        } => {
            let reference: CredentialRef = reference.parse()?;
            let store = open_store(config)?;
            let metadata = store
                .list()
                .await?
                .into_iter()
                .find(|record| record.reference == reference)
                .ok_or_else(|| {
                    failure(
                        "invalid_credential_ref",
                        "Credential reference was not found",
                    )
                })?;
            store.revoke(reference, &metadata.scope).await?;
            Ok(serde_json::json!({"reference":reference,"revoked":true}))
        }
        McpAction::Credentials {
            action:
                CredentialAction::Import {
                    server,
                    from_env,
                    scope: extra,
                    approve_scopes,
                },
        } => {
            let value = SecretValue::new(config.secret(from_env)?.to_owned())?;
            let mut secrets = known_credentials(config)?;
            secrets.push(value.expose().to_owned());
            let scope = scope(config, server, extra, &secrets)?;
            approve(&scope, *approve_scopes, &secrets, cancel.as_ref()).await?;
            let reference = open_store(config)?
                .create(scope, CredentialValue::Secret(value))
                .await?;
            Ok(serde_json::json!({"reference":reference,"server":server}))
        }
        McpAction::Login(args) => {
            let secrets = known_credentials(config)?;
            ensure_clean_metadata(
                &serde_json::json!({"client_id":args.client_id}).to_string(),
                &secrets,
            )?;
            let scope = scope(config, &args.server, &args.scope, &secrets)?;
            approve(&scope, args.approve_scopes, &secrets, cancel.as_ref()).await?;
            let interactive = io::stdin().is_terminal() && io::stderr().is_terminal();
            let flow = saya_core::credentials::McpOAuthLogin::begin(
                open_store(config)?,
                scope,
                saya_core::credentials::OAuthLoginOptions {
                    interactive,
                    client_id: args.client_id.clone(),
                    timeout: Duration::from_secs(args.timeout_seconds),
                    callback_port: args.callback_port,
                },
                cancel.as_ref(),
            )
            .await?;
            ensure_clean_metadata(flow.authorization_url(), &secrets)?;
            eprintln!(
                "Open this URL to authorize MCP access\n{}",
                flow.authorization_url()
            );
            if args.open_browser {
                open_browser(flow.authorization_url(), cancel.as_ref()).await?;
            }
            let reference = flow.wait_for_callback(cancel.as_ref()).await?;
            Ok(serde_json::json!({"reference":reference,"server":args.server}))
        }
    }
}
async fn open_browser(
    url: &str,
    cancel: &dyn saya_core::boundary::Cancellation,
) -> Result<(), BoundaryError> {
    if cancel.is_cancelled() {
        return Err(failure("cancelled", "MCP authorization was cancelled"));
    }
    #[cfg(target_os = "macos")]
    let program = "/usr/bin/open";
    #[cfg(target_os = "linux")]
    let program = "/usr/bin/xdg-open";
    let mut child = tokio::process::Command::new(program)
        .arg(url)
        .kill_on_drop(true)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map_err(|_| failure("browser_error", "System URL handler could not be started"))?;
    let status = tokio::select! {
        biased;
        _ = cancel.cancelled() => None,
        result = tokio::time::timeout(Duration::from_secs(10), child.wait()) => result.ok().and_then(Result::ok),
    };
    let Some(status) = status else {
        let _ = child.kill().await;
        let _ = child.wait().await;
        return Err(if cancel.is_cancelled() {
            failure("cancelled", "MCP authorization was cancelled")
        } else {
            failure("browser_error", "System URL handler did not complete")
        });
    };
    if status.success() {
        Ok(())
    } else {
        Err(failure("browser_error", "System URL handler failed"))
    }
}
fn error_exit_code(code: &str) -> u8 {
    match code {
        "cancelled" => 130,
        "needs_input" | "needs_auth" => 4,
        "needs_confirmation" => 3,
        "limit_reached" | "credential_limit" => 5,
        "invalid_config"
        | "invalid_args"
        | "invalid_credential"
        | "invalid_credential_ref"
        | "credential_scope" => 2,
        _ => 1,
    }
}
pub async fn run(config: &Config, action: &McpAction) -> u8 {
    let result = perform(config, action).await;
    let (data, error, code) = match result {
        Ok(data) => (Some(data), None, 0),
        Err(error) => {
            let code = error_exit_code(&error.0.code);
            (None, Some(error.0), code)
        }
    };
    let mut value = serde_json::json!({"schema_version":1,"data":data,"error":error});
    let secrets = known_credentials(config);
    let safe = secrets.is_ok_and(|secrets| {
        saya_core::redaction::redact(&mut value, &secrets).is_ok()
            && !saya_core::redaction::contains_secret(&value.to_string(), &secrets)
    });
    if !safe {
        return super::query_error(
            "Sensitive MCP administration details were withheld",
            Some(config.output),
        );
    }
    if config.output == Output::Jsonl {
        value["type"] = "mcp.result".into();
    }
    println!("{value}");
    code
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn import_checks_an_unconfigured_source_before_requesting_approval() {
        use saya_core::config::{Environment, Overrides};
        let directory = tempfile::tempdir().unwrap();
        let secret = "SYNTHETIC-UNCONFIGURED-IMPORT-TOKEN";
        let path = directory.path().join("config.toml");
        std::fs::write(&path, format!(
            "schema_version=1\n[mcp.servers.fixture.transport]\ntype=\"http\"\nendpoint=\"http://127.0.0.1:12345/{secret}\"\nallow_insecure_http=true\n"
        )).unwrap();
        let environment = Environment::from_pairs([
            ("HOME".into(), directory.path().as_os_str().to_owned()),
            ("UNCONFIGURED_SOURCE".into(), secret.into()),
        ]);
        let config = saya_core::config::load(
            Overrides {
                config: Some(path),
                ..Default::default()
            },
            environment,
            directory.path(),
        )
        .unwrap();
        assert!(config.secret_env_names().is_empty());
        let error = perform(
            &config,
            &McpAction::Credentials {
                action: CredentialAction::Import {
                    server: "fixture".into(),
                    from_env: "UNCONFIGURED_SOURCE".into(),
                    scope: Vec::new(),
                    approve_scopes: false,
                },
            },
        )
        .await
        .unwrap_err();
        assert_eq!(error.0.code, "sensitive_data");
        assert!(!config.state_dir.exists());
    }

    #[test]
    fn authorization_metadata_rejects_known_credentials_before_display_or_navigation() {
        let secret = "SYNTHETIC/key+percent=%中文".to_owned();
        for metadata in [
            serde_json::json!({"server":secret}).to_string(),
            "https://example.invalid/authorize?token=SYNTHETIC%2fkey%2Bpercent%3d%25%E4%B8%AD%E6%96%87".into(),
            "https://example.invalid/authorize?token=SYNTHETIC%252fkey%252Bpercent%253d%2525%25E4%25B8%25AD%25E6%2596%2587".into(),
        ] {
            let error = ensure_clean_metadata(&metadata, std::slice::from_ref(&secret)).unwrap_err();
            assert_eq!(error.0.code, "sensitive_data");
            assert!(!error.to_string().contains(&secret));
        }
        let url = "https://example.invalid/authorize?state=csrf-value&code_challenge=public-pkce-value&scope=files%3Aread";
        ensure_clean_metadata(url, &[secret]).unwrap();
    }

    #[test]
    fn administration_exit_codes_follow_the_shared_cli_contract() {
        for (error, code) in [
            ("needs_input", 4),
            ("needs_auth", 4),
            ("needs_confirmation", 3),
            ("cancelled", 130),
            ("invalid_config", 2),
            ("invalid_credential_ref", 2),
            ("credential_scope", 2),
            ("credential_io", 1),
            ("permission_denied", 1),
            ("limit_reached", 5),
        ] {
            assert_eq!(error_exit_code(error), code);
        }
    }
}
