//! Runtime module state store and effective browser/VM toggle resolution.

use crate::{
    browser::BrowserSettings,
    contract::{BoundaryError, error_codes},
};
use rusqlite::{Connection, OpenFlags, OptionalExtension, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use std::{
    fmt,
    fs::{self, File},
    io::ErrorKind,
    path::{Path, PathBuf},
    str::FromStr,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

/// Directory under `data_dir` that holds persistent module toggles.
pub const MODULE_DIR_NAME: &str = "modules";
/// SQLite database filename storing persisted module enablement overrides.
pub const MODULE_DB_NAME: &str = "modules.sqlite3";

const LOCK_NAME: &str = ".modules.lock";
const APPLICATION_ID: i64 = 0x534d_4f31;
const SCHEMA_VERSION: i64 = 1;
const BUSY_TIMEOUT: Duration = Duration::from_secs(5);

/// Toggleable runtime capability module managed via `soyo module`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModuleKind {
    Browser,
    Vm,
}

impl ModuleKind {
    /// All known modules in deterministic display order.
    pub const fn all() -> [Self; 2] {
        [Self::Browser, Self::Vm]
    }

    /// Canonical CLI and database identifier for this module.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Browser => "browser",
            Self::Vm => "vm",
        }
    }

    /// Short human-readable summary of what this module controls.
    pub const fn description(self) -> &'static str {
        match self {
            Self::Browser => "Browser capability (`browser.enabled`)",
            Self::Vm => "VM-isolated browser backend (`browser.vm.enabled`)",
        }
    }

    /// Parse a module identifier (`browser` or `vm`).
    pub fn parse(raw: &str) -> Result<Self, BoundaryError> {
        match raw {
            "browser" => Ok(Self::Browser),
            "vm" => Ok(Self::Vm),
            _ => Err(BoundaryError::new(
                error_codes::INVALID_ARGS,
                format!("unknown module '{raw}'; expected browser or vm"),
            )),
        }
    }
}

impl fmt::Display for ModuleKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for ModuleKind {
    type Err = BoundaryError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::parse(s)
    }
}

/// Origin of a module's current effective enablement state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModuleStateSource {
    Config,
    ModuleStore,
}

/// Explicit module overrides stored in `data_dir/modules/modules.sqlite3`.
///
/// Unset entries (`None`) inherit the corresponding `enabled` boolean from
/// `config.toml`; explicitly toggled entries override `config.toml`.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModuleOverrides {
    pub browser: Option<bool>,
    pub vm: Option<bool>,
}

impl ModuleOverrides {
    /// Read the explicit override for `module`, if present.
    pub const fn get(self, module: ModuleKind) -> Option<bool> {
        match module {
            ModuleKind::Browser => self.browser,
            ModuleKind::Vm => self.vm,
        }
    }

    /// Update the in-memory override for `module`.
    pub fn set(&mut self, module: ModuleKind, value: Option<bool>) {
        match module {
            ModuleKind::Browser => self.browser = value,
            ModuleKind::Vm => self.vm = value,
        }
    }

    /// Apply stored overrides onto `settings` without mutating `config.toml`.
    pub fn apply(self, settings: &BrowserSettings) -> BrowserSettings {
        let mut effective = settings.clone();
        if let Some(enabled) = self.browser {
            effective.enabled = enabled;
        }
        if let Some(vm_enabled) = self.vm {
            effective.vm.enabled = vm_enabled;
        }
        effective
    }
}

/// Effective status snapshot for one module against the loaded configuration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModuleStatus {
    pub module: ModuleKind,
    pub description: String,
    pub enabled: bool,
    pub configured_enabled: bool,
    pub override_enabled: Option<bool>,
    pub source: ModuleStateSource,
    pub active: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub issue: Option<String>,
}

/// Return the SQLite database path under `data_dir` for persisted module state.
pub fn module_db_path(data_dir: &Path) -> PathBuf {
    data_dir.join(MODULE_DIR_NAME).join(MODULE_DB_NAME)
}

