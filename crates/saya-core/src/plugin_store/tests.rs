use super::*;
use crate::plugin_manifest::{MANIFEST_NAME, SourceKind, inspect_package};
use std::{
    fs,
    os::unix::fs::{MetadataExt, PermissionsExt, symlink},
};

const SIMPLE: &str = r#"schema_version = 1
[plugin]
id = "io.saya.test"
version = "1.0.0"
description = "A private local package."
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

fn package(
    id: &str,
    body: &str,
    dependency: Option<(&str, &str)>,
) -> (tempfile::TempDir, VerifiedPackage) {
    let directory = tempfile::tempdir().unwrap();
    let mut manifest = SIMPLE.replace("io.saya.test", id);
    if let Some((id, range)) = dependency {
        manifest.push_str(&format!(
            "\n[[dependencies]]\nid = {id:?}\nversion = {range:?}\n"
        ));
    }
    fs::create_dir(directory.path().join("resources")).unwrap();
    fs::write(directory.path().join(MANIFEST_NAME), manifest).unwrap();
    fs::write(directory.path().join("resources/guide.md"), body).unwrap();
    let package = inspect_package(directory.path()).unwrap();
    (directory, package)
}
fn source() -> PackageSource {
    PackageSource {
        kind: SourceKind::LocalDirectory,
        reference: "./explicit-package".into(),
    }
}
fn features() -> BTreeSet<String> {
    BTreeSet::from(["plugin-manifest-v1".into()])
}
struct PrivateRoot(tempfile::TempDir);
impl std::ops::Deref for PrivateRoot {
    type Target = tempfile::TempDir;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}
impl Drop for PrivateRoot {
    fn drop(&mut self) {
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
        writable(self.0.path());
    }
}
fn private_root() -> PrivateRoot {
    PrivateRoot(tempfile::tempdir().unwrap())
}
fn store(root: &tempfile::TempDir) -> PluginStore {
    PluginStore::open(
        root.path().join("plugins"),
        Arc::new(SecretRegistry::default()),
        StoreLimits::default(),
    )
    .unwrap()
}
fn enable(store: &PluginStore, package: &VerifiedPackage, scope: PluginScope) {
    store
        .enable(
            package.digest(),
            scope,
            EnableGrant::requested(package.manifest()),
            "0.2.0",
            &features(),
        )
        .unwrap();
}
fn acquire(store: &PluginStore, workspace: &Path) -> PluginLease {
    store
        .acquire(
            "io.saya.test",
            workspace,
            SessionId(Uuid::now_v7()),
            RunId(Uuid::now_v7()),
            "0.2.0",
            &features(),
        )
        .unwrap()
}

#[test]
fn sealed_snapshot_installs_without_reopening_mutable_source_and_is_disabled() {
    let root = private_root();
    let store = store(&root);
    let (directory, package) = package("io.saya.test", "original bytes", None);
    fs::write(
        directory.path().join("resources/guide.md"),
        "changed after verification",
    )
    .unwrap();
    drop(directory);
    let staged = store.stage(&package, source()).unwrap();
    assert_eq!(staged.state, InstallationState::Staged);
    assert!(!staged.selected);
    assert!(staged.enabled_scopes.is_empty());
    let installed = store.install_staged(package.digest()).unwrap();
    assert_eq!(installed.state, InstallationState::Installed);
    assert!(installed.selected);
    assert!(
        store
            .acquire(
                "io.saya.test",
                root.path(),
                SessionId(Uuid::now_v7()),
                RunId(Uuid::now_v7()),
                "0.2.0",
                &features()
            )
            .is_err()
    );
    let object = store.inner.disk.load_package(package.digest()).unwrap();
    assert_eq!(
        object.file("resources/guide.md").unwrap().bytes(),
        b"original bytes"
    );
    let inspection = store.inspect(package.digest()).unwrap();
    assert_eq!(inspection.lock.unwrap().packages.len(), 1);
    assert_eq!(store.list().unwrap().len(), 1);
    assert_eq!(
        fs::metadata(store.protected_path()).unwrap().mode() & 0o777,
        0o700
    );
    assert_eq!(
        fs::metadata(store.inner.disk.package_path(package.digest()))
            .unwrap()
            .mode()
            & 0o777,
        0o500
    );
    assert_eq!(
        fs::metadata(
            store
                .inner
                .disk
                .package_path(package.digest())
                .join("resources/guide.md")
        )
        .unwrap()
        .mode()
            & 0o777,
        0o400
    );
    assert_eq!(
        fs::metadata(store.protected_path().join("index.json"))
            .unwrap()
            .mode()
            & 0o777,
        0o600
    );
}

