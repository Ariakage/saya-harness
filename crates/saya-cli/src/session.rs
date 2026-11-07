//! Durable session preparation. Real side effects remain disabled until recovery is ready.
use super::*;
use saya_core::{
    boundary::StoreChange,
    sqlite::{SessionLock, SqliteStore},
};
use std::path::{Path, PathBuf};

pub struct Prepared {
    pub store: Arc<SqliteStore>,
    pub session: Session,
    pub _lock: SessionLock,
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
pub fn open(config: &Config) -> Result<Arc<SqliteStore>, Box<dyn std::error::Error>> {
    let secrets = config
        .secret_env_names()
        .iter()
        .map(|name| config.secret(name).map(str::to_owned))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(Arc::new(SqliteStore::open(&config.data_dir, secrets)?))
}
pub fn parse_id(raw: &str) -> Result<SessionId, Box<dyn std::error::Error>> {
    Ok(serde_json::from_value(serde_json::json!(raw)).map_err(|_| "invalid session ID")?)
}
pub async fn create(
    config: &Config,
    session: Session,
) -> Result<Prepared, Box<dyn std::error::Error>> {
    check_roots(config, &session.workspace, session.work_mode)?;
    let store = open(config)?;
    let lock = store.lock_session(session.id)?;
    let profile = config
        .profiles
        .get(&session.profile)
        .ok_or("session profile not found")?;
    store
        .create_session(&session, &profile.fingerprint()?)
        .await?;
    Ok(Prepared {
        store,
        session,
        _lock: lock,
    })
}
pub async fn resume(
    config: &Config,
    raw: &str,
    accept: bool,
    cwd_explicit: bool,
    profile_explicit: bool,
) -> Result<Prepared, Box<dyn std::error::Error>> {
    // An explicitly selected PLAN must never create its database inside the workspace.
    check_roots(config, &config.cwd, config.mode)?;
    let id = parse_id(raw)?;
    let preview = SqliteStore::peek_session(&config.data_dir, id)?.ok_or("session not found")?;
    let preview_workspace = if cwd_explicit {
        &config.cwd
    } else {
        &preview.workspace
    };
    check_roots(config, preview_workspace, preview.work_mode)?;
    let store = open(config)?;
    let lock = store.lock_session(id)?;
    let mut session = store.load_session(id).await?.ok_or("session not found")?;
    if store.has_unfinished_run(id).await? {
        return Err("session contains an interrupted run; recovery is not available yet".into());
    }
    if profile_explicit && config.profile.as_ref() != Some(&session.profile) {
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
    if original != fingerprint && !accept {
        return Err("session configuration changed; use --accept-config-change".into());
    }
    let workspace = if cwd_explicit {
        std::fs::canonicalize(&config.cwd)?
    } else {
        std::fs::canonicalize(&session.workspace)
            .map_err(|_| "workspace is unavailable; use --cwd")?
    };
    if !cwd_explicit && workspace != session.workspace {
        return Err("workspace moved; use --cwd".into());
    }
    check_roots(config, &workspace, session.work_mode)?;
    if original != fingerprint || session.workspace != workspace {
        session.workspace = workspace;
        store.update_session_config(&session, &fingerprint).await?;
    }
    Ok(Prepared {
        store,
        session,
        _lock: lock,
    })
}
pub async fn append(
    prepared: &mut Prepared,
    text: String,
) -> Result<(), Box<dyn std::error::Error>> {
    let item = HistoryItem::UserText(text);
    prepared
        .store
        .commit(&[StoreChange::History {
            session_id: prepared.session.id,
            item: item.clone(),
        }])
        .await?;
    prepared.session.history.push(item);
    Ok(())
}