/// Read persisted module overrides from `data_dir` without creating files.
///
/// Returns default (empty) overrides when `data_dir`, its `modules` directory,
/// or `modules.sqlite3` does not yet exist. Fails closed if an existing path
/// is a symlink, has unsafe ownership/permissions, or is corrupt.
pub fn load_overrides(data_dir: &Path) -> Result<ModuleOverrides, BoundaryError> {
    let root_meta = match fs::symlink_metadata(data_dir) {
        Ok(meta) => meta,
        Err(err) if err.kind() == ErrorKind::NotFound => return Ok(ModuleOverrides::default()),
        Err(_) => return Err(storage_error("module store root cannot be inspected")),
    };
    if root_meta.file_type().is_symlink() || !root_meta.is_dir() {
        return Err(storage_error("module store root must be a real directory"));
    }

    let dir_path = data_dir.join(MODULE_DIR_NAME);
    let Some((_dir, dir_meta)) =
        soyo_fs::open_existing_private_dir(&dir_path, error_codes::STORAGE_ERROR, "module")?
    else {
        return Ok(ModuleOverrides::default());
    };
    let canonical_dir =
        soyo_fs::verify_canonical_dir(&dir_path, &dir_meta, error_codes::STORAGE_ERROR, "module")?;
    let db_path = canonical_dir.join(MODULE_DB_NAME);
    match existing_db_presence(&db_path)? {
        DbPresence::Missing | DbPresence::Empty => return Ok(ModuleOverrides::default()),
        DbPresence::Present => {}
    }

    let connection = Connection::open_with_flags(
        &db_path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NOFOLLOW,
    )
    .map_err(|_| storage_error("cannot open module database for reading"))?;
    connection
        .busy_timeout(BUSY_TIMEOUT)
        .map_err(|_| storage_error("cannot configure module database timeout"))?;
    match schema_state(&connection)? {
        // An interrupted initialization left a valid but table-less file; it
        // holds no overrides yet, so reads behave as if none were stored.
        SchemaState::Incomplete => Ok(ModuleOverrides::default()),
        SchemaState::Foreign => Err(storage_error("unrecognized or unsupported module database")),
        SchemaState::Ready => read_overrides(&connection),
    }
}

/// Resolve effective [`BrowserSettings`] by applying any stored module overrides
/// and validating the resulting configuration fail-closed.
pub fn resolve_browser_settings(
    data_dir: &Path,
    configured: &BrowserSettings,
) -> Result<(BrowserSettings, ModuleOverrides), BoundaryError> {
    let overrides = load_overrides(data_dir)?;
    let effective = overrides.apply(configured);
    effective.validate()?;
    Ok((effective, overrides))
}

/// Build effective status snapshots for all known modules without mutating disk.
pub fn statuses(
    data_dir: &Path,
    configured: &BrowserSettings,
) -> Result<Vec<ModuleStatus>, BoundaryError> {
    let overrides = load_overrides(data_dir)?;
    let effective = overrides.apply(configured);
    Ok(ModuleKind::all()
        .into_iter()
        .map(|module| build_status(module, configured, &effective, overrides))
        .collect())
}

/// Build the effective status snapshot for one module without mutating disk.
pub fn status_of(
    data_dir: &Path,
    configured: &BrowserSettings,
    module: ModuleKind,
) -> Result<ModuleStatus, BoundaryError> {
    let overrides = load_overrides(data_dir)?;
    let effective = overrides.apply(configured);
    Ok(build_status(module, configured, &effective, overrides))
}

/// Persistent SQLite store for explicit module toggles under `data_dir/modules`.
pub struct ModuleStore {
    connection: Connection,
    _lock: File,
}

