use super::*;
use crate::plugin_manifest::{
    Dependency, MANIFEST_NAME, Manifest, PackageSource, SourceKind, VerifiedPackage,
    inspect_package, parse_manifest,
};
use crate::plugin_store::{InstanceState, StoreLimits};
use std::{
    fs,
    os::unix::fs::{PermissionsExt, symlink},
};
use uuid::Uuid;

const SIMPLE: &str = r#"schema_version = 1
[plugin]
id = "io.saya.runtime"
version = "1.0.0"
description = "A controlled runtime fixture."
license = "MIT"
[compatibility]
requires_saya = ">=0.2.0, <1.0.0"
requires_features = ["plugin-manifest-v1"]
[capabilities]
requested = ["workspace.read"]
[[resources]]
id = "guide"
path = "resources/guide.md"
media_type = "text/markdown"
"#;

fn features() -> BTreeSet<String> {
    BTreeSet::from([
        "plugin-manifest-v1".into(),
        "mcp-stdio-v1".into(),
        "mcp-http-v1".into(),
        "hook-events-v1".into(),
    ])
}
fn source() -> PackageSource {
    PackageSource {
        kind: SourceKind::LocalDirectory,
        reference: "./fixture".into(),
    }
}
fn stdio_manifest() -> Manifest {
    let mut manifest = parse_manifest(SIMPLE).unwrap();
    manifest
        .compatibility
        .requires_features
        .push("mcp-stdio-v1".into());
    manifest
        .capabilities
        .requested
        .extend(["mcp.stdio".into(), "process.exec".into()]);
    manifest.mcp.push(McpEntry::Stdio {
        id: "local".into(),
        program: "fixture".into(),
        args: vec!["@plugin/resources/guide.md".into()],
        credential_slots: vec![],
    });
    manifest
}
fn http_manifest() -> Manifest {
    let mut manifest = parse_manifest(SIMPLE).unwrap();
    manifest
        .compatibility
        .requires_features
        .push("mcp-http-v1".into());
    manifest.capabilities.requested.extend([
        "mcp.http".into(),
        "network.outbound".into(),
        "credential.use".into(),
    ]);
    manifest
        .capabilities
        .network_origins
        .push("https://mcp.example.test".into());
    manifest
        .capabilities
        .credential_slots
        .push("account".into());
    manifest.mcp.push(McpEntry::Http {
        id: "remote".into(),
        url: "https://mcp.example.test/v1".into(),
        credential_slots: vec!["account".into()],
    });
    manifest
}
fn package(manifest: &Manifest, body: &str) -> (tempfile::TempDir, VerifiedPackage) {
    let directory = tempfile::tempdir().unwrap();
    fs::create_dir(directory.path().join("resources")).unwrap();
    fs::write(
        directory.path().join(MANIFEST_NAME),
        toml::to_string(manifest).unwrap(),
    )
    .unwrap();
    fs::write(directory.path().join("resources/guide.md"), body).unwrap();
    let package = inspect_package(directory.path()).unwrap();
    (directory, package)
}
fn writable(path: &Path) {
    let Ok(metadata) = fs::symlink_metadata(path) else {
        return;
    };
    if metadata.is_dir() {
        let _ = fs::set_permissions(path, fs::Permissions::from_mode(0o700));
        if let Ok(entries) = fs::read_dir(path) {
            for entry in entries.flatten() {
                writable(&entry.path());
            }
        }
    }
}
struct Fixture {
    root: tempfile::TempDir,
    store: PluginStore,
    context: PluginRunContext,
    bindings: PluginHostBindings,
    pool: Arc<ViewPool>,
}
impl Fixture {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let workspace = root.path().join("workspace");
        fs::create_dir(&workspace).unwrap();
        let secrets = Arc::new(SecretRegistry::default());
        let store = PluginStore::open(
            root.path().join("store"),
            secrets.clone(),
            StoreLimits::default(),
        )
        .unwrap();
        let pool =
            Arc::new(ViewPool::open(&root.path().join("views"), ViewLimits::default()).unwrap());
        let context = PluginRunContext {
            workspace: fs::canonicalize(workspace).unwrap(),
            session_id: SessionId(Uuid::now_v7()),
            run_id: RunId(Uuid::now_v7()),
            host_version: "0.2.0".into(),
            features: features(),
            native_isolation_available: true,
            protected_roots: vec![store.protected_path().into()],
            host_write_roots: vec![],
            environment: Environment::from_pairs([
                ("HOST_HOME".into(), "/host/private/home".into()),
                ("HOST_UNRELATED".into(), "unrelated-private-fixture".into()),
                (
                    "TEST_ACCOUNT".into(),
                    "fixture-account-credential-value".into(),
                ),
                ("TEST_LANG".into(), "C.UTF-8".into()),
            ]),
            secrets,
        };
        let bindings = PluginHostBindings {
            runtimes: BTreeMap::from([(
                "fixture".into(),
                ApprovedRuntime {
                    program: fs::canonicalize("/bin/cat").unwrap(),
                    ..Default::default()
                },
            )]),
            ..Default::default()
        };
        Self {
            root,
            store,
            context,
            bindings,
            pool,
        }
    }
    fn install(&self, package: &VerifiedPackage) {
        self.store.install(package, source()).unwrap();
    }
    fn enable(&self, package: &VerifiedPackage, grant: Option<EnableGrant>) {
        self.store
            .enable(
                package.digest(),
                PluginScope::workspace(&self.context.workspace).unwrap(),
                grant.unwrap_or_else(|| EnableGrant::requested(package.manifest())),
                "0.2.0",
                &features(),
            )
            .unwrap();
    }
    fn prepare(&self) -> ActivePlugins {
        ActivePlugins::prepare(
            &self.store,
            &self.context,
            &self.bindings,
            self.pool.clone(),
            &AtomicBool::new(false),
        )
        .unwrap()
    }
    fn bind_environment(&mut self, package: &VerifiedPackage) {
        self.bindings.slots.insert(
            slot_key(&package.manifest().plugin.id, package.digest(), "account"),
            CredentialSlotBinding::Environment {
                destination: "SERVICE_TOKEN".into(),
                reference: "TEST_ACCOUNT".into(),
            },
        );
    }
    fn bind_bearer(&mut self, package: &VerifiedPackage, audience: &str) {
        self.bindings.slots.insert(
            slot_key(&package.manifest().plugin.id, package.digest(), "account"),
            CredentialSlotBinding::HttpBearer {
                reference: "TEST_ACCOUNT".into(),
                audience: audience.into(),
            },
        );
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        writable(self.root.path());
    }
}
fn stopped(active: &ActivePlugins) -> BTreeMap<String, StopEvidence> {
    active
        .servers()
        .keys()
        .chain(active.hooks().keys())
        .map(|id| (id.clone(), StopEvidence::Stopped))
        .collect()
}

