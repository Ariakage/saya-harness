//! Host selection of enabled packages and private extension resources.
use saya_core::{
    config::Config,
    contract::{BoundaryError, ErrorInfo, Session},
    credentials::SecretRegistry,
    execution::SandboxPolicy,
    hook_store::{HookStore, HookStoreLimits},
    plugin_runtime::{ViewLimits, ViewPool},
    plugin_store::{PluginStore, StoreLimits},
    runtime::AgentRuntime,
};
use std::{
    fs::{self, File},
    os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt},
    path::{Path, PathBuf},
    sync::Arc,
};

pub(super) fn selected_store(
    config: &Config,
    session: &Session,
    secrets: Arc<SecretRegistry>,
) -> Result<Option<PluginStore>, BoundaryError> {
    if !config.plugins.enabled {
        return Ok(None);
    }
    let path = config.data_dir.join("plugins");
    match std::fs::symlink_metadata(&path) {
        Err(failure) if failure.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err(failure("plugin_io", "Plugin store metadata is unavailable")),
        Ok(_) => {}
    }
    let store = PluginStore::open(path, secrets, StoreLimits::default())?;
    if store.enabled_for(&session.workspace)?.is_empty() {
        Ok(None)
    } else {
        Ok(Some(store))
    }
}

fn failure(code: &str, message: &str) -> BoundaryError {
    BoundaryError(
        ErrorInfo {
            code: code.into(),
            message: message.into(),
        },
        None,
        None,
    )
}

fn view_path(session: &Session, sandbox: &SandboxPolicy) -> Result<PathBuf, BoundaryError> {
    let workspace = std::fs::canonicalize(&session.workspace)
        .map_err(|_| failure("plugin_view_scope", "Workspace is unavailable"))?;
    // SAFETY: geteuid has no pointer parameters or ownership requirements.
    let uid = unsafe { libc::geteuid() };
    for candidate in [std::env::temp_dir(), PathBuf::from("/var/tmp")] {
        let Ok(parent) = std::fs::canonicalize(candidate) else {
            continue;
        };
        let path = parent.join(format!("saya-plugin-views-{uid}"));
        if !path.starts_with(&workspace)
            && !sandbox
                .write_roots
                .iter()
                .any(|root| path.starts_with(root))
            && !sandbox
                .protected_paths
                .iter()
                .any(|root| path.starts_with(root))
        {
            return Ok(path);
        }
    }
    Err(failure(
        "plugin_view_scope",
        "No private plugin view location is available outside writable and protected roots",
    ))
}

pub(super) fn attach(
    runtime: AgentRuntime,
    config: &Config,
    session: &Session,
    sandbox: &SandboxPolicy,
    store: PluginStore,
    secrets: Arc<SecretRegistry>,
) -> Result<AgentRuntime, BoundaryError> {
    let pool = Arc::new(ViewPool::open(
        &view_path(session, sandbox)?,
        ViewLimits::default(),
    )?);
    let journal = hook_journal(&config.state_dir, session, secrets)?;
    runtime.with_plugins(store, pool, config.plugins.bindings.clone(), journal)
}

// Keep the opened parent alive while HookStore establishes its own stable root.
fn hook_journal(
    state: &Path,
    session: &Session,
    secrets: Arc<SecretRegistry>,
) -> Result<Arc<HookStore>, BoundaryError> {
    let private_error = || {
        failure(
            "hook_journal_permissions",
            "Hook journal parent is unavailable or unsafe",
        )
    };
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(state)
        .map_err(|_| private_error())?;
    let state_file = fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(state)
        .map_err(|_| private_error())?;
    let state_metadata = state_file.metadata().map_err(|_| private_error())?;
    // SAFETY: geteuid takes no pointers and retains no ownership.
    let user = unsafe { libc::geteuid() };
    if !state_metadata.is_dir()
        || state_metadata.uid() != user
        || state_metadata.mode() & 0o022 != 0
    {
        return Err(private_error());
    }
    let state_dir = cap_std::fs::Dir::from_std_file(state_file);
    let mut builder = cap_std::fs::DirBuilder::new();
    cap_std::fs::DirBuilderExt::mode(&mut builder, 0o700);
    match state_dir.create_dir_with("hooks", &builder) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(_) => return Err(private_error()),
    }
    let mut options = cap_std::fs::OpenOptions::new();
    options.read(true);
    cap_std::fs::OpenOptionsExt::custom_flags(
        &mut options,
        libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
    );
    let parent: File = state_dir
        .open_with("hooks", &options)
        .map_err(|_| private_error())?
        .into_std();
    let metadata = parent.metadata().map_err(|_| private_error())?;
    if !metadata.is_dir() || metadata.uid() != user || metadata.mode() & 0o077 != 0 {
        return Err(private_error());
    }
    let path = state.join("hooks");
    let named = fs::symlink_metadata(&path).map_err(|_| private_error())?;
    if named.dev() != metadata.dev() || named.ino() != metadata.ino() {
        return Err(private_error());
    }
    let journal = Arc::new(HookStore::open(
        &path.join(session.id.0.to_string()),
        secrets,
        HookStoreLimits::default(),
    )?);
    let named = fs::symlink_metadata(&path).map_err(|_| private_error())?;
    if named.dev() != metadata.dev() || named.ino() != metadata.ino() {
        return Err(private_error());
    }
    Ok(journal)
}

#[cfg(test)]
mod tests {
    use super::*;
    use saya_core::contract::{ModelProtocol, SessionId, WorkMode};
    use std::os::unix::fs::{PermissionsExt, symlink};
    fn session(workspace: &Path) -> Session {
        Session {
            id: SessionId::new(),
            workspace: workspace.into(),
            work_mode: WorkMode::Work,
            profile: "fixture".into(),
            protocol: ModelProtocol::Responses,
            model: "fixture".into(),
            history: vec![],
        }
    }
    #[test]
    fn hook_journal_creates_missing_private_parents_on_the_normal_attachment_path() {
        let root = tempfile::tempdir().unwrap();
        let state = root.path().join("state/saya");
        let journal = hook_journal(
            &state,
            &session(root.path()),
            Arc::new(SecretRegistry::default()),
        )
        .unwrap();
        for path in [&state, &state.join("hooks")] {
            assert_eq!(
                fs::metadata(path).unwrap().permissions().mode() & 0o777,
                0o700
            );
        }
        drop(journal);
        assert_eq!(fs::read_dir(state.join("hooks")).unwrap().count(), 1);
    }
    #[test]
    fn hook_journal_rejects_linked_or_permissive_parents_without_creating_session_files() {
        let root = tempfile::tempdir().unwrap();
        let state = root.path().join("state");
        let other = root.path().join("other");
        fs::create_dir(&state).unwrap();
        fs::create_dir(&other).unwrap();
        symlink(&other, state.join("hooks")).unwrap();
        assert!(
            hook_journal(
                &state,
                &session(root.path()),
                Arc::new(SecretRegistry::default())
            )
            .is_err()
        );
        assert_eq!(fs::read_dir(&other).unwrap().count(), 0);
        fs::remove_file(state.join("hooks")).unwrap();
        fs::create_dir(state.join("hooks")).unwrap();
        fs::set_permissions(state.join("hooks"), fs::Permissions::from_mode(0o755)).unwrap();
        assert!(
            hook_journal(
                &state,
                &session(root.path()),
                Arc::new(SecretRegistry::default())
            )
            .is_err()
        );
        assert_eq!(fs::read_dir(state.join("hooks")).unwrap().count(), 0);
    }
}