impl ModuleStore {
    /// Open or initialize the private module state database under `data_dir`.
    ///
    /// Refuses symlinks, non-regular files, unexpected link counts, and files or
    /// directories owned by another user or writable/readable by group/other.
    pub fn open(data_dir: &Path) -> Result<Self, BoundaryError> {
        soyo_fs::ensure_root_dir(data_dir, error_codes::STORAGE_ERROR, "module store")?;
        let dir_path = data_dir.join(MODULE_DIR_NAME);
        let (_dir_guard, _dir_meta, canonical_dir) =
            soyo_fs::ensure_private_dir(&dir_path, error_codes::STORAGE_ERROR, "module")?;

        let lock = soyo_fs::acquire_flock(
            &canonical_dir.join(LOCK_NAME),
            BUSY_TIMEOUT,
            error_codes::STORAGE_ERROR,
            error_codes::STORAGE_ERROR,
            "module store",
        )?;
        let db_path = canonical_dir.join(MODULE_DB_NAME);
        let (created, _meta) =
            soyo_fs::create_or_verify_private_file(&db_path, error_codes::STORAGE_ERROR, "module")?;
        let presence = if created {
            DbPresence::Missing
        } else {
            match existing_db_presence(&db_path)? {
                DbPresence::Missing => {
                    return Err(storage_error("module database disappeared while opening"));
                }
                presence => presence,
            }
        };

        let connection = Connection::open_with_flags(
            &db_path,
            OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NOFOLLOW,
        )
        .map_err(|_| storage_error("cannot open module database"))?;
        connection
            .busy_timeout(BUSY_TIMEOUT)
            .map_err(|_| storage_error("cannot configure module database timeout"))?;
        connection
            .execute_batch(
                "PRAGMA journal_mode = WAL;
                 PRAGMA synchronous = FULL;
                 PRAGMA temp_store = MEMORY;
                 PRAGMA foreign_keys = ON;",
            )
            .map_err(|_| storage_error("cannot configure module database pragmas"))?;

        match presence {
            // A zero-byte or partially initialized file is completed under the
            // store lock so an interrupted first open cannot wedge the store.
            DbPresence::Missing | DbPresence::Empty => initialize_schema(&connection)?,
            DbPresence::Present => match schema_state(&connection)? {
                SchemaState::Ready => {}
                SchemaState::Incomplete => initialize_schema(&connection)?,
                SchemaState::Foreign => {
                    return Err(storage_error("unrecognized or unsupported module database"));
                }
            },
        }

        Ok(Self {
            connection,
            _lock: lock,
        })
    }

    /// Persist an enable/disable override for `module`.
    ///
    /// Enabling a module preflight-validates the resulting effective
    /// [`BrowserSettings`] (including D-066 VM/worker mutual exclusion,
    /// `[browser.vm]` host paths, and `[browser.network]` prerequisites) before
    /// modifying disk. Disabling is permitted so users can resolve conflicts or
    /// turn off a module without needing backend files present.
    pub fn set(
        &mut self,
        configured: &BrowserSettings,
        module: ModuleKind,
        enabled: bool,
    ) -> Result<ModuleOverrides, BoundaryError> {
        let mut next = read_overrides(&self.connection)?;
        next.set(module, Some(enabled));
        if enabled {
            let candidate = next.apply(configured);
            candidate.validate()?;
        }

        let updated_at = unix_timestamp_secs();
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|_| storage_error("cannot start module update transaction"))?;
        tx.execute(
            "INSERT INTO module_states (name, enabled, updated_at)
             VALUES (?1, ?2, ?3)
             ON CONFLICT(name) DO UPDATE SET
                 enabled = excluded.enabled,
                 updated_at = excluded.updated_at",
            params![module.as_str(), i64::from(enabled), updated_at],
        )
        .map_err(|_| storage_error("cannot persist module override"))?;
        tx.commit()
            .map_err(|_| storage_error("cannot commit module override"))?;