#[test]
fn workspace_tombstone_overrides_global_enablement_and_stops_owned_global_lease() {
    let root = private_root();
    let store = store(&root);
    let (_source, package) = package("io.saya.test", "guide", None);
    store.install(&package, source()).unwrap();
    enable(&store, &package, PluginScope::User);
    let workspace = tempfile::tempdir().unwrap();
    let other = tempfile::tempdir().unwrap();
    let lease = acquire(&store, workspace.path());
    assert_eq!(lease.record().enable_scope, PluginScope::User);
    assert_eq!(
        lease.record().scope,
        PluginScope::workspace(workspace.path()).unwrap()
    );
    let scope = PluginScope::workspace(workspace.path()).unwrap();
    let changed = store.disable("io.saya.test", Some(&scope)).unwrap();
    assert_eq!(changed.cleanup_pending.len(), 1);
    assert_eq!(
        changed.cleanup_pending[0].state,
        InstanceState::StopRequested
    );
    assert!(lease.check_active().is_err());
    assert!(
        store
            .acquire(
                "io.saya.test",
                workspace.path(),
                SessionId(Uuid::now_v7()),
                RunId(Uuid::now_v7()),
                "0.2.0",
                &features()
            )
            .is_err()
    );
    acquire(&store, other.path())
        .finish(CleanupOutcome::Cleaned)
        .unwrap();
    lease.finish(CleanupOutcome::Cleaned).unwrap();
}

#[test]
fn enable_requires_actual_host_features_and_bounded_user_ceiling() {
    let root = private_root();
    let store = store(&root);
    let (_source, package) = package("io.saya.test", "guide", None);
    store.install(&package, source()).unwrap();
    let mut grant = EnableGrant::requested(package.manifest());
    assert!(
        store
            .enable(
                package.digest(),
                PluginScope::User,
                grant.clone(),
                "0.2.0",
                &BTreeSet::new()
            )
            .is_err()
    );
    grant.capabilities.insert("process.exec".into());
    assert!(
        store
            .enable(
                package.digest(),
                PluginScope::User,
                grant,
                "0.2.0",
                &features()
            )
            .is_err()
    );
    assert!(store.list().unwrap()[0].enabled_scopes.is_empty());
    store
        .enable(
            package.digest(),
            PluginScope::User,
            EnableGrant::default(),
            "0.2.0",
            &features(),
        )
        .unwrap();
    let lease = acquire(&store, root.path());
    assert!(lease.record().grant.capabilities.is_empty());
    lease.finish(CleanupOutcome::Cleaned).unwrap();
}

#[test]
fn update_pins_existing_run_and_requires_new_enablement_without_silent_transfer() {
    let root = private_root();
    let store = store(&root);
    let (_old_source, old) = package("io.saya.test", "old", None);
    let (_new_source, new) = package("io.saya.test", "new", None);
    store.install(&old, source()).unwrap();
    enable(&store, &old, PluginScope::User);
    let old_lease = acquire(&store, root.path());
    let selected = store.update(old.digest(), &new, source()).unwrap();
    assert!(selected.selected);
    assert!(selected.source_conflict);
    assert!(old_lease.check_active().is_ok());
    assert!(old_lease.verify_contents().is_ok());
    assert_eq!(
        old_lease
            .package()
            .file("resources/guide.md")
            .unwrap()
            .bytes(),
        b"old"
    );
    assert!(
        store
            .acquire(
                "io.saya.test",
                root.path(),
                SessionId(Uuid::now_v7()),
                RunId(Uuid::now_v7()),
                "0.2.0",
                &features()
            )
            .is_err()
    );
    assert!(store.update(old.digest(), &new, source()).is_err());
    enable(&store, &new, PluginScope::User);
    let new_lease = acquire(&store, root.path());
    assert_eq!(new_lease.record().digest, new.digest());
    assert_eq!(old_lease.record().digest, old.digest());
    new_lease.finish(CleanupOutcome::Cleaned).unwrap();
    old_lease.finish(CleanupOutcome::Cleaned).unwrap();
    assert!(store.uninstall(old.digest()).unwrap().removed);
    assert_eq!(store.list().unwrap().len(), 1);
}