#[test]
fn disabled_installation_does_not_acquire_or_materialize() {
    let fixture = Fixture::new();
    let (_source, package) = package(&stdio_manifest(), "sealed bytes");
    fixture.install(&package);
    let active = fixture.prepare();
    assert!(active.servers().is_empty());
    assert!(active.skill_leases().is_empty());
    assert!(fixture.pool.inventory().unwrap().is_empty());
    assert!(fixture.store.recover().unwrap().is_empty());
}

#[test]
fn stdio_uses_sealed_readonly_view_and_a_separate_private_scratch() {
    let fixture = Fixture::new();
    let (source, package) = package(&stdio_manifest(), "sealed bytes");
    fixture.install(&package);
    fixture.enable(&package, None);
    fs::write(
        source.path().join("resources/guide.md"),
        "mutable replacement",
    )
    .unwrap();
    drop(source);
    let mut active = fixture.prepare();
    assert!(active.diagnostics().is_empty());
    let server = active.servers().values().next().unwrap();
    let policy = server.process.as_ref().unwrap();
    let McpTransportSettings::Stdio(stdio) = &server.settings.transport else {
        panic!("expected stdio");
    };
    assert_eq!(fs::read(&stdio.argv[1]).unwrap(), b"sealed bytes");
    assert_eq!(
        stdio.argv[0],
        fs::canonicalize("/bin/cat").unwrap().to_str().unwrap()
    );
    assert!(!policy.inherit_host_environment);
    assert!(policy.native_isolation_required);
    assert_eq!(
        policy.write_roots,
        vec![policy.package_view.scratch.clone()]
    );
    assert!(policy.read_roots.contains(&fixture.context.workspace));
    assert!(
        !policy
            .read_roots
            .contains(&fixture.store.protected_path().to_owned())
    );
    assert_eq!(server.ceiling.workspace_access, WorkspaceAccess::ReadOnly);
    assert_eq!(
        fs::metadata(policy.package_view.package.join("resources/guide.md"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o400
    );
    assert!(!policy.package_view.package.join("index.json").exists());
    assert!(server.environment().get_os("HOST_HOME").is_none());
    assert!(server.environment().get_os("HOST_UNRELATED").is_none());
    active.check_entry(&server.server_id).unwrap();
    let root = policy.package_view.root.clone();
    let report = active
        .cleanup(&stopped(&active), &AtomicBool::new(false))
        .unwrap();
    assert_eq!(report.cleaned_leases.len(), 1);
    assert!(!root.exists());
    assert!(fixture.pool.inventory().unwrap().is_empty());
}

#[test]
fn process_credentials_require_exact_slot_and_filtered_references() {
    let mut fixture = Fixture::new();
    let mut manifest = stdio_manifest();
    manifest
        .capabilities
        .requested
        .push("credential.use".into());
    manifest
        .capabilities
        .credential_slots
        .push("account".into());
    let McpEntry::Stdio {
        credential_slots, ..
    } = &mut manifest.mcp[0]
    else {
        unreachable!()
    };
    credential_slots.push("account".into());
    let (_source, package) = package(&manifest, "credential fixture");
    fixture.install(&package);
    fixture.enable(&package, None);
    let mut missing = fixture.prepare();
    assert!(missing.servers().is_empty());
    assert_eq!(missing.diagnostics()[0].code, "plugin_credential_missing");
    missing
        .cleanup(&BTreeMap::new(), &AtomicBool::new(false))
        .unwrap();
    fixture.bind_environment(&package);
    fixture
        .bindings
        .runtimes
        .get_mut("fixture")
        .unwrap()
        .env
        .insert("LANG".into(), "TEST_LANG".into());
    let active = fixture.prepare();
    let server = active.servers().values().next().unwrap();
    let McpTransportSettings::Stdio(stdio) = &server.settings.transport else {
        unreachable!()
    };
    assert_eq!(stdio.env["SERVICE_TOKEN"], "TEST_ACCOUNT");
    assert_eq!(stdio.env["LANG"], "TEST_LANG");
    assert_eq!(stdio.env["HOME"], stdio.env["TMPDIR"]);
    assert_eq!(
        server.environment().get_os("TEST_ACCOUNT").unwrap(),
        "fixture-account-credential-value"
    );
    assert!(server.environment().get_os("HOST_UNRELATED").is_none());
    assert!(
        fixture
            .context
            .secrets
            .snapshot()
            .unwrap()
            .contains(&"fixture-account-credential-value".into())
    );
    assert!(!format!("{server:?}").contains("fixture-account-credential-value"));
}

#[test]
fn native_isolation_missing_or_runtime_missing_never_falls_back_to_host() {
    let mut fixture = Fixture::new();
    let (_source, package) = package(&stdio_manifest(), "fixture");
    fixture.install(&package);
    fixture.enable(&package, None);
    fixture.context.native_isolation_available = false;
    let mut missing = fixture.prepare();
    assert!(missing.servers().is_empty());
    assert_eq!(
        missing.diagnostics()[0].code,
        "plugin_isolation_unavailable"
    );
    assert_eq!(missing.skill_leases().len(), 1);
    assert!(fixture.pool.inventory().unwrap().is_empty());
    missing
        .cleanup(&BTreeMap::new(), &AtomicBool::new(false))
        .unwrap();
    fixture.context.native_isolation_available = true;
    fixture.bindings.runtimes.clear();
    let active = fixture.prepare();
    assert!(active.servers().is_empty());
    assert_eq!(active.diagnostics()[0].code, "plugin_runtime_missing");
    assert!(fixture.pool.inventory().unwrap().is_empty());
}

#[test]
fn effective_workspace_ceiling_and_bootstrap_roots_never_widen_each_other() {
    let mut fixture = Fixture::new();
    let mut manifest = stdio_manifest();
    manifest
        .capabilities
        .requested
        .push("workspace.write".into());
    let (_source, package) = package(&manifest, "fixture");
    fixture.install(&package);
    let mut grant = EnableGrant::requested(package.manifest());
    grant.capabilities.remove("workspace.read");
    fixture.enable(&package, Some(grant));
    let mut active = fixture.prepare();
    assert_eq!(active.diagnostics()[0].code, "plugin_runtime_scope");
    active
        .cleanup(&BTreeMap::new(), &AtomicBool::new(false))
        .unwrap();
    fixture.enable(&package, None);
    fixture
        .bindings
        .runtimes
        .get_mut("fixture")
        .unwrap()
        .read_roots
        .push(fixture.context.workspace.clone());
    let mut active = fixture.prepare();
    assert!(active.servers().is_empty());
    assert_eq!(active.diagnostics()[0].code, "plugin_runtime_scope");
    active
        .cleanup(&BTreeMap::new(), &AtomicBool::new(false))
        .unwrap();
    fixture
        .bindings
        .runtimes
        .get_mut("fixture")
        .unwrap()
        .read_roots
        .clear();
    let active = fixture.prepare();
    let process = active
        .servers()
        .values()
        .next()
        .unwrap()
        .process
        .as_ref()
        .unwrap();
    assert!(process.write_roots.contains(&fixture.context.workspace));
}

#[test]
fn http_binding_is_bound_to_origin_exact_audience_and_exact_digest() {
    let mut fixture = Fixture::new();
    let (_source, package) = package(&http_manifest(), "fixture");
    fixture.install(&package);
    fixture.enable(&package, None);
    fixture.bind_bearer(&package, "https://mcp.example.test/other");
    let mut denied = fixture.prepare();
    assert!(denied.servers().is_empty());
    assert_eq!(denied.diagnostics()[0].code, "plugin_credential_scope");
    denied
        .cleanup(&BTreeMap::new(), &AtomicBool::new(false))
        .unwrap();
    fixture.bind_bearer(&package, "https://mcp.example.test/v1");
    let active = fixture.prepare();
    let server = active.servers().values().next().unwrap();
    assert!(server.process.is_none());
    assert!(fixture.pool.inventory().unwrap().is_empty());
    let McpTransportSettings::Http(http) = &server.settings.transport else {
        unreachable!()
    };
    assert_eq!(http.bearer_env.as_deref(), Some("TEST_ACCOUNT"));
    assert!(!http.allow_insecure_http);
    assert!(
        server
            .ceiling
            .network_origins
            .contains("https://mcp.example.test")
    );
    assert!(server.ceiling.credential_slots.contains("account"));
    assert!(server.settings.read_only_tools.is_empty());
    assert_eq!(server.environment().iter().count(), 1);
}

#[test]
fn reduced_origin_or_slot_grant_denies_http_without_materialization() {
    let mut fixture = Fixture::new();
    let (_source, package) = package(&http_manifest(), "fixture");
    fixture.install(&package);
    fixture.bind_bearer(&package, "https://mcp.example.test/v1");
    let mut grant = EnableGrant::requested(package.manifest());
    grant.network_origins.clear();
    fixture.enable(&package, Some(grant));
    let mut active = fixture.prepare();
    assert_eq!(active.diagnostics()[0].code, "plugin_network_scope");
    active
        .cleanup(&BTreeMap::new(), &AtomicBool::new(false))
        .unwrap();
    let mut grant = EnableGrant::requested(package.manifest());
    grant.credential_slots.clear();
    fixture.enable(&package, Some(grant));
    let active = fixture.prepare();
    assert_eq!(active.diagnostics()[0].code, "plugin_credential_scope");
    assert!(fixture.pool.inventory().unwrap().is_empty());
}

#[test]
fn service_ids_and_model_aliases_are_version_bound_and_readonly_tools_are_host_only() {
    let mut fixture = Fixture::new();
    let (_source, first) = package(&stdio_manifest(), "first");
    let (_source2, second) = package(&stdio_manifest(), "second");
    fixture.install(&first);
    fixture.enable(&first, None);
    let first_identity = PluginEntryIdentity {
        plugin_id: first.manifest().plugin.id.clone(),
        package_digest: first.digest().into(),
        entry_id: "local".into(),
    };
    fixture.bindings.entries.insert(
        first_identity.key(),
        HostEntrySettings {
            read_only_tools: BTreeSet::from(["lookup".into()]),
            ..Default::default()
        },
    );
    let active = fixture.prepare();
    let server = active.servers().values().next().unwrap();
    assert!(server.settings.read_only_tools.contains("lookup"));
    assert_eq!(server.server_id, first_identity.server_id());
    assert_eq!(
        server.model_alias("lookup").unwrap(),
        crate::registry::mcp_model_name(&server.server_id, "lookup").unwrap()
    );
    fixture
        .store
        .update(first.digest(), &second, source())
        .unwrap();
    fixture.enable(&second, None);
    let active2 = fixture.prepare();
    let second_server = active2.servers().values().next().unwrap();
    assert_ne!(server.server_id, second_server.server_id);
    assert!(second_server.settings.read_only_tools.is_empty());
    active.check_entry(&server.server_id).unwrap();
}

#[test]
fn explicit_dependency_disable_revokes_entries_without_claiming_process_stop() {
    let fixture = Fixture::new();
    let mut dependency = parse_manifest(SIMPLE).unwrap();
    dependency.plugin.id = "io.saya.dependency".into();
    let (_source, dependency) = package(&dependency, "dep");
    fixture.install(&dependency);
    let mut manifest = stdio_manifest();
    manifest.dependencies.push(Dependency {
        id: "io.saya.dependency".into(),
        version: "^1.0".into(),
    });
    let (_source, package) = package(&manifest, "plugin");
    fixture.install(&package);
    fixture.enable(&package, None);
    let mut active = fixture.prepare();
    let server = active.servers().values().next().unwrap().clone();
    let view = server.process.as_ref().unwrap().package_view.root.clone();
    fixture
        .store
        .disable(
            "io.saya.dependency",
            Some(&PluginScope::workspace(&fixture.context.workspace).unwrap()),
        )
        .unwrap();
    assert!(active.check_entry(&server.server_id).is_err());
    let revoked = active.reconcile().unwrap();
    assert_eq!(revoked.revoked.len(), 1);
    assert_eq!(
        revoked.revoked[0].server_ids,
        vec![server.server_id.clone()]
    );
    assert!(view.exists());
    assert!(active.servers().is_empty());
    let pending = active
        .cleanup(&BTreeMap::new(), &AtomicBool::new(false))
        .unwrap();
    assert_eq!(pending.cleanup_pending.len(), 1);
    assert!(view.exists());
    let report = active
        .cleanup(
            &BTreeMap::from([(server.server_id, StopEvidence::Stopped)]),
            &AtomicBool::new(false),
        )
        .unwrap();
    assert_eq!(report.cleaned_leases.len(), 1);
    assert!(!view.exists());
}

#[test]
fn unknown_stop_and_external_skill_owners_preserve_views_and_pins() {
    let fixture = Fixture::new();
    let (_source, package) = package(&stdio_manifest(), "fixture");
    fixture.install(&package);
    fixture.enable(&package, None);
    let mut active = fixture.prepare();
    let extra = active.skill_leases();
    let root = active
        .servers()
        .values()
        .next()
        .unwrap()
        .process
        .as_ref()
        .unwrap()
        .package_view
        .root
        .clone();
    let pending = active
        .cleanup(&stopped(&active), &AtomicBool::new(false))
        .unwrap();
    assert_eq!(pending.cleanup_pending.len(), 1);
    drop(extra);
    let ids: Vec<_> = active.servers().keys().cloned().collect();
    let pending = active
        .cleanup(
            &BTreeMap::from([(ids[0].clone(), StopEvidence::Unknown)]),
            &AtomicBool::new(false),
        )
        .unwrap();
    assert_eq!(pending.cleanup_pending.len(), 1);
    assert!(root.exists());
    let report = active
        .cleanup(&stopped(&active), &AtomicBool::new(false))
        .unwrap();
    assert_eq!(report.cleaned_leases.len(), 1);
    assert!(!root.exists());
}

#[test]
fn drop_and_recovery_keep_unknown_views_until_explicit_cleanup_evidence() {
    let fixture = Fixture::new();
    let (_source, package) = package(&stdio_manifest(), "fixture");
    fixture.install(&package);
    fixture.enable(&package, None);
    let active = fixture.prepare();
    let inventory = fixture.pool.inventory().unwrap();
    assert_eq!(inventory.len(), 1);
    drop(active);
    let leases = fixture.store.recover().unwrap();
    assert!(
        leases
            .iter()
            .any(|lease| lease.state == InstanceState::Unknown)
    );
    assert!(
        fixture
            .pool
            .remove_recovered(
                &inventory[0].identity,
                StopEvidence::Unknown,
                &AtomicBool::new(false)
            )
            .is_err()
    );
    assert!(inventory[0].identity.root.exists());
    fixture
        .pool
        .remove_recovered(
            &inventory[0].identity,
            StopEvidence::Stopped,
            &AtomicBool::new(false),
        )
        .unwrap();
    fixture
        .store
        .confirm_cleanup(&inventory[0].identity.lease_id)
        .unwrap();
    assert!(!inventory[0].identity.root.exists());
}

#[test]
fn materialized_byte_or_mode_changes_close_entry_dispatch() {
    let fixture = Fixture::new();
    let (_source, package) = package(&stdio_manifest(), "fixture");
    fixture.install(&package);
    fixture.enable(&package, None);
    let mut active = fixture.prepare();
    let server = active.servers().values().next().unwrap().clone();
    let file = server
        .process
        .as_ref()
        .unwrap()
        .package_view
        .package
        .join("resources/guide.md");
    fs::set_permissions(&file, fs::Permissions::from_mode(0o600)).unwrap();
    assert!(active.check_entry(&server.server_id).is_err());
    fs::write(&file, "changed").unwrap();
    fs::set_permissions(&file, fs::Permissions::from_mode(0o400)).unwrap();
    assert!(active.check_entry(&server.server_id).is_err());
    assert_eq!(active.reconcile().unwrap().revoked.len(), 1);
}

#[test]
fn view_location_rejects_workspace_protected_write_roots_and_unicode_aliases() {
    let fixture = Fixture::new();
    assert!(
        fixture
            .pool
            .validate_location(
                &fixture.context.workspace,
                &[],
                &[fixture.root.path().to_path_buf()]
            )
            .is_err()
    );
    assert!(
        fixture
            .pool
            .validate_location(
                &fixture.context.workspace,
                &[fixture.pool.path().to_path_buf()],
                &[]
            )
            .is_err()
    );
    assert!(view::overlaps(Path::new("/"), Path::new("/tmp/anything")).unwrap());
    assert!(view::overlaps(Path::new("/café"), Path::new("/cafe\u{301}/child")).unwrap());
    assert!(!view::overlaps(Path::new("/a"), Path::new("/ab")).unwrap());
    let workspace_pool = ViewPool::open(
        &fixture.context.workspace.join("views"),
        ViewLimits::default(),
    )
    .unwrap();
    assert!(
        workspace_pool
            .validate_location(&fixture.context.workspace, &[], &[])
            .is_err()
    );
}

#[test]
fn view_pool_quotas_cancellation_and_root_replacement_fail_closed() {
    let fixture = Fixture::new();
    let (_source, package) = package(&stdio_manifest(), "fixture");
    fixture.install(&package);
    fixture.enable(&package, None);
    let pool = Arc::new(
        ViewPool::open(
            &fixture.root.path().join("limited-views"),
            ViewLimits {
                max_views: 1,
                ..Default::default()
            },
        )
        .unwrap(),
    );
    let first = ActivePlugins::prepare(
        &fixture.store,
        &fixture.context,
        &fixture.bindings,
        pool.clone(),
        &AtomicBool::new(false),
    )
    .unwrap();
    let second = ActivePlugins::prepare(
        &fixture.store,
        &fixture.context,
        &fixture.bindings,
        pool.clone(),
        &AtomicBool::new(false),
    )
    .unwrap();
    assert_eq!(second.diagnostics()[0].code, "plugin_view_limit");
    assert!(
        ActivePlugins::prepare(
            &fixture.store,
            &fixture.context,
            &fixture.bindings,
            pool.clone(),
            &AtomicBool::new(true)
        )
        .is_err()
    );
    assert_eq!(pool.inventory().unwrap().len(), 1);
    let original = fixture.root.path().join("renamed-views");
    fs::rename(pool.path(), &original).unwrap();
    fs::create_dir(pool.path()).unwrap();
    fs::set_permissions(pool.path(), fs::Permissions::from_mode(0o700)).unwrap();
    assert!(pool.inventory().is_err());
    assert!(
        first
            .check_entry(first.servers().keys().next().unwrap())
            .is_err()
    );
}

#[test]
fn cleanup_never_follows_scratch_links_and_requires_all_mcp_and_hook_stops() {
    let fixture = Fixture::new();
    let mut manifest = stdio_manifest();
    manifest
        .compatibility
        .requires_features
        .push("hook-events-v1".into());
    manifest.capabilities.requested.push("hook.observe".into());
    manifest.hooks.push(HookEntry {
        id: "audit".into(),
        event: "tool.finished".into(),
        kind: "observer".into(),
        program: "fixture".into(),
        args: vec!["@plugin/resources/guide.md".into()],
        timeout_ms: 500,
    });
    let (_source, package) = package(&manifest, "fixture");
    fixture.install(&package);
    fixture.enable(&package, None);
    let mut active = fixture.prepare();
    assert_eq!(active.hooks().len(), 1);
    let descriptor = active.servers().values().next().unwrap();
    let root = descriptor.process.as_ref().unwrap().package_view.clone();
    let untouched = fixture.root.path().join("untouched.txt");
    fs::write(&untouched, "outside").unwrap();
    symlink(&untouched, root.scratch.join("outside-link")).unwrap();
    let zero = root.scratch.join("mode-zero");
    fs::create_dir(&zero).unwrap();
    fs::write(zero.join("data"), "scratch").unwrap();
    fs::set_permissions(&zero, fs::Permissions::from_mode(0o0)).unwrap();
    let server_only = BTreeMap::from([(descriptor.server_id.clone(), StopEvidence::Stopped)]);
    assert_eq!(
        active
            .cleanup(&server_only, &AtomicBool::new(false))
            .unwrap()
            .cleanup_pending
            .len(),
        1
    );
    let report = active
        .cleanup(&stopped(&active), &AtomicBool::new(false))
        .unwrap();
    assert_eq!(report.cleaned_leases.len(), 1);
    assert_eq!(fs::read_to_string(untouched).unwrap(), "outside");
    assert!(!root.root.exists());
}

#[test]
fn server_identity_collision_in_config_and_secret_host_metadata_are_rejected() {
    let mut fixture = Fixture::new();
    let (_source, package) = package(&stdio_manifest(), "fixture");
    fixture.install(&package);
    fixture.enable(&package, None);
    let active = fixture.prepare();
    let server = active.servers().values().next().unwrap();
    let base = McpSettings {
        servers: BTreeMap::from([(server.server_id.clone(), server.settings.clone())]),
    };
    assert!(active.settings_for_registration(&base).is_err());
    fixture
        .context
        .secrets
        .register(&["private-binding-value".into()])
        .unwrap();
    fixture
        .bindings
        .runtimes
        .get_mut("fixture")
        .unwrap()
        .args_prefix
        .push("private-binding-value".into());
    let failure = ActivePlugins::prepare(
        &fixture.store,
        &fixture.context,
        &fixture.bindings,
        fixture.pool.clone(),
        &AtomicBool::new(false),
    )
    .unwrap_err();
    assert_eq!(failure.0.code, "sensitive_data");
    assert!(!failure.to_string().contains("private-binding-value"));
}

#[test]
fn failed_view_publication_preserves_orphan_identity_and_unknown_lease_pin() {
    let fixture = Fixture::new();
    let (_source, package) = package(&stdio_manifest(), "fixture");
    fixture.install(&package);
    fixture.enable(&package, None);
    fixture.pool.fail_publication.store(true, Ordering::Relaxed);
    let failure = ActivePlugins::prepare(
        &fixture.store,
        &fixture.context,
        &fixture.bindings,
        fixture.pool.clone(),
        &AtomicBool::new(false),
    )
    .unwrap_err();
    assert_eq!(failure.0.code, "plugin_view_unknown");
    let views = fixture.pool.inventory().unwrap();
    assert_eq!(views.len(), 1);
    let recovered = fixture.store.recover().unwrap();
    assert_eq!(recovered.len(), 1);
    assert_eq!(recovered[0].state, InstanceState::Unknown);
    assert_eq!(recovered[0].lease_id, views[0].identity.lease_id);
    fixture
        .pool
        .remove_recovered(
            &views[0].identity,
            StopEvidence::NeverStarted,
            &AtomicBool::new(false),
        )
        .unwrap();
    fixture
        .store
        .confirm_cleanup(&recovered[0].lease_id)
        .unwrap();
}

#[test]
fn workspace_replacement_invalidates_existing_run_entry_before_dispatch() {
    let fixture = Fixture::new();
    let (_source, package) = package(&stdio_manifest(), "fixture");
    fixture.install(&package);
    fixture.enable(&package, None);
    let mut active = fixture.prepare();
    let id = active.servers().keys().next().unwrap().clone();
    fs::rename(
        &fixture.context.workspace,
        fixture.root.path().join("old-workspace"),
    )
    .unwrap();
    fs::create_dir(&fixture.context.workspace).unwrap();
    assert!(active.check_entry(&id).is_err());
    assert_eq!(active.reconcile().unwrap().revoked.len(), 1);
}