        // Keep WAL compacted so subsequent read-only inspections never need
        // write access to recover uncheckpointed frames.
        let _ = self
            .connection
            .execute_batch("PRAGMA wal_checkpoint(TRUNCATE);");
        Ok(next)
    }

    /// Read the currently persisted overrides using this open connection.
    pub fn overrides(&self) -> Result<ModuleOverrides, BoundaryError> {
        read_overrides(&self.connection)
    }
}

fn build_status(
    module: ModuleKind,
    configured: &BrowserSettings,
    effective: &BrowserSettings,
    overrides: ModuleOverrides,
) -> ModuleStatus {
    let configured_enabled = match module {
        ModuleKind::Browser => configured.enabled,
        ModuleKind::Vm => configured.vm.enabled,
    };
    let enabled = match module {
        ModuleKind::Browser => effective.enabled,
        ModuleKind::Vm => effective.vm.enabled,
    };
    let override_enabled = overrides.get(module);
    let source = if override_enabled.is_some() {
        ModuleStateSource::ModuleStore
    } else {
        ModuleStateSource::Config
    };

    let validation_issue = effective.validate().err().map(|err| err.0.message);
    let issue = match module {
        ModuleKind::Browser => validation_issue.or_else(|| {
            (effective.enabled && !effective.worker.enabled && !effective.vm.enabled).then(|| {
                "browser.enabled requires browser.worker.enabled or browser.vm.enabled".to_owned()
            })
        }),
        ModuleKind::Vm => {
            if effective.vm.enabled {
                validation_issue
            } else {
                None
            }
        }
    };

    let active = match module {
        ModuleKind::Browser => {
            effective.enabled
                && (effective.worker.enabled || effective.vm.enabled)
                && issue.is_none()
        }
        ModuleKind::Vm => effective.enabled && effective.vm.enabled && issue.is_none(),
    };

    ModuleStatus {
        module,
        description: module.description().to_owned(),
        enabled,
        configured_enabled,
        override_enabled,
        source,
        active,
        issue,
    }
}

/// Whether the module database file is absent, present but empty, or has
/// content. A zero-byte file is a recoverable interrupted creation, not
/// corruption: readers see default overrides and writers complete the schema.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DbPresence {
    Missing,
    Empty,
    Present,
}

fn existing_db_presence(db_path: &Path) -> Result<DbPresence, BoundaryError> {
    let Some((_file, meta)) = soyo_fs::open_existing_private_file_read(
        db_path,
        error_codes::STORAGE_ERROR,
        "module database",
    )?
    else {
        return Ok(DbPresence::Missing);
    };
    for suffix in ["-wal", "-shm"] {
        let sidecar = PathBuf::from(format!("{}{suffix}", db_path.display()));
        let _ = soyo_fs::open_existing_private_file_read(
            &sidecar,
            error_codes::STORAGE_ERROR,
            "module database sidecar",
        )?;
    }
    Ok(if meta.len() == 0 {
        DbPresence::Empty
    } else {
        DbPresence::Present
    })
}

/// Schema state of an opened module database. `Incomplete` covers an empty
/// file or one whose headers/table were never committed; writers may finish
/// initialization while readers treat it as holding no overrides.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SchemaState {
    Ready,
    Incomplete,
    Foreign,
}

fn schema_state(connection: &Connection) -> Result<SchemaState, BoundaryError> {
    let app_id: i64 = connection
        .pragma_query_value(None, "application_id", |row| row.get(0))
        .map_err(|_| storage_error("cannot read module database application_id"))?;
    let version: i64 = connection
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .map_err(|_| storage_error("cannot read module database user_version"))?;
    let table: Option<String> = connection
        .query_row(
            "SELECT name FROM sqlite_master WHERE type = 'table' AND name = 'module_states'",
            [],
            |row| row.get(0),
        )
        .optional()
        .map_err(|_| storage_error("cannot inspect module database tables"))?;
    Ok(match (app_id, version, table.is_some()) {
        (APPLICATION_ID, SCHEMA_VERSION, true) => SchemaState::Ready,
        // Recognized headers without the table, or a completely untouched
        // SQLite file, can be completed idempotently. Any other combination
        // belongs to another schema or a corrupted file and fails closed.
        (APPLICATION_ID, SCHEMA_VERSION, false) | (0, 0, false) => SchemaState::Incomplete,
        _ => SchemaState::Foreign,
    })
}