#[test]
fn orphaned_lease_prevents_gc_until_explicit_cleanup_is_confirmed() {
    let root = private_root();
    let store = store(&root);
    let (_source, package) = package("io.saya.test", "guide", None);
    store.install(&package, source()).unwrap();
    enable(&store, &package, PluginScope::User);
    let lease = acquire(&store, root.path());
    let id = lease.record().lease_id.clone();
    let removal = store.uninstall(package.digest()).unwrap();
    assert!(!removal.removed);
    assert_eq!(
        removal.cleanup_pending[0].state,
        InstanceState::StopRequested
    );
    assert!(store.confirm_cleanup(&id).is_err());
    drop(lease);
    let pending = store.recover().unwrap();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].state, InstanceState::Unknown);
    assert!(store.inner.disk.package_path(package.digest()).exists());
    store.confirm_cleanup(&id).unwrap();
    assert!(store.recover().unwrap().is_empty());
    assert!(store.list().unwrap().is_empty());
    assert!(!store.inner.disk.package_path(package.digest()).exists());
}

#[test]
fn explicit_unknown_cleanup_remains_pinned_across_reopen() {
    let root = private_root();
    let store = store(&root);
    let (_source, package) = package("io.saya.test", "guide", None);
    store.install(&package, source()).unwrap();
    enable(&store, &package, PluginScope::User);
    let lease = acquire(&store, root.path());
    let id = lease.record().lease_id.clone();
    lease.finish(CleanupOutcome::Unknown).unwrap();
    let reopened = PluginStore::open(
        store.protected_path(),
        Arc::new(SecretRegistry::default()),
        StoreLimits::default(),
    )
    .unwrap();
    assert_eq!(reopened.recover().unwrap()[0].state, InstanceState::Unknown);
    assert!(!reopened.uninstall(package.digest()).unwrap().removed);
    reopened.confirm_cleanup(&id).unwrap();
    assert!(reopened.recover().unwrap().is_empty());
}

#[test]
fn dependency_resolution_uses_installed_pins_and_blocks_uninstall_until_dependents_leave() {
    let root = private_root();
    let store = store(&root);
    let (_parent_source, parent) = package("io.saya.test", "parent", Some(("io.saya.dep", "^1.0")));
    assert!(store.install(&parent, source()).is_err());
    let (_dependency_source, dependency) = package("io.saya.dep", "dependency", None);
    store.install(&dependency, source()).unwrap();
    store.install_staged(parent.digest()).unwrap();
    let inspection = store.inspect(parent.digest()).unwrap();
    assert_eq!(inspection.lock.as_ref().unwrap().packages.len(), 2);
    assert!(store.uninstall(dependency.digest()).is_err());
    enable(&store, &parent, PluginScope::User);
    let lease = acquire(&store, root.path());
    assert_eq!(lease.record().dependency_digests, [dependency.digest()]);
    assert!(!store.uninstall(parent.digest()).unwrap().removed);
    assert!(!store.uninstall(dependency.digest()).unwrap().removed);
    lease.finish(CleanupOutcome::Cleaned).unwrap();
    assert!(store.recover().unwrap().is_empty());
    assert!(store.list().unwrap().is_empty());
}

#[test]
fn tampered_content_and_lock_fail_before_new_lease_and_revoke_owned_dependents() {
    let root = private_root();
    let store = store(&root);
    let (_dependency_source, dependency) = package("io.saya.dep", "dependency", None);
    let (_parent_source, parent) = package("io.saya.test", "parent", Some(("io.saya.dep", "^1.0")));
    store.install(&dependency, source()).unwrap();
    store.install(&parent, source()).unwrap();
    enable(&store, &parent, PluginScope::User);
    let lease = acquire(&store, root.path());
    let path = store
        .inner
        .disk
        .package_path(dependency.digest())
        .join("resources/guide.md");
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
    fs::write(&path, "changed").unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o400)).unwrap();
    assert!(lease.verify_contents().is_err());
    assert!(
        store
            .acquire(
                "io.saya.test",
                root.path(),
                SessionId(Uuid::now_v7()),
                RunId(Uuid::now_v7()),
                "0.2.0",
                &features()
            )
            .is_err()
    );
    assert!(store.inspect(dependency.digest()).is_err());
    assert!(lease.check_active().is_err());
    assert_eq!(
        store
            .list()
            .unwrap()
            .iter()
            .find(|summary| summary.digest == dependency.digest())
            .unwrap()
            .state,
        InstallationState::Corrupt
    );
    lease.finish(CleanupOutcome::Cleaned).unwrap();
    let lock = store
        .protected_path()
        .join("objects")
        .join(parent.digest())
        .join("plugin.lock");
    fs::write(lock, "broken lock").unwrap();
    assert!(store.inspect(parent.digest()).is_err());
}

