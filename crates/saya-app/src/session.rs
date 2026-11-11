//! Durable session creation, validation, recovery and exclusive ownership.
use saya_core::{
    boundary::Store,
    config::{Config, MediaCapabilities, Protocol},
    contract::{HistoryItem, ModelProtocol, Session, SessionId, WorkMode},
    output_store::OutputStore,
};
use saya_core::{
    boundary::StoreChange,
    sqlite::{RecoverySummary, SessionLock, SqliteStore},
};
use std::path::{Path, PathBuf};
use std::sync::Arc;

pub struct Prepared {
    pub store: Arc<SqliteStore>,
    pub media: Arc<OutputStore>,
    pub media_capabilities: MediaCapabilities,
    pub session: Session,
    pub _lock: SessionLock,
    pub recovery: RecoverySummary,
}

pub struct ResumeOptions {
    pub accept_config_change: bool,
    pub cwd_explicit: bool,
    pub profile_explicit: bool,
    pub mode_override: Option<WorkMode>,
}

fn resolved(path: &Path) -> Result<PathBuf, Box<dyn std::error::Error>> {
    let mut current = path.to_path_buf();
    let mut missing = Vec::new();
    while !current.exists() {
        missing.push(
            current
                .file_name()
                .ok_or("invalid storage path")?
                .to_owned(),
        );
        current = current.parent().ok_or("invalid storage path")?.to_owned();
    }
    let mut result = std::fs::canonicalize(current)?;
    for component in missing.into_iter().rev() {
        result.push(component);
    }
    Ok(result)
}
/// Validate PLAN storage separation before opening or creating storage.
pub fn check_roots(
    config: &Config,
    workspace: &Path,
    mode: WorkMode,
) -> Result<(), Box<dyn std::error::Error>> {
    if mode == WorkMode::Plan {
        let root = std::fs::canonicalize(workspace)?;
        for path in [&config.data_dir, &config.state_dir, &config.cache_dir] {
            let location = resolved(path)?;
            if location.starts_with(&root) || root.starts_with(&location) {
                return Err("PLAN storage directories overlap the workspace".into());
            }
        }
    }
    Ok(())
}
/// Open durable storage with configured redaction. May create or migrate the database.
pub fn open(config: &Config) -> Result<Arc<SqliteStore>, Box<dyn std::error::Error>> {
    let secrets = config
        .secret_env_names()
        .iter()
        .map(|name| config.secret(name).map(str::to_owned))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(Arc::new(SqliteStore::open(&config.data_dir, secrets)?))
}
fn media_store(config: &Config) -> Result<Arc<OutputStore>, Box<dyn std::error::Error>> {
    let secrets = config
        .secret_env_names()
        .iter()
        .map(|name| config.secret(name).map(str::to_owned))
        .collect::<Result<Vec<_>, _>>()?;
    let registry = Arc::new(saya_core::credentials::SecretRegistry::default());
    registry.register(&secrets)?;
    Ok(Arc::new(OutputStore::open(
        config.data_dir.join("outputs"),
        config.output_capture.limits,
        registry,
    )?))
}
fn capture_history(
    session: &mut Session,
    media: &OutputStore,
    capabilities: MediaCapabilities,
) -> Result<Vec<saya_core::media::MediaCapture>, Box<dyn std::error::Error>> {
    let mut captures = Vec::new();
    for item in &mut session.history {
        let HistoryItem::UserText(text) = item else {
            continue;
        };
        if !saya_core::media::has_media(text) {
            continue;
        }
        let capture = saya_core::media::capture_user(text, capabilities, media, session.id)?;
        *text = capture.text.clone();
        captures.push(capture);
    }
    Ok(captures)
}
/// Parse a session identifier, returning a validation error for malformed input.
pub fn parse_id(raw: &str) -> Result<SessionId, Box<dyn std::error::Error>> {
    Ok(serde_json::from_value(serde_json::json!(raw)).map_err(|_| "invalid session ID")?)
}
/// Create durable session metadata and acquire an exclusive lock held by Prepared.
pub async fn create(
    config: &Config,
    mut session: Session,
) -> Result<Prepared, Box<dyn std::error::Error>> {
    check_roots(config, &session.workspace, session.work_mode)?;
    let store = open(config)?;
    let media = media_store(config)?;
    let lock = store.lock_session(session.id)?;
    let profile = config
        .profiles
        .get(&session.profile)
        .ok_or("session profile not found")?;
    let media_capabilities = profile.multimodal;
    let captures = capture_history(&mut session, &media, media_capabilities)?;
    store
        .create_session(&session, &profile.fingerprint()?)
        .await?;
    drop(captures);
    Ok(Prepared {
        store,
        media,
        media_capabilities,
        session,
        _lock: lock,
        recovery: RecoverySummary::default(),
    })
}
/// Validate current configuration under an exclusive lock and recover interrupted runs.
/// Historical approvals are never restored. Lock release follows Prepared lifetime.
pub async fn resume(
    config: &Config,
    raw: &str,
    options: ResumeOptions,
) -> Result<Prepared, Box<dyn std::error::Error>> {
    let id = parse_id(raw)?;
    let read_candidates =
        || -> Result<Vec<saya_core::sqlite::GuardCandidate>, Box<dyn std::error::Error>> {
            if let Some(candidates) = SqliteStore::guard_candidates(&config.data_dir, id)? {
                return Ok(candidates);
            }
            let session =
                SqliteStore::peek_session(&config.data_dir, id)?.ok_or("session not found")?;
            Ok(vec![saya_core::sqlite::GuardCandidate {
                workspace: session.workspace,
                work_mode: session.work_mode,
            }])
        };
    let preflight = |candidates: &[saya_core::sqlite::GuardCandidate]| -> Result<(), Box<dyn std::error::Error>> {
        for candidate in candidates {
            let workspace = if options.cwd_explicit { &config.cwd } else { &candidate.workspace };
            check_roots(config, workspace, options.mode_override.unwrap_or(candidate.work_mode))?;
        }
        Ok(())
    };
    preflight(&read_candidates()?)?;
    let lock = SqliteStore::lock_existing(&config.data_dir, id)?;
    let locked_preview = read_candidates()?;
    preflight(&locked_preview)?;
    let store = open(config)?;
    let mut session = store.load_session(id).await?.ok_or("session not found")?;
    if !locked_preview.iter().any(|candidate| {
        candidate.workspace == session.workspace && candidate.work_mode == session.work_mode
    }) {
        return Err("session guard does not match stored metadata".into());
    }
    if options.profile_explicit && config.profile.as_ref() != Some(&session.profile) {
        return Err("session profile differs; start a new session".into());
    }
    let profile = config
        .profiles
        .get(&session.profile)
        .ok_or("session profile not found")?;
    let protocol = match profile.protocol {
        Protocol::ChatCompletions => ModelProtocol::ChatCompletions,
        Protocol::Responses => ModelProtocol::Responses,
        Protocol::AnthropicMessages => ModelProtocol::AnthropicMessages,
    };
    if session.model != profile.model || session.protocol != protocol {
        return Err("session model or protocol changed; start a new session".into());
    }
    let fingerprint = profile.fingerprint()?;
    let original = store.fingerprint(id).await?;
    if original != fingerprint && !options.accept_config_change {
        return Err("session configuration changed; use --accept-config-change".into());
    }
    let workspace = if options.cwd_explicit {
        std::fs::canonicalize(&config.cwd)?
    } else {
        std::fs::canonicalize(&session.workspace)
            .map_err(|_| "workspace is unavailable; use --cwd")?
    };
    if !options.cwd_explicit && workspace != session.workspace {
        return Err("workspace moved; use --cwd".into());
    }
    let mode = options.mode_override.unwrap_or(session.work_mode);
    check_roots(config, &workspace, mode)?;
    if original != fingerprint || session.workspace != workspace || session.work_mode != mode {
        session.workspace = workspace;
        session.work_mode = mode;
        store.update_session_config(&session, &fingerprint).await?;
    }
    if SqliteStore::guard_candidates(&config.data_dir, id)?.is_none() || locked_preview.len() > 1 {
        store
            .commit(&[StoreChange::Session(session.clone())])
            .await?;
    }
    let recovery = store.recover_interrupted(id).await?;
    if recovery.interrupted_runs > 0 {
        session = store.load_session(id).await?.ok_or("session not found")?;
    }
    let media = media_store(config)?;
    let media_capabilities = profile.multimodal;
    Ok(Prepared {
        store,
        media,
        media_capabilities,
        session,
        _lock: lock,
        recovery,
    })
}
/// Persist user input, then reload the durable session. Propagates storage errors.
pub async fn append(
    prepared: &mut Prepared,
    text: String,
) -> Result<(), Box<dyn std::error::Error>> {
    let capture = saya_core::media::capture_user(
        &text,
        prepared.media_capabilities,
        &prepared.media,
        prepared.session.id,
    )?;
    let item = HistoryItem::UserText(capture.text.clone());
    prepared
        .store
        .commit(&[StoreChange::History {
            session_id: prepared.session.id,
            item: item.clone(),
        }])
        .await?;
    drop(capture);
    prepared.session = prepared
        .store
        .load_session(prepared.session.id)
        .await?
        .ok_or("session not found")?;
    Ok(())
}