fn initialize_schema(connection: &Connection) -> Result<(), BoundaryError> {
    connection
        .execute_batch(&format!(
            "BEGIN IMMEDIATE;
             CREATE TABLE IF NOT EXISTS module_states (
                 name TEXT PRIMARY KEY,
                 enabled INTEGER NOT NULL CHECK (enabled IN (0, 1)),
                 updated_at INTEGER NOT NULL
             ) STRICT;
             PRAGMA application_id = {APPLICATION_ID};
             PRAGMA user_version = {SCHEMA_VERSION};
             COMMIT;"
        ))
        .map_err(|_| storage_error("cannot initialize module database schema"))?;
    Ok(())
}

fn read_overrides(connection: &Connection) -> Result<ModuleOverrides, BoundaryError> {
    let mut stmt = connection
        .prepare("SELECT name, enabled FROM module_states")
        .map_err(|_| storage_error("cannot prepare module state query"))?;
    let mut rows = stmt
        .query([])
        .map_err(|_| storage_error("cannot query module states"))?;
    let mut overrides = ModuleOverrides::default();
    while let Some(row) = rows
        .next()
        .map_err(|_| storage_error("cannot decode module state row"))?
    {
        let name: String = row
            .get(0)
            .map_err(|_| storage_error("invalid module name in store"))?;
        let raw_enabled: i64 = row
            .get(1)
            .map_err(|_| storage_error("invalid module state flag in store"))?;
        let enabled = match raw_enabled {
            0 => false,
            1 => true,
            _ => return Err(storage_error("corrupt module state value in store")),
        };
        let module = ModuleKind::parse(&name)
            .map_err(|_| storage_error("unknown module key persisted in module database"))?;
        overrides.set(module, Some(enabled));
    }
    Ok(overrides)
}

fn unix_timestamp_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|elapsed| i64::try_from(elapsed.as_secs()).ok())
        .unwrap_or(0)
}