#[test]
fn post_rename_failure_reports_unknown_and_retry_observes_committed_identity() {
    let root = private_root();
    let store = store(&root);
    let (_source, package) = package("io.saya.test", "guide", None);
    store
        .inner
        .disk
        .fail_publication_sync
        .store(true, std::sync::atomic::Ordering::SeqCst);
    let failure = store.stage(&package, source()).unwrap_err();
    assert_eq!(failure.0.code, "plugin_commit_unknown");
    assert_eq!(store.list().unwrap().len(), 1);
    store.stage(&package, source()).unwrap();
    store.install_staged(package.digest()).unwrap();
    assert_eq!(store.list().unwrap().len(), 1);
}

#[test]
fn concurrent_handles_preserve_distinct_installations_without_lost_updates() {
    let root = private_root();
    let first = store(&root);
    let second = PluginStore::open(
        first.protected_path(),
        Arc::new(SecretRegistry::default()),
        StoreLimits::default(),
    )
    .unwrap();
    let (_a_source, a) = package("io.saya.first", "first", None);
    let (_b_source, b) = package("io.saya.second", "second", None);
    std::thread::scope(|scope| {
        let one = scope.spawn(|| first.install(&a, source()));
        let two = scope.spawn(|| second.install(&b, source()));
        one.join().unwrap().unwrap();
        two.join().unwrap().unwrap();
    });
    assert_eq!(first.list().unwrap().len(), 2);
    assert_eq!(second.list().unwrap().len(), 2);
}

#[test]
fn private_storage_rejects_linked_roots_indexes_objects_and_replaced_root_identity() {
    let root = private_root();
    let outside = tempfile::tempdir().unwrap();
    symlink(outside.path(), root.path().join("linked")).unwrap();
    assert!(
        PluginStore::open(
            root.path().join("linked"),
            Arc::new(SecretRegistry::default()),
            StoreLimits::default()
        )
        .is_err()
    );
    let store = store(&root);
    let (_source, package) = package("io.saya.test", "guide", None);
    store.install(&package, source()).unwrap();
    let index = store.protected_path().join("index.json");
    let target = outside.path().join("target");
    fs::write(&target, b"untouched").unwrap();
    fs::remove_file(&index).unwrap();
    symlink(&target, &index).unwrap();
    assert!(store.list().is_err());
    assert_eq!(fs::read(&target).unwrap(), b"untouched");
    fs::remove_file(&index).unwrap();
    fs::hard_link(&target, &index).unwrap();
    assert!(store.list().is_err());
    fs::remove_file(&index).unwrap();
    let original = root.path().join("moved");
    fs::rename(store.protected_path(), &original).unwrap();
    fs::create_dir(store.protected_path()).unwrap();
    assert!(store.list().is_err());
}

#[test]
fn secret_metadata_and_store_capacity_fail_without_creating_published_records() {
    let root = private_root();
    let secrets = Arc::new(SecretRegistry::default());
    secrets.register(&["protected-value-12345".into()]).unwrap();
    let store = PluginStore::open(
        root.path().join("plugins"),
        secrets,
        StoreLimits {
            max_objects: 1,
            ..StoreLimits::default()
        },
    )
    .unwrap();
    let (source_root, _old) = package("io.saya.test", "guide", None);
    let text = fs::read_to_string(source_root.path().join(MANIFEST_NAME))
        .unwrap()
        .replace("A private local package.", "protected-value-12345");
    fs::write(source_root.path().join(MANIFEST_NAME), text).unwrap();
    let sensitive = inspect_package(source_root.path()).unwrap();
    let failure = store.stage(&sensitive, source()).unwrap_err();
    assert!(!failure.to_string().contains("protected-value-12345"));
    assert!(store.list().unwrap().is_empty());
    let (_good_source, good) = package("io.saya.test", "good", None);
    let (_other_source, other) = package("io.saya.other", "other", None);
    store.install(&good, source()).unwrap();
    assert!(store.install(&other, source()).is_err());
    assert_eq!(store.list().unwrap().len(), 1);
}

#[test]
fn recovery_reclaims_unpublished_objects_and_unowned_unindexed_lease_files() {
    let root = private_root();
    let store = store(&root);
    let (_source, package) = package("io.saya.test", "unpublished", None);
    {
        let _lock = store.inner.disk.lock_index().unwrap();
        store.inner.disk.stage_object(&package).unwrap();
        let id = Uuid::now_v7().to_string();
        let file = store.inner.disk.create_lease(&id).unwrap();
        drop(file);
    }
    assert!(store.inner.disk.package_path(package.digest()).exists());
    assert!(store.recover().unwrap().is_empty());
    assert!(!store.inner.disk.package_path(package.digest()).exists());
    assert_eq!(
        fs::read_dir(store.protected_path().join("leases"))
            .unwrap()
            .count(),
        0
    );
    store.install(&package, source()).unwrap();
    assert_eq!(store.list().unwrap().len(), 1);
}

#[test]
fn explicit_dependency_disable_closes_transitive_scheduling_and_requests_cross_id_cleanup() {
    let root = private_root();
    let store = store(&root);
    let (_dependency_source, dependency) = package("io.saya.dep", "dependency", None);
    let (_parent_source, parent) = package("io.saya.test", "parent", Some(("io.saya.dep", "^1.0")));
    store.install(&dependency, source()).unwrap();
    store.install(&parent, source()).unwrap();
    enable(&store, &parent, PluginScope::User);
    let first_workspace = tempfile::tempdir().unwrap();
    let other_workspace = tempfile::tempdir().unwrap();
    let first = acquire(&store, first_workspace.path());
    let other = acquire(&store, other_workspace.path());
    let scope = PluginScope::workspace(first_workspace.path()).unwrap();
    let disabled = store.disable("io.saya.dep", Some(&scope)).unwrap();
    assert_eq!(disabled.cleanup_pending.len(), 1);
    assert_eq!(disabled.cleanup_pending[0].plugin_id, "io.saya.test");
    assert_eq!(
        disabled.cleanup_pending[0].state,
        InstanceState::StopRequested
    );
    assert!(first.check_active().is_err());
    assert!(first.verify_contents().is_err());
    assert!(other.verify_contents().is_ok());
    assert!(
        store
            .acquire(
                "io.saya.test",
                first_workspace.path(),
                SessionId::new(),
                RunId::new(),
                "0.2.0",
                &features()
            )
            .is_err()
    );
    enable(&store, &dependency, scope);
    assert!(first.check_active().is_err());
    let replacement = acquire(&store, first_workspace.path());
    first.finish(CleanupOutcome::Cleaned).unwrap();
    let disabled = store.disable("io.saya.dep", None).unwrap();
    assert_eq!(disabled.cleanup_pending.len(), 2);
    assert!(other.check_active().is_err());
    assert!(replacement.verify_contents().is_err());
    assert!(
        store
            .acquire(
                "io.saya.test",
                other_workspace.path(),
                SessionId::new(),
                RunId::new(),
                "0.2.0",
                &features()
            )
            .is_err()
    );
    other.finish(CleanupOutcome::Cleaned).unwrap();
    replacement.finish(CleanupOutcome::Cleaned).unwrap();
    assert!(store.inspect(dependency.digest()).is_ok());
}

#[test]
fn updating_dependency_selection_does_not_revoke_an_existing_fixed_closure() {
    let root = private_root();
    let store = store(&root);
    let (_old_source, old) = package("io.saya.dep", "old dependency", None);
    let (_new_source, new) = package("io.saya.dep", "new dependency", None);
    let (_parent_source, parent) = package("io.saya.test", "parent", Some(("io.saya.dep", "^1.0")));
    store.install(&old, source()).unwrap();
    enable(&store, &old, PluginScope::User);
    store.install(&parent, source()).unwrap();
    enable(&store, &parent, PluginScope::User);
    let lease = acquire(&store, root.path());
    store.update(old.digest(), &new, source()).unwrap();
    assert!(lease.verify_contents().is_ok());
    assert_eq!(lease.record().dependency_digests, [old.digest()]);
    assert!(
        store
            .inspect(parent.digest())
            .unwrap()
            .lock
            .unwrap()
            .packages
            .iter()
            .any(|pin| pin.digest == old.digest())
    );
    let disabled = store.disable("io.saya.dep", None).unwrap();
    assert_eq!(disabled.cleanup_pending.len(), 1);
    assert!(lease.check_active().is_err());
    lease.finish(CleanupOutcome::Cleaned).unwrap();
}