fn storage_error(message: &str) -> BoundaryError {
    BoundaryError::new(error_codes::STORAGE_ERROR, message)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::browser::vm::BrowserVmSettings;
    use std::os::unix::fs::{PermissionsExt, symlink};
    use tempfile::tempdir;

    fn valid_vm_settings() -> BrowserVmSettings {
        BrowserVmSettings {
            enabled: false,
            colima: Some(PathBuf::from("/usr/local/bin/colima")),
            ssh: Some(PathBuf::from("/usr/bin/ssh")),
            profile: Some("soyo-browser".to_owned()),
            host: Some("127.0.0.1".to_owned()),
            user: Some("soyo".to_owned()),
            port: Some(2222),
            identity_file: Some(PathBuf::from("/var/lib/soyo/vm_id_ed25519")),
            known_hosts: Some(PathBuf::from("/var/lib/soyo/known_hosts")),
            node: Some("/opt/soyo/node".to_owned()),
            worker_root: Some("/opt/soyo/worker".to_owned()),
            chromium: Some("/opt/soyo/chromium".to_owned()),
            identity_manifest: Some("/opt/soyo/identity.json".to_owned()),
            identity_sha256: Some("a".repeat(64)),
            remote_temp_root: Some("/tmp/saya-runs".to_owned()),
            ..Default::default()
        }
    }

    fn configured_vm_capable_browser() -> BrowserSettings {
        let mut settings = BrowserSettings {
            vm: valid_vm_settings(),
            ..Default::default()
        };
        settings.network.enabled = true;
        settings.network.public_web = true;
        settings
    }

    #[test]
    fn load_overrides_does_not_create_files_when_missing() {
        let dir = tempdir().unwrap();
        let data_dir = dir.path().join("absent-data-dir");
        let overrides = load_overrides(&data_dir).unwrap();
        assert_eq!(overrides, ModuleOverrides::default());
        assert!(!data_dir.exists());
    }

    #[test]
    fn overrides_take_precedence_over_toml_and_unset_inherits_toml() {
        let dir = tempdir().unwrap();
        let mut configured = configured_vm_capable_browser();
        configured.enabled = true;
        configured.vm.enabled = true;

        let (initial, overrides) = resolve_browser_settings(dir.path(), &configured).unwrap();
        assert_eq!(overrides, ModuleOverrides::default());
        assert!(initial.enabled);
        assert!(initial.vm.enabled);

        let mut store = ModuleStore::open(dir.path()).unwrap();
        store.set(&configured, ModuleKind::Vm, false).unwrap();

        let (after_vm_off, overrides) = resolve_browser_settings(dir.path(), &configured).unwrap();
        assert_eq!(overrides.vm, Some(false));
        assert_eq!(overrides.browser, None);
        assert!(after_vm_off.enabled);
        assert!(!after_vm_off.vm.enabled);

        store.set(&configured, ModuleKind::Browser, false).unwrap();
        let (after_both_off, overrides) =
            resolve_browser_settings(dir.path(), &configured).unwrap();
        assert_eq!(overrides.browser, Some(false));
        assert_eq!(overrides.vm, Some(false));
        assert!(!after_both_off.enabled);
        assert!(!after_both_off.vm.enabled);
    }

    #[test]
    fn enabling_vm_preflight_validates_d066_and_vm_prerequisites() {
        let dir = tempdir().unwrap();
        let incomplete = BrowserSettings {
            enabled: true,
            ..Default::default()
        };
        let mut store = ModuleStore::open(dir.path()).unwrap();

        let err = store.set(&incomplete, ModuleKind::Vm, true).unwrap_err();
        assert_eq!(err.0.code, error_codes::INVALID_CONFIG);
        assert_eq!(load_overrides(dir.path()).unwrap().vm, None);

        let mut worker_enabled = configured_vm_capable_browser();
        worker_enabled.enabled = true;
        worker_enabled.worker.enabled = true;
        worker_enabled.worker.node = Some(PathBuf::from("/opt/soyo/node"));
        worker_enabled.worker.worker_root = Some(PathBuf::from("/opt/soyo/worker"));
        worker_enabled.worker.chromium = Some(PathBuf::from("/opt/soyo/chromium"));
        worker_enabled.worker.identity_manifest = Some(PathBuf::from("/opt/soyo/identity.json"));
        worker_enabled.worker.identity_sha256 = Some("b".repeat(64));
        let err = store
            .set(&worker_enabled, ModuleKind::Vm, true)
            .unwrap_err();
        assert_eq!(err.0.code, error_codes::INVALID_CONFIG);
        assert!(
            err.0
                .message
                .contains("native worker or the VM backend, not both")
        );
        assert_eq!(load_overrides(dir.path()).unwrap().vm, None);
    }

    #[test]
    fn statuses_report_source_active_and_missing_backend_issue() {
        let dir = tempdir().unwrap();
        let configured = BrowserSettings::default();
        let mut store = ModuleStore::open(dir.path()).unwrap();
        store.set(&configured, ModuleKind::Browser, true).unwrap();

        let all = statuses(dir.path(), &configured).unwrap();
        assert_eq!(all.len(), 2);
        let browser = &all[0];
        assert_eq!(browser.module, ModuleKind::Browser);
        assert!(browser.enabled);
        assert!(!browser.configured_enabled);
        assert_eq!(browser.override_enabled, Some(true));
        assert_eq!(browser.source, ModuleStateSource::ModuleStore);
        assert!(!browser.active);
        assert!(browser.issue.is_some());

        let vm = &all[1];
        assert_eq!(vm.module, ModuleKind::Vm);
        assert!(!vm.enabled);
        assert_eq!(vm.source, ModuleStateSource::Config);
        assert!(!vm.active);
        assert!(vm.issue.is_none());
    }

    #[test]
    fn reject_symlink_and_unsafe_permissions_fail_closed() {
        let dir = tempdir().unwrap();
        let target = dir.path().join("real-modules");
        fs::create_dir(&target).unwrap();
        symlink(&target, dir.path().join(MODULE_DIR_NAME)).unwrap();
        let err = load_overrides(dir.path()).unwrap_err();
        assert_eq!(err.0.code, error_codes::STORAGE_ERROR);

        let dir2 = tempdir().unwrap();
        {
            let mut store = ModuleStore::open(dir2.path()).unwrap();
            let cfg = configured_vm_capable_browser();
            store.set(&cfg, ModuleKind::Browser, true).unwrap();
        }
        let db = module_db_path(dir2.path());
        fs::set_permissions(&db, fs::Permissions::from_mode(0o644)).unwrap();
        let err = load_overrides(dir2.path()).unwrap_err();
        assert_eq!(err.0.code, error_codes::STORAGE_ERROR);
    }

    fn make_private_db_file(dir: &tempfile::TempDir) -> PathBuf {
        let db = module_db_path(dir.path());
        let parent = db.parent().unwrap();
        fs::create_dir_all(parent).unwrap();
        fs::set_permissions(parent, fs::Permissions::from_mode(0o700)).unwrap();
        db
    }

    #[test]
    fn empty_database_self_heals_and_reads_default() {
        let dir = tempdir().unwrap();
        let db = make_private_db_file(&dir);
        fs::File::create(&db).unwrap();
        fs::set_permissions(&db, fs::Permissions::from_mode(0o600)).unwrap();
        assert_eq!(
            load_overrides(dir.path()).unwrap(),
            ModuleOverrides::default()
        );
        {
            let mut store = ModuleStore::open(dir.path()).unwrap();
            let configured = configured_vm_capable_browser();
            store.set(&configured, ModuleKind::Browser, true).unwrap();
        }
        assert_eq!(load_overrides(dir.path()).unwrap().browser, Some(true));
    }

    #[test]
    fn uninitialized_sqlite_database_self_heals() {
        let dir = tempdir().unwrap();
        let db = make_private_db_file(&dir);
        // Simulate an interrupted first open: a valid SQLite file whose
        // schema was never committed (headers and table both absent).
        let connection = Connection::open(&db).unwrap();
        connection
            .execute_batch("CREATE TABLE scratch (id INTEGER); DROP TABLE scratch;")
            .unwrap();
        drop(connection);
        fs::set_permissions(&db, fs::Permissions::from_mode(0o600)).unwrap();
        assert!(db.metadata().unwrap().len() > 0);
        assert_eq!(
            load_overrides(dir.path()).unwrap(),
            ModuleOverrides::default()
        );
        {
            let mut store =
                ModuleStore::open(dir.path()).expect("uninitialized database should self-heal");
            let configured = BrowserSettings::default();
            store.set(&configured, ModuleKind::Browser, true).unwrap();
        }
        assert_eq!(load_overrides(dir.path()).unwrap().browser, Some(true));
    }

    #[test]
    fn foreign_database_still_fails_closed() {
        let dir = tempdir().unwrap();
        let db = make_private_db_file(&dir);
        let connection = Connection::open(&db).unwrap();
        connection
            .execute_batch("CREATE TABLE other (id INTEGER); PRAGMA application_id = 42;")
            .unwrap();
        drop(connection);
        fs::set_permissions(&db, fs::Permissions::from_mode(0o600)).unwrap();
        assert_eq!(
            load_overrides(dir.path()).unwrap_err().0.code,
            error_codes::STORAGE_ERROR
        );
        assert_eq!(
            ModuleStore::open(dir.path()).err().unwrap().0.code,
            error_codes::STORAGE_ERROR
        );
    }
}
